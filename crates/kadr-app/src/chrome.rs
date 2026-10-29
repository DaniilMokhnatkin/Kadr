//! Shared native button treatment. The BUTTON HWND retains its name, keyboard and UIA semantics.
use kadr_render::theme::Theme;
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    UI::{
        Input::KeyboardAndMouse::{TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent},
        Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
        WindowsAndMessaging::*,
    },
};
use windows::core::BOOL;
mod motion;
pub use motion::{progress, tick, timer_available};

pub fn color(c: windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F) -> COLORREF {
    COLORREF((c.r * 255.0).round() as u32 | ((c.g * 255.0).round() as u32) << 8 | ((c.b * 255.0).round() as u32) << 16)
}
pub fn round(dc: HDC, r: RECT, bg: COLORREF, edge: COLORREF, radius: i32, stroke: i32) {
    unsafe {
        let brush = CreateSolidBrush(bg);
        let pen = CreatePen(PS_SOLID, stroke, edge);
        let old_brush = SelectObject(dc, brush.into());
        let old_pen = SelectObject(dc, pen.into());
        let _ = RoundRect(dc, r.left, r.top, r.right, r.bottom, radius * 2, radius * 2);
        SelectObject(dc, old_pen);
        SelectObject(dc, old_brush);
        let _ = DeleteObject(pen.into());
        let _ = DeleteObject(brush.into());
    }
}
pub fn button(dc: HDC, r: RECT, primary: bool, focused: bool, pressed: bool, hover: f32, scale: f32) -> COLORREF {
    let t = Theme::current();
    let bg = if primary {
        t.accent
    } else if pressed {
        t.selected
    } else {
        mix(t.control, t.selected, hover)
    };
    let edge = if focused { t.accent } else { bg };
    round(
        dc,
        r,
        color(bg),
        color(edge),
        (10.0 * scale).round() as i32,
        if focused { (2.0 * scale).round() as i32 } else { 1 },
    );
    if focused && primary {
        let i = (3.0 * scale).round() as i32;
        round(
            dc,
            RECT {
                left: r.left + i,
                top: r.top + i,
                right: r.right - i,
                bottom: r.bottom - i,
            },
            color(bg),
            color(t.on_accent),
            (7.0 * scale).round() as i32,
            1,
        );
    }
    color(if primary {
        t.on_accent
    } else if pressed {
        t.on_selected
    } else {
        mix(t.text, t.on_selected, hover)
    })
}
pub fn mix(
    a: windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F,
    b: windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F,
    t: f32,
) -> windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F {
    let t = t.clamp(0.0, 1.0);
    windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F {
        r: a.r + (b.r - a.r) * t,
        g: a.g + (b.g - a.g) * t,
        b: a.b + (b.b - a.b) * t,
        a: a.a + (b.a - a.a) * t,
    }
}
pub fn hovered(hwnd: HWND) -> bool {
    unsafe {
        let mut p = POINT::default();
        let mut r = RECT::default();
        GetCursorPos(&mut p).is_ok()
            && ScreenToClient(hwnd, &mut p).as_bool()
            && GetClientRect(hwnd, &mut r).is_ok()
            && p.x >= 0
            && p.y >= 0
            && p.x < r.right
            && p.y < r.bottom
    }
}
pub fn track(hwnd: HWND) {
    unsafe {
        let _ = SetWindowSubclass(hwnd, Some(hover_proc), 1, 0);
    }
}

unsafe extern "system" fn hover_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM, id: usize, _data: usize) -> LRESULT {
    if msg == WM_MOUSEMOVE {
        let mut track = TRACKMOUSEEVENT {
            cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
            dwFlags: TME_LEAVE,
            hwndTrack: hwnd,
            ..Default::default()
        };
        unsafe {
            let _ = TrackMouseEvent(&mut track);
        }
        motion::begin(hwnd, true);
    } else if msg == WM_MOUSELEAVE {
        motion::begin(hwnd, false);
    } else if msg == WM_NCDESTROY {
        motion::remove(hwnd);
        unsafe {
            let _: BOOL = RemoveWindowSubclass(hwnd, Some(hover_proc), id);
        }
    }
    unsafe { DefSubclassProc(hwnd, msg, wp, lp) }
}
