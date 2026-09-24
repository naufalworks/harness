use anyhow::{bail, Context, Result};
use chrono::Utc;
use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

/// An operating-system lock held for the complete lifetime of one Harness process.
///
/// The adjacent metadata file may survive a crash, but the kernel lock never does. A new
/// process can therefore recover stale ownership without guessing whether a recorded PID is
/// still alive, while a live owner cannot be displaced or mistaken for stale metadata.
///
/// Release is explicit rather than implicit in closing the file. A `flock` lock belongs to the
/// open file description, not to the descriptor, so any child process that was forked while the
/// lock was held inherits that description and keeps the lock alive after the owner drops its
/// own descriptor. Relying on close alone therefore makes ownership outlive the owner: a restart
/// is refused with "already owned by another live Harness process" when no owner exists.
/// `LOCK_UN` acts on the description itself, so it releases the lock even when it is shared.
pub struct ProcessLock {
    _file: Option<File>,
}

impl ProcessLock {
    pub fn acquire(database: &str) -> Result<Self> {
        if database == ":memory:" {
            return Ok(Self { _file: None });
        }
        #[cfg(not(unix))]
        bail!("single-process database ownership is currently supported only on Unix");

        #[cfg(unix)]
        {
            let database = normalized_database_path(database)?;
            let lock_path = lock_path(&database)?;
            let mut file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                // Never truncate at open: a contending process must not destroy a live
                // owner's metadata before flock decides who owns the database. The proven
                // owner truncates below with set_len(0).
                .truncate(false)
                .mode(0o600)
                .open(&lock_path)
                .with_context(|| format!("open database process lock {}", lock_path.display()))?;
            std::fs::set_permissions(&lock_path, std::fs::Permissions::from_mode(0o600))?;
            // SAFETY: file owns a live descriptor for the duration of this call and the
            // operation flags are the platform flock constants.
            if unsafe { flock(file.as_raw_fd(), LOCK_EX | LOCK_NB) } != 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::WouldBlock {
                    bail!(
                        "database is already owned by another live Harness process: {}",
                        database.display()
                    );
                }
                return Err(error).with_context(|| {
                    format!("acquire database process lock {}", lock_path.display())
                });
            }

            // Only the proven owner rewrites metadata. Stale contents are diagnostic data, never
            // authority, so PID reuse cannot steal or retain ownership.
            file.set_len(0)?;
            file.seek(SeekFrom::Start(0))?;
            writeln!(file, "pid={}", std::process::id())?;
            writeln!(file, "started_at={}", Utc::now().to_rfc3339())?;
            writeln!(file, "database={}", database.display())?;
            file.sync_data()?;
            Ok(Self { _file: Some(file) })
        }
    }
}

#[cfg(unix)]
impl Drop for ProcessLock {
    fn drop(&mut self) {
        if let Some(file) = self._file.as_ref() {
            // Best effort: if this fails the descriptor still closes, which is the previous
            // behaviour. There is no recovery action available from a destructor.
            // SAFETY: file still owns the descriptor until this destructor returns.
            unsafe { flock(file.as_raw_fd(), LOCK_UN) };
        }
    }
}

fn normalized_database_path(database: &str) -> Result<PathBuf> {
    let input = Path::new(database);
    if input.exists() {
        return std::fs::canonicalize(input)
            .with_context(|| format!("resolve database path {}", input.display()));
    }
    let absolute = if input.is_absolute() {
        input.to_path_buf()
    } else {
        std::env::current_dir()?.join(input)
    };
    let parent = absolute.parent().context("database path has no parent")?;
    let parent = std::fs::canonicalize(parent)
        .with_context(|| format!("resolve database directory {}", parent.display()))?;
    let name = absolute
        .file_name()
        .context("database path has no file name")?;
    Ok(parent.join(name))
}

fn lock_path(database: &Path) -> Result<PathBuf> {
    let name = database
        .file_name()
        .and_then(|name| name.to_str())
        .context("database file name must be valid UTF-8")?;
    Ok(database.with_file_name(format!("{name}.lock")))
}

#[cfg(unix)]
const LOCK_EX: i32 = 2;
#[cfg(unix)]
const LOCK_NB: i32 = 4;
#[cfg(unix)]
const LOCK_UN: i32 = 8;
#[cfg(unix)]
extern "C" {
    fn flock(fd: i32, operation: i32) -> i32;
}
#[cfg(all(unix, test))]
extern "C" {
    fn dup(fd: i32) -> i32;
    fn close(fd: i32) -> i32;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (PathBuf, PathBuf) {
        let directory =
            std::env::temp_dir().join(format!("harness-process-lock-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let database = directory.join("harness.db");
        (directory, database)
    }

    #[test]
    fn a_live_owner_excludes_a_second_process_lock() {
        let (directory, database) = fixture();
        let first = ProcessLock::acquire(database.to_str().unwrap()).unwrap();
        let error = ProcessLock::acquire(database.to_str().unwrap())
            .err()
            .expect("a second owner must be rejected");
        assert!(error.to_string().contains("another live Harness process"));
        drop(first);
        ProcessLock::acquire(database.to_str().unwrap()).unwrap();
        std::fs::remove_dir_all(directory).ok();
    }

    #[test]
    fn stale_metadata_is_recovered_only_after_kernel_ownership() {
        let (directory, database) = fixture();
        let metadata = directory.join("harness.db.lock");
        std::fs::write(&metadata, "pid=999999\nstarted_at=stale\n").unwrap();
        let lock = ProcessLock::acquire(database.to_str().unwrap()).unwrap();
        let current = std::fs::read_to_string(&metadata).unwrap();
        assert!(current.contains(&format!("pid={}", std::process::id())));
        assert!(current.contains(&format!("database={}", database.display())));
        drop(lock);
        std::fs::remove_dir_all(directory).ok();
    }

    #[test]
    fn independent_databases_have_independent_owners() {
        let (directory, first) = fixture();
        let second = directory.join("other.db");
        let _first = ProcessLock::acquire(first.to_str().unwrap()).unwrap();
        let _second = ProcessLock::acquire(second.to_str().unwrap()).unwrap();
        std::fs::remove_dir_all(directory).ok();
    }

    /// Ownership must not outlive the owner just because some other descriptor still refers to
    /// the same open file description. A child process forked while the lock was held inherits
    /// exactly that state, and `dup` reproduces it deterministically in-process. Without an
    /// explicit `LOCK_UN` on release, the lock survives the owner and a restart is refused with
    /// "already owned by another live Harness process" when in fact nothing owns the database.
    #[test]
    fn releasing_ownership_does_not_depend_on_every_descriptor_being_closed() {
        let (directory, database) = fixture();
        let owner = ProcessLock::acquire(database.to_str().unwrap()).unwrap();
        // SAFETY: the source descriptor belongs to the live owner for this entire call.
        let inherited = unsafe { dup(owner._file.as_ref().unwrap().as_raw_fd()) };
        assert!(
            inherited >= 0,
            "dup must succeed for this test to mean anything"
        );

        drop(owner);

        let reacquired = ProcessLock::acquire(database.to_str().unwrap());
        // SAFETY: inherited is the successful result of dup above and is closed once.
        unsafe { close(inherited) };
        reacquired.expect("a database with no live owner must be claimable again");

        std::fs::remove_dir_all(directory).ok();
    }
}
