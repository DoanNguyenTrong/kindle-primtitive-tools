//! Unpacking a package safely. Every name is checked before anything is written: no absolute paths, no `..`, no
//! backslashes, no links, no folders outside the package's own top folder, and size limits. The files are then checked
//! against the manifest: exactly the listed files, in the allowed places.

use crate::catalog::Manifest;
use crate::hooks::{HookScripts, MAX_HOOK_SIZE, POST_INSTALL_PATH, PRE_UNINSTALL_PATH};
use std::collections::BTreeSet;
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};

const MAX_MANIFEST_BYTES: u64 = 256 * 1024;
const RESERVED: [&str; 6] = ["documents", "extensions", "system", "fonts", "screensaver", "koreader"];

/// Limits on a package. A Kindle has a few hundred megabytes free; a package is refused past these.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_total_bytes: u64,
    pub max_files: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits { max_total_bytes: 200 * 1024 * 1024, max_files: 5000 }
    }
}

#[derive(Debug)]
pub enum UnpackError {
    Zip(String),
    UnsafeName(String),
    Symlink(String),
    TooLarge { limit: u64 },
    TooManyFiles { limit: usize },
    TopFolder(String),
    Io(io::Error),
    NotInManifest(String),
    MissingFromPackage(String),
    DuplicateFile(String),
    ReservedPlace(String),
    BadManifest(String),
    UnsupportedHooks,
    InvalidHooks,
}

impl fmt::Display for UnpackError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UnpackError::Zip(e) => write!(f, "the package is not a valid zip file: {e}"),
            UnpackError::UnsafeName(n) => write!(f, "the package has an unsafe file name: {n}"),
            UnpackError::Symlink(n) => write!(f, "the package contains a link, which is not allowed: {n}"),
            UnpackError::TooLarge { limit } => write!(f, "the package is larger than {limit} bytes"),
            UnpackError::TooManyFiles { limit } => write!(f, "the package has more than {limit} files"),
            UnpackError::TopFolder(n) => write!(f, "the package must have one top folder, found {n}"),
            UnpackError::Io(e) => write!(f, "cannot write the package: {e}"),
            UnpackError::NotInManifest(n) => write!(f, "{n} is in the package but not in its manifest"),
            UnpackError::MissingFromPackage(n) => write!(f, "{n} is in the manifest but not in the package"),
            UnpackError::DuplicateFile(n) => write!(f, "{n} appears more than once in the package"),
            UnpackError::ReservedPlace(n) => write!(f, "{n} is outside the places an app may use"),
            UnpackError::BadManifest(e) => write!(f, "the manifest is not valid: {e}"),
            UnpackError::UnsupportedHooks => write!(f, "this package needs reviewed install steps"),
            UnpackError::InvalidHooks => write!(f, "the package must contain exactly its declared install and undo hooks"),
        }
    }
}

impl From<io::Error> for UnpackError {
    fn from(e: io::Error) -> Self {
        UnpackError::Io(e)
    }
}

/// Files that are part of a package but are not installed into the Kindle (the store reads or keeps them).
pub fn is_package_only(name: &str) -> bool {
    matches!(name, "manifest.json" | "INSTALL.md" | "LICENSE" | "LICENSE-DOCS")
}

/// A relative path inside the package: no absolute paths, no `..`, no backslashes, no empty or `.` parts, no NUL.
pub fn is_safe_relative(name: &str) -> bool {
    if name.is_empty() || name.contains('\\') || name.contains('\0') || name.starts_with('/') {
        return false;
    }
    // Every part must be a plain name: an empty part (from `//`) or `.`/`..` is refused before the path is looked at,
    // since a path normalises `//` away and `join` with an absolute part would leave the folder.
    name.split('/').all(|p| !p.is_empty() && p != "." && p != "..")
        && Path::new(name).components().all(|c| matches!(c, Component::Normal(_)))
}

/// Whether an installed path is in a place a module may write to: its own folder (`dir/`), its KUAL extension
/// (`extensions/dir/`), or the library entries (`documents/`). Reserved folders are never allowed.
pub fn is_allowed_place(name: &str, dir: &str) -> bool {
    if !valid_module_dir(dir) {
        return false;
    }
    let mut parts = name.splitn(3, '/');
    let first = parts.next().unwrap_or("");
    match first {
        "documents" => parts.next().is_some(),
        "extensions" => parts.next() == Some(dir) && parts.next().is_some(),
        "launcher" if parts.next().is_some_and(|part| part.eq_ignore_ascii_case("store")) => false,
        other => other == dir && name.contains('/'),
    }
}

pub fn valid_module_dir(dir: &str) -> bool {
    !dir.is_empty() && !dir.starts_with('-') && dir.len() <= 40
        && dir.bytes().all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && !RESERVED.contains(&dir)
}

/// Unpacks a package into `dest` and checks it against `manifest`. Returns the installed file names, relative to the
/// package's top folder and sorted.
pub fn unpack(zip_path: &Path, dest: &Path, manifest: &Manifest, limits: Limits) -> Result<Vec<String>, UnpackError> {
    unpack_inner(zip_path, dest, manifest, limits, false).map(|(files, _)| files)
}

/// Inventory both hooks, stage them separately from installed files, and return their digests for approval.
pub(crate) fn unpack_reviewed(zip_path: &Path, dest: &Path, manifest: &Manifest, limits: Limits) -> Result<HookScripts, UnpackError> {
    unpack_inner(zip_path, dest, manifest, limits, true).map(|(_, hooks)| hooks)
}

fn unpack_inner(zip_path: &Path, dest: &Path, manifest: &Manifest, limits: Limits, review_hooks: bool) -> Result<(Vec<String>, HookScripts), UnpackError> {
    let declared_hooks: BTreeSet<_> = manifest.hooks.iter().map(String::as_str).collect();
    if !review_hooks && !declared_hooks.is_empty() {
        return Err(UnpackError::UnsupportedHooks);
    }
    if !declared_hooks.is_empty()
        && (declared_hooks.len() != manifest.hooks.len()
            || declared_hooks != BTreeSet::from([POST_INSTALL_PATH, PRE_UNINSTALL_PATH]))
    {
        return Err(UnpackError::InvalidHooks);
    }
    let file = File::open(zip_path)?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| UnpackError::Zip(e.to_string()))?;
    if archive.len() > limits.max_files {
        return Err(UnpackError::TooManyFiles { limit: limits.max_files });
    }

    // Validate the complete archive before writing payload files.
    let mut top: Option<String> = None;
    let mut total = 0u64;
    let mut entries: Vec<(usize, String)> = Vec::new();
    let mut hooks = Vec::new();
    let mut names = BTreeSet::new();
    for i in 0..archive.len() {
        let entry = archive.by_index(i).map_err(|e| UnpackError::Zip(e.to_string()))?;
        let name = entry.name();
        let relative = name.strip_suffix('/').unwrap_or(name);
        if !is_safe_relative(relative) {
            return Err(UnpackError::UnsafeName(name.to_string()));
        }
        if entry.unix_mode().is_some_and(|m| m & 0o170000 == 0o120000) {
            return Err(UnpackError::Symlink(name.to_string()));
        }
        if !names.insert(relative.to_ascii_lowercase()) {
            return Err(UnpackError::DuplicateFile(name.to_string()));
        }
        let (folder, rest) = relative.split_once('/').unwrap_or((relative, ""));
        match &top {
            None => top = Some(folder.to_string()),
            Some(t) if t != folder => return Err(UnpackError::TopFolder(format!("{t} and {folder}"))),
            _ => {}
        }
        if entry.is_dir() {
            continue;
        }
        if rest.is_empty() {
            return Err(UnpackError::UnsafeName(name.to_string()));
        }
        total = total.saturating_add(entry.size());
        if total > limits.max_total_bytes {
            return Err(UnpackError::TooLarge { limit: limits.max_total_bytes });
        }
        if rest.split('/').next().is_some_and(|part| part.eq_ignore_ascii_case("hooks")) {
            if !review_hooks || !declared_hooks.contains(rest) {
                return Err(UnpackError::UnsupportedHooks);
            }
            if entry.size() == 0 || entry.size() > MAX_HOOK_SIZE {
                return Err(UnpackError::InvalidHooks);
            }
            hooks.push((i, rest.to_string()));
        } else if !is_package_only(rest) {
            entries.push((i, rest.to_string()));
        }
    }
    top.ok_or_else(|| UnpackError::Zip("the package is empty".into()))?;
    if declared_hooks.len() != hooks.len() {
        return Err(UnpackError::InvalidHooks);
    }

    // The manifest's own list must be safe too.
    for f in &manifest.files {
        if !is_safe_relative(f) || !is_allowed_place(f, &manifest.dir) {
            return Err(UnpackError::ReservedPlace(f.clone()));
        }
    }
    let listed: BTreeSet<&str> = manifest.files.iter().map(String::as_str).collect();
    if listed.len() != manifest.files.len() {
        return Err(UnpackError::BadManifest("a file appears twice in the manifest".into()));
    }
    let present: BTreeSet<&str> = entries.iter().map(|(_, name)| name.as_str()).collect();
    if let Some(extra) = present.difference(&listed).next() {
        return Err(UnpackError::NotInManifest((*extra).to_string()));
    }
    if let Some(missing) = listed.difference(&present).next() {
        return Err(UnpackError::MissingFromPackage((*missing).to_string()));
    }

    // Second pass: write the files.
    let mut written: Vec<String> = Vec::with_capacity(entries.len());
    let mut scripts = HookScripts::default();
    let mut remaining = limits.max_total_bytes;
    for (i, rest) in hooks.iter().chain(&entries) {
        let mut entry = archive.by_index(*i).map_err(|e| UnpackError::Zip(e.to_string()))?;
        let is_hook = matches!(rest.as_str(), POST_INSTALL_PATH | PRE_UNINSTALL_PATH);
        let out: PathBuf = dest.join(rest);
        if let Some(parent) = out.parent() {
            fs::create_dir_all(parent)?;
        }
        let mode = entry.unix_mode();
        let mut out_file = File::create(&out)?;
        let mut buf = [0u8; 64 * 1024];
        let mut hook_room = MAX_HOOK_SIZE;
        loop {
            let n = entry.read(&mut buf)?;
            if n == 0 {
                break;
            }
            remaining = remaining.checked_sub(n as u64)
                .ok_or(UnpackError::TooLarge { limit: limits.max_total_bytes })?;
            if is_hook {
                hook_room = hook_room.checked_sub(n as u64).ok_or(UnpackError::InvalidHooks)?;
                if buf[..n].contains(&0) {
                    return Err(UnpackError::InvalidHooks);
                }
            }
            out_file.write_all(&buf[..n])?;
        }
        drop(out_file);
        if is_hook {
            let (digest, length) = crate::verify::sha256_file(&out)?;
            if length == 0 {
                return Err(UnpackError::InvalidHooks);
            }
            match rest.as_str() {
                POST_INSTALL_PATH => scripts.post_install = Some(digest),
                PRE_UNINSTALL_PATH => scripts.pre_uninstall = Some(digest),
                _ => unreachable!("only the known hook filenames were inventoried"),
            }
            continue;
        }
        // Keep the package's permissions: a program or a library entry must stay executable on the Kindle.
        if let Some(m) = mode {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&out, fs::Permissions::from_mode(m & 0o777))?;
        }
        written.push(rest.clone());
    }
    written.sort();

    Ok((written, scripts))
}

/// Reads and checks a package's `manifest.json` (the entry inside the zip, at its top folder).
pub fn read_manifest(zip_path: &Path) -> Result<Manifest, UnpackError> {
    let file = File::open(zip_path)?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| UnpackError::Zip(e.to_string()))?;
    let file_limit = Limits::default().max_files;
    if archive.len() > file_limit {
        return Err(UnpackError::TooManyFiles { limit: file_limit });
    }
    let mut manifest = None;
    for index in 0..archive.len() {
        let entry = archive.by_index(index).map_err(|e| UnpackError::Zip(e.to_string()))?;
        let name = entry.name();
        if !name.ends_with("/manifest.json") || name.matches('/').count() != 1 {
            continue;
        }
        if manifest.replace(index).is_some() {
            return Err(UnpackError::BadManifest("more than one manifest.json".into()));
        }
    }
    let index = manifest.ok_or_else(|| UnpackError::BadManifest("no manifest.json at the top folder".into()))?;
    let entry = archive.by_index(index).map_err(|e| UnpackError::Zip(e.to_string()))?;
    let mut text = String::new();
    if entry.size() > MAX_MANIFEST_BYTES || entry.take(MAX_MANIFEST_BYTES + 1).read_to_string(&mut text)? as u64 > MAX_MANIFEST_BYTES {
        return Err(UnpackError::TooLarge { limit: MAX_MANIFEST_BYTES });
    }
    serde_json::from_str(&text).map_err(|e| UnpackError::BadManifest(e.to_string()))
}

#[cfg(test)]
pub(crate) mod fixtures {
    use std::io::Write;
    use std::path::{Path, PathBuf};
    use zip::write::SimpleFileOptions;

    /// Writes a zip with the given `(name, content)` entries; a name ending in `/` is a folder; a `link` entry has its
    /// mode set to a symbolic link.
    pub fn make_zip(path: &Path, entries: &[(&str, &[u8], bool)]) -> PathBuf {
        let file = std::fs::File::create(path).unwrap();
        let mut zw = zip::ZipWriter::new(file);
        for (name, content, link) in entries {
            if name.ends_with('/') {
                zw.add_directory(*name, SimpleFileOptions::default()).unwrap();
                continue;
            }
            if *link {
                zw.add_symlink(*name, std::str::from_utf8(content).unwrap(), SimpleFileOptions::default()).unwrap();
                continue;
            }
            zw.start_file(*name, SimpleFileOptions::default()).unwrap();
            zw.write_all(content).unwrap();
        }
        zw.finish().unwrap();
        path.to_path_buf()
    }

    pub fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("kl-unpack-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::{make_zip, scratch};
    use super::*;
    use crate::catalog::Firmware;
    use std::collections::BTreeMap;

    fn manifest(files: &[&str]) -> Manifest {
        Manifest {
            schema: 1,
            id: "browser".into(),
            dir: "browser".into(),
            version: "1.0.0".into(),
            arch: "armhf".into(),
            firmware: Firmware { min: "5.16.3".into(), max: None },
            requires: BTreeMap::new(),
            files: files.iter().map(|s| s.to_string()).collect(),
            hooks: vec![],
        }
    }

    #[test]
    fn unpacks_a_good_package_and_skips_package_only_files() {
        let d = scratch("good");
        let z = make_zip(
            &d.join("p.zip"),
            &[
                ("browser-1.0.0/", b"", false),
                ("browser-1.0.0/browser/", b"", false),
                ("browser-1.0.0/manifest.json", b"{}", false),
                ("browser-1.0.0/LICENSE", b"license", false),
                ("browser-1.0.0/browser/kb-kindle", b"bin", false),
                ("browser-1.0.0/documents/Browser.sh", b"sh", false),
            ],
        );
        let out = d.join("out");
        fs::create_dir_all(&out).unwrap();
        let written = unpack(&z, &out, &manifest(&["browser/kb-kindle", "documents/Browser.sh"]), Limits::default()).unwrap();
        assert_eq!(written, vec!["browser/kb-kindle", "documents/Browser.sh"]);
        assert!(out.join("browser/kb-kindle").exists());
        assert!(!out.join("LICENSE").exists(), "package-only files are not installed");
    }

    #[test]
    fn refuses_names_that_escape_the_folder() {
        for bad in ["browser-1.0.0/../evil", "browser-1.0.0//abs", "/etc/passwd", "browser-1.0.0/a\\b"] {
            let d = scratch("escape");
            let z = make_zip(&d.join("p.zip"), &[(bad, b"x", false)]);
            let out = d.join("out");
            fs::create_dir_all(&out).unwrap();
            let r = unpack(&z, &out, &manifest(&[]), Limits::default());
            assert!(matches!(r, Err(UnpackError::UnsafeName(_)) | Err(UnpackError::TopFolder(_))), "{bad}: {r:?}");
        }
    }

    #[test]
    fn keeps_the_execute_bit_of_a_program() {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;
        use zip::write::SimpleFileOptions;
        let d = scratch("exec");
        let z = d.join("p.zip");
        {
            let mut zw = zip::ZipWriter::new(File::create(&z).unwrap());
            zw.start_file("browser-1.0.0/browser/kb-kindle", SimpleFileOptions::default().unix_permissions(0o755)).unwrap();
            zw.write_all(b"binary").unwrap();
            zw.start_file("browser-1.0.0/browser/icon.png", SimpleFileOptions::default().unix_permissions(0o644)).unwrap();
            zw.write_all(b"picture").unwrap();
            zw.finish().unwrap();
        }
        let out = d.join("out");
        fs::create_dir_all(&out).unwrap();
        unpack(&z, &out, &manifest(&["browser/kb-kindle", "browser/icon.png"]), Limits::default()).unwrap();
        assert_eq!(fs::metadata(out.join("browser/kb-kindle")).unwrap().permissions().mode() & 0o777, 0o755);
        assert_eq!(fs::metadata(out.join("browser/icon.png")).unwrap().permissions().mode() & 0o777, 0o644);
    }

    #[test]
    fn refuses_links() {
        let d = scratch("link");
        let z = make_zip(&d.join("p.zip"), &[("browser-1.0.0/browser/x", b"target", true)]);
        let out = d.join("out");
        fs::create_dir_all(&out).unwrap();
        assert!(matches!(unpack(&z, &out, &manifest(&["browser/x"]), Limits::default()), Err(UnpackError::Symlink(_))));
    }

    #[test]
    fn refuses_two_top_folders_and_oversize_packages() {
        let d = scratch("top");
        let z = make_zip(&d.join("p.zip"), &[("a-1/x", b"1", false), ("b-1/y", b"2", false)]);
        let out = d.join("out");
        fs::create_dir_all(&out).unwrap();
        assert!(matches!(unpack(&z, &out, &manifest(&[]), Limits::default()), Err(UnpackError::TopFolder(_))));

        let z2 = make_zip(&d.join("big.zip"), &[("a-1/x", &[0u8; 100], false)]);
        let tiny = Limits { max_total_bytes: 10, max_files: 10 };
        assert!(matches!(unpack(&z2, &out, &manifest(&[]), tiny), Err(UnpackError::TooLarge { .. })));
    }

    #[test]
    fn checks_the_file_list_against_the_manifest_both_ways() {
        let d = scratch("list");
        let z = make_zip(&d.join("p.zip"), &[("b-1/browser/a", b"a", false), ("b-1/browser/extra", b"e", false)]);
        let out = d.join("out");
        fs::create_dir_all(&out).unwrap();
        assert!(matches!(
            unpack(&z, &out, &manifest(&["browser/a"]), Limits::default()),
            Err(UnpackError::NotInManifest(n)) if n == "browser/extra"
        ));
        let z2 = make_zip(&d.join("p2.zip"), &[("b-1/browser/a", b"a", false)]);
        assert!(matches!(
            unpack(&z2, &out, &manifest(&["browser/a", "browser/b"]), Limits::default()),
            Err(UnpackError::MissingFromPackage(n)) if n == "browser/b"
        ));
        assert_eq!(fs::read_dir(&out).unwrap().count(), 0);
    }

    #[test]
    fn refuses_undeclared_hooks_without_unpacking_payload_files() {
        let d = scratch("undeclared-hook");
        let z = make_zip(&d.join("p.zip"), &[
            ("b-1/browser/a", b"a", false),
            ("b-1/hooks/post-install.sh", b"hook", false),
        ]);
        let out = d.join("out");
        fs::create_dir_all(&out).unwrap();
        assert!(matches!(unpack(&z, &out, &manifest(&["browser/a"]), Limits::default()), Err(UnpackError::UnsupportedHooks)));
        assert_eq!(fs::read_dir(out).unwrap().count(), 0);
    }

    #[test]
    fn allowed_places_are_the_module_folder_extensions_and_documents() {
        assert!(is_allowed_place("browser/kb-kindle", "browser"));
        assert!(is_allowed_place("documents/Browser.sh", "browser"));
        assert!(is_allowed_place("extensions/browser/menu.json", "browser"));
        assert!(!is_allowed_place("system/bin/x", "browser"));
        assert!(!is_allowed_place("fonts/x.ttf", "browser"));
        assert!(!is_allowed_place("terminal/kt", "browser"));
        assert!(!is_allowed_place("extensions/terminal/x", "browser"));
        assert!(!is_allowed_place("browser", "browser"));
        assert!(!is_allowed_place("launcher/STORE/installed.json", "launcher"));
        assert!(!is_allowed_place("documents/Protected.sh", "koreader"));
        assert!(is_allowed_place("launcher/kl-launcher", "launcher"));
    }
}

#[cfg(test)]
mod packager_compat {
    use super::*;

    /// Reads a package made by the build repo's packager, when `KL_TEST_PACKAGE` names one. Skipped otherwise, so the
    /// normal test run does not depend on it. The check: the manifest reads, and the files unpack exactly as listed.
    #[test]
    fn accepts_a_package_from_the_build_packager() {
        let Some(zip) = std::env::var_os("KL_TEST_PACKAGE") else {
            return;
        };
        let zip = PathBuf::from(zip);
        let manifest = read_manifest(&zip).expect("the manifest reads");
        let dest = std::env::temp_dir().join(format!("kl-pkg-compat-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dest);
        std::fs::create_dir_all(&dest).unwrap();
        let written = unpack(&zip, &dest, &manifest, Limits::default()).expect("the package unpacks");
        assert_eq!(written, {
            let mut f = manifest.files.clone();
            f.sort();
            f
        });
        let _ = std::fs::remove_dir_all(&dest);
    }
}
