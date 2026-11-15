//! Thin Win32 helpers.

use std::os::windows::ffi::OsStrExt;
use std::os::windows::process::CommandExt;
use std::path::Path;
use windows_sys::Win32::Graphics::Dwm::DwmFlush;
use windows_sys::Win32::Storage::FileSystem::{GetDiskFreeSpaceExW, GetLogicalDrives};
use windows_sys::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_BELOW_NORMAL};
use windows_sys::Win32::UI::Shell::{FO_DELETE, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_WANTNUKEWARNING, SHFILEOPSTRUCTW, SHFileOperationW, ShellExecuteW};
use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MessageBoxW, SW_SHOWNORMAL};

fn wide(s: &Path) -> Vec<u16> {
    s.as_os_str().encode_wide().chain([0, 0]).collect() // double NUL: SHFileOperation list format
}

/// Run the calling thread below normal priority: a scan's burst yields to the UI and other apps.
pub fn below_normal() {
    unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL) };
}

/// Block until the next DWM composition: frame pacing without GPU busy-wait.
pub fn dwm_flush() {
    unsafe { DwmFlush() };
}

/// Drive roots (instant: no drive is asked anything).
pub fn drives() -> Vec<String> {
    let mask = unsafe { GetLogicalDrives() };
    (0..26).filter(|i| mask & (1 << i) != 0).map(|i| format!("{}:\\", (b'A' + i as u8) as char)).collect()
}

/// (total, free) bytes of the volume containing `p`.
pub fn disk_space(p: &Path) -> Option<(u64, u64)> {
    let w = wide(p);
    let (mut free, mut total) = (0u64, 0u64);
    let ok = unsafe { GetDiskFreeSpaceExW(w.as_ptr(), std::ptr::null_mut(), &mut total, &mut free) };
    (ok != 0).then_some((total, free))
}

/// Move to the Recycle Bin. Caller already asked for confirmation.
pub fn recycle(p: &Path) -> bool {
    let w = wide(p);
    let mut op: SHFILEOPSTRUCTW = unsafe { std::mem::zeroed() };
    op.wFunc = FO_DELETE;
    op.pFrom = w.as_ptr();
    // WANTNUKEWARNING: Windows still asks before deleting for good what the bin cannot take (too
    // big, or a drive without one), which NOCONFIRMATION alone would do silently.
    op.fFlags = (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_WANTNUKEWARNING) as u16;
    unsafe { SHFileOperationW(&mut op) == 0 && op.fAnyOperationsAborted == 0 }
}

pub fn open(p: &Path) {
    let w = wide(p);
    let verb: Vec<u16> = "open\0".encode_utf16().collect();
    unsafe { ShellExecuteW(std::ptr::null_mut(), verb.as_ptr(), w.as_ptr(), std::ptr::null(), std::ptr::null(), SW_SHOWNORMAL) };
}

/// The app has no console: tell the user why it could not start.
pub fn error_box(msg: &str) {
    let z = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
    unsafe { MessageBoxW(std::ptr::null_mut(), z(msg).as_ptr(), z("SpaceSniffer-rs").as_ptr(), MB_ICONERROR) };
}

pub fn reveal(p: &Path) {
    // explorer needs the quotes inside the /select, argument, so bypass std's quoting.
    let _ = std::process::Command::new("explorer").raw_arg(format!("/select,\"{}\"", p.display())).spawn();
}
