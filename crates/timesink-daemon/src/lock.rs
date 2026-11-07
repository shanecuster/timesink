//! Makes sure only one daemon records at a time.
//!
//! Two daemons writing to the same database would each record the same
//! focus time, so every total would be counted twice. The daemon therefore
//! takes an exclusive lock on a small file at startup and keeps it until it
//! exits. The operating system releases the lock automatically if the
//! process dies, so a crash can never leave a stale lock behind.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::Path;

use anyhow::{bail, Context, Result};

/// Holds the lock for as long as it is alive; dropping it releases the lock.
pub struct InstanceLock {
    /// The open file. The lock lives and dies with this handle.
    _file: File,
}

impl InstanceLock {
    /// Take the lock stored in `path`, creating the file if needed.
    ///
    /// Fails with a clear message if another daemon already holds it.
    pub fn acquire(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path)
            .with_context(|| format!("opening lock file {}", path.display()))?;
        match file.try_lock() {
            Ok(()) => Ok(Self { _file: file }),
            Err(TryLockError::WouldBlock) => bail!(
                "another timesink daemon is already running (lock held on {}); \
                 stop it first, for example: systemctl --user stop timesink-daemon",
                path.display()
            ),
            Err(TryLockError::Error(e)) => {
                Err(e).with_context(|| format!("locking {}", path.display()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A unique scratch path for each test.
    fn scratch(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("timesink-lock-{}-{name}", std::process::id()))
    }

    #[test]
    fn second_instance_is_refused() {
        let path = scratch("second");
        let first = InstanceLock::acquire(&path).unwrap();
        let second = InstanceLock::acquire(&path);
        assert!(second.is_err());
        assert!(second
            .err()
            .unwrap()
            .to_string()
            .contains("already running"));
        drop(first);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn lock_is_free_again_after_release() {
        let path = scratch("release");
        drop(InstanceLock::acquire(&path).unwrap());
        assert!(InstanceLock::acquire(&path).is_ok());
        let _ = std::fs::remove_file(&path);
    }
}
