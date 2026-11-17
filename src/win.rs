//! Thin Win32 helpers.

use std::os::windows::ffi::OsStrExt;
use std::os::windows::process::CommandExt;
use std::path::Path;
use windows_sys::Win32::Storage::FileSystem::{GetDiskFreeSpaceExW, GetLogicalDrives};
use windows_sys::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_BELOW_NORMAL};
use windows_sys::Win32::UI::Shell::{FO_DELETE, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_WANTNUKEWARNING, SHFILEOPSTRUCTW, SHFileOperationW, ShellExecuteW};
use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MessageBoxW, SW_SHOWNORMAL};

/// NUL-terminated UTF-16, twice: a path list for SHFileOperation, a plain string everywhere else.
fn wide(s: impl AsRef<std::ffi::OsStr>) -> Vec<u16> {
    s.as_ref().encode_wide().chain([0, 0]).collect()
}

/// Run the calling thread below normal priority: a scan's burst yields to the UI and other apps.
pub fn below_normal() {
    unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL) };
}

/// Map a file read-only for good: pages load on touch and stay file-backed, so a 20 MB font costs only its drawn glyphs.
pub fn map_file(path: &str) -> Option<&'static [u8]> {
    use windows_sys::Win32::Foundation::{CloseHandle, GENERIC_READ, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, GetFileSizeEx, OPEN_EXISTING};
    use windows_sys::Win32::System::Memory::{CreateFileMappingW, FILE_MAP_READ, MapViewOfFile, PAGE_READONLY};
    unsafe {
        let f = CreateFileW(wide(path).as_ptr(), GENERIC_READ, FILE_SHARE_READ, std::ptr::null(), OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, std::ptr::null_mut());
        if f == INVALID_HANDLE_VALUE {
            return None;
        }
        let mut size = 0i64;
        let ok = GetFileSizeEx(f, &mut size) != 0 && size > 0;
        let m = if ok { CreateFileMappingW(f, std::ptr::null(), PAGE_READONLY, 0, 0, std::ptr::null()) } else { std::ptr::null_mut() };
        CloseHandle(f);
        if m.is_null() {
            return None;
        }
        // The view keeps the mapping alive; it is never unmapped (fonts live as long as the app).
        let view = MapViewOfFile(m, FILE_MAP_READ, 0, 0, 0);
        CloseHandle(m);
        (!view.Value.is_null()).then(|| std::slice::from_raw_parts(view.Value as *const u8, size as usize))
    }
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
    // WANTNUKEWARNING: still ask before deleting for good what the bin cannot take.
    op.fFlags = (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_WANTNUKEWARNING) as u16;
    unsafe { SHFileOperationW(&mut op) == 0 && op.fAnyOperationsAborted == 0 }
}

pub fn open(p: &Path) {
    unsafe { ShellExecuteW(std::ptr::null_mut(), wide("open").as_ptr(), wide(p).as_ptr(), std::ptr::null(), std::ptr::null(), SW_SHOWNORMAL) };
}

/// The app has no console: tell the user why it could not start.
pub fn error_box(msg: &str) {
    unsafe { MessageBoxW(std::ptr::null_mut(), wide(msg).as_ptr(), wide("SpaceSniffer-rs").as_ptr(), MB_ICONERROR) };
}

pub fn reveal(p: &Path) {
    // explorer needs the quotes inside the /select, argument, so bypass std's quoting.
    let _ = std::process::Command::new("explorer").raw_arg(format!("/select,\"{}\"", p.display())).spawn();
}

/// Window setup that needs the HWND (once, from `App::new`): icons from the exe resource (eframe would
/// decode a PNG through the stubbed `image` crate); pure moves skip the repaint (each one waited in
/// present inside the modal move loop, so dragging juddered); a dark title bar.
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
        SetWindowSubclass(hwnd, Some(filter), 1, 0);
        dark_title(hwnd);
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

/// The UI is always dark, so is the title bar, whatever the system theme.
fn dark_title(hwnd: windows_sys::Win32::Foundation::HWND) {
    use windows_sys::Win32::Graphics::Dwm::{DWMWA_USE_IMMERSIVE_DARK_MODE, DwmSetWindowAttribute};
    let dark: i32 = 1;
    unsafe { DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE as u32, (&raw const dark).cast(), size_of::<i32>() as u32) };
}

unsafe extern "system" fn filter(
    hwnd: windows_sys::Win32::Foundation::HWND,
    msg: u32,
    wp: windows_sys::Win32::Foundation::WPARAM,
    lp: windows_sys::Win32::Foundation::LPARAM,
    _id: usize,
    _data: usize,
) -> windows_sys::Win32::Foundation::LRESULT {
    use windows_sys::Win32::UI::Shell::DefSubclassProc;
    use windows_sys::Win32::UI::WindowsAndMessaging::{DefWindowProcW, SWP_NOMOVE, SWP_NOSIZE, WINDOWPOS, WM_SETTINGCHANGE, WM_WINDOWPOSCHANGED};
    unsafe {
        if msg == WM_WINDOWPOSCHANGED {
            let flags = (*(lp as *const WINDOWPOS)).flags;
            if flags & SWP_NOSIZE != 0 && flags & SWP_NOMOVE == 0 {
                return DefWindowProcW(hwnd, msg, wp, lp);
            }
        }
        let r = DefSubclassProc(hwnd, msg, wp, lp);
        // winit resets the title bar to the system theme on every settings broadcast: dark again.
        if msg == WM_SETTINGCHANGE {
            dark_title(hwnd);
        }
        r
    }
}
