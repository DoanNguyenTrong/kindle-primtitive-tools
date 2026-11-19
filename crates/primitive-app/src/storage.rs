use kl_core::storage_lease::StorageLease;
use std::cell::Cell;
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

static LEASE: Mutex<Option<StorageLease>> = Mutex::new(None);
static SAVE_DEFERRED: AtomicBool = AtomicBool::new(false);

thread_local! {
    static ACTIVE: Cell<bool> = const { Cell::new(false) };
}

pub(super) fn enable(run: &Path) -> io::Result<bool> {
    if std::env::var_os("KL_NATIVE_APP_RUN").as_deref() != Some(run.join("native-app").as_os_str())
    {
        return Ok(false);
    }
    let mut storage = LEASE
        .lock()
        .map_err(|_| io::Error::other("Could not start USB storage coordination."))?;
    let mut lease = StorageLease::new(run);
    lease.release()?;
    *storage = Some(lease);
    Ok(true)
}

/// Use local Kindle storage for a bounded operation. Return only owned data from the callback.
/// Opening files, imports, and custom writes should finish inside this callback.
/// Desktop and legacy launcher sessions already own their storage.
pub fn with_storage<T>(operation: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
    if ACTIVE.get() {
        return operation();
    }
    let mut lock = LEASE
        .lock()
        .map_err(|_| io::Error::other("Could not reserve Kindle storage."))?;
    let Some(lease) = lock.as_mut() else {
        drop(lock);
        return operation();
    };
    lease.begin_write().map_err(|error| {
        SAVE_DEFERRED.store(true, Ordering::Release);
        // Recoverable lease errors must keep an app's unsaved state in memory.
        io::Error::new(io::ErrorKind::WouldBlock, error)
    })?;
    ACTIVE.set(true);
    let _active = ActiveLease(lease);
    operation()
}

struct ActiveLease<'a>(&'a mut StorageLease);

impl Drop for ActiveLease<'_> {
    fn drop(&mut self) {
        ACTIVE.set(false);
        if let Err(error) = self.0.release() {
            kl_kindle::log(&format!("Could not release Kindle storage: {error}"));
        }
    }
}

pub(super) fn pending_save() -> bool {
    SAVE_DEFERRED.swap(false, Ordering::AcqRel)
}
