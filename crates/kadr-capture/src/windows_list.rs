//! Окна для подсветки под курсором: снимок z-порядка в момент захвата.

use std::ffi::c_void;

use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GWL_EXSTYLE, GetClassNameW, GetWindowLongPtrW, GetWindowRect, GetWindowThreadProcessId, IsIconic,
    IsWindowVisible, WS_EX_TRANSPARENT,
};
use windows::core::BOOL;

use kadr_doc::Rect;

/// Видимые окна верхнего уровня сверху вниз по z-порядку, по рамке DWM без тени.
/// Пропускаются свои окна (оверлеи), рабочий стол, скрытые DWM (cloaked) и прозрачные для клика
/// (оверлеи NVIDIA, Discord): иначе они перекрыли бы весь экран.
pub fn top_level_windows() -> Vec<Rect> {
    let mut found: Vec<Rect> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(collect), LPARAM(&mut found as *mut Vec<Rect> as isize));
    }
    found
}

unsafe extern "system" fn collect(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let found = unsafe { &mut *(lparam.0 as *mut Vec<Rect>) };
    if let Some(rect) = unsafe { window_rect(hwnd) } {
        found.push(rect);
    }
    true.into()
}

unsafe fn window_rect(hwnd: HWND) -> Option<Rect> {
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
            return None;
        }
        if GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_TRANSPARENT.0 != 0 {
            return None;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == GetCurrentProcessId() {
            return None;
        }
        let mut cloaked = 0u32;
        let cloaked_ptr = &mut cloaked as *mut u32 as *mut c_void;
        if DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, cloaked_ptr, 4).is_ok() && cloaked != 0 {
            return None;
        }
        let mut class = [0u16; 32];
        let len = GetClassNameW(hwnd, &mut class) as usize;
        let class = String::from_utf16_lossy(&class[..len]);
        if class == "Progman" || class == "WorkerW" {
            return None;
        }
        let mut r = RECT::default();
        let r_ptr = &mut r as *mut RECT as *mut c_void;
        if DwmGetWindowAttribute(hwnd, DWMWA_EXTENDED_FRAME_BOUNDS, r_ptr, size_of::<RECT>() as u32).is_err() {
            GetWindowRect(hwnd, &mut r).ok()?;
        }
        let rect = Rect::new(r.left, r.top, r.right, r.bottom);
        (rect.width() >= 8 && rect.height() >= 8).then_some(rect)
    }
}
