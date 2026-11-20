//! The foreground app's local USB storage handshake with the launcher supervisor.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

// Older Store builds use this same mailbox.
const REQUEST: &str = "store-storage-request";
const REQUEST_TMP: &str = "store-storage-request.tmp";
const REPLY: &str = "store-storage-reply";
const TIMEOUT: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_millis(25);

pub struct StorageLease {
    run: PathBuf,
    pid: u32,
    sequence: u32,
    timeout: Duration,
}

impl StorageLease {
    /// Use only when the supervisor advertises the storage lease capability to this process.
    pub fn new(run: &Path) -> Self {
        let pid = std::process::id();
        // Exec keeps the supervisor's foreground PID and its sequence. Continue from the RAM
        // mailbox after one native app hands over to another under an active lease.
        let sequence = [REQUEST, REPLY]
            .into_iter()
            .filter_map(|name| fs::read_to_string(run.join(name)).ok())
            .filter_map(|line| mailbox_sequence(&line, pid))
            .max()
            .unwrap_or_default();
        Self {
            run: run.to_path_buf(),
            pid,
            sequence,
            timeout: TIMEOUT,
        }
    }

    /// Keep the storage mounted locally until `release` or process exit.
    pub fn begin_write(&mut self) -> io::Result<()> {
        self.begin_if(user_storage_is_mounted)
    }

    fn begin_if(&mut self, mounted: impl Fn() -> bool) -> io::Result<()> {
        if !mounted() {
            return Err(unavailable());
        }
        let result = self.wait_for_pause().and_then(|()| {
            if mounted() {
                Ok(())
            } else {
                Err(unavailable())
            }
        });
        if result.is_err() {
            // Supersede a request that may have timed out before a late acknowledgment.
            let _ = self.release();
        }
        result
    }

    /// Give the USB daemon control again. Close any user-storage handles before calling this.
    pub fn release(&mut self) -> io::Result<()> {
        self.send("idle").map(|_| ())
    }

    fn send(&mut self, state: &str) -> io::Result<String> {
        // The shell accepts positive decimal sequences with up to nine digits.
        if self.sequence == 999_999_999 {
            return Err(io::Error::other("USB request sequence exhausted"));
        }
        self.sequence += 1;
        let token = format!("{} {}", self.pid, self.sequence);
        let temporary = self.run.join(REQUEST_TMP);
        fs::write(&temporary, format!("{token} {state}\n"))?;
        fs::rename(temporary, self.run.join(REQUEST))?;
        Ok(token)
    }

    fn wait_for_pause(&mut self) -> io::Result<()> {
        let token = self.send("write")?;
        let paused = format!("{token} paused\n");
        let unavailable_reply = format!("{token} unavailable\n");
        let deadline = Instant::now() + self.timeout;
        let reply = self.run.join(REPLY);
        loop {
            match fs::read_to_string(&reply) {
                Ok(line) if line == paused => return Ok(()),
                Ok(line) if line == unavailable_reply => return Err(unavailable()),
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "The launcher did not confirm access to Kindle storage.",
                ));
            }
            thread::sleep(POLL);
        }
    }
}

fn mailbox_sequence(line: &str, pid: u32) -> Option<u32> {
    let mut fields = line.split_whitespace();
    let owner = fields.next()?.parse::<u32>().ok()?;
    let sequence = fields.next()?;
    let state = fields.next()?;
    if owner != pid
        || sequence.len() > 9
        || !sequence.bytes().all(|byte| byte.is_ascii_digit())
        || !matches!(state, "idle" | "write" | "paused" | "unavailable")
        || fields.next().is_some()
    {
        return None;
    }
    sequence.parse().ok()
}

fn unavailable() -> io::Error {
    io::Error::new(
        io::ErrorKind::WouldBlock,
        "Kindle storage is unavailable. Eject the drive and try again.",
    )
}

/// `/mnt/us` stays mounted as FUSE even while its backing partition is exported to a computer.
pub fn user_storage_is_mounted() -> bool {
    fs::read_to_string("/proc/mounts").is_ok_and(|mounts| {
        mounts
            .lines()
            .any(|line| line.split_whitespace().nth(1) == Some("/mnt/base-us"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let number = NEXT.fetch_add(1, Ordering::Relaxed);
            let run =
                std::env::temp_dir().join(format!("storage-lease-{}-{number}", std::process::id()));
            fs::create_dir(&run).unwrap();
            Self(run)
        }

        fn controller(&self) -> StorageLease {
            StorageLease {
                pid: 42,
                timeout: Duration::from_millis(100),
                ..StorageLease::new(&self.0)
            }
        }

        fn request(&self) -> String {
            fs::read_to_string(self.0.join(REQUEST)).unwrap()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn checks_the_mount_at_both_ends_of_the_handshake() {
        let fixture = Fixture::new();
        let mut controller = fixture.controller();
        assert_eq!(
            controller.begin_if(|| false).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        assert!(!fixture.0.join(REQUEST).exists());

        fs::write(fixture.0.join(REPLY), "42 1 paused\n").unwrap();
        let mounted = Cell::new(true);
        assert_eq!(
            controller
                .begin_if(|| mounted.replace(false))
                .unwrap_err()
                .kind(),
            io::ErrorKind::WouldBlock
        );
        assert_eq!(fixture.request(), "42 2 idle\n");
    }

    #[test]
    fn stale_pause_times_out_and_releases_the_pending_request() {
        let fixture = Fixture::new();
        let mut controller = fixture.controller();
        fs::write(fixture.0.join(REPLY), "41 1 paused\n").unwrap();
        assert_eq!(
            controller.begin_if(|| true).unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
        assert_eq!(fixture.request(), "42 2 idle\n");
    }

    #[test]
    fn an_exact_reply_grants_access_until_released() {
        let fixture = Fixture::new();
        let mut controller = fixture.controller();
        fs::write(fixture.0.join(REPLY), "42 1 paused\n").unwrap();
        controller.begin_if(|| true).unwrap();
        assert_eq!(fixture.request(), "42 1 write\n");
        controller.release().unwrap();
        assert_eq!(fixture.request(), "42 2 idle\n");
    }

    #[test]
    fn a_replacement_app_continues_the_same_pid_sequence() {
        let fixture = Fixture::new();
        let pid = std::process::id();
        fs::write(fixture.0.join(REQUEST), format!("{pid} 46 write\n")).unwrap();
        fs::write(fixture.0.join(REPLY), format!("{pid} 46 paused\n")).unwrap();
        let mut replacement = StorageLease::new(&fixture.0);
        replacement.release().unwrap();
        assert_eq!(fixture.request(), format!("{pid} 47 idle\n"));

        fs::write(fixture.0.join(REPLY), format!("{pid} 48 paused\n")).unwrap();
        replacement.begin_if(|| true).unwrap();
        assert_eq!(fixture.request(), format!("{pid} 48 write\n"));
    }

    #[test]
    fn restart_ignores_mailbox_rows_from_another_process_or_invalid_sequences() {
        let fixture = Fixture::new();
        let pid = std::process::id();
        fs::write(fixture.0.join(REQUEST), format!("{} 300 idle\n", pid + 1)).unwrap();
        fs::write(fixture.0.join(REPLY), format!("{pid} 1234567890 paused\n")).unwrap();
        StorageLease::new(&fixture.0).release().unwrap();
        assert_eq!(fixture.request(), format!("{pid} 1 idle\n"));
    }
}
