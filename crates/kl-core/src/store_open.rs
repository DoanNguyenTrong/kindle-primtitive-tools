//! A Store-to-launcher handoff carries only an installed app ID.
//! The launcher independently resolves that ID against the installed-file record and its library.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::Path;

use kl_store::catalog::valid_app_id;
use kl_store::record::Installed;
use kl_store::unpack::is_safe_relative;

use crate::store::{LAUNCHER_ID, STORE_ID};

pub const EXIT_OPEN_APP: i32 = 11;
pub const CAPABILITY: &str = "KL_STORE_OPEN_APP";
const REQUEST_FILE: &str = "store-open";

fn can_request(id: &str) -> bool {
    valid_app_id(id) && !matches!(id, STORE_ID | LAUNCHER_ID)
}

/// A package with one direct library entry has an unambiguous target for Open.
/// Packages whose entry lives elsewhere, or which provide multiple entries, stay manageable in the Store.
pub fn entry_file(app: &Installed) -> Option<&str> {
    if !can_request(&app.id) {
        return None;
    }
    let mut scripts = app.files.iter().filter_map(|path| {
        let name = path.strip_prefix("documents/")?;
        (is_safe_relative(path)
            && name.ends_with(".sh")
            && !name.starts_with('.')
            && !name.contains(['/', '\r', '\n']))
        .then_some(name)
    });
    let script = scripts.next()?;
    (scripts.next().is_none() && !script.eq_ignore_ascii_case(crate::apps::STORE_ENTRY_FILE))
        .then_some(script)
}

pub fn installed_entry(launcher: &Path, documents: &Path, id: &str) -> io::Result<String> {
    let record = kl_store::record::load(&launcher.join("store/installed.json"))?;
    let name = record
        .apps
        .get(id)
        .filter(|app| app.id == id)
        .and_then(entry_file)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "This app has no single launcher entry.",
            )
        })?;
    let metadata = fs::symlink_metadata(documents.join(name)).map_err(|_| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "The app's launcher entry is missing.",
        )
    })?;
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "The app's launcher entry is not a regular file.",
        ));
    }
    Ok(name.to_owned())
}

pub fn request(run: &Path, id: &str) -> io::Result<()> {
    if !can_request(id) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "This app cannot be opened from the Store.",
        ));
    }
    let pending = run.join("store-open.tmp");
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&pending)?;
    let result = file.write_all(id.as_bytes());
    drop(file);
    let result = result.and_then(|()| fs::rename(&pending, run.join(REQUEST_FILE)));
    if result.is_err() {
        let _ = fs::remove_file(pending);
    }
    result
}

/// Consume at most one handoff. The file contains no path or executable command.
pub fn take_request(run: &Path) -> io::Result<Option<String>> {
    let request = run.join(REQUEST_FILE);
    let metadata = match fs::symlink_metadata(&request) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        other => other?,
    };
    let result = (|| {
        if !metadata.is_file() || metadata.len() > 40 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Invalid Store launch request.",
            ));
        }
        let mut id = String::new();
        File::open(&request)?.take(41).read_to_string(&mut id)?;
        if !can_request(&id) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Invalid Store launch request.",
            ));
        }
        Ok(Some(id))
    })();
    fs::remove_file(request)?;
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_one_direct_owned_scriptlet_is_launchable() {
        let mut app = Installed {
            id: "sample-app".into(),
            ..Installed::default()
        };
        app.files = [
            "sample-app/run",
            "documents/Example App.sh",
            "documents/Manual.pdf",
        ]
        .into_iter()
        .map(str::to_string)
        .collect();
        assert_eq!(entry_file(&app), Some("Example App.sh"));
        app.files.push("documents/Another.sh".into());
        assert_eq!(entry_file(&app), None);
        app.files = vec!["documents/../Another.sh".into()];
        assert_eq!(entry_file(&app), None);
        app.files = vec!["documents/Store.sh".into()];
        assert_eq!(entry_file(&app), None);
        app.files = vec!["documents/Example App.sh".into()];
        app.id = LAUNCHER_ID.into();
        assert_eq!(entry_file(&app), None);
    }

    #[test]
    fn handoff_is_single_use_and_requires_an_installed_library_entry() {
        let root = std::env::temp_dir().join(format!("kl-store-open-{}", std::process::id()));
        let run = root.join("run");
        let launcher = root.join("launcher");
        let documents = root.join("documents");
        fs::create_dir_all(&run).unwrap();
        fs::create_dir_all(&documents).unwrap();
        let app = Installed {
            id: "sample-app".into(),
            files: vec!["documents/Example App.sh".into()],
            ..Installed::default()
        };
        let mut record = kl_store::record::Record::new();
        record.apps.insert(app.id.clone(), app);
        kl_store::record::save(&launcher.join("store/installed.json"), &record).unwrap();
        assert!(installed_entry(&launcher, &documents, "sample-app").is_err());
        fs::write(documents.join("Example App.sh"), "#!/bin/sh\n").unwrap();
        assert_eq!(
            installed_entry(&launcher, &documents, "sample-app").unwrap(),
            "Example App.sh"
        );
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(
                documents.join("Example App.sh"),
                documents.join("Alias.sh"),
            )
            .unwrap();
            record.apps.get_mut("sample-app").unwrap().files[0] = "documents/Alias.sh".into();
            kl_store::record::save(&launcher.join("store/installed.json"), &record).unwrap();
            assert!(installed_entry(&launcher, &documents, "sample-app").is_err());
        }
        request(&run, "sample-app").unwrap();
        assert_eq!(take_request(&run).unwrap().as_deref(), Some("sample-app"));
        assert_eq!(take_request(&run).unwrap(), None);
        assert!(request(&run, "../sample-app").is_err());
        fs::write(run.join(REQUEST_FILE), "sample-app\nscript\n/tmp/fake").unwrap();
        assert!(take_request(&run).is_err());
        assert_eq!(take_request(&run).unwrap(), None);
        fs::remove_dir_all(root).unwrap();
    }
}
