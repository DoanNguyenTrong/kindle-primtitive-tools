//! What the store has installed: `installed.json` in the store's folder. The file list of each app is what removal and
//! update use, so nothing outside it is ever deleted.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write};
use std::path::Path;

pub const SCHEMA: u32 = 1;

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
pub struct Record {
    pub schema: u32,
    #[serde(default)]
    pub apps: BTreeMap<String, Installed>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
pub struct Installed {
    pub id: String,
    pub dir: String,
    pub version: String,
    pub arch: String,
    pub installed_at: String,
    pub source_url: String,
    pub sha256: String,
    /// Every file the app installed, relative to the Kindle's storage root.
    pub files: Vec<String>,
    /// The requirements the app declared (`id` → `>=version`), so removal can refuse to break another app.
    #[serde(default)]
    pub requires: BTreeMap<String, String>,
}

impl Record {
    pub fn new() -> Record {
        Record { schema: SCHEMA, apps: BTreeMap::new() }
    }

    /// Apps that require `id` (any version), so removing `id` would break them.
    pub fn dependents_of(&self, id: &str) -> Vec<String> {
        self.apps.values().filter(|a| a.requires.contains_key(id)).map(|a| a.id.clone()).collect()
    }
}

/// Loads the record; a missing file is an empty record. A file that cannot be read is an error, never silently empty.
pub fn load(path: &Path) -> io::Result<Record> {
    match fs::read_to_string(path) {
        Ok(text) => {
            let record: Record = serde_json::from_str(&text).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            if record.schema != SCHEMA {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "the Store record has an unsupported version"));
            }
            Ok(record)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Record::new()),
        Err(e) => Err(e),
    }
}

/// Writes the record atomically: a temporary file, flushed, then renamed over the old one.
pub fn save(path: &Path, record: &Record) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    let text = serde_json::to_string_pretty(record).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.write_all(b"\n")?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(id: &str, requires: &[(&str, &str)]) -> Installed {
        Installed {
            id: id.into(),
            dir: id.into(),
            version: "1.0.0".into(),
            arch: "armhf".into(),
            installed_at: "2026-11-19".into(),
            source_url: "https://example.com/x.zip".into(),
            sha256: "0".repeat(64),
            files: vec![format!("{id}/x")],
            requires: requires.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect(),
        }
    }

    #[test]
    fn round_trips_through_a_file_and_a_missing_file_is_empty() {
        let dir = std::env::temp_dir().join(format!("kl-record-{}", std::process::id()));
        let path = dir.join("installed.json");
        let _ = fs::remove_file(&path);
        assert_eq!(load(&path).unwrap(), Record::new());
        let mut r = Record::new();
        r.apps.insert("notes".into(), app("notes", &[("kterm", ">=2.6")]));
        save(&path, &r).unwrap();
        assert_eq!(load(&path).unwrap(), r);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_damaged_file_is_an_error_not_an_empty_record() {
        let dir = std::env::temp_dir().join(format!("kl-record-bad-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("installed.json");
        fs::write(&path, "{ not json").unwrap();
        assert!(load(&path).is_err());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn finds_the_apps_that_depend_on_one() {
        let mut r = Record::new();
        r.apps.insert("textadept".into(), app("textadept", &[("kterm", ">=2.6")]));
        r.apps.insert("notes".into(), app("notes", &[]));
        assert_eq!(r.dependents_of("kterm"), vec!["textadept"]);
        assert!(r.dependents_of("notes").is_empty());
    }
}
