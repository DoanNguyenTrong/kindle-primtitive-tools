//! Refreshing the catalog cache on the Kindle: the index, each listing, and its icon and screenshots, from the
//! catalog's published site. The source of the files is given by the caller (the device fetches over HTTPS; the tests use
//! a fake). An app that fails keeps the copy it had; an app the catalog no longer lists is removed.
//!
//! Cache layout (under `launcher/store/cache`): `index.json`, `apps/<id>.json`, `icons/<id>.png`, `shots/<id>-<n>.png`.

use crate::catalog::{valid_app_id, Asset, Card, CollectionIndex, Index, Listing};
use crate::signatures::{self, PackageIdentity, TrustedKey, VerifiedIndex};
use std::collections::BTreeSet;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Where the files come from. `text` reads a small document; `bytes` reads a picture, refused past `max` bytes.
pub trait Source {
    fn text(&self, url: &str) -> Result<String, String>;
    fn bytes(&self, url: &str, max: u64) -> Result<Vec<u8>, String>;
}

pub const MAX_INDEX: u64 = 1024 * 1024;
pub const MAX_LISTING: u64 = 256 * 1024;
pub const MAX_PICTURE: u64 = 1024 * 1024;
/// Screenshots shown per app (the product page has room for two).
pub const SHOTS_PER_APP: usize = 2;
const SIGNED_CURRENT: &str = "signed-current";

fn read_limited(path: &Path, max: u64) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    fs::File::open(path)?.take(max + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "the cached catalog file is too large"));
    }
    Ok(bytes)
}

fn index_from_bytes(bytes: &[u8]) -> Result<Index, String> {
    if bytes.len() as u64 > MAX_INDEX {
        return Err("the index is too large".into());
    }
    let index: Index = serde_json::from_slice(bytes).map_err(|e| format!("the index is not valid: {e}"))?;
    if index.schema != 1 {
        return Err(format!("the index has schema {}, this store reads schema 1", index.schema));
    }
    Ok(index)
}

fn verified_index(key: &TrustedKey, bytes: &[u8], sidecar: &[u8]) -> Result<(Index, VerifiedIndex), String> {
    let proof = key.verify_index(bytes, sidecar).map_err(|error| error.to_string())?;
    let index = index_from_bytes(bytes)?;
    let mut ids = BTreeSet::new();
    for card in &index.apps {
        if !valid_app_id(&card.id) || !ids.insert(&card.id) || !proof.contains_listing(&card.id) {
            return Err("the signed index has an invalid, duplicate, or unsigned app id".into());
        }
    }
    if proof.listing_count() != ids.len() {
        return Err("the signature contains listings absent from the index".into());
    }
    Ok((index, proof))
}

pub struct CachedCatalog {
    directory: PathBuf,
    proof: Option<VerifiedIndex>,
}

impl CachedCatalog {
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    pub fn listing(&self, id: &str) -> Result<Listing, String> {
        if !valid_app_id(id) {
            return Err("the store listing has an invalid app id".into());
        }
        let path = self.directory.join("apps").join(format!("{id}.json"));
        let bytes = read_limited(&path, MAX_LISTING).map_err(|error| error.to_string())?;
        parse_listing(id, &bytes, self.proof.as_ref())
    }
}

pub fn cached_catalog(cache: &Path, key: Option<&TrustedKey>) -> Result<CachedCatalog, String> {
    let Some(key) = key else {
        return Ok(CachedCatalog { directory: cache.to_path_buf(), proof: None });
    };
    let directory = read_current_snapshot(cache)?.ok_or("refresh the signed catalog first")?;
    let index = read_limited(&directory.join("index.json"), MAX_INDEX).map_err(|error| error.to_string())?;
    let sidecar = read_limited(&directory.join(signatures::INDEX_SIGNATURE_FILENAME), signatures::MAX_INDEX_SIDECAR_BYTES)
        .map_err(|error| error.to_string())?;
    let (_, proof) = verified_index(key, &index, &sidecar)?;
    Ok(CachedCatalog { directory, proof: Some(proof) })
}

fn snapshot_name(name: &str) -> bool {
    name.strip_prefix("signed-").is_some_and(|value| {
        value.split_once('-').is_some_and(|(pid, stamp)| {
            !pid.is_empty() && !stamp.is_empty() && pid.bytes().chain(stamp.bytes()).all(|byte| byte.is_ascii_digit())
        })
    })
}

fn read_current_snapshot(cache: &Path) -> Result<Option<PathBuf>, String> {
    let bytes = match read_limited(&cache.join(SIGNED_CURRENT), 128) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    let name = std::str::from_utf8(&bytes).map_err(|error| error.to_string())?.trim_end();
    if !snapshot_name(name) {
        return Err("the signed catalog cache has an invalid snapshot name".into());
    }
    Ok(Some(cache.join(name)))
}

fn fetch_index_proof(base: &str, source: &dyn Source) -> Result<(Vec<u8>, Vec<u8>), String> {
    let bytes = source.bytes(&format!("{base}/index.json"), MAX_INDEX)?;
    let sidecar = source.bytes(&format!("{base}/{}", signatures::INDEX_SIGNATURE_FILENAME), signatures::MAX_INDEX_SIDECAR_BYTES)?;
    Ok((bytes, sidecar))
}

fn parse_listing(id: &str, bytes: &[u8], proof: Option<&VerifiedIndex>) -> Result<Listing, String> {
    if bytes.len() as u64 > MAX_LISTING {
        return Err("the listing is too large".into());
    }
    if let Some(proof) = proof {
        proof.verify_listing(id, bytes).map_err(|error| error.to_string())?;
    }
    let listing: Listing = serde_json::from_slice(bytes).map_err(|e| format!("the listing is not valid: {e}"))?;
    if listing.schema != 1 || listing.id != id {
        return Err("the listing is for another app".into());
    }
    Ok(listing)
}

/// Loads one listing for Launcher bootstrap. Signed mode verifies the index before using any listing fields.
pub fn fetch_listing(base: &str, id: &str, source: &dyn Source, key: Option<&TrustedKey>) -> Result<Listing, String> {
    if !valid_app_id(id) {
        return Err("the app id is invalid".into());
    }
    let base = base.trim_end_matches('/');
    let proof = if let Some(key) = key {
        let (index, signature) = fetch_index_proof(base, source)?;
        let (index, proof) = verified_index(key, &index, &signature)?;
        if !index.apps.iter().any(|card| card.id == id) {
            return Err("the app is absent from the signed index".into());
        }
        Some(proof)
    } else {
        None
    };
    let listing = source.bytes(&format!("{base}/apps/{id}.json"), MAX_LISTING)?;
    parse_listing(id, &listing, proof.as_ref())
}

/// Verifies the release signature over the expected ZIP identity. The download still checks its size and SHA-256.
pub fn verify_asset_signature(source: &dyn Source, key: &TrustedKey, id: &str, version: &str, arch: &str, asset: &Asset) -> Result<(), String> {
    let url = signatures::package_signature_url(&asset.url).map_err(|error| error.to_string())?;
    let signature = source.bytes(&url, signatures::MAX_SIGNATURE_BYTES)?;
    let signature = std::str::from_utf8(&signature).map_err(|_| "the package signature is not text")?;
    key.verify_package(PackageIdentity { id, version, arch, sha256: &asset.sha256, size: asset.size }, signature.trim())
        .map_err(|error| error.to_string())
}

/// Remove the offline catalog while preserving the installed app record and package staging.
pub fn clear_cache(launcher: &Path) -> io::Result<()> {
    match fs::remove_dir_all(launcher.join("store/cache")) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// Apps whose files were refreshed.
    pub refreshed: usize,
    /// Apps that failed, with the reason; their cached copy is kept.
    pub failed: Vec<(String, String)>,
    /// Cached files of apps the catalog no longer lists, removed.
    pub removed: usize,
}

fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("part");
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, path)
}

fn is_png(bytes: &[u8]) -> bool {
    bytes.len() > 8 && bytes[..8] == [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]
}

/// Fetches one app's files and writes them, all or nothing: any failure leaves the app's cached files as they were.
fn fetch_app(base: &str, cache: &Path, card: &Card, source: &dyn Source, proof: Option<&VerifiedIndex>) -> Result<(), String> {
    let bytes = source.bytes(&format!("{base}/apps/{}.json", card.id), MAX_LISTING)?;
    let listing = parse_listing(&card.id, &bytes, proof)?;
    // The icon comes from this site's own folder, as the screenshots do, whatever URL the index gives.
    let icon = source.bytes(&format!("{base}/icons/{}.png", card.id), MAX_PICTURE)?;
    if !is_png(&icon) {
        return Err("the icon is not a PNG".into());
    }
    let shots_count = listing.screenshots.len().min(SHOTS_PER_APP);
    let mut shots = Vec::new();
    for n in 0..shots_count {
        let bytes = source.bytes(&format!("{base}/shots/{}-{n}.png", card.id), MAX_PICTURE)?;
        if !is_png(&bytes) {
            return Err(format!("screenshot {} is not a PNG", n + 1));
        }
        shots.push((n, bytes));
    }
    write_atomic(&cache.join("apps").join(format!("{}.json", card.id)), &bytes).map_err(|e| e.to_string())?;
    write_atomic(&cache.join("icons").join(format!("{}.png", card.id)), &icon).map_err(|e| e.to_string())?;
    for (n, bytes) in &shots {
        write_atomic(&cache.join("shots").join(format!("{}-{n}.png", card.id)), bytes).map_err(|e| e.to_string())?;
    }
    // Screenshots the app no longer has.
    for n in shots_count..SHOTS_PER_APP {
        let _ = fs::remove_file(cache.join("shots").join(format!("{}-{n}.png", card.id)));
    }
    Ok(())
}

/// The id of a cached file: `apps/<id>.json`, `icons/<id>.png`, or `shots/<id>-<n>.png`.
fn cached_id(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    let parent = path.parent()?.file_name()?.to_str()?;
    match parent {
        "shots" => stem.rsplit_once('-').map(|(id, _)| id.to_string()),
        _ => Some(stem.to_string()),
    }
}

/// Refreshes the cache from the catalog at `base` (its `v1` site). Returns what happened to each app; an error only when
/// the index itself cannot be read, in which case nothing is changed.
pub fn refresh(base: &str, cache: &Path, source: &dyn Source) -> Result<Report, String> {
    refresh_with_key(base, cache, source, None)
}

pub fn refresh_with_key(base: &str, cache: &Path, source: &dyn Source, key: Option<&TrustedKey>) -> Result<Report, String> {
    let base = base.trim_end_matches('/');
    if let Some(key) = key {
        return refresh_signed(base, cache, source, key);
    }
    let index_text = source.text(&format!("{base}/index.json"))?;
    let index = index_from_bytes(index_text.as_bytes())?;
    let report = refresh_files(base, cache, source, &index, None);
    write_atomic(&cache.join("index.json"), index_text.as_bytes()).map_err(|e| e.to_string())?;
    Ok(report)
}

fn refresh_signed(base: &str, cache: &Path, source: &dyn Source, key: &TrustedKey) -> Result<Report, String> {
    let (raw_index, signature) = fetch_index_proof(base, source)?;
    let (index, proof) = verified_index(key, &raw_index, &signature)?;
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    let name = format!("signed-{}-{stamp}", std::process::id());
    fs::create_dir_all(cache).map_err(|error| error.to_string())?;
    let pending = cache.join(&name);
    fs::create_dir(&pending).map_err(|error| error.to_string())?;

    let pointer = pending.join(SIGNED_CURRENT);
    let mut previous = None;
    let publish = (|| {
        let report = refresh_files(base, &pending, source, &index, Some(&proof));
        if let Some((id, reason)) = report.failed.first() {
            return Err(format!("the signed catalog was not replaced. {id}: {reason}"));
        }
        write_atomic(&pending.join("index.json"), &raw_index).map_err(|error| error.to_string())?;
        write_atomic(&pending.join(signatures::INDEX_SIGNATURE_FILENAME), &signature).map_err(|error| error.to_string())?;
        fs::write(&pointer, name.as_bytes()).map_err(|error| error.to_string())?;
        previous = read_current_snapshot(cache).ok().flatten();
        fs::rename(&pointer, cache.join(SIGNED_CURRENT)).map_err(|error| error.to_string())?;
        Ok(report)
    })();
    if publish.is_err() {
        let _ = fs::remove_dir_all(pending);
    } else if let Some(previous) = previous.filter(|path| *path != pending) {
        let _ = fs::remove_dir_all(previous);
    }
    publish
}

fn refresh_files(base: &str, cache: &Path, source: &dyn Source, index: &Index, proof: Option<&VerifiedIndex>) -> Report {

    let mut report = Report::default();
    let mut listed: BTreeSet<String> = BTreeSet::new();
    for card in &index.apps {
        if !valid_app_id(&card.id) {
            report.failed.push((card.id.clone(), "the app id is invalid".to_string()));
            continue;
        }
        if !listed.insert(card.id.clone()) {
            continue;
        }
        match fetch_app(base, cache, card, source, proof) {
            Ok(()) => report.refreshed += 1,
            Err(reason) => report.failed.push((card.id.clone(), reason)),
        }
    }

    // Older catalog deployments have no collections endpoint.
    if let Ok(text) = source.text(&format!("{base}/collections.json")) {
        if text.len() as u64 <= MAX_LISTING
            && serde_json::from_str::<CollectionIndex>(&text).is_ok_and(|index| index.schema == 1)
        {
            let _ = write_atomic(&cache.join("collections.json"), text.as_bytes());
        }
    }

    // Remove what the catalog no longer lists.
    for folder in ["apps", "icons", "shots"] {
        let Ok(read) = fs::read_dir(cache.join(folder)) else { continue };
        for entry in read.flatten() {
            let path = entry.path();
            if let Some(id) = cached_id(&path) {
                if !listed.contains(&id) && fs::remove_file(&path).is_ok() {
                    report.removed += 1;
                }
            }
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// A catalog in memory: documents by URL; a URL missing from the map is a failed fetch.
    #[derive(Default)]
    struct Fake {
        docs: HashMap<String, Vec<u8>>,
    }

    impl Source for Fake {
        fn text(&self, url: &str) -> Result<String, String> {
            self.docs.get(url).map(|b| String::from_utf8_lossy(b).into_owned()).ok_or_else(|| format!("not found: {url}"))
        }
        fn bytes(&self, url: &str, max: u64) -> Result<Vec<u8>, String> {
            let b = self.docs.get(url).ok_or_else(|| format!("not found: {url}"))?;
            if b.len() as u64 > max {
                return Err("too large".into());
            }
            Ok(b.clone())
        }
    }

    const BASE: &str = "https://site.example/v1";
    const PNG: &[u8] = b"\x89PNG\r\n\x1a\nrest-of-a-picture";

    fn listing(id: &str, shots: usize) -> String {
        let screenshots: Vec<String> = (1..=shots).map(|n| format!("shots/{n}.png")).collect();
        format!(
            r#"{{"schema":1,"id":"{id}","name":"{id}","subtitle":"A test app","developer":{{"name":"Test"}},"categories":["utilities"],"description":"A test app for the refresh.","repo":"https://github.com/x/{id}","licence":"AGPL-3.0-only","source":"link","icon":"icon.png","screenshots":{},"changes":{{"network":false,"stops_kindle_ui":false,"starts_at_boot":false,"root_filesystem":false,"hooks":false}},"versions":[{{"version":"1.0.0","date":"2026-11-19","notes":"n","firmware":{{"min":"5.16.3"}},"tested_on":[]}}]}}"#,
            serde_json::to_string(&screenshots).unwrap()
        )
    }

    fn index(ids: &[&str]) -> String {
        let apps: Vec<String> = ids
            .iter()
            .map(|id| {
                format!(
                    r#"{{"id":"{id}","name":"{id}","subtitle":"A test app","categories":["utilities"],"icon":"{BASE}/icons/{id}.png","source":"link","latest_version":"1.0.0","arch":[],"updated_at":"2026-11-19"}}"#
                )
            })
            .collect();
        format!(r#"{{"schema":1,"generated_at":"2026-11-19T10:00:00Z","categories":[],"apps":[{}]}}"#, apps.join(","))
    }

    fn site(ids: &[&str], shots_per_app: usize) -> Fake {
        let mut f = Fake::default();
        f.docs.insert(format!("{BASE}/index.json"), index(ids).into_bytes());
        for id in ids {
            f.docs.insert(format!("{BASE}/apps/{id}.json"), listing(id, shots_per_app).into_bytes());
            f.docs.insert(format!("{BASE}/icons/{id}.png"), PNG.to_vec());
            for n in 0..shots_per_app {
                f.docs.insert(format!("{BASE}/shots/{id}-{n}.png"), PNG.to_vec());
            }
        }
        f
    }

    fn cache(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("kl-refresh-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    fn sign_site(site: &mut Fake, ids: &[&str]) -> TrustedKey {
        use ring::signature::{Ed25519KeyPair, KeyPair};
        let key = Ed25519KeyPair::from_seed_unchecked(&[37; 32]).unwrap();
        let index = &site.docs[&format!("{BASE}/index.json")];
        let listings = ids.iter().map(|id| {
            ((*id).to_owned(), site.docs[&format!("{BASE}/apps/{id}.json")].clone())
        }).collect();
        let signature = signatures::sign_index(&key, index, &listings).unwrap();
        site.docs.insert(format!("{BASE}/{}", signatures::INDEX_SIGNATURE_FILENAME), signature);
        TrustedKey::from_hex(&signatures::encode_hex(key.public_key().as_ref())).unwrap()
    }

    #[test]
    fn a_signed_refresh_publishes_only_a_complete_snapshot() {
        let dir = cache("signed-snapshot");
        let mut first = site(&["notes"], 0);
        refresh(BASE, &dir, &first).unwrap();
        let key = sign_site(&mut first, &["notes"]);
        assert!(cached_catalog(&dir, Some(&key)).is_err(), "unsigned cache cannot satisfy the policy");

        refresh_with_key(BASE, &dir, &first, Some(&key)).unwrap();
        let accepted = cached_catalog(&dir, Some(&key)).unwrap();
        assert_eq!(accepted.listing("notes").unwrap().id, "notes");
        let mut next = site(&["notes", "words"], 0);
        sign_site(&mut next, &["notes", "words"]);
        next.docs.get_mut(&format!("{BASE}/apps/words.json")).unwrap().push(b' ');
        assert!(refresh_with_key(BASE, &dir, &next, Some(&key)).is_err());
        assert_eq!(cached_catalog(&dir, Some(&key)).unwrap().directory(), accepted.directory());

        let offline = accepted.directory().join("apps/notes.json");
        let mut tampered = fs::read(&offline).unwrap();
        tampered.push(b' ');
        fs::write(offline, tampered).unwrap();
        assert!(accepted.listing("notes").is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn signed_bootstrap_rejects_a_replaced_listing_and_missing_index_proof() {
        let mut site = site(&["notes"], 0);
        let key = sign_site(&mut site, &["notes"]);
        assert_eq!(fetch_listing(BASE, "notes", &site, Some(&key)).unwrap().id, "notes");
        site.docs.get_mut(&format!("{BASE}/apps/notes.json")).unwrap().push(b' ');
        assert!(fetch_listing(BASE, "notes", &site, Some(&key)).is_err());
        assert!(fetch_listing(BASE, "notes", &site, None).is_ok());
        site.docs.remove(&format!("{BASE}/{}", signatures::INDEX_SIGNATURE_FILENAME));
        assert!(fetch_listing(BASE, "notes", &site, Some(&key)).is_err());
    }

    #[test]
    fn a_fresh_refresh_writes_the_index_listings_icons_and_screenshots() {
        let c = cache("fresh");
        let r = refresh(BASE, &c, &site(&["notes", "words"], 2)).unwrap();
        assert_eq!(r, Report { refreshed: 2, failed: vec![], removed: 0 });
        assert!(c.join("index.json").exists());
        assert!(c.join("apps/notes.json").exists());
        assert!(c.join("icons/words.png").exists());
        assert!(c.join("shots/notes-0.png").exists() && c.join("shots/notes-1.png").exists());
        let _ = fs::remove_dir_all(c);
    }

    #[test]
    fn an_app_that_fails_keeps_its_cached_copy_and_the_others_refresh() {
        let c = cache("partial");
        refresh(BASE, &c, &site(&["notes", "words"], 0)).unwrap();
        let mut broken = site(&["notes", "words"], 0);
        broken.docs.remove(&format!("{BASE}/icons/notes.png"));
        broken.docs.insert(format!("{BASE}/apps/words.json"), listing("words", 0).replace("1.0.0", "2.0.0").into_bytes());
        let r = refresh(BASE, &c, &broken).unwrap();
        assert_eq!(r.refreshed, 1);
        assert_eq!(r.failed.len(), 1);
        assert_eq!(r.failed[0].0, "notes");
        assert!(c.join("apps/notes.json").exists(), "the failed app keeps its cached listing");
        let words = fs::read_to_string(c.join("apps/words.json")).unwrap();
        assert!(words.contains("2.0.0"));
        let _ = fs::remove_dir_all(c);
    }

    #[test]
    fn an_app_the_catalog_drops_is_removed_from_the_cache() {
        let c = cache("drop");
        refresh(BASE, &c, &site(&["notes", "words"], 1)).unwrap();
        let r = refresh(BASE, &c, &site(&["notes"], 1)).unwrap();
        assert_eq!(r.refreshed, 1);
        assert!(r.removed >= 3, "listing, icon and screenshot of the dropped app: {r:?}");
        assert!(!c.join("apps/words.json").exists());
        assert!(!c.join("icons/words.png").exists());
        assert!(!c.join("shots/words-0.png").exists());
        let _ = fs::remove_dir_all(c);
    }

    #[test]
    fn a_bad_index_changes_nothing() {
        let c = cache("bad");
        refresh(BASE, &c, &site(&["notes"], 0)).unwrap();
        let before = fs::read(c.join("index.json")).unwrap();
        let mut f = site(&["notes"], 0);
        f.docs.insert(format!("{BASE}/index.json"), b"{ not json".to_vec());
        assert!(refresh(BASE, &c, &f).is_err());
        assert_eq!(fs::read(c.join("index.json")).unwrap(), before);
        let mut other = site(&["notes"], 0);
        other.docs.insert(format!("{BASE}/index.json"), index(&["notes"]).replace("\"schema\":1", "\"schema\":2").into_bytes());
        assert!(refresh(BASE, &c, &other).unwrap_err().contains("schema 2"));
        let _ = fs::remove_dir_all(c);
    }

    /// Reads the published site from disk: the URL `base/<path>` maps to `root/<path>`.
    struct Disk {
        root: std::path::PathBuf,
        base: String,
    }

    impl Source for Disk {
        fn text(&self, url: &str) -> Result<String, String> {
            let rel = url.strip_prefix(&self.base).ok_or("outside the site")?.trim_start_matches('/');
            fs::read_to_string(self.root.join(rel)).map_err(|e| e.to_string())
        }
        fn bytes(&self, url: &str, _max: u64) -> Result<Vec<u8>, String> {
            let rel = url.strip_prefix(&self.base).ok_or("outside the site")?.trim_start_matches('/');
            fs::read(self.root.join(rel)).map_err(|e| e.to_string())
        }
    }

    /// The catalog's own build output, when `KL_TEST_SITE` names its `site/v1` folder: the refresh reads it as the
    /// device will. Skipped otherwise. This is the check that the published layout and the device's URLs agree.
    #[test]
    fn refreshes_from_the_catalog_build_output() {
        let Some(site) = std::env::var_os("KL_TEST_SITE") else {
            return;
        };
        let base = "https://doannguyentrong.github.io/kindle-primitive-catalog/v1".to_string();
        let source = Disk { root: std::path::PathBuf::from(site), base: base.clone() };
        let c = cache("catalog");
        let r = refresh(&base, &c, &source).expect("the index reads");
        assert!(r.failed.is_empty(), "every app refreshes from the build output: {:?}", r.failed);
        assert!(r.refreshed >= 1);
        let _ = fs::remove_dir_all(c);
    }

    #[test]
    fn a_picture_that_is_not_a_png_is_refused() {
        let c = cache("notpng");
        let mut f = site(&["notes"], 0);
        f.docs.insert(format!("{BASE}/icons/notes.png"), b"<html>".to_vec());
        let r = refresh(BASE, &c, &f).unwrap();
        assert_eq!(r.refreshed, 0);
        assert!(r.failed[0].1.contains("not a PNG"));
        let _ = fs::remove_dir_all(c);
    }
}
