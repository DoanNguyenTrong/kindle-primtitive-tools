use kl_store::catalog::{Asset, Changes, Developer, Listing, Manifest, Source, VersionEntry};
use kl_store::install::{self, LauncherUpdate, Paths, LAUNCHER_ID};
use kl_store::{pipeline, record, resolve::Device, unpack, Version};
use std::fs;
use std::path::{Path, PathBuf};

struct Fixture(PathBuf);

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn listing(manifest: &Manifest, archive: &Path, url: &str) -> Listing {
    let sidecar = fs::read_to_string(archive.with_extension("zip.sha256")).unwrap();
    let asset = Asset {
        url: url.into(),
        sha256: sidecar.split_whitespace().next().unwrap().into(),
        size: archive.metadata().unwrap().len(),
    };
    Listing {
        schema: 1,
        id: manifest.id.clone(),
        dir: Some(manifest.dir.clone()),
        name: manifest.id.clone(),
        subtitle: String::new(),
        developer: Developer {
            name: "Local simulation".into(),
            url: None,
        },
        categories: Vec::new(),
        description: String::new(),
        repo: String::new(),
        licence: String::new(),
        source: Source::Hosted,
        icon: String::new(),
        screenshots: Vec::new(),
        changes: Changes {
            network: false,
            stops_kindle_ui: false,
            starts_at_boot: false,
            root_filesystem: false,
            hooks: false,
            notes: String::new(),
        },
        versions: vec![VersionEntry {
            version: manifest.version.clone(),
            date: "2026-11-19".into(),
            notes: String::new(),
            firmware: manifest.firmware.clone(),
            requires: manifest.requires.clone(),
            tested_on: Vec::new(),
            withdrawn: None,
            assets: [(manifest.arch.clone(), asset)].into(),
        }],
    }
}

#[test]
fn installs_and_removes_the_local_package_set() {
    let Some(package_dir) = std::env::var_os("KL_TEST_PACKAGE_SET") else {
        return;
    };
    let mut packages = Vec::new();
    for dir in fs::read_dir(package_dir).unwrap() {
        for entry in fs::read_dir(dir.unwrap().path()).unwrap() {
            let archive = entry.unwrap().path();
            if archive.extension().is_some_and(|ext| ext == "zip") {
                packages.push((unpack::read_manifest(&archive).unwrap(), archive));
            }
        }
    }
    assert!(!packages.is_empty());
    packages.sort_by_key(|(manifest, _)| (manifest.id != LAUNCHER_ID, manifest.id.clone()));

    let fixture =
        Fixture(std::env::temp_dir().join(format!("primitive-package-set-{}", std::process::id())));
    let paths = Paths::under(&fixture.0);
    let downloads = paths.root.join("launcher/store/downloads");
    let documents = paths.root.join("documents");
    fs::create_dir_all(&downloads).unwrap();
    fs::create_dir_all(&documents).unwrap();
    let user_file = documents.join("Personal note.txt");
    fs::write(&user_file, "user data").unwrap();

    for (manifest, archive) in &packages {
        let zip = downloads.join(archive.file_name().unwrap());
        fs::copy(archive, &zip).unwrap();
        let url = "https://example.invalid/local-package.zip";
        let listing = listing(manifest, archive, url);
        let device = Device {
            arch: manifest.arch.clone(),
            firmware: Version::parse("5.18.2").unwrap(),
        };
        let version =
            pipeline::install_downloaded(&paths, &listing, &device, &zip, url, "2026-11-19")
                .unwrap_or_else(|error| panic!("installing {}: {error}", manifest.id));
        assert_eq!(version, manifest.version);
        if manifest.id == LAUNCHER_ID {
            assert_eq!(
                install::launcher_update_begin(&paths).unwrap(),
                LauncherUpdate::Trial
            );
            assert!(install::launcher_update_accept(&paths).unwrap());
        }
        assert!(!zip.exists());
        for file in &manifest.files {
            assert!(paths.root.join(file).is_file(), "{}: {file}", manifest.id);
        }
        println!("Installed {} ({})", manifest.id, manifest.arch);
    }

    assert_eq!(
        record::load(&paths.record).unwrap().apps.len(),
        packages.len()
    );
    for (manifest, _) in packages
        .iter()
        .rev()
        .filter(|(manifest, _)| !matches!(manifest.id.as_str(), LAUNCHER_ID | "primitive-store"))
    {
        install::remove(&paths, &manifest.id)
            .unwrap_or_else(|error| panic!("removing {}: {error}", manifest.id));
        for file in &manifest.files {
            assert!(!paths.root.join(file).exists(), "{}: {file}", manifest.id);
        }
        println!("Removed {} ({})", manifest.id, manifest.arch);
    }
    assert_eq!(fs::read_to_string(user_file).unwrap(), "user data");
    assert!(install::recover(&paths).unwrap().is_none());
}
