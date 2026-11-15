//! Thin Win32 helpers.

use std::os::windows::ffi::OsStrExt;
use std::os::windows::process::CommandExt;
use std::path::Path;
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

/// Window setup that needs the HWND (called once from `App::new`):
/// - Icons from the exe's icon resource (build.rs embeds assets/icon.ico as id 1; Windows decodes its
///   PNG entries itself), at the system's big/small sizes. eframe would otherwise decode a PNG through
///   the `image` crate, which is stubbed out (see stub/image).
/// - Pure moves don't repaint: dragging the title bar sends WM_WINDOWPOSCHANGED per mouse step,
///   winit turns it into `Moved`, egui repaints, and each frame waits in present inside the
///   modal move loop, so the window lags and judders. Nothing we draw depends on the window position.
pub fn setup_window(h: &impl raw_window_handle::HasWindowHandle) {
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::Shell::SetWindowSubclass;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetSystemMetrics, ICON_BIG, ICON_SMALL, IMAGE_ICON, LR_DEFAULTCOLOR, LoadImageW, SM_CXICON, SM_CXSMICON, SendMessageW, WM_SETICON,
    };
    let Ok(wh) = h.window_handle() else { return };
    let raw_window_handle::RawWindowHandle::Win32(w) = wh.as_raw() else { return };
    let hwnd = w.hwnd.get() as windows_sys::Win32::Foundation::HWND;
    unsafe {
        SetWindowSubclass(hwnd, Some(move_filter), 1, 0);
        let module = GetModuleHandleW(std::ptr::null());
        for (metric, which) in [(SM_CXICON, ICON_BIG), (SM_CXSMICON, ICON_SMALL)] {
            let s = GetSystemMetrics(metric);
            let icon = LoadImageW(module, 1 as _, IMAGE_ICON, s, s, LR_DEFAULTCOLOR); // MAKEINTRESOURCE(1)
            if !icon.is_null() {
                SendMessageW(hwnd, WM_SETICON, which as usize, icon as isize);
            }
        }
    }
}

unsafe extern "system" fn move_filter(
    hwnd: windows_sys::Win32::Foundation::HWND,
    msg: u32,
    wp: windows_sys::Win32::Foundation::WPARAM,
    lp: windows_sys::Win32::Foundation::LPARAM,
    _id: usize,
    _data: usize,
) -> windows_sys::Win32::Foundation::LRESULT {
    use windows_sys::Win32::UI::Shell::DefSubclassProc;
    use windows_sys::Win32::UI::WindowsAndMessaging::{DefWindowProcW, SWP_NOMOVE, SWP_NOSIZE, WINDOWPOS, WM_WINDOWPOSCHANGED};
    unsafe {
        if msg == WM_WINDOWPOSCHANGED {
            let flags = (*(lp as *const WINDOWPOS)).flags;
            if flags & SWP_NOSIZE != 0 && flags & SWP_NOMOVE == 0 {
                return DefWindowProcW(hwnd, msg, wp, lp);
            }
        }
        DefSubclassProc(hwnd, msg, wp, lp)
    }
}
