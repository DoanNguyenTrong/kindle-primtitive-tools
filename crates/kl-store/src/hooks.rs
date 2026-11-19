//! Identities and verified storage for reviewed Store hooks. The engine never executes a script itself.

use crate::catalog::valid_app_id;
use crate::install::LAUNCHER_ID;
use crate::record::Installed;
use crate::unpack::valid_module_dir;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

pub const MAX_HOOK_SIZE: u64 = 64 * 1024;
pub const POST_INSTALL_PATH: &str = "hooks/post-install.sh";
pub const PRE_UNINSTALL_PATH: &str = "hooks/pre-uninstall.sh";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookPhase {
    PostInstall,
    PreUninstall,
}

impl HookPhase {
    pub const fn file_name(self) -> &'static str {
        match self {
            Self::PostInstall => "post-install.sh",
            Self::PreUninstall => "pre-uninstall.sh",
        }
    }

    pub const fn manifest_path(self) -> &'static str {
        match self {
            Self::PostInstall => POST_INSTALL_PATH,
            Self::PreUninstall => PRE_UNINSTALL_PATH,
        }
    }
}

const PHASES: [HookPhase; 2] = [HookPhase::PostInstall, HookPhase::PreUninstall];

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct HookScripts {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post_install: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pre_uninstall: Option<String>,
}

impl HookScripts {
    pub fn is_empty(&self) -> bool {
        self.post_install.is_none() && self.pre_uninstall.is_none()
    }

    pub fn digest(&self, phase: HookPhase) -> Option<&str> {
        match phase {
            HookPhase::PostInstall => self.post_install.as_deref(),
            HookPhase::PreUninstall => self.pre_uninstall.as_deref(),
        }
    }

    pub(crate) fn check_manifest(&self, names: &[String]) -> io::Result<()> {
        if self.is_empty() && names.is_empty()
            || names.len() == PHASES.len()
                && PHASES.iter().all(|&phase| {
                    self.digest(phase).is_some_and(valid_digest)
                        && names.iter().any(|name| name == phase.manifest_path())
                })
        {
            Ok(())
        } else {
            Err(invalid(
                "a reviewed package must declare both install and rollback hooks",
            ))
        }
    }
}

/// The caller supplies a trust decision separate from the package, its disk record, and its journal.
pub trait HookRunner {
    /// Preflight the identity, archive digest, and both script digests against trusted review data.
    fn approve(&self, package: &Installed) -> io::Result<()>;

    /// Run the exact bytes approved for this phase. Use `verified_bytes` when copying them into a private runtime
    /// directory to prevent a change between verification and execution. Enforce the runtime's execution policy.
    fn run(&self, package: &Installed, phase: HookPhase, script: &Path) -> io::Result<()>;
}

pub fn verified_bytes(path: &Path, expected: &str) -> io::Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file()
        || !(1..=MAX_HOOK_SIZE).contains(&metadata.len())
        || !valid_digest(expected)
    {
        return Err(invalid(
            "a reviewed hook is missing or has an invalid size or digest",
        ));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    fs::File::open(path)?
        .take(MAX_HOOK_SIZE + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 != metadata.len() || sha256(&bytes) != expected {
        return Err(invalid("a reviewed hook does not match its SHA-256"));
    }
    Ok(bytes)
}

pub fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub(crate) fn validate(package: &Installed) -> io::Result<()> {
    if package.hooks.is_empty() {
        return Ok(());
    }
    if !valid_app_id(&package.id)
        || matches!(package.id.as_str(), LAUNCHER_ID | "primitive-store")
        || !valid_module_dir(&package.dir)
        || matches!(package.dir.as_str(), "launcher" | "store")
        || !valid_digest(&package.sha256)
        || !PHASES
            .iter()
            .all(|&phase| package.hooks.digest(phase).is_some_and(valid_digest))
    {
        return Err(invalid(
            "the Store cannot use hooks for this app or package identity",
        ));
    }
    Ok(())
}

fn valid_digest(digest: &str) -> bool {
    digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn require_directory(path: &Path, create: bool) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Ok(_) => Err(invalid("a reviewed hook directory is not a real directory")),
        Err(error) if create && error.kind() == io::ErrorKind::NotFound => fs::create_dir(path),
        Err(error) => Err(error),
    }
}

fn hook_dir(root: &Path, package: &Installed, create_parents: bool) -> io::Result<PathBuf> {
    validate(package)?;
    if package.hooks.is_empty() {
        return Err(invalid("no reviewed hooks for this package"));
    }
    let mut path = root.to_path_buf();
    for component in ["launcher", "store", "hooks", package.id.as_str()] {
        path.push(component);
        require_directory(&path, create_parents)?;
    }
    Ok(path.join(&package.sha256))
}

fn verify_directory(directory: &Path, package: &Installed) -> io::Result<()> {
    require_directory(directory, false)?;
    for phase in PHASES {
        let digest = package
            .hooks
            .digest(phase)
            .ok_or_else(|| invalid("missing reviewed hook digest"))?;
        verified_bytes(&directory.join(phase.file_name()), digest)?;
    }
    Ok(())
}

pub(crate) fn retain(
    root: &Path,
    staging: &Path,
    package: &Installed,
    same_package: bool,
) -> io::Result<()> {
    check_staged(root, staging, package, same_package)?;
    if same_package {
        return Ok(());
    }
    let directory = hook_dir(root, package, true)?;
    if fs::symlink_metadata(&directory).is_ok() {
        return Err(invalid(
            "an unmanaged retained hook directory already exists",
        ));
    }
    // The unpacker and the retained directory use the same file system. Both scripts become visible together.
    fs::rename(staging.join(&package.id).join("hooks"), &directory)?;
    for phase in PHASES {
        fs::File::open(directory.join(phase.file_name()))?.sync_all()?;
    }
    for parent in directory.ancestors() {
        fs::File::open(parent)?.sync_all()?;
        if parent == root {
            break;
        }
    }
    Ok(())
}

pub(crate) fn check_staged(
    root: &Path,
    staging: &Path,
    package: &Installed,
    same_package: bool,
) -> io::Result<()> {
    let available = hook_dir(root, package, false);
    if same_package {
        return verify_directory(&available?, package);
    }
    match available.and_then(fs::symlink_metadata) {
        Ok(_) => {
            return Err(invalid(
                "an unmanaged retained hook directory already exists",
            ))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    require_directory(staging, false)?;
    let app_stage = staging.join(&package.id);
    require_directory(&app_stage, false)?;
    verify_directory(&app_stage.join("hooks"), package)
}

pub(crate) fn preflight(
    root: &Path,
    package: &Installed,
    runner: &dyn HookRunner,
) -> io::Result<()> {
    runner.approve(package)?;
    verify_directory(&hook_dir(root, package, false)?, package)
}

pub(crate) fn invoke(
    root: &Path,
    package: &Installed,
    phase: HookPhase,
    runner: &dyn HookRunner,
) -> io::Result<()> {
    runner.approve(package)?;
    let directory = hook_dir(root, package, false)?;
    require_directory(&directory, false)?;
    let script = directory.join(phase.file_name());
    let digest = package
        .hooks
        .digest(phase)
        .ok_or_else(|| invalid("missing reviewed hook digest"))?;
    verified_bytes(&script, digest)?;
    runner.run(package, phase, &script)
}

pub(crate) fn purge(root: &Path, package: &Installed) -> io::Result<()> {
    let result = (|| {
        let directory = hook_dir(root, package, false)?;
        require_directory(&directory, false)?;
        for phase in PHASES {
            let path = directory.join(phase.file_name());
            let digest = package
                .hooks
                .digest(phase)
                .ok_or_else(|| invalid("missing reviewed hook digest"))?;
            match verified_bytes(&path, digest) {
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        for phase in PHASES {
            let path = directory.join(phase.file_name());
            match fs::remove_file(path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        fs::remove_dir(&directory)?;
        if let Some(parent) = directory.parent() {
            fs::File::open(parent)?.sync_all()?;
        }
        Ok(())
    })();
    match result {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}
