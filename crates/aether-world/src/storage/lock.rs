//! An exclusive lock on a world directory.
//!
//! A world is one journal and one pair of item-id/sequence reservations in a
//! single KV store. Two processes opening the same directory would each mint
//! ids and numbers from the same bounds, handing out duplicates that the
//! ledger can only report after the fact — and each would overwrite the
//! other's sub-chunks. The lock below is what makes "one world, one server"
//! true rather than merely intended: [`FjallStore::open`] takes it, holds it
//! for the process's lifetime, and refuses to open a directory another process
//! already holds.
//!
//! [`FjallStore::open`]: super::FjallStore::open

use crate::storage::StorageError;

/// The lock file's name inside a world directory. A file of its own, not a
/// sub-chunk or a meta key, so it is never confused with world data.
#[cfg(unix)]
const LOCK_FILE: &str = "aether.lock";

#[cfg(unix)]
mod imp {
    use super::*;
    use std::fs::OpenOptions;
    use std::io::{Read, Seek, SeekFrom, Write};
    use std::os::unix::io::AsRawFd;
    use std::path::Path;

    /// A held `flock(LOCK_EX)` on a world directory's lock file.
    ///
    /// Dropping it — when the [`FjallStore`](super::super::FjallStore) is
    /// dropped, normally at process exit — releases the lock; the kernel does
    /// the same if the process dies, so a crash never leaves a world locked.
    #[derive(Debug)]
    pub(crate) struct WorldLock {
        // Kept alive only for the descriptor it holds the lock on.
        _file: std::fs::File,
    }

    impl WorldLock {
        /// Take the lock on `dir`, creating the directory and lock file if
        /// needed.
        ///
        /// A non-blocking `flock`: if another process holds it, this fails at
        /// once with an error naming that process's pid (recorded in the lock
        /// file), rather than hanging an operator who cannot tell why the
        /// server will not start.
        pub(crate) fn acquire(dir: &Path) -> Result<WorldLock, StorageError> {
            std::fs::create_dir_all(dir).map_err(|e| {
                StorageError::Backend(format!(
                    "cannot create world directory `{}`: {e}",
                    dir.display()
                ))
            })?;
            let path = dir.join(LOCK_FILE);
            let mut file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(&path)
                .map_err(|e| {
                    StorageError::Backend(format!(
                        "cannot open the world lock file `{}`: {e}",
                        path.display()
                    ))
                })?;
            // SAFETY: `file` owns a valid, open descriptor for as long as the
            // returned lock exists; `flock` only inspects it.
            let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if rc != 0 {
                let err = std::io::Error::last_os_error();
                // EWOULDBLOCK is what a held `LOCK_EX` reports; EINTR is worth
                // retrying, but an operator restarting is a fine response too.
                if err.raw_os_error() == Some(libc::EWOULDBLOCK) {
                    let holder = read_pid(&path);
                    return Err(StorageError::Backend(format!(
                        "world directory `{}` is already open by another process{}; \
                         a second server or converter would mint duplicate item ids and \
                         journal sequence numbers, so this one refuses to open it",
                        dir.display(),
                        match holder {
                            Some(pid) => format!(" (pid {pid})"),
                            None => String::new(),
                        }
                    )));
                }
                return Err(StorageError::Backend(format!(
                    "cannot lock the world directory `{}`: {err}",
                    dir.display()
                )));
            }
            // Ours now. Record the pid so the next contender can be told who
            // holds it. Failure here is not fatal: the lock is taken, only the
            // error message would be less specific.
            let pid = std::process::id();
            let _ = file.set_len(0);
            let _ = file.seek(SeekFrom::Start(0));
            let _ = file.write_all(pid.to_string().as_bytes());
            let _ = file.flush();
            Ok(WorldLock { _file: file })
        }
    }

    /// The pid recorded by the process holding the lock, if it wrote one.
    fn read_pid(path: &Path) -> Option<u32> {
        let mut s = String::new();
        std::fs::File::open(path)
            .ok()?
            .read_to_string(&mut s)
            .ok()?;
        s.trim().parse().ok()
    }
}

/// On a non-unix target there is no `flock`; the store still opens, without an
/// inter-process guard. The server and converter are Unix-targeted.
#[cfg(not(unix))]
mod imp {
    use std::path::Path;

    #[derive(Debug)]
    pub(crate) struct WorldLock;

    impl WorldLock {
        pub(crate) fn acquire(_dir: &Path) -> Result<WorldLock, crate::storage::StorageError> {
            Ok(WorldLock)
        }
    }
}

pub(crate) use imp::WorldLock;

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("aether-lock-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn a_second_lock_on_the_same_directory_is_refused_and_names_the_holder() {
        let dir = scratch("contend");
        let first = WorldLock::acquire(&dir).expect("first lock");
        let err = WorldLock::acquire(&dir).expect_err("a second lock must be refused");
        let msg = err.to_string();
        assert!(
            msg.contains(&std::process::id().to_string()),
            "the error must name the holding pid: {msg}"
        );
        // Dropping the holder hands the directory to the next opener.
        drop(first);
        let _second = WorldLock::acquire(&dir).expect("lock after release");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn different_directories_lock_independently() {
        let a = scratch("a");
        let b = scratch("b");
        let _la = WorldLock::acquire(&a).expect("lock a");
        let _lb = WorldLock::acquire(&b).expect("lock b");
        let _ = std::fs::remove_dir_all(&a);
        let _ = std::fs::remove_dir_all(&b);
    }
}
