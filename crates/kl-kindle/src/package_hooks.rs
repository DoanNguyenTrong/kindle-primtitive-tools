//! Kindle trust anchors and execution of explicitly approved package hooks.
//!
//! Reviewed hook scripts must complete their work in the foreground without background or detached processes.

use std::io;
use std::path::Path;

use kl_store::hooks::{HookPhase, HookRunner};
use kl_store::record::Installed;

pub const HOOK_POLICY_FILE: &str = "package-hooks.json";
pub const SIGNING_KEY_FILE: &str = "catalog-ed25519.pub";
const MAX_POLICY_BYTES: usize = 128 * 1024;

/// Read a fixed Store trust file under the boot mirror, with ownership and symlink checks.
/// An absent signing key leaves catalog signing opt-in even on firmware with group-writable
/// directories. A present trust file must have a secure path.
pub fn read_store_policy_file(file_name: &str, max_len: usize) -> io::Result<Option<Vec<u8>>> {
    if !matches!(file_name, HOOK_POLICY_FILE | SIGNING_KEY_FILE)
        || max_len == 0
        || max_len > MAX_POLICY_BYTES
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Invalid Store trust file request",
        ));
    }

    #[cfg(target_os = "linux")]
    {
        let directory = Path::new("/var/local/primitive-launcher");
        runner::read_policy(std::fs::File::open("/")?, directory, file_name, max_len, 0)
    }
    #[cfg(not(target_os = "linux"))]
    {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Kindle trust files require Linux",
        ))
    }
}

/// Runs only hooks named in the Kindle's root-owned approval policy.
pub struct KindleHookRunner;

impl HookRunner for KindleHookRunner {
    fn approve(&self, package: &Installed) -> io::Result<()> {
        #[cfg(target_os = "linux")]
        {
            runner::approve(package)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = package;
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Package hooks require Kindle Linux",
            ))
        }
    }

    fn run(&self, package: &Installed, phase: HookPhase, script: &Path) -> io::Result<()> {
        #[cfg(target_os = "linux")]
        {
            runner::run(package, phase, script)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (package, phase, script);
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Package hooks require Kindle Linux",
            ))
        }
    }
}

#[cfg(any(target_os = "linux", test))]
mod runner {
    use std::collections::BTreeSet;
    use std::ffi::{CString, OsStr};
    use std::fs::File;
    use std::io::{self, Read};
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::{ffi::OsStrExt, fs::MetadataExt};
    use std::path::{Component, Path};

    use kl_store::catalog::valid_app_id;
    use kl_store::hooks::{sha256, HookPhase, MAX_HOOK_SIZE};
    use kl_store::record::Installed;
    use serde::Deserialize;

    const TRUST_OWNER_REQUIRED: &str =
        "Store trust paths must be owned by root and writable only by the owner";

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Policy {
        schema: u32,
        packages: Vec<Approval>,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Approval {
        id: String,
        version: String,
        arch: String,
        sha256: String,
        post_install: String,
        pre_uninstall: String,
    }

    fn denied(message: &str) -> io::Error {
        io::Error::new(io::ErrorKind::PermissionDenied, message.to_owned())
    }

    fn valid_digest(digest: &str) -> bool {
        digest.len() == 64
            && digest
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    }

    fn match_policy(bytes: &[u8], package: &Installed) -> io::Result<()> {
        let policy: Policy = serde_json::from_slice(bytes).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "Malformed package hook approval policy",
            )
        })?;
        let mut identities = BTreeSet::new();
        if policy.schema != 1
            || policy.packages.iter().any(|row| {
                !valid_app_id(&row.id)
                    || kl_store::version::Version::parse(&row.version).is_none()
                    || !matches!(row.arch.as_str(), "armhf" | "armel")
                    || !valid_digest(&row.sha256)
                    || !valid_digest(&row.post_install)
                    || !valid_digest(&row.pre_uninstall)
                    || !identities.insert((&row.id, &row.version, &row.arch))
            })
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Invalid package hook approval policy",
            ));
        }

        let approved = policy.packages.iter().any(|row| {
            row.id == package.id
                && row.version == package.version
                && row.arch == package.arch
                && row.sha256 == package.sha256
                && Some(row.post_install.as_str()) == package.hooks.post_install.as_deref()
                && Some(row.pre_uninstall.as_str()) == package.hooks.pre_uninstall.as_deref()
        });
        if !approved {
            return Err(denied(
                "This exact package and its hooks require a device approval",
            ));
        }
        Ok(())
    }

    fn open_at(directory: &File, component: &OsStr, flags: libc::c_int) -> io::Result<File> {
        let name = CString::new(component.as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Invalid path component"))?;
        // SAFETY: name is NUL-terminated and directory remains open for this call.
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | flags,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the descriptor just opened belongs to this File.
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    fn owner_controlled(file: &File, expected_uid: libc::uid_t) -> io::Result<bool> {
        let meta = file.metadata()?;
        Ok(meta.uid() == expected_uid && meta.mode() & 0o022 == 0)
    }

    fn strict_components(path: &Path) -> io::Result<impl Iterator<Item = &OsStr>> {
        let raw = path.as_os_str().as_bytes();
        if !path.is_absolute()
            || (raw != b"/"
                && raw[1..]
                    .split(|byte| *byte == b'/')
                    .any(|part| part.is_empty() || part == b"." || part == b".."))
        {
            return Err(denied("Store paths must be absolute and free of aliases"));
        }
        Ok(path.components().filter_map(|part| match part {
            Component::Normal(value) => Some(value),
            _ => None,
        }))
    }

    fn open_directory(
        mut directory: File,
        path: &Path,
        mut check: impl FnMut(&File) -> io::Result<()>,
    ) -> io::Result<Option<File>> {
        let components = strict_components(path)?;
        check(&directory)?;
        for part in components {
            directory = match open_at(&directory, part, libc::O_DIRECTORY) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
                result => result?,
            };
            check(&directory)?;
        }
        Ok(Some(directory))
    }

    fn read_bounded(file: File, max_len: usize) -> io::Result<Vec<u8>> {
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.nlink() != 1 {
            return Err(denied(
                "Store policy and hook files must be regular files with a single link",
            ));
        }
        if metadata.len() > max_len as u64 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Store policy or hook file is too large",
            ));
        }
        let mut bytes = Vec::new();
        file.take(max_len as u64 + 1).read_to_end(&mut bytes)?;
        if bytes.len() > max_len {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Store policy or hook file is too large",
            ));
        }
        Ok(bytes)
    }

    pub(super) fn read_policy(
        root: File,
        path: &Path,
        file_name: &str,
        max_len: usize,
        owner: libc::uid_t,
    ) -> io::Result<Option<Vec<u8>>> {
        let mut trusted_path = true;
        let Some(directory) = open_directory(root, path, |directory| {
            if !owner_controlled(directory, owner)? {
                if file_name != super::SIGNING_KEY_FILE {
                    return Err(denied(TRUST_OWNER_REQUIRED));
                }
                trusted_path = false;
            }
            Ok(())
        })?
        else {
            return Ok(None);
        };
        let file = match open_at(
            &directory,
            OsStr::new(file_name),
            libc::O_NONBLOCK | libc::O_NOCTTY,
        ) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            result => result?,
        };
        if !trusted_path || !owner_controlled(&file, owner)? {
            return Err(denied(TRUST_OWNER_REQUIRED));
        }
        read_bounded(file, max_len).map(Some)
    }

    fn verified_script(package: &Installed, phase: HookPhase, path: &Path) -> io::Result<Vec<u8>> {
        // Validate the complete path before Path::parent can normalize away trailing components.
        let file_name = phase.file_name();
        if strict_components(path)?.last() != Some(OsStr::new(file_name)) {
            return Err(denied("The hook did not use the declared script name"));
        }
        let expected = package
            .hooks
            .digest(phase)
            .ok_or_else(|| denied("The package does not declare this hook"))?;
        let parent = path
            .parent()
            .ok_or_else(|| denied("The hook has no containing directory"))?;
        let directory = open_directory(File::open("/")?, parent, |_| Ok(()))?
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "Hook directory is missing"))?;
        let file = open_at(&directory, OsStr::new(file_name), libc::O_NONBLOCK)?;
        let bytes = read_bounded(file, MAX_HOOK_SIZE as usize)?;
        if bytes.is_empty() || bytes.contains(&0) || sha256(&bytes) != expected {
            return Err(denied("Hook bytes do not match the reviewed script"));
        }
        Ok(bytes)
    }

    #[cfg(target_os = "linux")]
    pub(super) fn approve(package: &Installed) -> io::Result<()> {
        // SAFETY: reading the UID does not access any caller memory.
        if unsafe { libc::getuid() != 0 || libc::geteuid() != 0 } {
            return Err(denied(
                "Only the Kindle's root Store service may run package hooks",
            ));
        }
        let policy =
            super::read_store_policy_file(super::HOOK_POLICY_FILE, super::MAX_POLICY_BYTES)?
                .ok_or_else(|| {
                    denied("This Kindle does not have a package hook approval policy")
                })?;
        match_policy(&policy, package)
    }

    #[cfg(target_os = "linux")]
    pub(super) fn run(package: &Installed, phase: HookPhase, script: &Path) -> io::Result<()> {
        approve(package)?;
        let bytes = verified_script(package, phase, script)?;
        namespace::run(&bytes, std::time::Duration::from_secs(60))
    }

    #[cfg(target_os = "linux")]
    mod namespace {
        use std::ffi::CString;
        use std::fs::OpenOptions;
        use std::io::{self, Read, Write};
        use std::os::fd::{AsRawFd, RawFd};
        use std::os::unix::net::UnixStream;
        use std::os::unix::process::ExitStatusExt;
        use std::process::ExitStatus;
        use std::ptr;
        use std::thread;
        use std::time::{Duration, Instant};

        const READY: u8 = b'R';
        const START: u8 = b'S';
        const FAILED_SETUP: libc::c_int = 126;

        // The libc clone wrapper takes the high end of each stack. The allocation
        // grows down and is aligned to at least the ARM EABI stack requirement.
        #[derive(Clone, Copy)]
        #[repr(C, align(16))]
        struct StackBlock([u8; 16]);

        fn stack() -> Vec<StackBlock> {
            vec![StackBlock([0; 16]); 4096]
        }

        fn stack_top(stack: &mut [StackBlock]) -> *mut libc::c_void {
            stack.as_mut_ptr().wrapping_add(stack.len()).cast()
        }

        struct HookInit {
            parent_socket: RawFd,
            child_socket: RawFd,
            null: RawFd,
            shell_stack: *mut libc::c_void,
            argv: [*const libc::c_char; 5],
            envp: [*const libc::c_char; 4],
            inherited_mask: libc::sigset_t,
            last_signal: libc::c_int,
        }

        extern "C" fn init(context: *mut libc::c_void) -> libc::c_int {
            // SAFETY: clone gave this process a copy of the prepared context and
            // both stacks. The callbacks use only reentrant libc calls and no
            // Rust allocation, locks, destructors or registered fork handlers.
            unsafe {
                let args = &*context.cast::<HookInit>();
                libc::close(args.parent_socket);
                let zero: libc::c_ulong = 0;
                if libc::prctl(
                    libc::PR_SET_PDEATHSIG,
                    libc::SIGKILL as libc::c_ulong,
                    zero,
                    zero,
                    zero,
                ) != 0
                    || libc::signal(libc::SIGCHLD, libc::SIG_DFL) == libc::SIG_ERR
                    || libc::chdir(c"/".as_ptr()) != 0
                    || libc::dup2(args.null, libc::STDIN_FILENO) < 0
                    || libc::dup2(args.null, libc::STDOUT_FILENO) < 0
                    || libc::dup2(args.null, libc::STDERR_FILENO) < 0
                {
                    return FAILED_SETUP;
                }
                libc::close(args.null);

                // getppid is zero across the namespace boundary. Only the live
                // spawning thread can acknowledge readiness after prctl succeeds.
                let mut start = 0u8;
                if libc::send(
                    args.child_socket,
                    (&READY as *const u8).cast(),
                    1,
                    libc::MSG_NOSIGNAL,
                ) != 1
                    || libc::recv(args.child_socket, (&mut start as *mut u8).cast(), 1, 0) != 1
                    || start != START
                {
                    return FAILED_SETUP;
                }
                libc::close(args.child_socket);

                // PID 1 keeps its death signal while the shell execs and runs
                // helpers. When PID 1 exits, the kernel kills and reaps the rest
                // of the namespace before Store's wait completes.
                let child = libc::clone(exec_shell, args.shell_stack, libc::SIGCHLD, context);
                if child < 0 {
                    return FAILED_SETUP;
                }
                let mut status = 0;
                loop {
                    let exited = libc::waitpid(-1, &mut status, 0);
                    if exited == child {
                        if libc::WIFEXITED(status) {
                            return libc::WEXITSTATUS(status);
                        }
                        return 128 + libc::WTERMSIG(status);
                    }
                    if exited < 0 && *libc::__errno_location() != libc::EINTR {
                        return FAILED_SETUP;
                    }
                }
            }
        }

        extern "C" fn exec_shell(context: *mut libc::c_void) -> libc::c_int {
            // SAFETY: the argv and envp strings and terminators were constructed
            // in Store before cloning and still exist in this address space.
            unsafe {
                let args = &*context.cast::<HookInit>();
                // Signals were blocked before clone. Remove inherited handlers
                // before unblocking the shell so none can run before exec.
                for signal in 1..=args.last_signal {
                    libc::signal(signal, libc::SIG_DFL);
                }
                if libc::sigprocmask(libc::SIG_SETMASK, &args.inherited_mask, ptr::null_mut()) == 0
                {
                    libc::execve(c"/bin/sh".as_ptr(), args.argv.as_ptr(), args.envp.as_ptr());
                }
            }
            FAILED_SETUP
        }

        fn spawn_init(args: &mut HookInit, stack: &mut [StackBlock]) -> io::Result<libc::pid_t> {
            // SAFETY: sigfillset initializes all, pthread_sigmask writes to the
            // inherited mask and clone only shares resources explicitly flagged.
            unsafe {
                let mut all = std::mem::zeroed();
                libc::sigfillset(&mut all);
                let result =
                    libc::pthread_sigmask(libc::SIG_SETMASK, &all, &mut args.inherited_mask);
                if result != 0 {
                    return Err(io::Error::from_raw_os_error(result));
                }
                let pid = libc::clone(
                    init,
                    stack_top(stack),
                    libc::CLONE_NEWPID | libc::CLONE_NEWNET | libc::SIGCHLD,
                    (args as *mut HookInit).cast(),
                );
                let error = (pid < 0).then(io::Error::last_os_error);
                // This call uses the valid mask returned by the same API above.
                libc::pthread_sigmask(libc::SIG_SETMASK, &args.inherited_mask, ptr::null_mut());
                match error {
                    Some(error) => Err(io::Error::new(
                        error.kind(),
                        format!("Could not create isolated package hook namespaces: {error}"),
                    )),
                    None => Ok(pid),
                }
            }
        }

        fn wait(pid: libc::pid_t, options: libc::c_int) -> io::Result<Option<ExitStatus>> {
            loop {
                let mut status = 0;
                // SAFETY: status is writable and pid identifies this direct child.
                match unsafe { libc::waitpid(pid, &mut status, options) } {
                    0 => return Ok(None),
                    result if result > 0 => return Ok(Some(ExitStatus::from_raw(status))),
                    _ => {
                        let error = io::Error::last_os_error();
                        if error.kind() != io::ErrorKind::Interrupted {
                            return Err(error);
                        }
                    }
                }
            }
        }

        fn stop(pid: libc::pid_t) -> io::Result<()> {
            // SAFETY: the direct child hasn't been reaped, so its PID cannot
            // have been reused. SIGKILL reaches PID 1 from the parent namespace.
            if unsafe { libc::kill(pid, libc::SIGKILL) } < 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::ESRCH) {
                    return Err(error);
                }
            }
            wait(pid, 0)?;
            Ok(())
        }

        fn remaining(deadline: Instant) -> io::Result<Duration> {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "The package hook exceeded its time limit",
                ));
            }
            Ok(left)
        }

        fn start(mut channel: UnixStream, deadline: Instant) -> io::Result<()> {
            channel.set_read_timeout(Some(remaining(deadline)?))?;
            let mut ready = [0];
            channel.read_exact(&mut ready).map_err(|error| {
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) {
                    io::Error::new(
                        io::ErrorKind::TimedOut,
                        "The package hook timed out during setup",
                    )
                } else {
                    io::Error::new(
                        error.kind(),
                        format!("Could not initialize the package hook: {error}"),
                    )
                }
            })?;
            if ready != [READY] {
                return Err(io::Error::other("Invalid package hook startup response"));
            }
            channel.set_write_timeout(Some(remaining(deadline)?))?;
            channel.write_all(&[START])
        }

        fn supervise(pid: libc::pid_t, deadline: Instant) -> io::Result<()> {
            loop {
                match wait(pid, libc::WNOHANG) {
                    Ok(Some(status)) if status.success() => return Ok(()),
                    Ok(Some(status)) => {
                        return Err(io::Error::other(format!(
                            "The package hook failed ({status})"
                        )));
                    }
                    Err(error) => {
                        if error.raw_os_error() != Some(libc::ECHILD) {
                            stop(pid)?;
                        }
                        return Err(error);
                    }
                    Ok(None) => {}
                }
                match remaining(deadline) {
                    Ok(left) => thread::sleep(left.min(Duration::from_millis(25))),
                    Err(error) => {
                        stop(pid)?;
                        return Err(error);
                    }
                }
            }
        }

        pub(super) fn run(script: &[u8], limit: Duration) -> io::Result<()> {
            let deadline = Instant::now() + limit;
            let script = CString::new(script)?;
            let (channel, child_channel) = UnixStream::pair()?;
            child_channel.set_read_timeout(Some(remaining(deadline)?))?;
            let null = OpenOptions::new()
                .read(true)
                .write(true)
                .open("/dev/null")?;
            let mut init_stack = stack();
            let mut shell_stack = stack();
            let mut args = HookInit {
                parent_socket: channel.as_raw_fd(),
                child_socket: child_channel.as_raw_fd(),
                null: null.as_raw_fd(),
                shell_stack: stack_top(&mut shell_stack),
                argv: [
                    c"/bin/sh".as_ptr(),
                    c"-c".as_ptr(),
                    script.as_ptr(),
                    c"primitive-package-hook".as_ptr(),
                    ptr::null(),
                ],
                envp: [
                    c"PATH=/usr/sbin:/usr/bin:/sbin:/bin".as_ptr(),
                    c"HOME=/".as_ptr(),
                    c"LC_ALL=C".as_ptr(),
                    ptr::null(),
                ],
                // SAFETY: spawn_init fills this sigset before cloning.
                inherited_mask: unsafe { std::mem::zeroed() },
                last_signal: libc::SIGRTMAX(),
            };
            if [args.parent_socket, args.child_socket, args.null]
                .iter()
                .any(|fd| *fd <= libc::STDERR_FILENO)
            {
                return Err(io::Error::other(
                    "The Store service closed its standard descriptors",
                ));
            }

            let pid = spawn_init(&mut args, &mut init_stack)?;
            drop(child_channel);
            if let Err(error) = start(channel, deadline) {
                stop(pid)?;
                return Err(error);
            }
            supervise(pid, deadline)
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::fs::{self, Permissions};
        use std::os::unix::fs::{symlink, PermissionsExt};
        use std::path::PathBuf;

        struct Fixture(PathBuf);

        impl Fixture {
            fn new(name: &str) -> Self {
                let path =
                    std::env::temp_dir().join(format!("kl-hooks-{}-{name}", std::process::id()));
                fs::create_dir(&path).unwrap();
                Self(path.canonicalize().unwrap())
            }
        }

        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }

        fn package() -> Installed {
            let mut package = Installed {
                id: "approved-app".into(),
                version: "1.2.3".into(),
                arch: "armhf".into(),
                sha256: "a".repeat(64),
                ..Installed::default()
            };
            package.hooks.post_install = Some(sha256(b"exit 0\n"));
            package.hooks.pre_uninstall = Some("b".repeat(64));
            package
        }

        fn approval_row(package: &Installed) -> serde_json::Value {
            serde_json::json!({
                "id": package.id, "version": package.version, "arch": package.arch,
                "sha256": package.sha256, "post_install": package.hooks.post_install,
                "pre_uninstall": package.hooks.pre_uninstall,
            })
        }

        #[test]
        fn approval_binds_identity_archive_and_both_scripts() {
            let approved = package();
            let row = approval_row(&approved);
            let policy = serde_json::json!({"schema": 1, "packages": [&row]}).to_string();
            match_policy(policy.as_bytes(), &approved).unwrap();

            for mut other in [
                Installed {
                    id: "other-app".into(),
                    ..approved.clone()
                },
                Installed {
                    version: "1.2.3.0".into(),
                    ..approved.clone()
                },
                Installed {
                    arch: "armel".into(),
                    ..approved.clone()
                },
                Installed {
                    sha256: "c".repeat(64),
                    ..approved.clone()
                },
                {
                    let mut value = approved.clone();
                    value.hooks.post_install = Some("c".repeat(64));
                    value
                },
                {
                    let mut value = approved.clone();
                    value.hooks.pre_uninstall = None;
                    value
                },
            ] {
                assert_eq!(
                    match_policy(policy.as_bytes(), &other).unwrap_err().kind(),
                    io::ErrorKind::PermissionDenied
                );
                other.hooks.post_install = None;
                assert!(match_policy(policy.as_bytes(), &other).is_err());
            }
            let duplicate = serde_json::json!({"schema": 1, "packages": [&row, &row]}).to_string();
            assert_eq!(
                match_policy(duplicate.as_bytes(), &approved)
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::InvalidData
            );
            let typo =
                serde_json::json!({"schema": 1, "packages": [&row], "approval": true}).to_string();
            assert_eq!(
                match_policy(typo.as_bytes(), &approved).unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
        }

        #[test]
        fn policy_reads_only_single_link_owner_controlled_files() {
            let fixture = Fixture::new("approval");
            let file_name = crate::package_hooks::SIGNING_KEY_FILE;
            let path = fixture.0.join(file_name);
            let directory = File::open(&fixture.0).unwrap();
            let owner = directory.metadata().unwrap().uid();
            let read = |max_len, owner| {
                read_policy(
                    directory.try_clone().unwrap(),
                    Path::new("/"),
                    file_name,
                    max_len,
                    owner,
                )
            };
            assert!(read(32, owner).unwrap().is_none());
            fs::write(&path, b"approved").unwrap();
            fs::set_permissions(&path, Permissions::from_mode(0o600)).unwrap();
            assert_eq!(read(32, owner).unwrap(), Some(b"approved".to_vec()));
            assert!(read(2, owner).is_err());
            assert!(read(32, owner.wrapping_add(1)).is_err());

            fs::set_permissions(&path, Permissions::from_mode(0o620)).unwrap();
            assert!(read(32, owner).is_err());
            fs::set_permissions(&path, Permissions::from_mode(0o600)).unwrap();
            fs::hard_link(&path, fixture.0.join("hard-link")).unwrap();
            assert!(read(32, owner).is_err());
            fs::remove_file(&path).unwrap();
            symlink("hard-link", &path).unwrap();
            assert!(read(32, owner).is_err());
            fs::remove_file(&path).unwrap();
            fs::create_dir(&path).unwrap();
            assert!(read(32, owner).is_err());
        }

        #[test]
        fn absent_signing_key_allows_unsigned_catalog_on_stock_directory_modes() {
            use crate::package_hooks::{HOOK_POLICY_FILE, SIGNING_KEY_FILE};

            let fixture = Fixture::new("stock-trust-path");
            let local = fixture.0.join("var/local");
            let mirror = local.join("primitive-launcher");
            fs::create_dir_all(&mirror).unwrap();
            for directory in [&local, &mirror] {
                fs::set_permissions(directory, Permissions::from_mode(0o2775)).unwrap();
                assert_eq!(fs::metadata(directory).unwrap().mode() & 0o777, 0o775);
            }
            let root = File::open(&fixture.0).unwrap();
            let owner = root.metadata().unwrap().uid();
            let path = "/var/local/primitive-launcher";
            let read = |path, file_name, owner| {
                read_policy(
                    root.try_clone().unwrap(),
                    Path::new(path),
                    file_name,
                    128,
                    owner,
                )
            };
            assert_eq!(read(path, SIGNING_KEY_FILE, owner).unwrap(), None);
            assert_eq!(
                read(path, SIGNING_KEY_FILE, owner.wrapping_add(1)).unwrap(),
                None
            );
            assert_eq!(
                read(path, HOOK_POLICY_FILE, owner).unwrap_err().kind(),
                io::ErrorKind::PermissionDenied
            );
            assert!(read("/var/local/./primitive-launcher", SIGNING_KEY_FILE, owner).is_err());
            symlink("missing", local.join("alias")).unwrap();
            assert!(read("/var/local/alias", SIGNING_KEY_FILE, owner).is_err());
            fs::write(local.join("regular-file"), b"").unwrap();
            assert!(read("/var/local/regular-file", SIGNING_KEY_FILE, owner).is_err());

            let key = mirror.join(SIGNING_KEY_FILE);
            symlink("missing", &key).unwrap();
            assert!(read(path, SIGNING_KEY_FILE, owner).is_err());
            fs::remove_file(&key).unwrap();
            fs::write(&key, b"configured pin").unwrap();
            fs::set_permissions(&key, Permissions::from_mode(0o600)).unwrap();
            assert_eq!(
                read(path, SIGNING_KEY_FILE, owner).unwrap_err().kind(),
                io::ErrorKind::PermissionDenied
            );
            fs::set_permissions(&mirror, Permissions::from_mode(0o755)).unwrap();
            assert!(read(path, SIGNING_KEY_FILE, owner).is_err());
            fs::set_permissions(&local, Permissions::from_mode(0o755)).unwrap();
            assert_eq!(
                read(path, SIGNING_KEY_FILE, owner).unwrap(),
                Some(b"configured pin".to_vec())
            );
        }

        #[test]
        fn script_uses_reviewed_bytes_and_refuses_path_aliases() {
            let fixture = Fixture::new("script");
            let directory = fixture.0.join("hooks");
            fs::create_dir(&directory).unwrap();
            let path = directory.join("post-install.sh");
            let installed = package();
            fs::write(&path, b"exit 0\n").unwrap();
            let verified = verified_script(&installed, HookPhase::PostInstall, &path).unwrap();
            assert_eq!(verified, b"exit 0\n");
            fs::write(&path, b"exit 1\n").unwrap();
            assert!(verified_script(&installed, HookPhase::PostInstall, &path).is_err());
            assert_eq!(verified, b"exit 0\n");

            fs::write(&path, b"exit 0\n").unwrap();
            assert!(verified_script(&installed, HookPhase::PreUninstall, &path).is_err());
            fs::hard_link(&path, directory.join("alias.sh")).unwrap();
            assert!(verified_script(&installed, HookPhase::PostInstall, &path).is_err());
            fs::remove_file(directory.join("alias.sh")).unwrap();
            symlink(&directory, fixture.0.join("alias")).unwrap();
            let through_alias = fixture.0.join("alias").join("post-install.sh");
            assert!(verified_script(&installed, HookPhase::PostInstall, &through_alias).is_err());
            assert!(verified_script(
                &installed,
                HookPhase::PostInstall,
                &directory.join("./post-install.sh")
            )
            .is_err());
        }

        #[cfg(target_os = "linux")]
        fn quoted(path: &Path) -> String {
            format!("'{}'", path.to_str().unwrap().replace('\'', "'\\''"))
        }

        #[cfg(target_os = "linux")]
        #[test]
        #[ignore = "requires CAP_SYS_ADMIN and Linux PID and network namespaces"]
        fn namespace_stops_helpers_when_the_hook_exits_or_times_out() {
            use std::time::Duration;

            namespace::run(
                b"test \"$PWD\" = / && test \"$HOME\" = / && test \"$LC_ALL\" = C",
                Duration::from_secs(3),
            )
            .unwrap();
            let failure = namespace::run(b"exit 7", Duration::from_secs(3)).unwrap_err();
            assert!(failure.to_string().contains('7'));

            let fixture = Fixture::new("namespace-exit");
            let after_exit = fixture.0.join("after-exit");
            let script = format!(
                "(sleep 1; printf finished > {}) & exit 0",
                quoted(&after_exit)
            );
            namespace::run(script.as_bytes(), Duration::from_secs(3)).unwrap();

            let after_timeout = fixture.0.join("after-timeout");
            let script = format!(
                "(sleep 1; printf finished > {}) & wait",
                quoted(&after_timeout)
            );
            let error = namespace::run(script.as_bytes(), Duration::from_millis(150)).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::TimedOut);
            std::thread::sleep(Duration::from_millis(1100));
            assert!(!after_exit.exists());
            assert!(!after_timeout.exists());
        }

        #[cfg(target_os = "linux")]
        #[test]
        #[ignore = "requires CAP_SYS_ADMIN and Linux PID and network namespaces"]
        fn namespace_stops_foreground_helpers_when_store_is_killed() {
            use std::process::Command;
            use std::thread;
            use std::time::{Duration, Instant};

            const FIXTURE: &str = "PRIMITIVE_NAMESPACE_DEATH_TEST_DIRECTORY";
            if let Some(directory) = std::env::var_os(FIXTURE).map(PathBuf::from) {
                let helper = format!(
                    "printf ready > {}; sleep 2; printf finished > {}",
                    quoted(&directory.join("ready")),
                    quoted(&directory.join("finished")),
                );
                // Keep the outer shell waiting for a foreground child shell.
                let script = format!("/bin/sh -c '{}'; exit $?", helper.replace('\'', "'\\''"));
                namespace::run(script.as_bytes(), Duration::from_secs(6)).unwrap();
                return;
            }

            let fixture = Fixture::new("namespace-parent-death");
            let mut worker = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--ignored",
                    "--exact",
                    "package_hooks::runner::tests::namespace_stops_foreground_helpers_when_store_is_killed",
                ])
                .env(FIXTURE, &fixture.0)
                .spawn()
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(3);
            while !fixture.0.join("ready").exists() {
                if let Some(status) = worker.try_wait().unwrap() {
                    panic!("Namespace fixture exited before its foreground helper ran ({status})");
                }
                if Instant::now() >= deadline {
                    worker.kill().unwrap();
                    worker.wait().unwrap();
                    panic!("Namespace fixture never started its foreground helper");
                }
                thread::sleep(Duration::from_millis(10));
            }
            worker.kill().unwrap();
            worker.wait().unwrap();
            thread::sleep(Duration::from_millis(2200));
            assert!(!fixture.0.join("finished").exists());
        }
    }
}
