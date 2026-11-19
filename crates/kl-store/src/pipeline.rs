//! Installing an app from the store, from a downloaded package: choose the version for the device, check the package
//! against the listing (size, SHA-256, the manifest's id and version, the requirements), unpack it safely into staging, and
//! install it with the journal. The download itself is the caller's; this takes the file it wrote.

use crate::catalog::{valid_app_id, Listing, Manifest};
use crate::hooks::{HookRunner, HookScripts};
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

pub struct DownloadedPackage<'a> {
    pub paths: &'a Paths,
    pub listing: &'a Listing,
    pub device: &'a Device,
    pub zip: &'a Path,
    pub source_url: &'a str,
    pub installed_at: &'a str,
}

pub fn install_downloaded_with_progress(
    paths: &Paths,
    listing: &Listing,
    device: &Device,
    zip: &Path,
    source_url: &str,
    installed_at: &str,
    progress: impl FnMut(Stage),
) -> Result<String, String> {
    install_package(DownloadedPackage { paths, listing, device, zip, source_url, installed_at }, None, progress)
}

/// Use a reviewer-backed runner for an install or for updates of a previously reviewed app.
pub fn install_downloaded_reviewed(
    package: DownloadedPackage<'_>,
    runner: &dyn HookRunner,
    progress: impl FnMut(Stage),
) -> Result<String, String> {
    install_package(package, Some(runner), progress)
}

fn install_package(
    package: DownloadedPackage<'_>,
    runner: Option<&dyn HookRunner>,
    mut progress: impl FnMut(Stage),
) -> Result<String, String> {
    progress(Stage::Checking);
    let zip = package.zip;
    let result = install_inner(package, runner, &mut progress);
    let _ = fs::remove_file(zip);
    result
}

fn install_inner(
    package: DownloadedPackage<'_>,
    runner: Option<&dyn HookRunner>,
    progress: &mut impl FnMut(Stage),
) -> Result<String, String> {
    let DownloadedPackage { paths, listing, device, zip, source_url, installed_at } = package;
    install::require_idle(paths).map_err(|error| error.to_string())?;
    let chosen = resolve::pick_version(listing, device).map_err(|e| e.to_string())?;
    if listing.schema != 1 || !valid_app_id(&listing.id) || !unpack::valid_module_dir(listing.dir()) {
        return Err("the catalog listing has an invalid app name or folder".into());
    }
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
    let has_hooks = !manifest.hooks.is_empty();
    if has_hooks != listing.changes.hooks {
        return Err("the package and catalog disagree on custom install steps".into());
    }
    if has_hooks && (runner.is_none() || matches!(manifest.id.as_str(), install::LAUNCHER_ID | "primitive-store")) {
        return Err("this package cannot use reviewed install steps".into());
    }

    let rec = record::load(&paths.record).map_err(|e| e.to_string())?;
    if let Some(already_installed) = rec.apps.get(&listing.id) {
        if Version::parse(&already_installed.version) > Version::parse(&chosen.version) {
            return Err("a newer version is already installed".into());
        }
    }
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
    let unpacked = if has_hooks {
        unpack::unpack_reviewed(zip, &staging, &manifest, Limits::default())
    } else {
        unpack::unpack(zip, &staging, &manifest, Limits::default()).map(|_| HookScripts::default())
    };
    let scripts = unpacked.map_err(|error| {
        let _ = fs::remove_dir_all(&staging);
        error.to_string()
    })?;
    let source = Source { manifest: &manifest, source_url, sha256: &asset.sha256, installed_at };
    progress(Stage::Installing);
    let result = if manifest.id == install::LAUNCHER_ID {
        install::prepare_launcher_update(paths, &source).map_err(|error| error.to_string())
    } else if let Some(runner) = runner {
        install::install_with_hooks(paths, &source, scripts, runner, None).map_err(|error| error.to_string())
    } else {
        install::install(paths, &source, None).map_err(|error| error.to_string())
    };
    if let Err(error) = result {
        if install::require_idle(paths).is_ok() {
            let _ = fs::remove_dir_all(&staging);
        }
        return Err(error);
    }
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

    #[test]
    fn a_reviewed_archive_stages_and_runs_only_its_declared_hooks() {
        use crate::hooks::{verified_bytes, HookPhase, POST_INSTALL_PATH, PRE_UNINSTALL_PATH};
        use std::{cell::RefCell, io};
        struct FixtureRunner(RefCell<Vec<HookPhase>>);
        impl HookRunner for FixtureRunner {
            fn approve(&self, package: &record::Installed) -> io::Result<()> {
                assert_eq!(package.id, "primitive-browser");
                assert!(!package.hooks.is_empty());
                Ok(())
            }

            fn run(&self, package: &record::Installed, phase: HookPhase, script: &Path) -> io::Result<()> {
                assert_eq!(verified_bytes(script, package.hooks.digest(phase).unwrap())?, b"#!/bin/sh\nexit 0\n");
                self.0.borrow_mut().push(phase);
                Ok(())
            }
        }

        let paths = paths("reviewed-pipeline");
        let root = paths.root.join("downloads");
        fs::create_dir_all(&root).unwrap();
        let mut manifest = unpack::read_manifest(&package(&root, "primitive-browser", "0.2.18", "{}").0).unwrap();
        manifest.hooks = [POST_INSTALL_PATH, PRE_UNINSTALL_PATH].map(str::to_string).to_vec();
        let bytes = serde_json::to_vec(&manifest).unwrap();
        let zip = make_zip(&root.join("reviewed.zip"), &[
            ("primitive-browser-0.2.18/manifest.json", &bytes, false),
            ("primitive-browser-0.2.18/primitive-browser/app.bin", b"payload", false),
            ("primitive-browser-0.2.18/hooks/post-install.sh", b"#!/bin/sh\nexit 0\n", false),
            ("primitive-browser-0.2.18/hooks/pre-uninstall.sh", b"#!/bin/sh\nexit 0\n", false),
        ]);
        let (sha, size) = verify::sha256_file(&zip).unwrap();
        let mut listing = listing_for(&sha, size, "0.2.18");
        listing.changes.hooks = true;
        let kindle = device();
        let downloaded = DownloadedPackage { paths: &paths, listing: &listing, device: &kindle, zip: &zip, source_url: "https://example.invalid/reviewed.zip", installed_at: "2026-11-19" };
        let runner = FixtureRunner(RefCell::new(Vec::new()));
        install_downloaded_reviewed(downloaded, &runner, |_| {}).unwrap();
        assert!(!paths.root.join("primitive-browser/hooks").exists());
        install::remove_with_hooks(&paths, "primitive-browser", &runner).unwrap();
        assert_eq!(*runner.0.borrow(), [HookPhase::PostInstall, HookPhase::PreUninstall]);
        assert!(!zip.exists());
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
    fn a_destination_conflict_removes_uncommitted_staging() {
        let p = paths("install-conflict");
        let backup = p.root.join("primitive-browser/app.bin.prev");
        fs::create_dir_all(backup.parent().unwrap()).unwrap();
        fs::write(&backup, b"user backup").unwrap();
        let (zip, sha, size) = package(&p.root, "primitive-browser", "0.2.18", "{}");
        let listing = listing_for(&sha, size, "0.2.18");

        let err = install_downloaded(&p, &listing, &device(), &zip, "u", "d").unwrap_err();
        assert!(err.contains("backup path"), "{err}");
        assert_eq!(fs::read(&backup).unwrap(), b"user backup");
        assert!(!p.staging.join("primitive-browser").exists());
        assert!(!p.journal.exists());
        assert!(!zip.exists());
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
