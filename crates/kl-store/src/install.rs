//! Installing, updating and removing an app's files, with a journal so a power cut or an error leaves the old version
//! working.
//!
//! The steps of an install: write the journal; move each file from staging into place, keeping the old file as
//! `<path>.prev` and logging each move in the journal; save the record; then delete the old-only files and the `.prev`
//! copies and the journal. If a step fails, the logged moves are undone in reverse. If the Kindle stops in the middle,
//! the next start calls `recover`, which does the same.

use crate::catalog::Manifest;
use crate::hooks::{self, HookPhase, HookRunner, HookScripts};
use crate::record::{self, Installed, Record};
use crate::unpack::{is_allowed_place, is_safe_relative, valid_module_dir};
use crate::verify::sha256_file;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

pub const LAUNCHER_ID: &str = "primitive-launcher";
const LAUNCHER_TRIAL_LIMIT: u8 = 3;

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
    /// A verified launcher update is staged and waiting for the supervisor.
    Prepared,
    /// Files are being moved; the record is not yet updated.
    Moving,
    /// The record is saved; the old files are being cleaned up.
    Recorded,
    Removing,
    Removed,
    /// The candidate files are in place and the original files are kept until a healthy start.
    Trial,
    Reverting,
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
    #[serde(default)]
    pub trial: Option<Trial>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    hooks: Option<HookChange>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct HookChange {
    candidate: Option<Installed>,
    previous_started: bool,
    candidate_started: bool,
    files_restored: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Trial {
    candidate: Installed,
    attempts: u8,
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
    fs::rename(&tmp, path)?;
    if j.hooks.is_some() {
        // The intent must survive before a hook changes files outside the Store journal.
        if let Some(parent) = path.parent() {
            fs::File::open(parent)?.sync_all()?;
        }
    }
    Ok(())
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

fn sync_payload_directories<'a>(root: &Path, paths: impl IntoIterator<Item = &'a Path>) -> io::Result<()> {
    let mut directories = BTreeSet::new();
    for path in paths {
        for parent in path.ancestors().skip(1) {
            directories.insert(parent);
            if parent == root {
                break;
            }
        }
    }
    for directory in directories.into_iter().rev() {
        match fs::File::open(directory) {
            Ok(file) => file.sync_all()?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn sync_hook_payload(paths: &Paths, relatives: impl IntoIterator<Item = impl AsRef<Path>>) -> io::Result<()> {
    let targets: Vec<_> = relatives.into_iter().map(|relative| paths.root.join(relative)).collect();
    sync_payload_directories(&paths.root, targets.iter().map(PathBuf::as_path))
}

fn clear_journal(paths: &Paths, journal: &Journal) -> io::Result<()> {
    remove_if_exists(&paths.journal)?;
    if journal.hooks.is_some() {
        sync_payload_directories(&paths.root, [paths.journal.as_path()])?;
    }
    let _ = fs::remove_dir_all(paths.staging.join(&journal.id));
    Ok(())
}

fn prev_of(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".prev");
    PathBuf::from(s)
}

fn invalid_data(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

pub(crate) fn require_idle(paths: &Paths) -> io::Result<()> {
    if paths.journal.try_exists()? {
        return Err(invalid_data("an unfinished Store change needs recovery before another app can be changed".into()));
    }
    Ok(())
}

/// Checks on-disk components and returns the size of an existing regular file.
fn validate_destination(paths: &Paths, relative: &str, dir: &str) -> io::Result<Option<u64>> {
    if !is_safe_relative(relative) || !is_allowed_place(relative, dir) {
        return Err(invalid_data(format!("{relative} is outside this app's library and module folders")));
    }
    let mut path = paths.root.clone();
    let mut pieces = Path::new(relative).components().peekable();
    while let Some(piece) = pieces.next() {
        path.push(piece);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if pieces.peek().is_some() && metadata.is_dir() => {}
            Ok(metadata) if pieces.peek().is_none() && metadata.is_file() => return Ok(Some(metadata.len())),
            Ok(_) => return Err(invalid_data(format!("{relative} does not name a regular file in the app's folder"))),
            Err(error) if error.kind() == io::ErrorKind::NotFound => break,
            Err(error) => return Err(error),
        }
    }
    Ok(None)
}

/// Unrecorded USB/SSH installs predate the Store. Use exact bytes from known installers,
/// including their generated library icons, to distinguish them from local files with the same name.
fn known_library_entry(target: &Path, id: &str, dir: &str, relative: &str, size: u64) -> io::Result<bool> {
    let fingerprints: &[(u64, &str)] = match (id, dir, relative) {
        (LAUNCHER_ID, "launcher", "documents/Launcher.sh") => &[
            // Launcher v0.3.20 and 0.4.4, USB and SSH.
            (52_396, "fbf79e8551026d0cfaa81aedf658d725e1f4af8900fb565f2d12e0242e838b4d"),
            (52_556, "922758977e66a739ca5b1aed6d55375c18fe12adcb24342035a4b6efbc0f4ef8"),
        ],
        ("primitive-browser", "browser", "documents/Primitive Browser.sh") => &[
            // Browser v0.2.17 with the packaged icon or the icon used by deploy/install*.sh.
            (38_528, "297a908f8d4b5c3014cefc96e2ca21660106cc483dff6d9e4478fae1e1136502"),
            (124_908, "8b13b59b7375e8db1aed89e6e77dace928134946031e687aac398bd44cc1e8eb"),
        ],
        ("primitive-store", "store", "documents/Store.sh") => &[
            (486, "34ea9e6d89807113d457f5dde46e1df67e01e0f022418196f6b175d1e7d73d66"),
        ],
        ("primitive-terminal", "terminal", "documents/Terminal.sh") => &[
            (34_099, "acd08514eca0138927b1cd69d8f263d6894b77beab6688ee5f059de763303151"),
        ],
        _ => &[],
    };
    let Some(&(_, expected)) = fingerprints.iter().find(|(length, _)| *length == size) else {
        return Ok(false);
    };
    let (hash, actual_size) = sha256_file(target)?;
    Ok(actual_size == size && hash == expected)
}

fn require_replaceable(paths: &Paths, id: &str, dir: &str, previous: Option<&Installed>, relative: &str, size: u64) -> io::Result<()> {
    let shared = relative.starts_with("documents/") || relative.starts_with("extensions/");
    // Use the recorded spelling so a case-sensitive host cannot mistake a second file for the first.
    if shared && !previous.is_some_and(|app| app.files.iter().any(|file| file == relative))
        && !known_library_entry(&paths.root.join(relative), id, dir, relative, size)?
    {
        return Err(invalid_data(format!("{relative} already exists and is not managed by this app. Move it somewhere safe and try again.")));
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
    let previous = record.apps.get(id);
    for relative in files {
        let existing_size = validate_destination(paths, relative, dir)?;
        let normalized = relative.to_ascii_lowercase();
        if owned.contains(&normalized) {
            return Err(invalid_data(format!("{relative} belongs to another installed app")));
        }
        if let Some(size) = existing_size {
            require_replaceable(paths, id, dir, previous, relative, size)?;
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

fn installed_entry(source: &Source<'_>) -> Installed {
    Installed {
        id: source.manifest.id.clone(),
        dir: source.manifest.dir.clone(),
        version: source.manifest.version.clone(),
        arch: source.manifest.arch.clone(),
        installed_at: source.installed_at.to_string(),
        source_url: source.source_url.to_string(),
        sha256: source.sha256.to_string(),
        files: source.manifest.files.clone(),
        requires: source.manifest.requires.clone(),
        hooks: HookScripts::default(),
    }
}

fn journal_for_install(paths: &Paths, rec: &Record, source: &Source<'_>) -> io::Result<Journal> {
    let id = source.manifest.id.clone();
    let previous = rec.apps.get(&id).cloned();
    validate_changes(paths, rec, &id, &source.manifest.dir, &source.manifest.files)?;
    if let Some(previous) = &previous {
        if previous.id != id {
            return Err(invalid_data("the Store record does not identify the installed app".into()));
        }
        validate_changes(paths, rec, &id, &previous.dir, &previous.files)?;
    }
    let obsolete: Vec<String> = previous
        .as_ref()
        .map(|p| p.files.iter()
            .filter(|file| !source.manifest.files.iter().any(|new_file| new_file.eq_ignore_ascii_case(file)))
            .cloned().collect())
        .unwrap_or_default();

    Ok(Journal {
        id,
        version: source.manifest.version.clone(),
        dir: Some(source.manifest.dir.clone()),
        state: State::Moving,
        done: Vec::new(),
        obsolete,
        previous,
        trial: None,
        hooks: None,
    })
}

fn move_files(paths: &Paths, journal: &mut Journal, dir: &str, files: &[String], fail_after: Option<usize>) -> io::Result<()> {
    for (n, relative) in files.iter().enumerate() {
        if fail_after == Some(n) {
            return Err(io::Error::other("a test asked for a failure"));
        }
        let src = paths.staging.join(&journal.id).join(relative);
        let dst = paths.root.join(relative);
        let existing_size = validate_destination(paths, relative, dir)?;
        if let Some(size) = existing_size {
            require_replaceable(paths, &journal.id, dir, journal.previous.as_ref(), relative, size)?;
        }
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent)?;
        }
        let had_old = existing_size.is_some();
        if journal.hooks.is_some() {
            fs::File::open(&src)?.sync_all()?;
        }
        journal.done.push(Done { path: relative.clone(), prev: had_old });
        write_journal(&paths.journal, journal)?;
        if had_old {
            fs::rename(&dst, prev_of(&dst))?;
        }
        fs::rename(&src, &dst).map_err(|e| io::Error::new(e.kind(), format!("{relative}: {e}")))?;
    }
    if journal.hooks.is_some() {
        let sources: Vec<_> = files.iter().map(|file| paths.staging.join(&journal.id).join(file)).collect();
        sync_payload_directories(&paths.root, sources.iter().map(PathBuf::as_path))?;
        sync_hook_payload(paths, files)?;
    }
    Ok(())
}

/// Installs or updates an app from verified unpacked staging. `fail_after` is used by rollback tests.
pub fn install(paths: &Paths, source: &Source<'_>, fail_after: Option<usize>) -> Result<(), InstallError> {
    install_package(paths, source, HookScripts::default(), None, fail_after)
}

/// Installs a reviewed package using the caller's approval and execution implementation.
pub fn install_with_hooks(
    paths: &Paths,
    source: &Source<'_>,
    scripts: HookScripts,
    runner: &dyn HookRunner,
    fail_after: Option<usize>,
) -> Result<(), InstallError> {
    install_package(paths, source, scripts, Some(runner), fail_after)
}

fn install_package(
    paths: &Paths,
    source: &Source<'_>,
    scripts: HookScripts,
    runner: Option<&dyn HookRunner>,
    fail_after: Option<usize>,
) -> Result<(), InstallError> {
    require_idle(paths)?;
    if source.manifest.id == LAUNCHER_ID {
        return Err(invalid_data("Launcher updates must restart through the Store".into()).into());
    }
    scripts.check_manifest(&source.manifest.hooks)?;
    let mut rec = record::load(&paths.record)?;
    let mut journal = journal_for_install(paths, &rec, source)?;
    let mut installed = installed_entry(source);
    installed.hooks = scripts;
    let candidate = (!installed.hooks.is_empty()).then_some(&installed);
    let previous = journal.previous.as_ref().filter(|app| !app.hooks.is_empty());
    let shared_hooks = same_hooks(candidate, previous)?;
    if let Some(previous) = previous {
        hooks::preflight(&paths.root, previous, reviewed_runner(runner)?)?;
    }
    if let Some(candidate) = candidate {
        hooks::validate(candidate)?;
        reviewed_runner(runner)?.approve(candidate)?;
        hooks::check_staged(&paths.root, &paths.staging, candidate, shared_hooks)?;
    }
    if candidate.is_some() || previous.is_some() {
        journal.hooks = Some(HookChange { candidate: candidate.cloned(), ..HookChange::default() });
    }
    write_journal(&paths.journal, &journal)?;

    let changed = (|| -> io::Result<()> {
        if !installed.hooks.is_empty() {
            hooks::retain(&paths.root, &paths.staging, &installed, shared_hooks)?;
        }
        start_previous_hook(paths, &mut journal, runner)?;
        move_files(paths, &mut journal, &source.manifest.dir, &source.manifest.files, fail_after)?;
        start_candidate_hook(paths, &mut journal, runner)?;
        // The record becomes authoritative once the next journal state commits.
        rec.schema = record::SCHEMA;
        rec.apps.insert(installed.id.clone(), installed);
        record::save(&paths.record, &rec)
            .map_err(|error| io::Error::new(error.kind(), format!("cannot save the record: {error}")))
    })();
    if let Err(error) = changed {
        return Err(rollback_error(paths, &mut journal, runner, error));
    }

    journal.state = State::Recorded;
    write_journal(&paths.journal, &journal)?;
    finish(paths, &journal)?;
    Ok(())
}

fn reviewed_runner(runner: Option<&dyn HookRunner>) -> io::Result<&dyn HookRunner> {
    runner.ok_or_else(|| invalid_data("this Store change needs a reviewed hook runner".into()))
}

fn same_hooks(candidate: Option<&Installed>, previous: Option<&Installed>) -> io::Result<bool> {
    let (Some(candidate), Some(previous)) = (candidate, previous) else { return Ok(false) };
    if candidate.sha256 != previous.sha256 {
        return Ok(false);
    }
    if candidate.id != previous.id || candidate.hooks != previous.hooks {
        return Err(invalid_data("the retained hook identity conflicts with the installed package".into()));
    }
    Ok(true)
}

fn start_previous_hook(paths: &Paths, journal: &mut Journal, runner: Option<&dyn HookRunner>) -> io::Result<()> {
    let Some(previous) = journal.previous.as_ref().filter(|app| !app.hooks.is_empty()) else { return Ok(()) };
    let runner = reviewed_runner(runner)?;
    journal.hooks.as_mut().expect("reviewed transaction was prepared").previous_started = true;
    if let Err(error) = write_journal(&paths.journal, journal) {
        journal.hooks.as_mut().expect("reviewed transaction was prepared").previous_started = false;
        return Err(error);
    }
    hooks::invoke(&paths.root, previous, HookPhase::PreUninstall, runner)
}

fn start_candidate_hook(paths: &Paths, journal: &mut Journal, runner: Option<&dyn HookRunner>) -> io::Result<()> {
    let Some(change) = journal.hooks.as_mut().filter(|change| change.candidate.is_some()) else { return Ok(()) };
    let runner = reviewed_runner(runner)?;
    change.candidate_started = true;
    if let Err(error) = write_journal(&paths.journal, journal) {
        journal.hooks.as_mut().expect("reviewed transaction was prepared").candidate_started = false;
        return Err(error);
    }
    let candidate = journal.hooks.as_ref().and_then(|change| change.candidate.as_ref())
        .expect("reviewed candidate was prepared");
    hooks::invoke(&paths.root, candidate, HookPhase::PostInstall, runner)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LauncherUpdate {
    Idle,
    Trial,
    Reverted,
}

fn launcher_candidate(journal: &Journal) -> io::Result<&Trial> {
    let trial = journal.trial.as_ref().ok_or_else(|| invalid_data("missing Launcher trial data".into()))?;
    let candidate = &trial.candidate;
    if journal.id != LAUNCHER_ID || candidate.id != journal.id || candidate.dir != "launcher"
        || candidate.version != journal.version
    {
        return Err(invalid_data("the Store journal does not identify this Launcher update".into()));
    }
    for required in ["launcher/kl-launcher", "launcher/supervisor.sh", "launcher/setup-boot.sh"] {
        if !candidate.files.iter().any(|file| file == required) {
            return Err(invalid_data(format!("the Launcher update is missing {required}")));
        }
    }
    Ok(trial)
}

/// Records an already verified update for the supervisor. The running Launcher remains untouched.
pub fn prepare_launcher_update(paths: &Paths, source: &Source<'_>) -> io::Result<()> {
    require_idle(paths)?;
    let record = record::load(&paths.record)?;
    if !source.manifest.hooks.is_empty() || record.apps.get(LAUNCHER_ID).is_some_and(|app| !app.hooks.is_empty()) {
        return Err(invalid_data("Launcher updates cannot use reviewed hooks".into()));
    }
    let mut journal = journal_for_install(paths, &record, source)?;
    journal.state = State::Prepared;
    journal.trial = Some(Trial { candidate: installed_entry(source), attempts: 0 });
    launcher_candidate(&journal)?;
    write_journal(&paths.journal, &journal)
}

/// Applies a prepared update and reserves one persistent attempt before the supervisor starts it.
pub fn launcher_update_begin(paths: &Paths) -> io::Result<LauncherUpdate> {
    let Some(mut journal) = read_journal(&paths.journal)? else { return Ok(LauncherUpdate::Idle) };
    if journal.id != LAUNCHER_ID || journal.trial.is_none() {
        return Ok(LauncherUpdate::Idle);
    }
    let record = record::load(&paths.record)?;
    validate_journal(paths, &journal, &record)?;
    let trial = launcher_candidate(&journal)?.clone();
    match journal.state {
        State::Moving | State::Reverting => {
            recover(paths)?;
            return Ok(LauncherUpdate::Reverted);
        }
        State::Recorded => {
            recover(paths)?;
            return Ok(LauncherUpdate::Idle);
        }
        State::Prepared => {
            if let Err(error) = validate_changes(paths, &record, LAUNCHER_ID, "launcher", &trial.candidate.files) {
                eprintln!("Launcher update: {error}");
                launcher_update_revert(paths)?;
                return Ok(LauncherUpdate::Reverted);
            }
            journal.state = State::Moving;
            write_journal(&paths.journal, &journal)?;
            if let Err(error) = move_files(paths, &mut journal, &trial.candidate.dir, &trial.candidate.files, None) {
                eprintln!("Launcher update: {error}");
                recover(paths)?;
                return Ok(LauncherUpdate::Reverted);
            }
            journal.state = State::Trial;
        }
        State::Trial => {}
        State::Removing | State::Removed => return Ok(LauncherUpdate::Idle),
    }
    if trial.attempts >= LAUNCHER_TRIAL_LIMIT {
        launcher_update_revert(paths)?;
        return Ok(LauncherUpdate::Reverted);
    }
    journal.trial = Some(Trial { attempts: trial.attempts + 1, ..trial });
    write_journal(&paths.journal, &journal)?;
    Ok(LauncherUpdate::Trial)
}

/// Records a trial that presented a healthy Launcher screen and commits its files.
pub fn launcher_update_accept(paths: &Paths) -> io::Result<bool> {
    let Some(mut journal) = read_journal(&paths.journal)? else { return Ok(false) };
    if journal.id != LAUNCHER_ID || journal.state != State::Trial {
        return Ok(false);
    }
    let mut record = record::load(&paths.record)?;
    validate_journal(paths, &journal, &record)?;
    let candidate = launcher_candidate(&journal)?.candidate.clone();
    record.apps.insert(candidate.id.clone(), candidate);
    record::save(&paths.record, &record)?;
    journal.state = State::Recorded;
    write_journal(&paths.journal, &journal)?;
    finish(paths, &journal)?;
    Ok(true)
}

/// Cancels a prepared update or restores the original files of a trial.
pub fn launcher_update_revert(paths: &Paths) -> io::Result<bool> {
    let Some(mut journal) = read_journal(&paths.journal)? else { return Ok(false) };
    if journal.id != LAUNCHER_ID || journal.trial.is_none()
        || !matches!(journal.state, State::Prepared | State::Moving | State::Trial | State::Reverting)
    {
        return Ok(false);
    }
    let record = record::load(&paths.record)?;
    validate_journal(paths, &journal, &record)?;
    launcher_candidate(&journal)?;
    journal.state = State::Reverting;
    write_journal(&paths.journal, &journal)?;
    recover(paths)?;
    Ok(true)
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
    let previous = journal.previous.as_ref().filter(|app| !app.hooks.is_empty());
    let candidate = journal.hooks.as_ref().and_then(|change| change.candidate.as_ref());
    if let Some(previous) = previous {
        if !same_hooks(candidate, Some(previous))? {
            hooks::purge(&paths.root, previous)?;
        }
    }
    if journal.hooks.is_some() {
        sync_hook_payload(paths, journal.obsolete.iter().chain(journal.done.iter().map(|file| &file.path)))?;
    }
    clear_journal(paths, journal)
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
    let previous = journal.previous.as_ref().filter(|app| !app.hooks.is_empty());
    if let Some(previous) = previous {
        if previous.id != journal.id || journal.hooks.is_none() {
            return Err(invalid_data("invalid previous hook identity in the Store journal".into()));
        }
        hooks::validate(previous)?;
    }
    if let Some(change) = &journal.hooks {
        let candidate = change.candidate.as_ref();
        if matches!(journal.state, State::Prepared | State::Trial | State::Reverting)
            || (change.previous_started && previous.is_none())
            || (change.candidate_started && (candidate.is_none() || change.files_restored))
        {
            return Err(invalid_data("invalid hook operation in the Store journal".into()));
        }
        if let Some(candidate) = candidate {
            if matches!(journal.state, State::Removing | State::Removed)
                || candidate.id != journal.id || candidate.version != journal.version
                || Some(candidate.dir.as_str()) != journal.dir.as_deref() || candidate.hooks.is_empty()
            {
                return Err(invalid_data("invalid candidate hook identity in the Store journal".into()));
            }
            hooks::validate(candidate)?;
        }
        same_hooks(candidate, previous)?;
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
    recover_package(paths, None)
}

/// Recover a transaction whose reviewed system changes may need compensation on restart.
pub fn recover_with_hooks(paths: &Paths, runner: &dyn HookRunner) -> io::Result<Option<String>> {
    recover_package(paths, Some(runner))
}

fn recover_package(paths: &Paths, runner: Option<&dyn HookRunner>) -> io::Result<Option<String>> {
    let Some(mut journal) = read_journal(&paths.journal)? else {
        return Ok(None);
    };
    let rec = record::load(&paths.record)?;
    validate_journal(paths, &journal, &rec)?;
    if journal.hooks.is_some() && runner.is_none() {
        return Err(invalid_data("Open Primitive Store to recover the reviewed system change.".into()));
    }
    match journal.state {
        State::Recorded | State::Removed => {
            let description = if journal.state == State::Removed { "removal" } else { "update" };
            finish(paths, &journal)?;
            Ok(Some(format!("finished the {description} of {}", journal.id)))
        }
        State::Prepared | State::Trial => Ok(None),
        State::Moving | State::Removing | State::Reverting => {
            rollback(paths, &mut journal, runner)?;
            let description = if journal.state == State::Removing { "removal" } else { "install" };
            Ok(Some(format!("undid the unfinished {description} of {} {}", journal.id, journal.version)))
        }
    }
}

fn rollback_error(paths: &Paths, journal: &mut Journal, runner: Option<&dyn HookRunner>, error: io::Error) -> InstallError {
    match rollback(paths, journal, runner) {
        Ok(()) => InstallError::RolledBack(error.to_string()),
        Err(recovery) => InstallError::Io(io::Error::other(format!(
            "the Store change failed: {error}. Recovery is still required: {recovery}"
        ))),
    }
}

fn rollback(paths: &Paths, journal: &mut Journal, runner: Option<&dyn HookRunner>) -> io::Result<()> {
    if let Some(change) = &journal.hooks {
        if change.previous_started {
            hooks::preflight(&paths.root, journal.previous.as_ref().unwrap(), reviewed_runner(runner)?)?;
        }
        if change.candidate_started {
            hooks::invoke(&paths.root, change.candidate.as_ref().unwrap(), HookPhase::PreUninstall, reviewed_runner(runner)?)?;
            journal.hooks.as_mut().unwrap().candidate_started = false;
            write_journal(&paths.journal, journal)?;
        }
    }

    if !journal.hooks.as_ref().is_some_and(|change| change.files_restored) {
        undo(paths, &journal.done)?;
        let mut rec = record::load(&paths.record)?;
        if rec.apps.get(&journal.id) != journal.previous.as_ref() {
            if let Some(previous) = &journal.previous {
                rec.apps.insert(journal.id.clone(), previous.clone());
            } else {
                rec.apps.remove(&journal.id);
            }
            record::save(&paths.record, &rec)?;
        }
        if let Some(change) = &mut journal.hooks {
            sync_hook_payload(paths, journal.done.iter().map(|file| &file.path))?;
            change.files_restored = true;
            write_journal(&paths.journal, journal)?;
        }
    }

    if let Some(change) = &journal.hooks {
        if change.previous_started {
            hooks::invoke(&paths.root, journal.previous.as_ref().unwrap(), HookPhase::PostInstall, reviewed_runner(runner)?)?;
            journal.hooks.as_mut().unwrap().previous_started = false;
            write_journal(&paths.journal, journal)?;
        }
        if let Some(candidate) = journal.hooks.as_ref().and_then(|change| change.candidate.as_ref()) {
            let previous = journal.previous.as_ref().filter(|app| !app.hooks.is_empty());
            if !same_hooks(Some(candidate), previous)? {
                hooks::purge(&paths.root, candidate)?;
            }
        }
    }
    clear_journal(paths, journal)
}

/// Removes an installed app: exactly the files in its record, then the empty folders above them (never the root).
/// Refused when another installed app requires it.
pub fn remove(paths: &Paths, id: &str) -> Result<(), InstallError> {
    remove_package(paths, id, None)
}

/// Remove a reviewed package after its system cleanup hook succeeds.
pub fn remove_with_hooks(paths: &Paths, id: &str, runner: &dyn HookRunner) -> Result<(), InstallError> {
    remove_package(paths, id, Some(runner))
}

fn remove_package(paths: &Paths, id: &str, runner: Option<&dyn HookRunner>) -> Result<(), InstallError> {
    require_idle(paths)?;
    let mut rec = record::load(&paths.record)?;
    let deps = rec.dependents_of(id);
    if !deps.is_empty() {
        return Err(InstallError::RolledBack(format!("{} needs {id}; remove it first", deps.join(", "))));
    }
    let Some(app) = rec.apps.get(id).cloned() else {
        return Ok(());
    };
    if app.id != id {
        return Err(invalid_data("the Store record does not identify the installed app".into()).into());
    }
    validate_changes(paths, &rec, id, &app.dir, &app.files)?;
    let uses_hooks = !app.hooks.is_empty();
    if uses_hooks {
        hooks::preflight(&paths.root, &app, reviewed_runner(runner)?)?;
    }
    let mut journal = Journal {
        id: id.to_string(),
        version: app.version.clone(),
        dir: Some(app.dir.clone()),
        state: State::Removing,
        done: Vec::new(),
        obsolete: Vec::new(),
        previous: Some(app.clone()),
        trial: None,
        hooks: uses_hooks.then(HookChange::default),
    };
    write_journal(&paths.journal, &journal)?;
    let moved = (|| -> io::Result<()> {
        start_previous_hook(paths, &mut journal, runner)?;
        for relative in &app.files {
            let target = paths.root.join(relative);
            if target.try_exists()? {
                journal.done.push(Done { path: relative.clone(), prev: true });
                write_journal(&paths.journal, &journal)?;
                fs::rename(&target, prev_of(&target))?;
            }
        }
        if uses_hooks {
            sync_hook_payload(paths, journal.done.iter().map(|file| &file.path))?;
        }
        Ok(())
    })();
    if let Err(error) = moved {
        return Err(rollback_error(paths, &mut journal, runner, error));
    }
    rec.apps.remove(id);
    if let Err(error) = record::save(&paths.record, &rec) {
        return Err(rollback_error(paths, &mut journal, runner, error));
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
    use std::cell::{Cell, RefCell};
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

    struct ReviewFixture<'a> {
        root: &'a Path,
        approved: Cell<bool>,
        calls: RefCell<Vec<(String, HookPhase)>>,
        fail: RefCell<Vec<(String, HookPhase)>>,
    }

    impl<'a> ReviewFixture<'a> {
        fn new(root: &'a Path) -> Self {
            Self { root, approved: Cell::new(true), calls: RefCell::default(), fail: RefCell::default() }
        }

        fn effect(&self) -> PathBuf {
            self.root.join("mock-system-setting")
        }

        fn fail_on(&self, version: &str, phase: HookPhase) {
            self.fail.borrow_mut().push((version.into(), phase));
        }
    }

    impl HookRunner for ReviewFixture<'_> {
        fn approve(&self, _package: &Installed) -> io::Result<()> {
            if self.approved.get() {
                Ok(())
            } else {
                Err(io::Error::new(io::ErrorKind::PermissionDenied, "fixture review denied"))
            }
        }

        fn run(&self, package: &Installed, phase: HookPhase, script: &Path) -> io::Result<()> {
            let bytes = hooks::verified_bytes(script, package.hooks.digest(phase).unwrap())?;
            assert_eq!(bytes, script_body(&package.version, phase));
            let app = fs::read_to_string(self.root.join(&package.files[0]))?;
            assert!(app.starts_with(&package.version), "hook saw wrong payload: {app}");
            let call = (package.version.clone(), phase);
            self.calls.borrow_mut().push(call.clone());
            match phase {
                HookPhase::PostInstall => fs::write(self.effect(), package.version.as_bytes())?,
                HookPhase::PreUninstall => remove_if_exists(&self.effect())?,
            }
            let mut fail = self.fail.borrow_mut();
            if let Some(index) = fail.iter().position(|expected| *expected == call) {
                fail.remove(index);
                return Err(io::Error::other("fixture stopped after its first system effect"));
            }
            Ok(())
        }
    }

    fn script_body(version: &str, phase: HookPhase) -> Vec<u8> {
        format!("{} {version}", phase.file_name()).into_bytes()
    }

    fn stage_reviewed(paths: &Paths, manifest: &mut Manifest, label: &str) -> HookScripts {
        manifest.hooks = vec![hooks::POST_INSTALL_PATH.into(), hooks::PRE_UNINSTALL_PATH.into()];
        stage(paths, manifest, label);
        let mut digests = HookScripts::default();
        let staging = paths.staging.join(&manifest.id);
        fs::create_dir_all(staging.join("hooks")).unwrap();
        for phase in [HookPhase::PostInstall, HookPhase::PreUninstall] {
            let bytes = script_body(&manifest.version, phase);
            fs::write(staging.join(phase.manifest_path()), &bytes).unwrap();
            match phase {
                HookPhase::PostInstall => digests.post_install = Some(hooks::sha256(&bytes)),
                HookPhase::PreUninstall => digests.pre_uninstall = Some(hooks::sha256(&bytes)),
            }
        }
        digests
    }

    fn reviewed_source<'a>(manifest: &'a Manifest, asset_sha: &'a str) -> Source<'a> {
        Source { sha256: asset_sha, ..src(manifest) }
    }

    fn retained_dir(paths: &Paths, digest: &str) -> PathBuf {
        paths.root.join("launcher/store/hooks/notes").join(digest)
    }

    #[test]
    fn reviewed_update_and_removal_keep_only_the_current_hook_pair() {
        let w = world("reviewed-hooks");
        let runner = ReviewFixture::new(&w.paths.root);
        let mut old = manifest("notes", "1.0.0", &["notes/app"], &[]);
        let mut new = manifest("notes", "2.0.0", &["notes/app"], &[]);
        let old_sha = "1".repeat(64);
        let new_sha = "2".repeat(64);
        let old_scripts = stage_reviewed(&w.paths, &mut old, "old");
        install_with_hooks(&w.paths, &reviewed_source(&old, &old_sha), old_scripts.clone(), &runner, None).unwrap();
        assert!(retained_dir(&w.paths, &old_sha).is_dir());
        assert_eq!(record::load(&w.paths.record).unwrap().apps["notes"].hooks, old_scripts);

        let new_scripts = stage_reviewed(&w.paths, &mut new, "new");
        install_with_hooks(&w.paths, &reviewed_source(&new, &new_sha), new_scripts, &runner, None).unwrap();
        assert!(!retained_dir(&w.paths, &old_sha).exists());
        assert!(retained_dir(&w.paths, &new_sha).is_dir());
        assert_eq!(fs::read_to_string(runner.effect()).unwrap(), "2.0.0");
        assert!(remove(&w.paths, "notes").is_err());
        assert!(!w.paths.journal.exists());
        remove_with_hooks(&w.paths, "notes", &runner).unwrap();
        assert!(!runner.effect().exists());
        assert!(!retained_dir(&w.paths, &new_sha).exists());
        assert_eq!(*runner.calls.borrow(), [
            ("1.0.0".into(), HookPhase::PostInstall),
            ("1.0.0".into(), HookPhase::PreUninstall),
            ("2.0.0".into(), HookPhase::PostInstall),
            ("2.0.0".into(), HookPhase::PreUninstall),
        ]);
    }

    #[test]
    fn recovery_retries_a_half_finished_hook_compensation_before_restoring_the_old_app() {
        let w = world("reviewed-retry");
        let runner = ReviewFixture::new(&w.paths.root);
        let mut old = manifest("notes", "1.0.0", &["notes/app"], &[]);
        let mut new = manifest("notes", "2.0.0", &["notes/app"], &[]);
        let old_sha = "1".repeat(64);
        let new_sha = "2".repeat(64);
        let scripts = stage_reviewed(&w.paths, &mut old, "old");
        install_with_hooks(&w.paths, &reviewed_source(&old, &old_sha), scripts, &runner, None).unwrap();
        let scripts = stage_reviewed(&w.paths, &mut new, "new");
        runner.fail_on("2.0.0", HookPhase::PostInstall);
        runner.fail_on("2.0.0", HookPhase::PreUninstall);
        let error = install_with_hooks(&w.paths, &reviewed_source(&new, &new_sha), scripts, &runner, None).unwrap_err();
        assert!(error.to_string().contains("Recovery is still required"));
        assert!(recover(&w.paths).is_err());
        assert_eq!(fs::read_to_string(w.paths.root.join("notes/app")).unwrap(), "2.0.0 new");

        recover_with_hooks(&w.paths, &runner).unwrap().unwrap();
        assert!(recover_with_hooks(&w.paths, &runner).unwrap().is_none());
        assert_eq!(fs::read_to_string(w.paths.root.join("notes/app")).unwrap(), "1.0.0 old");
        assert_eq!(fs::read_to_string(runner.effect()).unwrap(), "1.0.0");
        assert_eq!(record::load(&w.paths.record).unwrap().apps["notes"].version, "1.0.0");
        assert!(retained_dir(&w.paths, &old_sha).exists());
        assert!(!retained_dir(&w.paths, &new_sha).exists());
    }

    #[test]
    fn a_failed_removal_hook_reapplies_the_previous_system_setting() {
        let w = world("reviewed-remove-rollback");
        let runner = ReviewFixture::new(&w.paths.root);
        let mut app = manifest("notes", "1.0.0", &["notes/app"], &[]);
        let sha = "1".repeat(64);
        let scripts = stage_reviewed(&w.paths, &mut app, "old");
        install_with_hooks(&w.paths, &reviewed_source(&app, &sha), scripts, &runner, None).unwrap();
        runner.fail_on("1.0.0", HookPhase::PreUninstall);
        assert!(matches!(remove_with_hooks(&w.paths, "notes", &runner), Err(InstallError::RolledBack(_))));
        assert_eq!(fs::read_to_string(runner.effect()).unwrap(), "1.0.0");
        assert!(w.paths.root.join("notes/app").is_file());
        assert!(retained_dir(&w.paths, &sha).is_dir());
        assert!(!w.paths.journal.exists());
    }

    #[test]
    fn power_cut_between_record_and_commit_restores_both_payload_and_system_effects() {
        let w = world("reviewed-record-before-commit");
        let runner = ReviewFixture::new(&w.paths.root);
        let mut old = manifest("notes", "1.0.0", &["notes/app"], &[]);
        let mut new = manifest("notes", "2.0.0", &["notes/app"], &[]);
        let old_sha = "1".repeat(64);
        let new_sha = "2".repeat(64);
        let scripts = stage_reviewed(&w.paths, &mut old, "old");
        install_with_hooks(&w.paths, &reviewed_source(&old, &old_sha), scripts, &runner, None).unwrap();

        let scripts = stage_reviewed(&w.paths, &mut new, "new");
        let mut record = record::load(&w.paths.record).unwrap();
        let source = reviewed_source(&new, &new_sha);
        let mut journal = journal_for_install(&w.paths, &record, &source).unwrap();
        let mut candidate = installed_entry(&source);
        candidate.hooks = scripts;
        journal.hooks = Some(HookChange { candidate: Some(candidate.clone()), ..HookChange::default() });
        write_journal(&w.paths.journal, &journal).unwrap();
        hooks::retain(&w.paths.root, &w.paths.staging, &candidate, false).unwrap();
        start_previous_hook(&w.paths, &mut journal, Some(&runner)).unwrap();
        move_files(&w.paths, &mut journal, &new.dir, &new.files, None).unwrap();
        start_candidate_hook(&w.paths, &mut journal, Some(&runner)).unwrap();
        record.apps.insert(candidate.id.clone(), candidate);
        record::save(&w.paths.record, &record).unwrap();

        assert_eq!(fs::read_to_string(runner.effect()).unwrap(), "2.0.0");
        recover_with_hooks(&w.paths, &runner).unwrap().unwrap();
        assert_eq!(fs::read_to_string(runner.effect()).unwrap(), "1.0.0");
        assert_eq!(fs::read_to_string(w.paths.root.join("notes/app")).unwrap(), "1.0.0 old");
        assert_eq!(record::load(&w.paths.record).unwrap().apps["notes"].version, "1.0.0");
        assert!(!retained_dir(&w.paths, &new_sha).exists());
    }

    #[test]
    fn denied_review_and_changed_bytes_stop_before_any_hook_or_payload_change() {
        let w = world("reviewed-preflight");
        let runner = ReviewFixture::new(&w.paths.root);
        let mut app = manifest("notes", "1.0.0", &["notes/app"], &[]);
        let sha = "1".repeat(64);
        let scripts = stage_reviewed(&w.paths, &mut app, "new");
        let source = reviewed_source(&app, &sha);
        runner.approved.set(false);
        assert!(install_with_hooks(&w.paths, &source, scripts.clone(), &runner, None).is_err());
        runner.approved.set(true);
        fs::write(w.paths.staging.join("notes").join(hooks::PRE_UNINSTALL_PATH), "changed").unwrap();
        assert!(install_with_hooks(&w.paths, &source, scripts, &runner, None).is_err());
        assert!(runner.calls.borrow().is_empty());
        assert!(!runner.effect().exists());
        assert!(!w.paths.root.join("notes/app").exists());
        assert!(!w.paths.journal.exists());
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
            trial: None,
            hooks: None,
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
            trial: None,
            hooks: None,
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
            trial: None,
            hooks: None,
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
            trial: None,
            hooks: None,
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
    fn an_install_cannot_overwrite_unrecorded_shared_files() {
        for (name, relative) in [
            ("unowned-document", "documents/books/Personal.txt"),
            ("unowned-extension", "extensions/notes/config.xml"),
        ] {
            let w = world(name);
            let target = w.paths.root.join(relative);
            fs::create_dir_all(target.parent().unwrap()).unwrap();
            fs::write(&target, "kept by the user").unwrap();
            let app = manifest("notes", "1.0.0", &["notes/app", relative], &[]);
            stage(&w.paths, &app, "new");

            let err = install(&w.paths, &src(&app), None).unwrap_err().to_string();
            assert!(err.contains(relative) && err.contains("already exists"), "{err}");
            assert_eq!(fs::read(&target).unwrap(), b"kept by the user");
            assert!(!w.paths.root.join("notes/app").exists());
            assert!(!w.paths.journal.exists());
            assert!(record::load(&w.paths.record).unwrap().apps.is_empty());
        }
    }

    #[test]
    fn the_store_can_update_shared_files_it_installed() {
        let w = world("managed-shared-files");
        let files = ["documents/Notes.sh", "extensions/notes/config.xml"];
        for version in ["1.0.0", "1.0.0", "2.0.0"] {
            let app = manifest("notes", version, &files, &[]);
            stage(&w.paths, &app, "managed");
            install(&w.paths, &src(&app), None).unwrap();
            for file in files {
                assert_eq!(fs::read_to_string(w.paths.root.join(file)).unwrap(), format!("{version} managed"));
            }
        }
    }

    #[test]
    fn an_app_update_cannot_claim_new_shared_files_already_on_disk() {
        let w = world("update-unowned-document");
        let v1 = manifest("notes", "1.0.0", &["notes/app"], &[]);
        stage(&w.paths, &v1, "old");
        install(&w.paths, &src(&v1), None).unwrap();
        fs::create_dir_all(w.paths.root.join("documents")).unwrap();
        fs::write(w.paths.root.join("documents/Notes.sh"), "my local note script").unwrap();
        let v2 = manifest("notes", "2.0.0", &["notes/app", "documents/Notes.sh"], &[]);
        stage(&w.paths, &v2, "new");

        assert!(install(&w.paths, &src(&v2), None).unwrap_err().to_string().contains("documents/Notes.sh"));
        assert_eq!(fs::read(w.paths.root.join("documents/Notes.sh")).unwrap(), b"my local note script");
        assert_eq!(fs::read(w.paths.root.join("notes/app")).unwrap(), b"1.0.0 old");
        assert_eq!(record::load(&w.paths.record).unwrap().apps["notes"].version, "1.0.0");
    }

    #[test]
    fn a_legacy_entry_name_and_size_do_not_grant_ownership() {
        let w = world("lookalike-legacy");
        let relative = "documents/Store.sh";
        let target = w.paths.root.join(relative);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        let mut custom = b"#!/bin/sh\n# Name: Primitive Store\n# Author: Primitive AI\n".to_vec();
        custom.resize(486, b' ');
        fs::write(&target, &custom).unwrap();
        let mut app = manifest("primitive-store", "1.0.0", &[relative], &[]);
        app.dir = "store".into();
        stage(&w.paths, &app, "candidate");

        assert!(install(&w.paths, &src(&app), None).unwrap_err().to_string().contains(relative));
        assert_eq!(fs::read(target).unwrap(), custom);
    }

    fn launcher_fixture(paths: &Paths) -> Manifest {
        let mut candidate = manifest(
            LAUNCHER_ID,
            "1.2.0",
            &["launcher/kl-launcher", "launcher/setup-boot.sh", "launcher/supervisor.sh"],
            &[],
        );
        candidate.dir = "launcher".to_string();
        fs::create_dir_all(paths.root.join("launcher")).unwrap();
        for file in &candidate.files {
            fs::write(paths.root.join(file), b"old launcher").unwrap();
        }
        stage(paths, &candidate, "candidate");
        candidate
    }

    #[test]
    fn a_launcher_update_is_staged_then_committed_after_a_healthy_trial() {
        let w = world("launcher-accept");
        let candidate = launcher_fixture(&w.paths);
        let binary = w.paths.root.join("launcher/kl-launcher");
        assert!(install(&w.paths, &src(&candidate), None).is_err());
        prepare_launcher_update(&w.paths, &src(&candidate)).unwrap();
        assert!(recover(&w.paths).unwrap().is_none(), "startup recovery leaves staged updates alone");
        assert_eq!(fs::read(&binary).unwrap(), b"old launcher");
        assert_eq!(launcher_update_begin(&w.paths).unwrap(), LauncherUpdate::Trial);
        assert!(recover(&w.paths).unwrap().is_none(), "startup recovery preserves a healthy trial");
        assert_eq!(fs::read(&binary).unwrap(), b"1.2.0 candidate");
        assert_eq!(fs::read(prev_of(&binary)).unwrap(), b"old launcher");
        assert!(!record::load(&w.paths.record).unwrap().apps.contains_key(LAUNCHER_ID));
        assert!(launcher_update_accept(&w.paths).unwrap());
        assert_eq!(record::load(&w.paths.record).unwrap().apps[LAUNCHER_ID].version, "1.2.0");
        assert!(!prev_of(&binary).exists());
        assert_eq!(launcher_update_begin(&w.paths).unwrap(), LauncherUpdate::Idle);
    }

    #[test]
    fn three_launcher_attempts_restore_the_launcher_and_leave_the_old_record() {
        let w = world("launcher-revert");
        let candidate = launcher_fixture(&w.paths);
        let record = record::load(&w.paths.record).unwrap();
        prepare_launcher_update(&w.paths, &src(&candidate)).unwrap();
        for _ in 0..LAUNCHER_TRIAL_LIMIT {
            assert_eq!(launcher_update_begin(&w.paths).unwrap(), LauncherUpdate::Trial);
        }
        assert_eq!(launcher_update_begin(&w.paths).unwrap(), LauncherUpdate::Reverted);
        for file in &candidate.files {
            let file = w.paths.root.join(file);
            assert_eq!(fs::read(&file).unwrap(), b"old launcher");
            assert!(!prev_of(&file).exists());
        }
        assert_eq!(record::load(&w.paths.record).unwrap(), record);
        assert!(!w.paths.journal.exists());
    }

    #[test]
    fn a_missing_staged_launcher_file_restores_any_file_already_moved() {
        let w = world("launcher-interrupted");
        let candidate = launcher_fixture(&w.paths);
        prepare_launcher_update(&w.paths, &src(&candidate)).unwrap();
        fs::remove_file(w.paths.staging.join(LAUNCHER_ID).join("launcher/supervisor.sh")).unwrap();
        assert_eq!(launcher_update_begin(&w.paths).unwrap(), LauncherUpdate::Reverted);
        for file in &candidate.files {
            assert_eq!(fs::read(w.paths.root.join(file)).unwrap(), b"old launcher");
        }
        assert_eq!(launcher_update_begin(&w.paths).unwrap(), LauncherUpdate::Idle);
    }

    #[test]
    fn a_prepared_launcher_update_cannot_overwrite_a_new_user_file() {
        let w = world("launcher-document-conflict");
        let mut candidate = launcher_fixture(&w.paths);
        let relative = "documents/Launcher.sh";
        candidate.files.push(relative.into());
        stage(&w.paths, &candidate, "candidate");
        prepare_launcher_update(&w.paths, &src(&candidate)).unwrap();
        fs::create_dir_all(w.paths.root.join("documents")).unwrap();
        fs::write(w.paths.root.join(relative), b"user file").unwrap();

        assert_eq!(launcher_update_begin(&w.paths).unwrap(), LauncherUpdate::Reverted);
        assert_eq!(fs::read(w.paths.root.join(relative)).unwrap(), b"user file");
        assert_eq!(fs::read(w.paths.root.join("launcher/kl-launcher")).unwrap(), b"old launcher");
        assert!(!w.paths.journal.exists());
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
