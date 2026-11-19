//! Refreshing the catalog cache on the Kindle: the index, each listing, and its icon and screenshots, from the
//! catalog's published site. The source of the files is given by the caller (the device fetches over HTTPS; the tests use
//! a fake). An app that fails keeps the copy it had; an app the catalog no longer lists is removed.
//!
//! Cache layout (under `launcher/store/cache`): `index.json`, `apps/<id>.json`, `icons/<id>.png`, `shots/<id>-<n>.png`.

use crate::catalog::{valid_app_id, Card, CollectionIndex, Index, Listing};
use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::Path;

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
fn fetch_app(base: &str, cache: &Path, card: &Card, source: &dyn Source) -> Result<(), String> {
    let text = source.text(&format!("{base}/apps/{}.json", card.id))?;
    let listing: Listing = serde_json::from_str(&text).map_err(|e| format!("the listing is not valid: {e}"))?;
    if listing.schema != 1 || listing.id != card.id {
        return Err("the listing is for another app".into());
    }
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
    write_atomic(&cache.join("apps").join(format!("{}.json", card.id)), text.as_bytes()).map_err(|e| e.to_string())?;
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
    let base = base.trim_end_matches('/');
    let index_text = source.text(&format!("{base}/index.json"))?;
    if index_text.len() as u64 > MAX_INDEX {
        return Err("the index is too large".into());
    }
    let index: Index = serde_json::from_str(&index_text).map_err(|e| format!("the index is not valid: {e}"))?;
    if index.schema != 1 {
        return Err(format!("the index has schema {}, this store reads schema 1", index.schema));
    }

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
        match fetch_app(base, cache, card, source) {
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
    write_atomic(&cache.join("index.json"), index_text.as_bytes()).map_err(|e| e.to_string())?;
    Ok(report)
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
