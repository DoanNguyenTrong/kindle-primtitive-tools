//! The JSON the store reads: the catalog index, an app's listing, and a package's manifest. These follow the schemas in
//! `doannt-kindle/schema/`; fields the store can do without are optional here.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct Index {
    pub schema: u32,
    pub generated_at: String,
    #[serde(default)]
    pub categories: Vec<Category>,
    pub apps: Vec<Card>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct Category {
    pub id: String,
    pub name: String,
}

/// One app's card in the index: enough to show it in a list.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct Card {
    pub id: String,
    #[serde(default)]
    pub dir: Option<String>,
    pub name: String,
    pub subtitle: String,
    pub categories: Vec<String>,
    pub icon: String,
    pub source: Source,
    pub latest_version: Option<String>,
    pub arch: Vec<String>,
    pub updated_at: String,
    #[serde(default)]
    pub listing: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Hosted,
    Upstream,
    Link,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct Listing {
    pub schema: u32,
    pub id: String,
    #[serde(default)]
    pub dir: Option<String>,
    pub name: String,
    pub subtitle: String,
    pub developer: Developer,
    pub categories: Vec<String>,
    pub description: String,
    pub repo: String,
    pub licence: String,
    pub source: Source,
    pub icon: String,
    #[serde(default)]
    pub screenshots: Vec<String>,
    pub changes: Changes,
    pub versions: Vec<VersionEntry>,
}

impl Listing {
    /// The folder under `/mnt/us` this app installs into: `dir`, which defaults to the id.
    pub fn dir(&self) -> &str {
        self.dir.as_deref().unwrap_or(&self.id)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct Developer {
    pub name: String,
    #[serde(default)]
    pub url: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct Changes {
    pub network: bool,
    pub stops_kindle_ui: bool,
    pub starts_at_boot: bool,
    pub root_filesystem: bool,
    pub hooks: bool,
    #[serde(default)]
    pub notes: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct VersionEntry {
    pub version: String,
    pub date: String,
    pub notes: String,
    pub firmware: Firmware,
    #[serde(default)]
    pub requires: BTreeMap<String, String>,
    #[serde(default)]
    pub tested_on: Vec<String>,
    #[serde(default)]
    pub withdrawn: Option<String>,
    #[serde(default)]
    pub assets: BTreeMap<String, Asset>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct Firmware {
    pub min: String,
    #[serde(default)]
    pub max: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct Asset {
    pub url: String,
    pub sha256: String,
    pub size: u64,
}

/// The manifest inside a package: the exact list of files it installs.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct Manifest {
    pub schema: u32,
    pub id: String,
    pub dir: String,
    pub version: String,
    pub arch: String,
    pub firmware: Firmware,
    #[serde(default)]
    pub requires: BTreeMap<String, String>,
    pub files: Vec<String>,
    #[serde(default)]
    pub hooks: Vec<String>,
}

#[cfg(test)]
pub(crate) fn sample_listing() -> Listing {
    serde_json::from_str(
        r#"{
        "schema": 1, "id": "primitive-browser", "dir": "browser",
        "name": "Primitive Browser", "subtitle": "A browser for e-ink",
        "developer": {"name": "Primitive AI"}, "categories": ["internet"],
        "description": "A web browser made for e-ink, with night mode and image switch.",
        "repo": "https://github.com/DoanNguyenTrong/kindle-primitive-browser", "licence": "PolyForm-Noncommercial-1.0.0",
        "source": "hosted", "icon": "icon.png",
        "changes": {"network": true, "stops_kindle_ui": false, "starts_at_boot": false, "root_filesystem": false, "hooks": false},
        "versions": [
          {"version": "0.2.18", "date": "2026-11-20", "notes": "Newest.", "firmware": {"min": "5.16.3"},
           "tested_on": ["KindleOasis3"], "assets": {"armhf": {"url": "https://example.com/b-0.2.18-armhf.zip", "sha256": "aa", "size": 10}}},
          {"version": "0.2.17", "date": "2026-11-19", "notes": "Older.", "firmware": {"min": "5.16.3"},
           "tested_on": ["KindleOasis3"], "assets": {"armhf": {"url": "https://example.com/b-0.2.17-armhf.zip", "sha256": "bb", "size": 10}}}
        ]
    }"#,
    )
    .expect("the sample listing is valid")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_listing_and_defaults_the_directory_to_the_id() {
        let mut l = sample_listing();
        assert_eq!(l.dir(), "browser");
        l.dir = None;
        assert_eq!(l.dir(), "primitive-browser");
        assert_eq!(l.source, Source::Hosted);
    }

    #[test]
    fn parses_an_index_and_a_manifest() {
        let index: Index = serde_json::from_str(
            r#"{"schema":1,"generated_at":"2026-11-19T10:00:00Z","apps":[{"id":"koreader","name":"KOReader",
            "subtitle":"Reader","categories":["reading"],"icon":"https://x/i.png","source":"link",
            "latest_version":"2026.10","arch":["armhf"],"updated_at":"2026-11-19"}]}"#,
        )
        .unwrap();
        assert_eq!(index.apps[0].source, Source::Link);
        let m: Manifest = serde_json::from_str(
            r#"{"schema":1,"id":"x","dir":"x","version":"1.0.0","arch":"armhf",
            "firmware":{"min":"5.16.3"},"files":["x/a","documents/X.sh"]}"#,
        )
        .unwrap();
        assert_eq!(m.files.len(), 2);
        assert!(m.hooks.is_empty());
    }
}
