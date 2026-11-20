//! Change notifications for a scanned tree: one ReadDirectoryChangesW on its root, whole subtree.
//! The thread sleeps in the kernel until something changes, so watching costs nothing while idle.

use std::collections::HashSet;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};
use windows_sys::Win32::Foundation::{CloseHandle, ERROR_NOTIFY_ENUM_DIR, GetLastError, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OVERLAPPED, FILE_LIST_DIRECTORY, FILE_NOTIFY_CHANGE_DIR_NAME, FILE_NOTIFY_CHANGE_FILE_NAME,
    FILE_NOTIFY_CHANGE_SIZE, FILE_NOTIFY_INFORMATION, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, ReadDirectoryChangesW,
};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Threading::{CreateEventW, INFINITE, SetEvent, WaitForMultipleObjects};

/// Dirs whose listing changed, relative to the root (`None`: too many to tell, look at all of them).
pub type Changes = Option<Vec<PathBuf>>;

pub struct Watch {
    pub rx: Receiver<Changes>,
    stop: HANDLE,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Watch {
    fn drop(&mut self) {
        unsafe { SetEvent(self.stop) };
        self.thread.take().map(|t| t.join());
        unsafe { CloseHandle(self.stop) };
    }
}

/// Watch `root`; `wake` is called after each send. None if it cannot be watched (e.g. some network shares).
pub fn watch(root: &Path, wake: impl Fn() + Send + 'static) -> Option<Watch> {
    let wide: Vec<u16> = root.as_os_str().encode_wide().chain([0]).collect();
    let share = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;
    let flags = FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED;
    let dir = unsafe { CreateFileW(wide.as_ptr(), FILE_LIST_DIRECTORY, share, std::ptr::null(), OPEN_EXISTING, flags, std::ptr::null_mut()) };
    if dir == INVALID_HANDLE_VALUE {
        return None;
    }
    let (stop, io) = unsafe { (CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()), CreateEventW(std::ptr::null(), 0, 0, std::ptr::null())) };
    let (tx, rx) = channel();
    // HANDLEs are pointers (not Send): cross as integers.
    let (d, s, e) = (dir as usize, stop as usize, io as usize);
    let thread = std::thread::spawn(move || {
        let (dir, stop, io) = (d as HANDLE, s as HANDLE, e as HANDLE);
        let mut buf = vec![0u32; 16 * 1024]; // 64 KB, DWORD aligned (more fails on network shares)
        let filter = FILE_NOTIFY_CHANGE_FILE_NAME | FILE_NOTIFY_CHANGE_DIR_NAME | FILE_NOTIFY_CHANGE_SIZE;
        let mut ov: OVERLAPPED;
        loop {
            ov = OVERLAPPED { hEvent: io, ..unsafe { std::mem::zeroed() } };
            let len = (buf.len() * 4) as u32;
            if unsafe { ReadDirectoryChangesW(dir, buf.as_mut_ptr().cast(), len, 1, filter, std::ptr::null_mut(), &mut ov, None) } == 0 {
                break;
            }
            if unsafe { WaitForMultipleObjects(2, [io, stop].as_ptr(), 0, INFINITE) } != WAIT_OBJECT_0 {
                break; // stop
            }
            let mut n = 0;
            if unsafe { GetOverlappedResult(dir, &ov, &mut n, 0) } == 0 && unsafe { GetLastError() } != ERROR_NOTIFY_ENUM_DIR {
                break; // e.g. the root itself is gone
            }
            // Sent at once: the UI takes all that queued up once a frame, so it refreshes at most at
            // its frame rate (and the kernel keeps what comes meanwhile for the next read).
            let changes = (n > 0).then(|| changes(&buf));
            if tx.send(changes).is_err() {
                break;
            }
            wake();
        }
        unsafe {
            CancelIoEx(dir, &ov);
            let mut n = 0;
            GetOverlappedResult(dir, &ov, &mut n, 1); // the kernel is done with `buf` and `ov`
            CloseHandle(dir);
            CloseHandle(io);
        }
    });
    Some(Watch { rx, stop, thread: Some(thread) })
}

/// The dirs a filled notification buffer names (n == 0, overflow, is the caller's).
fn changes(buf: &[u32]) -> Vec<PathBuf> {
    let mut dirs = HashSet::new();
    let mut at = 0usize;
    loop {
        // Entries are DWORD aligned within the buffer; the name follows the 12-byte header.
        let p = unsafe { buf.as_ptr().cast::<u8>().add(at).cast::<FILE_NOTIFY_INFORMATION>() };
        let (next, bytes) = unsafe { ((*p).NextEntryOffset as usize, (*p).FileNameLength as usize) };
        let name = unsafe { std::slice::from_raw_parts(std::ptr::addr_of!((*p).FileName).cast::<u16>(), bytes / 2) };
        let path = PathBuf::from(std::ffi::OsString::from_wide(name));
        // The listing that changed is the parent's (a file's size, an entry added, removed or renamed).
        dirs.insert(path.parent().map(Path::to_path_buf).unwrap_or_default());
        if next == 0 {
            break;
        }
        at += next;
    }
    dirs.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn reports_the_dir_whose_listing_changed() {
        let root = std::env::temp_dir().join(format!("ss-rs-watch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("sub")).unwrap();
        let w = watch(&root, || {}).unwrap();
        std::fs::write(root.join("sub").join("f.txt"), [0u8; 10]).unwrap();
        let got = w.rx.recv_timeout(Duration::from_secs(5));
        drop(w); // joins the thread: stop works
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(got, Ok(Some(vec![PathBuf::from("sub")])));
    }
}
