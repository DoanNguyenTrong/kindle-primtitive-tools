use kl_store::catalog::{
    Asset, Changes, Developer, Index, Listing, Manifest, Source, VersionEntry,
};
use kl_store::install::{self, LauncherUpdate, Paths, LAUNCHER_ID};
use kl_store::{pipeline, record, refresh, resolve::Device, unpack, Version};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

const CATALOG_URL: &str = "https://catalog.invalid/v1";

struct Fixture(PathBuf);

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct LocalCatalog(PathBuf);

impl refresh::Source for LocalCatalog {
    fn text(&self, url: &str) -> Result<String, String> {
        self.bytes(url, refresh::MAX_INDEX)
            .and_then(|data| String::from_utf8(data).map_err(|error| error.to_string()))
    }

    fn bytes(&self, url: &str, max: u64) -> Result<Vec<u8>, String> {
        let file = url
            .strip_prefix(CATALOG_URL)
            .and_then(|path| path.strip_prefix('/'));
        let file = file.ok_or("expected a URL in the private catalog")?;
        let data = fs::read(self.0.join(file)).map_err(|error| format!("{file}: {error}"))?;
        if data.len() as u64 > max {
            return Err(format!("{file}: exceeds the {max}-byte limit"));
        }
        Ok(data)
    }
}

fn refresh_catalog(site: PathBuf, cache: &Path, packages: &[(Manifest, PathBuf)]) {
    let site = LocalCatalog(site);
    let report = refresh::refresh(CATALOG_URL, cache, &site).unwrap();
    assert!(report.failed.is_empty(), "{report:?}");
    let index: Index =
        serde_json::from_slice(&fs::read(cache.join("index.json")).unwrap()).unwrap();
    assert_eq!(report.refreshed, index.apps.len());
    let actual: BTreeSet<_> = index
        .apps
        .iter()
        .filter(|card| card.source != Source::Link)
        .map(|card| card.id.as_str())
        .collect();
    let expected: BTreeSet<_> = packages
        .iter()
        .map(|(manifest, _)| manifest.id.as_str())
        .collect();
    assert_eq!(actual, expected, "catalog and package set disagree");
    assert_eq!(
        fs::read(cache.join("collections.json")).unwrap(),
        fs::read(site.0.join("collections.json")).unwrap()
    );

    for card in &index.apps {
        let id = &card.id;
        let path = format!("apps/{id}.json");
        let contents = fs::read(cache.join(&path)).unwrap();
        assert_eq!(contents, fs::read(site.0.join(path)).unwrap());
        let listing: Listing = serde_json::from_slice(&contents).unwrap();
        assert_eq!(listing.id, *id);
        assert!(listing
            .versions
            .iter()
            .all(|version| version.tested_on.is_empty()));
        let icon = format!("icons/{id}.png");
        assert_eq!(
            fs::read(cache.join(&icon)).unwrap(),
            fs::read(site.0.join(icon)).unwrap()
        );
        for number in 0..listing.screenshots.len().min(refresh::SHOTS_PER_APP) {
            let shot = format!("shots/{id}-{number}.png");
            assert_eq!(
                fs::read(cache.join(&shot)).unwrap(),
                fs::read(site.0.join(shot)).unwrap()
            );
        }
    }
    println!("Refreshed {} private catalog listings", report.refreshed);
}

fn synthetic_listing(manifest: &Manifest, archive: &Path, url: &str) -> Listing {
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
    let catalog_cache = std::env::var_os("KL_TEST_CATALOG_SITE").map(|site| {
        let cache = paths.root.join("launcher/store/cache");
        refresh_catalog(site.into(), &cache, &packages);
        cache
    });

    for (manifest, archive) in &packages {
        let zip = downloads.join(archive.file_name().unwrap());
        fs::copy(archive, &zip).unwrap();
        let listing = match &catalog_cache {
            Some(cache) => {
                let contents =
                    fs::read(cache.join("apps").join(format!("{}.json", manifest.id))).unwrap();
                serde_json::from_slice(&contents).unwrap()
            }
            None => synthetic_listing(
                manifest,
                archive,
                "https://example.invalid/local-package.zip",
            ),
        };
        let url = &listing.versions[0].assets[&manifest.arch].url;
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
