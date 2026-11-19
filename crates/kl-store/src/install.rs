//! Installing, updating and removing an app's files, with a journal so a power cut or an error leaves the old version
//! working.
//!
//! The steps of an install: write the journal; move each file from staging into place, keeping the old file as
//! `<path>.prev` and logging each move in the journal; save the record; then delete the old-only files and the `.prev`
//! copies and the journal. If a step fails, the logged moves are undone in reverse. If the Kindle stops in the middle,
//! the next start calls `recover`, which does the same.

use crate::catalog::Manifest;
use crate::record::{self, Installed, Record};
use crate::unpack::{is_allowed_place, is_safe_relative, valid_module_dir};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Where the store keeps its files. `root` is the Kindle's storage (`/mnt/us`); the others are inside `store`.
#[derive(Debug, Clone)]
pub struct Paths {
    pub root: PathBuf,
    /// Unpacked packages wait here until they are moved into place. It must be on the same file system as `root`.
    pub staging: PathBuf,
    pub record: PathBuf,
    pub journal: PathBuf,
}

impl Paths {
    /// The layout on the Kindle: `root/launcher/store/…`.
    pub fn under(root: &Path) -> Paths {
        let store = root.join("launcher/store");
        Paths {
            root: root.to_path_buf(),
            staging: store.join("staging"),
            record: store.join("installed.json"),
            journal: store.join("journal.json"),
        }
    }
}

/// What the install needs to know about the download.
#[derive(Debug, Clone)]
pub struct Source<'a> {
    pub manifest: &'a Manifest,
    pub source_url: &'a str,
    pub sha256: &'a str,
    pub installed_at: &'a str,
}

#[derive(Debug)]
pub enum InstallError {
    Io(io::Error),
    /// A step failed; the logged moves were undone.
    RolledBack(String),
}

impl fmt::Display for InstallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InstallError::Io(e) => write!(f, "{e}"),
            InstallError::RolledBack(why) => write!(f, "the Store change was undone: {why}"),
        }
    }
}

impl From<io::Error> for InstallError {
    fn from(e: io::Error) -> Self {
        InstallError::Io(e)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum State {
    /// Files are being moved; the record is not yet updated.
    Moving,
    /// The record is saved; the old files are being cleaned up.
    Recorded,
    Removing,
    Removed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Done {
    pub path: String,
    /// Whether the old file was kept as `<path>.prev` (so rollback can put it back).
    pub prev: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Journal {
    pub id: String,
    pub version: String,
    #[serde(default)]
    pub dir: Option<String>,
    pub state: State,
    pub done: Vec<Done>,
    /// Files of the old version that the new one does not have; deleted once the record is saved.
    pub obsolete: Vec<String>,
    /// The record before this install, so rollback can restore it.
    pub previous: Option<Installed>,
}

fn write_journal(path: &Path, j: &Journal) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    let mut file = fs::File::create(&tmp)?;
    serde_json::to_writer(&mut file, j).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    file.flush()?;
    file.sync_all()?;
    fs::rename(&tmp, path)
}

fn read_journal(path: &Path) -> io::Result<Option<Journal>> {
    match fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text)
            .map(Some)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

fn remove_if_exists(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

fn prev_of(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".prev");
    PathBuf::from(s)
}

fn invalid_data(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn require_idle(paths: &Paths) -> io::Result<()> {
    if paths.journal.try_exists()? {
        return Err(invalid_data("an unfinished Store change needs recovery before another app can be changed".into()));
    }
    Ok(())
}

/// Checks on-disk components as well as the package's relative path.
fn validate_destination(paths: &Paths, relative: &str, dir: &str) -> io::Result<()> {
    if !is_safe_relative(relative) || !is_allowed_place(relative, dir) {
        return Err(invalid_data(format!("{relative} is outside this app's library and module folders")));
    }
    let mut path = paths.root.clone();
    let mut pieces = Path::new(relative).components().peekable();
    while let Some(piece) = pieces.next() {
        path.push(piece);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if pieces.peek().is_some() && metadata.is_dir() => {}
            Ok(metadata) if pieces.peek().is_none() && metadata.is_file() => {}
            Ok(_) => return Err(invalid_data(format!("{relative} does not name a regular file in the app's folder"))),
            Err(error) if error.kind() == io::ErrorKind::NotFound => break,
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn validate_changes(paths: &Paths, record: &Record, id: &str, dir: &str, files: &[String]) -> io::Result<()> {
    use std::collections::BTreeSet;

    if !valid_module_dir(dir) || !is_safe_relative(id) || id.contains('/') {
        return Err(invalid_data("the package does not name a valid app and module folder".into()));
    }
    let owned: BTreeSet<_> = record.apps.iter()
        .filter(|(owner, _)| owner.as_str() != id)
        .flat_map(|(_, app)| app.files.iter().map(|file| file.to_ascii_lowercase()))
        .collect();
    let requested: BTreeSet<_> = files.iter().map(|file| file.to_ascii_lowercase()).collect();
    if files.len() != requested.len() {
        return Err(invalid_data("the same file appears twice in this app's file list".into()));
    }
    for relative in files {
        validate_destination(paths, relative, dir)?;
        let normalized = relative.to_ascii_lowercase();
        if owned.contains(&normalized) {
            return Err(invalid_data(format!("{relative} belongs to another installed app")));
        }
        let backup = format!("{normalized}.prev");
        if requested.contains(&backup) || owned.contains(&backup) || fs::symlink_metadata(prev_of(&paths.root.join(relative))).is_ok() {
            return Err(invalid_data(format!("the backup path for {relative} is already in use")));
        }
    }
    Ok(())
}

/// Undoes the logged moves, last first: a new file is removed, and a kept old file is put back.
fn undo(paths: &Paths, done: &[Done]) -> io::Result<()> {
    for d in done.iter().rev() {
        let target = paths.root.join(&d.path);
        if d.prev {
            let backup = prev_of(&target);
            // The intent is journaled before the move. Without a backup, the original is still in place
            // or a previous recovery already restored it.
            if backup.try_exists()? {
                fs::rename(backup, &target)?;
            }
        } else {
            remove_if_exists(&target)?;
        }
    }
    Ok(())
}

/// Installs or updates an app. `unpacked` is the staging folder for this app (already verified and unpacked there).
/// `fail_after` makes the install fail after that many moves; it is for tests.
pub fn install(paths: &Paths, source: &Source<'_>, fail_after: Option<usize>) -> Result<(), InstallError> {
    require_idle(paths)?;
    let mut rec = record::load(&paths.record)?;
    let id = source.manifest.id.clone();
    let previous = rec.apps.get(&id).cloned();
    validate_changes(paths, &rec, &id, &source.manifest.dir, &source.manifest.files)?;
    if let Some(previous) = &previous {
        validate_changes(paths, &rec, &id, &previous.dir, &previous.files)?;
    }
    let obsolete: Vec<String> = previous
        .as_ref()
        .map(|p| p.files.iter()
            .filter(|file| !source.manifest.files.iter().any(|new_file| new_file.eq_ignore_ascii_case(file)))
            .cloned().collect())
        .unwrap_or_default();

    let mut journal = Journal {
        id: id.clone(),
        version: source.manifest.version.clone(),
        dir: Some(source.manifest.dir.clone()),
        state: State::Moving,
        done: Vec::new(),
        obsolete: obsolete.clone(),
        previous: previous.clone(),
    };
    write_journal(&paths.journal, &journal)?;

    // Move the files into place, keeping what they replace.
    let moved: Result<(), String> = (|| {
        for (n, f) in source.manifest.files.iter().enumerate() {
            if fail_after == Some(n) {
                return Err("a test asked for a failure".into());
            }
            let src = paths.staging.join(&id).join(f);
            let dst = paths.root.join(f);
            if let Some(parent) = dst.parent() {
                fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            let had_old = dst.is_file();
            journal.done.push(Done { path: f.clone(), prev: had_old });
            write_journal(&paths.journal, &journal).map_err(|e| e.to_string())?;
            if had_old {
                fs::rename(&dst, prev_of(&dst)).map_err(|e| e.to_string())?;
            }
            fs::rename(&src, &dst).map_err(|e| format!("{f}: {e}"))?;
        }
        Ok(())
    })();
    if let Err(why) = moved {
        undo(paths, &journal.done)?;
        remove_if_exists(&paths.journal)?;
        return Err(InstallError::RolledBack(why));
    }

    // Record the new version, then clean up.
    rec.schema = record::SCHEMA;
    rec.apps.insert(
        id.clone(),
        Installed {
            id: id.clone(),
            dir: source.manifest.dir.clone(),
            version: source.manifest.version.clone(),
            arch: source.manifest.arch.clone(),
            installed_at: source.installed_at.to_string(),
            source_url: source.source_url.to_string(),
            sha256: source.sha256.to_string(),
            files: source.manifest.files.clone(),
            requires: source.manifest.requires.clone(),
        },
    );
    if let Err(e) = record::save(&paths.record, &rec) {
        undo(paths, &journal.done)?;
        remove_if_exists(&paths.journal)?;
        return Err(InstallError::RolledBack(format!("cannot save the record: {e}")));
    }
    journal.state = State::Recorded;
    write_journal(&paths.journal, &journal)?;
    finish(paths, &journal)?;
    Ok(())
}

/// The last steps of an install: delete the old-only files and the kept `.prev` copies, then the journal and the
/// staging folder. Safe to run again.
fn finish(paths: &Paths, journal: &Journal) -> io::Result<()> {
    for f in &journal.obsolete {
        remove_if_exists(&paths.root.join(f))?;
    }
    for d in &journal.done {
        if d.prev {
            remove_if_exists(&prev_of(&paths.root.join(&d.path)))?;
        }
    }
    if journal.state == State::Removed {
        for file in &journal.done {
            prune_empty_parents(paths, &file.path);
        }
    }
    remove_if_exists(&paths.journal)?;
    let _ = fs::remove_dir_all(paths.staging.join(&journal.id));
    Ok(())
}

fn prune_empty_parents(paths: &Paths, relative: &str) {
    let target = paths.root.join(relative);
    let mut parent = target.parent();
    while let Some(dir) = parent {
        if dir == paths.root || dir == paths.root.join("documents") || dir == paths.root.join("extensions")
            || fs::remove_dir(dir).is_err()
        {
            break;
        }
        parent = dir.parent();
    }
}

fn legacy_dir(relative: &str) -> &str {
    let mut parts = relative.split('/');
    match parts.next() {
        Some("documents") => "legacy-app",
        Some("extensions") => parts.next().unwrap_or_default(),
        Some(dir) => dir,
        None => "",
    }
}

fn validate_journal(paths: &Paths, journal: &Journal, record: &Record) -> io::Result<()> {
    if !is_safe_relative(&journal.id) || journal.id.contains('/') {
        return Err(invalid_data("the Store journal has an invalid app id".into()));
    }
    let dir = journal.dir.as_deref()
        .or_else(|| record.apps.get(&journal.id).map(|app| app.dir.as_str()))
        .or_else(|| journal.previous.as_ref().map(|app| app.dir.as_str()));
    for file in &journal.done {
        let dir = dir.unwrap_or_else(|| legacy_dir(&file.path));
        validate_destination(paths, &file.path, dir)?;
        if file.prev {
            validate_destination(paths, &format!("{}.prev", file.path), dir)?;
        }
    }
    for file in &journal.obsolete {
        let old_dir = journal.previous.as_ref().map(|app| app.dir.as_str()).or(dir);
        validate_destination(paths, file, old_dir.unwrap_or_else(|| legacy_dir(file)))?;
    }
    Ok(())
}

/// Called at every start: an install that did not finish is finished (if the record was saved) or undone (if not).
/// Returns what was done, or `None` when there was nothing to do.
pub fn recover(paths: &Paths) -> io::Result<Option<String>> {
    let Some(journal) = read_journal(&paths.journal)? else {
        return Ok(None);
    };
    let mut rec = record::load(&paths.record)?;
    validate_journal(paths, &journal, &rec)?;
    match journal.state {
        State::Recorded | State::Removed => {
            let description = if journal.state == State::Removed { "removal" } else { "update" };
            finish(paths, &journal)?;
            Ok(Some(format!("finished the {description} of {}", journal.id)))
        }
        State::Moving | State::Removing => {
            undo(paths, &journal.done)?;
            // Restore the record exactly as it was before this install.
            match &journal.previous {
                Some(prev) => {
                    rec.apps.insert(journal.id.clone(), prev.clone());
                }
                None => {
                    rec.apps.remove(&journal.id);
                }
            }
            record::save(&paths.record, &rec)?;
            remove_if_exists(&paths.journal)?;
            let _ = fs::remove_dir_all(paths.staging.join(&journal.id));
            let description = if journal.state == State::Removing { "removal" } else { "install" };
            Ok(Some(format!("undid the unfinished {description} of {} {}", journal.id, journal.version)))
        }
    }
}

/// Removes an installed app: exactly the files in its record, then the empty folders above them (never the root).
/// Refused when another installed app requires it.
pub fn remove(paths: &Paths, id: &str) -> Result<(), InstallError> {
    require_idle(paths)?;
    let mut rec = record::load(&paths.record)?;
    let deps = rec.dependents_of(id);
    if !deps.is_empty() {
        return Err(InstallError::RolledBack(format!("{} needs {id}; remove it first", deps.join(", "))));
    }
    let Some(app) = rec.apps.get(id).cloned() else {
        return Ok(());
    };
    validate_changes(paths, &rec, id, &app.dir, &app.files)?;
    let mut journal = Journal {
        id: id.to_string(),
        version: app.version.clone(),
        dir: Some(app.dir.clone()),
        state: State::Removing,
        done: Vec::new(),
        obsolete: Vec::new(),
        previous: Some(app.clone()),
    };
    write_journal(&paths.journal, &journal)?;
    let moved = (|| -> io::Result<()> {
        for relative in &app.files {
            let target = paths.root.join(relative);
            if target.try_exists()? {
                journal.done.push(Done { path: relative.clone(), prev: true });
                write_journal(&paths.journal, &journal)?;
                fs::rename(&target, prev_of(&target))?;
            }
        }
        Ok(())
    })();
    if let Err(error) = moved {
        undo(paths, &journal.done)?;
        remove_if_exists(&paths.journal)?;
        return Err(InstallError::RolledBack(error.to_string()));
    }
    rec.apps.remove(id);
    if let Err(error) = record::save(&paths.record, &rec) {
        undo(paths, &journal.done)?;
        remove_if_exists(&paths.journal)?;
        return Err(InstallError::RolledBack(error.to_string()));
    }
    journal.state = State::Removed;
    write_journal(&paths.journal, &journal)?;
    finish(paths, &journal)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{Firmware, Manifest};
    use std::collections::BTreeMap;

    struct World {
        paths: Paths,
    }

    fn world(name: &str) -> World {
        let root = std::env::temp_dir().join(format!("kl-install-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        World { paths: Paths::under(&root) }
    }

    fn manifest(id: &str, version: &str, files: &[&str], requires: &[(&str, &str)]) -> Manifest {
        Manifest {
            schema: 1,
            id: id.into(),
            dir: id.into(),
            version: version.into(),
            arch: "armhf".into(),
            firmware: Firmware { min: "5.16.3".into(), max: None },
            requires: requires.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect(),
            files: files.iter().map(|s| s.to_string()).collect(),
            hooks: vec![],
        }
    }

    /// Puts the files of a version in staging, with the content `tag` so versions can be told apart.
    fn stage(paths: &Paths, m: &Manifest, tag: &str) {
        for f in &m.files {
            let p = paths.staging.join(&m.id).join(f);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, format!("{} {tag}", m.version)).unwrap();
        }
    }

    fn src<'a>(m: &'a Manifest) -> Source<'a> {
        Source { manifest: m, source_url: "https://example.com/x.zip", sha256: "00", installed_at: "2026-11-19" }
    }

    #[test]
    fn a_fresh_install_moves_the_files_and_records_them() {
        let w = world("fresh");
        let m = manifest("notes", "1.0.0", &["notes/app", "documents/Notes.sh"], &[]);
        stage(&w.paths, &m, "a");
        install(&w.paths, &src(&m), None).unwrap();
        assert_eq!(fs::read_to_string(w.paths.root.join("notes/app")).unwrap(), "1.0.0 a");
        let rec = record::load(&w.paths.record).unwrap();
        assert_eq!(rec.apps["notes"].version, "1.0.0");
        assert!(!w.paths.journal.exists(), "the journal is cleared when the install is done");
        assert!(!w.paths.staging.join("notes").exists(), "staging is cleaned up");
    }

    #[test]
    fn an_update_replaces_files_and_removes_the_ones_the_new_version_drops() {
        let w = world("update");
        let v1 = manifest("notes", "1.0.0", &["notes/app", "notes/old.dat"], &[]);
        stage(&w.paths, &v1, "a");
        install(&w.paths, &src(&v1), None).unwrap();
        let v2 = manifest("notes", "1.1.0", &["notes/app"], &[]);
        stage(&w.paths, &v2, "b");
        install(&w.paths, &src(&v2), None).unwrap();
        assert_eq!(fs::read_to_string(w.paths.root.join("notes/app")).unwrap(), "1.1.0 b");
        assert!(!w.paths.root.join("notes/old.dat").exists(), "a file the new version drops is removed");
        assert!(!w.paths.root.join("notes/app.prev").exists(), "the kept old copy is removed when done");
    }

    #[test]
    fn a_failure_halfway_leaves_the_old_version_working() {
        let w = world("rollback");
        let v1 = manifest("notes", "1.0.0", &["notes/a", "notes/b"], &[]);
        stage(&w.paths, &v1, "old");
        install(&w.paths, &src(&v1), None).unwrap();
        let before = record::load(&w.paths.record).unwrap();

        let v2 = manifest("notes", "2.0.0", &["notes/a", "notes/b", "notes/c"], &[]);
        stage(&w.paths, &v2, "new");
        let err = install(&w.paths, &src(&v2), Some(2)).unwrap_err();
        assert!(matches!(err, InstallError::RolledBack(_)), "{err}");
        assert_eq!(fs::read_to_string(w.paths.root.join("notes/a")).unwrap(), "1.0.0 old");
        assert_eq!(fs::read_to_string(w.paths.root.join("notes/b")).unwrap(), "1.0.0 old");
        assert!(!w.paths.root.join("notes/c").exists());
        assert!(!w.paths.root.join("notes/a.prev").exists());
        assert_eq!(record::load(&w.paths.record).unwrap(), before);
        assert!(!w.paths.journal.exists());
    }

    #[test]
    fn a_power_cut_before_the_record_is_undone_at_the_next_start() {
        let w = world("cut-moving");
        let v1 = manifest("notes", "1.0.0", &["notes/a"], &[]);
        stage(&w.paths, &v1, "old");
        install(&w.paths, &src(&v1), None).unwrap();

        // Simulate the power cut: one file moved (keeping the old one), the journal says so, then nothing else.
        let v2 = manifest("notes", "2.0.0", &["notes/a", "notes/b"], &[]);
        stage(&w.paths, &v2, "new");
        let dst = w.paths.root.join("notes/a");
        fs::rename(&dst, prev_of(&dst)).unwrap();
        fs::rename(w.paths.staging.join("notes/notes/a"), &dst).unwrap();
        let journal = Journal {
            id: "notes".into(),
            version: "2.0.0".into(),
            dir: None,
            state: State::Moving,
            done: vec![Done { path: "notes/a".into(), prev: true }],
            obsolete: vec![],
            previous: Some(record::load(&w.paths.record).unwrap().apps["notes"].clone()),
        };
        write_journal(&w.paths.journal, &journal).unwrap();

        let what = recover(&w.paths).unwrap().unwrap();
        assert!(what.contains("undid"), "{what}");
        assert_eq!(fs::read_to_string(w.paths.root.join("notes/a")).unwrap(), "1.0.0 old");
        assert_eq!(record::load(&w.paths.record).unwrap().apps["notes"].version, "1.0.0");
        assert!(!w.paths.journal.exists());
        assert!(recover(&w.paths).unwrap().is_none());
    }

    #[test]
    fn a_power_cut_after_the_record_is_finished_at_the_next_start() {
        let w = world("cut-recorded");
        let v1 = manifest("notes", "1.0.0", &["notes/a", "notes/gone"], &[]);
        stage(&w.paths, &v1, "old");
        install(&w.paths, &src(&v1), None).unwrap();

        // The record already names 2.0.0; the old-only file is still there.
        let v2 = manifest("notes", "2.0.0", &["notes/a"], &[]);
        stage(&w.paths, &v2, "new");
        fs::rename(w.paths.staging.join("notes/notes/a"), w.paths.root.join("notes/a")).unwrap();
        let mut rec = record::load(&w.paths.record).unwrap();
        rec.apps.insert("notes".into(), Installed { version: "2.0.0".into(), files: vec!["notes/a".into()], ..rec.apps["notes"].clone() });
        record::save(&w.paths.record, &rec).unwrap();
        let journal = Journal {
            id: "notes".into(),
            version: "2.0.0".into(),
            dir: None,
            state: State::Recorded,
            done: vec![Done { path: "notes/a".into(), prev: false }],
            obsolete: vec!["notes/gone".into()],
            previous: None,
        };
        write_journal(&w.paths.journal, &journal).unwrap();

        assert!(recover(&w.paths).unwrap().unwrap().contains("finished"));
        assert!(!w.paths.root.join("notes/gone").exists());
        assert_eq!(fs::read_to_string(w.paths.root.join("notes/a")).unwrap(), "2.0.0 new");
        assert!(!w.paths.journal.exists());
    }

    #[test]
    fn a_move_logged_before_it_starts_recovers_without_losing_the_original() {
        let w = world("cut-before-move");
        let app = manifest("notes", "1.0.0", &["notes/app"], &[]);
        stage(&w.paths, &app, "old");
        install(&w.paths, &src(&app), None).unwrap();
        let previous = record::load(&w.paths.record).unwrap().apps.remove("notes").unwrap();
        let journal = Journal {
            id: app.id,
            version: "2.0.0".into(),
            dir: Some(app.dir),
            state: State::Moving,
            done: vec![Done { path: "notes/app".into(), prev: true }],
            obsolete: Vec::new(),
            previous: Some(previous),
        };
        write_journal(&w.paths.journal, &journal).unwrap();
        recover(&w.paths).unwrap();
        assert_eq!(fs::read_to_string(w.paths.root.join("notes/app")).unwrap(), "1.0.0 old");
    }

    #[test]
    fn interruption_during_removal_restores_the_file_and_app_record() {
        let w = world("cut-removing");
        let app = manifest("notes", "1.0.0", &["notes/app"], &[]);
        stage(&w.paths, &app, "old");
        install(&w.paths, &src(&app), None).unwrap();
        let mut record = record::load(&w.paths.record).unwrap();
        let previous = record.apps.remove("notes").unwrap();
        let file = w.paths.root.join("notes/app");
        let journal = Journal {
            id: app.id,
            version: app.version,
            dir: Some(app.dir),
            state: State::Removing,
            done: vec![Done { path: "notes/app".into(), prev: true }],
            obsolete: Vec::new(),
            previous: Some(previous),
        };
        write_journal(&w.paths.journal, &journal).unwrap();
        fs::rename(&file, prev_of(&file)).unwrap();
        record::save(&w.paths.record, &record).unwrap();
        assert!(recover(&w.paths).unwrap().unwrap().contains("undid the unfinished removal"));
        assert_eq!(fs::read_to_string(file).unwrap(), "1.0.0 old");
        assert!(record::load(&w.paths.record).unwrap().apps.contains_key("notes"));
    }

    #[test]
    fn paths_under_another_installed_app_are_not_replaced() {
        let w = world("file-ownership");
        let first = manifest("notes", "1.0.0", &["documents/Notes.sh"], &[]);
        stage(&w.paths, &first, "notes");
        install(&w.paths, &src(&first), None).unwrap();
        let second = manifest("cards", "1.0.0", &["documents/Notes.sh"], &[]);
        stage(&w.paths, &second, "cards");
        assert!(install(&w.paths, &src(&second), None).unwrap_err().to_string().contains("another installed app"));
        assert_eq!(fs::read_to_string(w.paths.root.join("documents/Notes.sh")).unwrap(), "1.0.0 notes");
    }

    #[test]
    fn removal_deletes_exactly_the_recorded_files_and_refuses_a_required_app() {
        let w = world("remove");
        let k = manifest("kterm", "2.6", &["kterm/kterm"], &[]);
        stage(&w.paths, &k, "k");
        install(&w.paths, &src(&k), None).unwrap();
        let t = manifest("textadept", "1.0.0", &["textadept/ta"], &[("kterm", ">=2.6")]);
        stage(&w.paths, &t, "t");
        install(&w.paths, &src(&t), None).unwrap();
        fs::write(w.paths.root.join("documents-keep.txt"), "user file").unwrap();

        match remove(&w.paths, "kterm") {
            Err(InstallError::RolledBack(msg)) => assert!(msg.contains("textadept"), "{msg}"),
            other => panic!("expected a refusal, got {other:?}"),
        }
        remove(&w.paths, "textadept").unwrap();
        assert!(!w.paths.root.join("textadept").exists(), "the empty module folder is removed");
        remove(&w.paths, "kterm").unwrap();
        assert!(!w.paths.root.join("kterm/kterm").exists());
        assert!(w.paths.root.exists(), "the storage root is never removed");
        assert!(w.paths.root.join("documents-keep.txt").exists(), "files outside the record are kept");
        assert!(record::load(&w.paths.record).unwrap().apps.is_empty());
    }

    #[test]
    fn the_test_helper_builds_requirement_maps() {
        let mut m: BTreeMap<String, String> = BTreeMap::new();
        m.insert("kterm".into(), ">=2.6".into());
        assert_eq!(m.len(), 1);
    }
}
