//! Installing an app from the store, from a downloaded package: choose the version for the device, check the package
//! against the listing (size, SHA-256, the manifest's id and version, the requirements), unpack it safely into staging, and
//! install it with the journal. The download itself is the caller's; this takes the file it wrote.

use crate::catalog::{Listing, Manifest};
use crate::install::{self, Paths, Source};
use crate::record;
use crate::resolve::{self, Device};
use crate::unpack::{self, Limits};
use crate::verify;
use crate::Version;
use std::fs;
use std::path::Path;

/// Installs the version of `listing` that suits `device`, from `zip` (already downloaded to the store's downloads folder).
/// Returns the installed version. On any failure nothing is installed, and the zip is removed.
pub fn install_downloaded(
    paths: &Paths,
    listing: &Listing,
    device: &Device,
    zip: &Path,
    source_url: &str,
    installed_at: &str,
) -> Result<String, String> {
    install_downloaded_with_progress(paths, listing, device, zip, source_url, installed_at, |_| {})
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Checking,
    Installing,
}

pub fn install_downloaded_with_progress(
    paths: &Paths,
    listing: &Listing,
    device: &Device,
    zip: &Path,
    source_url: &str,
    installed_at: &str,
    mut progress: impl FnMut(Stage),
) -> Result<String, String> {
    progress(Stage::Checking);
    let result = install_inner(paths, listing, device, zip, source_url, installed_at, &mut progress);
    let _ = fs::remove_file(zip);
    result
}

fn install_inner(
    paths: &Paths,
    listing: &Listing,
    device: &Device,
    zip: &Path,
    source_url: &str,
    installed_at: &str,
    progress: &mut impl FnMut(Stage),
) -> Result<String, String> {
    let chosen = resolve::pick_version(listing, device).map_err(|e| e.to_string())?;
    let asset = chosen
        .assets
        .get(&device.arch)
        .ok_or_else(|| format!("no package for this Kindle ({})", device.arch))?;
    verify::verify_file(zip, &asset.sha256, asset.size).map_err(|e| e.to_string())?;

    let manifest: Manifest = unpack::read_manifest(zip).map_err(|e| e.to_string())?;
    if manifest.id != listing.id {
        return Err("the package is for another app".into());
    }
    if manifest.version != chosen.version {
        return Err(format!("the package is version {}, the listing says {}", manifest.version, chosen.version));
    }
    if manifest.arch != device.arch {
        return Err("the package is for another kind of Kindle".into());
    }
    if manifest.schema != 1 || manifest.dir != listing.dir() || manifest.firmware != chosen.firmware
        || manifest.requires != chosen.requires
    {
        return Err("the package details do not match the catalog listing".into());
    }
    if !manifest.hooks.is_empty() {
        return Err("this store does not support packages with install steps".into());
    }

    let rec = record::load(&paths.record).map_err(|e| e.to_string())?;
    let missing = resolve::missing_requirements(&manifest.requires, &|id: &str| {
        rec.apps.get(id).and_then(|a| Version::parse(&a.version))
    });
    if let Some((id, range, have)) = missing.first() {
        return Err(match have {
            Some(v) => format!("needs {id} {range} (installed: {v:?})"),
            None => format!("needs {id} {range}, which is not installed"),
        });
    }

    let staging = paths.staging.join(&listing.id);
    let _ = fs::remove_dir_all(&staging);
    fs::create_dir_all(&staging).map_err(|e| e.to_string())?;
    unpack::unpack(zip, &staging, &manifest, Limits::default()).map_err(|e| e.to_string())?;
    let source = Source { manifest: &manifest, source_url, sha256: &asset.sha256, installed_at };
    progress(Stage::Installing);
    install::install(paths, &source, None).map_err(|e| e.to_string())?;
    Ok(manifest.version.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::sample_listing;
    use crate::resolve::Device;
    use crate::unpack::fixtures::{make_zip, scratch};

    /// A package for `id` version `version`, with one file in its own folder and the manifest inside.
    fn package(dir: &Path, id: &str, version: &str, requires: &str) -> (std::path::PathBuf, String, u64) {
        let manifest = format!(
            r#"{{"schema":1,"id":"{id}","dir":"{id}","version":"{version}","arch":"armhf","firmware":{{"min":"5.16.3"}},"requires":{requires},"files":["{id}/app.bin"],"hooks":[]}}"#
        );
        let top = format!("{id}-{version}");
        let zip = make_zip(
            &dir.join(format!("{id}-{version}.zip")),
            &[
                (&format!("{top}/"), b"", false),
                (&format!("{top}/manifest.json"), manifest.as_bytes(), false),
                (&format!("{top}/LICENSE"), b"license", false),
                (&format!("{top}/{id}/app.bin"), b"app bytes", false),
            ],
        );
        let bytes = fs::read(&zip).unwrap();
        let sha = {
            use sha2::{Digest, Sha256};
            Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect::<String>()
        };
        (zip, sha, bytes.len() as u64)
    }

    /// The sample listing (browser, version 0.2.18 and 0.2.17) with the asset of 0.2.18 set to this package.
    fn listing_for(sha: &str, size: u64, version: &str) -> Listing {
        let mut l = sample_listing();
        l.dir = None;
        let asset = l.versions[0].assets.get_mut("armhf").unwrap();
        asset.sha256 = sha.into();
        asset.size = size;
        l.versions[0].version = version.into();
        l
    }

    fn device() -> Device {
        Device { arch: "armhf".into(), firmware: Version::parse("5.18.2").unwrap() }
    }

    fn paths(name: &str) -> Paths {
        let root = scratch(name);
        Paths::under(&root)
    }

    #[test]
    fn installs_a_verified_package_and_records_it() {
        let p = paths("ok");
        let dl = p.root.join("launcher/store/downloads");
        fs::create_dir_all(&dl).unwrap();
        let (zip, sha, size) = package(&dl, "primitive-browser", "0.2.18", "{}");
        let listing = listing_for(&sha, size, "0.2.18");
        let v = install_downloaded(&p, &listing, &device(), &zip, "https://example.com/x.zip", "2026-11-19").unwrap();
        assert_eq!(v, "0.2.18");
        assert_eq!(fs::read(p.root.join("primitive-browser/app.bin")).unwrap(), b"app bytes");
        let rec = record::load(&p.record).unwrap();
        assert_eq!(rec.apps["primitive-browser"].version, "0.2.18");
        assert!(!zip.exists(), "the download is removed");
    }

    #[test]
    fn a_package_that_does_not_match_its_listing_installs_nothing() {
        let p = paths("mismatch");
        let dl = p.root.join("launcher/store/downloads");
        fs::create_dir_all(&dl).unwrap();
        let (zip, sha, size) = package(&dl, "primitive-browser", "0.2.18", "{}");
        let wrong_hash = listing_for(&"0".repeat(64), size, "0.2.18");
        assert!(install_downloaded(&p, &wrong_hash, &device(), &zip, "u", "d").unwrap_err().contains("SHA-256"));
        let (zip2, sha2, size2) = package(&dl, "primitive-browser", "0.2.18", "{}");
        let wrong_version = listing_for(&sha2, size2, "0.2.19");
        assert!(install_downloaded(&p, &wrong_version, &device(), &zip2, "u", "d").is_err());
        assert!(!p.root.join("primitive-browser").exists());
        assert!(record::load(&p.record).unwrap().apps.is_empty());
        let _ = sha;
    }

    #[test]
    fn a_requirement_that_is_not_installed_stops_the_install() {
        let p = paths("requires");
        let dl = p.root.join("launcher/store/downloads");
        fs::create_dir_all(&dl).unwrap();
        let (zip, sha, size) = package(&dl, "primitive-browser", "0.2.18", r#"{"kterm":">=2.6"}"#);
        let mut listing = listing_for(&sha, size, "0.2.18");
        listing.versions[0].requires.insert("kterm".into(), ">=2.6".into());
        let err = install_downloaded(&p, &listing, &device(), &zip, "u", "d").unwrap_err();
        assert!(err.contains("needs kterm"), "{err}");
        assert!(!p.root.join("primitive-browser").exists());
    }

    #[test]
    fn a_kindle_without_a_package_is_refused_with_its_architecture() {
        let p = paths("arch");
        let dl = p.root.join("launcher/store/downloads");
        fs::create_dir_all(&dl).unwrap();
        let (zip, sha, size) = package(&dl, "primitive-browser", "0.2.18", "{}");
        let listing = listing_for(&sha, size, "0.2.18");
        let soft = Device { arch: "armel".into(), firmware: Version::parse("5.18.2").unwrap() };
        assert!(install_downloaded(&p, &listing, &soft, &zip, "u", "d").unwrap_err().contains("armel"));
    }
}
