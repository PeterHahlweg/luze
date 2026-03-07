use std::{fs, io, path::Path};
#[cfg(not(unix))] use std::path::PathBuf;

/// RAII guard returned by [`acquire_write_lock`].
///
/// On Unix the guard holds the open file whose fd carries the `flock(2)` lease;
/// dropping it closes the fd and releases the lock.  On other platforms the
/// guard holds the path of the presence-lock file and deletes it on drop.
pub struct WriteLock {
    _file: fs::File,
    #[cfg(not(unix))]
    _path: PathBuf,
}

#[cfg(not(unix))]
impl Drop for WriteLock {
    fn drop(&mut self) { let _ = fs::remove_file(&self._path); }
}

/// Acquires a global exclusive write lock for the NoteBox.
///
/// Blocks until the lock becomes available. On Unix: `flock(2)` on
/// `writes.lock`. On other platforms: presence-based spin lock (creates
/// `writes.lock` exclusively; deletes it on drop). Acquire before any
/// write operation to prevent concurrent corruption of draw files or the index.
pub fn acquire_write_lock(dir: &Path) -> io::Result<WriteLock> {
    fs::create_dir_all(dir)?;
    acquire_lock_file(&dir.join("writes.lock"))
}

/// Acquires an exclusive lock on `path`.
/// Unix: opens (or creates) the file and calls `flock(LOCK_EX)`.
/// Other: spins on `create_new`, retrying every 10 ms for up to 30 s.
pub(crate) fn acquire_lock_file(path: &Path) -> io::Result<WriteLock> {
    #[cfg(unix)]
    {
        let file = fs::OpenOptions::new().write(true).create(true).open(path)?;
        use std::os::unix::io::AsRawFd;
        extern "C" { fn flock(fd: i32, operation: i32) -> i32; }
        const LOCK_EX: i32 = 2;
        let ret = unsafe { flock(file.as_raw_fd(), LOCK_EX) };
        if ret != 0 { return Err(io::Error::last_os_error()); }
        Ok(WriteLock { _file: file })
    }
    #[cfg(not(unix))]
    {
        use std::io::Write;
        const STALE_SECS: u64 = 30;
        const RETRY_MS:   u64 = 10;

        fn now_secs() -> u64 {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
        }

        fn is_stale(path: &Path, stale_secs: u64) -> bool {
            let age = if let Ok(s) = fs::read_to_string(path) {
                if let Ok(t) = s.trim().parse::<u64>() {
                    now_secs().saturating_sub(t)
                } else {
                    // No readable timestamp — fall back to file mtime.
                    fs::metadata(path)
                        .and_then(|m| m.modified())
                        .map(|t| t.elapsed().unwrap_or_default().as_secs())
                        .unwrap_or(0)
                }
            } else {
                0
            };
            age > stale_secs
        }

        loop {
            match fs::OpenOptions::new().write(true).create_new(true).open(path) {
                Ok(mut file) => {
                    let _ = write!(file, "{}", now_secs());
                    return Ok(WriteLock { _file: file, _path: path.to_owned() });
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    if is_stale(path, STALE_SECS) {
                        eprintln!("warning: removing stale write lock at {}", path.display());
                        let _ = fs::remove_file(path);
                        // retry immediately
                    } else {
                        std::thread::sleep(std::time::Duration::from_millis(RETRY_MS));
                    }
                }
                Err(e) => return Err(e),
            }
        }
    }
}
