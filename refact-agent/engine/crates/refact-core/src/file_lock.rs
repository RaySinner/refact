use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;

use fd_lock::{RwLock, RwLockWriteGuard};

pub fn open_lock(path: &Path) -> io::Result<RwLock<File>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(path)?;
    Ok(RwLock::new(file))
}

pub fn try_lock(lock: &mut RwLock<File>) -> io::Result<RwLockWriteGuard<'_, File>> {
    lock.try_write()
}

pub fn lock_blocking(lock: &mut RwLock<File>) -> io::Result<RwLockWriteGuard<'_, File>> {
    lock.write()
}

pub fn is_already_locked(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::WouldBlock
}

pub const WORKSPACE_BUSY_EXIT_CODE: i32 = 75;
pub const WORKSPACE_BUSY_MARKER: &str = "workspace busy";
pub const ENGINE_LOCKS_DIR_ENV: &str = "REFACT_ENGINE_LOCKS_DIR";
