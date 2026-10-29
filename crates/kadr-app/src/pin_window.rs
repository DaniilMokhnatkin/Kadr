//! Отдельные окна с уже плоским снимком. Никаких исходных пикселей под размытиями.
use kadr_doc::Image;
use kadr_editor::pin::Pin;
use kadr_render::theme::Theme;
use kadr_win::wide;
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
    sync::Arc,
};
use windows::{
    Win32::{
        Foundation::*,
        Graphics::Gdi::*,
        UI::{
            Controls::{DRAWITEMSTRUCT, ODS_FOCUS, ODS_SELECTED},
            HiDpi::GetDpiForWindow,
            Input::KeyboardAndMouse::{GetKeyState, VK_CONTROL, VK_SHIFT},
            WindowsAndMessaging::*,
        },
    },
    core::{BOOL, PCWSTR, Result, w},
};
mod paint;
pub enum Request {
    Copy(Arc<Image>),
    Recent,
}
struct Data {
    host: HWND,
    message: u32,
    image: Arc<Image>,
    pin: Pin,
    scale: f32,
    font: HFONT,
    buttons: Vec<HWND>,
    alive: Rc<Cell<bool>>,
    wheel: i32,
}
thread_local! {
    static WINDOWS:RefCell<HashMap<isize,Rc<RefCell<Data>>>>=RefCell::new(HashMap::new());
    static REQUESTS:RefCell<Vec<Request>>=const{RefCell::new(Vec::new())};
}
pub struct Window {
    pub hwnd: HWND,
    alive: Rc<Cell<bool>>,
}
const COPY: usize = 1;
const MINUS: usize = 3;
const ORIGINAL: usize = 4;
const PLUS: usize = 5;
const OPACITY: usize = 6;
const CLOSE: usize = 7;
const ACTIONS: [usize; 6] = [COPY, MINUS, ORIGINAL, PLUS, OPACITY, CLOSE];
fn data(hwnd: HWND) -> Option<Rc<RefCell<Data>>> {
    WINDOWS.with(|w| w.borrow().get(&(hwnd.0 as isize)).cloned())
}
fn work_area(hwnd: HWND) -> RECT {
    let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    unsafe {
        let _ = GetMonitorInfoW(monitor, &mut info);
    }
    info.rcWork
}
fn available(hwnd: HWND, scale: f32) -> (i32, i32) {
    let r = work_area(hwnd);
    (
        (r.right - r.left - 24).max(1),
        (r.bottom - r.top - (140.0 * scale) as i32).max(1),
    )
}
fn font(scale: f32) -> HFONT {
    unsafe {
        CreateFontW(
            -(14.0 * scale).round() as i32,
            0,
            0,
            0,
            400,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            DEFAULT_PITCH.0 as u32,
            w!("Segoe UI"),
        )
    }
}
impl Window {
    /// Создание скрытое: можно закончить захват и только затем вызвать show().
    pub fn new(instance: HINSTANCE, host: HWND, message: u32, image: Arc<Image>, icon: HICON) -> Result<Self> {
        unsafe {
            let class = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                style: CS_DBLCLKS,
                hInstance: instance,
                lpfnWndProc: Some(proc),
                hCursor: LoadCursorW(None, IDC_ARROW)?,
                hIcon: icon,
                lpszClassName: w!("KadrPin"),
                ..Default::default()
            };
            if RegisterClassExW(&class) == 0 && GetLastError() != ERROR_CLASS_ALREADY_EXISTS {
                return Err(windows::core::Error::from_thread());
            }
            let title = wide(&format!(
                "Kadr · Закреплённый снимок · {} × {}",
                image.width(),
                image.height()
            ));
            let hwnd = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_LAYERED | WS_EX_CONTROLPARENT,
                w!("KadrPin"),
                PCWSTR(title.as_ptr()),
                WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU,
                image.rect.left,
                image.rect.top,
                600,
                400,
                None,
                None,
                Some(instance),
                None,
            )?;
            let scale = GetDpiForWindow(hwnd) as f32 / 96.0;
            let alive = Rc::new(Cell::new(true));
            let pin = Pin::new(image.width(), image.height(), available(hwnd, scale));
            let state = Rc::new(RefCell::new(Data {
                host,
                message,
                image,
                pin,
                scale,
                font: font(scale),
                buttons: Vec::new(),
                alive: alive.clone(),
                wheel: 0,
            }));
            WINDOWS.with(|w| w.borrow_mut().insert(hwnd.0 as isize, state.clone()));
            let window = Self { hwnd, alive };
            for id in ACTIONS {
                let label = wide(&paint::accessible_label(id, &state.borrow().pin));
                let button = CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    w!("BUTTON"),
                    PCWSTR(label.as_ptr()),
                    WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(BS_OWNERDRAW as u32),
                    0,
                    0,
                    1,
                    1,
                    Some(hwnd),
                    Some(HMENU(id as *mut _)),
                    Some(instance),
                    None,
                )?;
                crate::chrome::track(button);
                state.borrow_mut().buttons.push(button);
            }
            let dark = BOOL::from(Theme::current().surface.r < 0.5);
            let _ = windows::Win32::Graphics::Dwm::DwmSetWindowAttribute(
                hwnd,
                windows::Win32::Graphics::Dwm::DWMWA_USE_IMMERSIVE_DARK_MODE,
                &dark as *const _ as *const _,
                size_of_val(&dark) as u32,
            );
            resize(hwnd);
            Ok(window)
        }
    }
    pub fn show(&self) {
        if self.alive.get() {
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_SHOWNORMAL);
                let _ = windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(Some(self.hwnd));
            }
        }
    }
    pub fn alive(&self) -> bool {
        self.alive.get()
    }
}
impl Drop for Window {
    fn drop(&mut self) {
        if self.alive.get() {
            unsafe {
                let _ = DestroyWindow(self.hwnd);
            }
        }
    }
}
pub fn requests() -> Vec<Request> {
    REQUESTS.with(|r| std::mem::take(&mut *r.borrow_mut()))
}
fn request(hwnd: HWND, value: Request) {
    if let Some(state) = data(hwnd) {
        let (host, message) = {
            let d = state.borrow();
            (d.host, d.message)
        };
        REQUESTS.with(|r| r.borrow_mut().push(value));
        unsafe {
            let _ = PostMessageW(Some(host), message, WPARAM(0), LPARAM(0));
        }
    }
}
fn command(hwnd: HWND, id: usize) {
    let Some(state) = data(hwnd) else { return };
    if id == COPY {
        let image = state.borrow().image.clone();
        request(hwnd, Request::Copy(image));
        return;
    }
    if id == CLOSE {
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
        return;
    }
    {
        let mut d = state.borrow_mut();
        let available = available(hwnd, d.scale);
        match id {
            MINUS => d.pin.zoom_by(-1, available),
            PLUS => d.pin.zoom_by(1, available),
            ORIGINAL => d.pin.original(available),
            OPACITY => {
                if d.pin.opacity <= 100 {
                    d.pin.opacity = 255;
                } else {
                    d.pin.fade(-3);
                }
            }
            _ => return,
        }
    }
    resize(hwnd);
}
fn resize(hwnd: HWND) {
    let Some(state) = data(hwnd) else { return };
    let (size, scale, alpha, buttons) = {
        let d = state.borrow();
        (d.pin.size(), d.scale, d.pin.opacity, d.buttons.clone())
    };
    let work = work_area(hwnd);
    let mut outer = RECT {
        left: 0,
        top: 0,
        right: size.0.max((600.0 * scale) as i32).min(work.right - work.left - 16),
        bottom: size.1 + paint::footer(scale),
    };
    unsafe {
        let _ = AdjustWindowRectEx(
            &mut outer,
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU,
            false,
            WS_EX_TOPMOST | WS_EX_LAYERED | WS_EX_CONTROLPARENT,
        );
        let mut current = RECT::default();
        let _ = GetWindowRect(hwnd, &mut current);
        let width = (outer.right - outer.left).min(work.right - work.left);
        let height = (outer.bottom - outer.top).min(work.bottom - work.top);
        let x = current.left.clamp(work.left, (work.right - width).max(work.left));
        let y = current.top.clamp(work.top, (work.bottom - height).max(work.top));
        let _ = SetWindowPos(hwnd, None, x, y, width, height, SWP_NOZORDER | SWP_NOACTIVATE);
        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), alpha, LWA_ALPHA);
        let mut client = RECT::default();
        let _ = GetClientRect(hwnd, &mut client);
        for ((id, button), r) in ACTIONS.iter().zip(buttons).zip(paint::buttons(client, scale)) {
            let label = wide(&paint::accessible_label(*id, &state.borrow().pin));
            let _ = SetWindowTextW(button, PCWSTR(label.as_ptr()));
            let _ = SetWindowPos(
                button,
                None,
                r.left,
                r.top,
                r.right - r.left,
                r.bottom - r.top,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
}
fn key(hwnd: HWND, vk: usize, ctrl: bool, shift: bool) -> bool {
    match vk {
        0x1B => command(hwnd, CLOSE),
        0x43 if ctrl => command(hwnd, COPY),
        0x48 if ctrl => request(hwnd, Request::Recent),
        0xBB | 0x6B => command(hwnd, PLUS),
        0xBD | 0x6D => command(hwnd, MINUS),
        0x30 | 0x60 => command(hwnd, ORIGINAL),
        0x25..=0x28 => unsafe {
            let step = if shift { 10 } else { 1 };
            let mut r = RECT::default();
            let _ = GetWindowRect(hwnd, &mut r);
            let (dx, dy) = match vk {
                0x25 => (-step, 0),
                0x26 => (0, -step),
                0x27 => (step, 0),
                _ => (0, step),
            };
            let _ = SetWindowPos(
                hwnd,
                None,
                r.left + dx,
                r.top + dy,
                0,
                0,
                SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
        },
        _ => return false,
    }
    true
}
pub fn dialog_message(msg: &MSG) -> bool {
    let parent = unsafe { GetAncestor(msg.hwnd, GA_ROOT) };
    if data(parent).is_none() {
        return false;
    }
    if msg.message == WM_KEYDOWN && msg.wParam.0 == 0x0D {
        let focus = unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetFocus() };
        let id = unsafe { GetDlgCtrlID(focus) } as usize;
        command(parent, if ACTIONS.contains(&id) { id } else { COPY });
        return true;
    }
    if msg.message == WM_KEYDOWN
        && key(
            parent,
            msg.wParam.0,
            unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0,
            unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0,
        )
    {
        return true;
    }
    unsafe { IsDialogMessageW(parent, msg).as_bool() }
}
unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_NCDESTROY => {
            if let Some(d) = WINDOWS.with(|w| w.borrow_mut().remove(&(hwnd.0 as isize))) {
                let d = d.borrow();
                d.alive.set(false);
                unsafe {
                    let _ = DeleteObject(d.font.into());
                }
            }
        }
        WM_ERASEBKGND => return LRESULT(1),
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let dc = unsafe { BeginPaint(hwnd, &mut ps) };
            if let Some(d) = data(hwnd) {
                let d = d.borrow();
                let mut r = RECT::default();
                unsafe {
                    let _ = GetClientRect(hwnd, &mut r);
                }
                paint::draw(dc, r, &d.image, &d.pin, d.scale);
            }
            unsafe {
                let _ = EndPaint(hwnd, &ps);
            }
            return LRESULT(0);
        }
        WM_DRAWITEM => {
            if let Some(d) = data(hwnd) {
                let d = d.borrow();
                let item = unsafe { &*(lp.0 as *const DRAWITEMSTRUCT) };
                paint::button(
                    item.hDC,
                    item.rcItem,
                    item.CtlID as usize,
                    &d.pin,
                    d.font,
                    item.itemState.0 & ODS_FOCUS.0 != 0,
                    item.itemState.0 & ODS_SELECTED.0 != 0,
                    crate::chrome::progress(item.hwndItem, crate::chrome::hovered(item.hwndItem)),
                );
            }
            return LRESULT(1);
        }
        WM_COMMAND if wp.0 >> 16 == 0 => {
            command(hwnd, wp.0 & 0xffff);
            return LRESULT(0);
        }
        WM_MOUSEWHEEL => {
            if let Some(d) = data(hwnd) {
                let mut d = d.borrow_mut();
                d.wheel += ((wp.0 >> 16) as u16 as i16) as i32;
                let steps = d.wheel / 120;
                d.wheel %= 120;
                if wp.0 & 8 != 0 {
                    d.pin.fade(steps);
                } else {
                    let size = available(hwnd, d.scale);
                    d.pin.zoom_by(steps, size);
                }
            }
            resize(hwnd);
            return LRESULT(0);
        }
        WM_NCHITTEST => {
            let hit = unsafe { DefWindowProcW(hwnd, msg, wp, lp) };
            if hit.0 == HTCLIENT as isize
                && let Some(d) = data(hwnd)
            {
                let d = d.borrow();
                let mut p = POINT {
                    x: lp.0 as u16 as i16 as i32,
                    y: (lp.0 >> 16) as u16 as i16 as i32,
                };
                let mut r = RECT::default();
                unsafe {
                    let _ = ScreenToClient(hwnd, &mut p);
                    let _ = GetClientRect(hwnd, &mut r);
                }
                if p.y < r.bottom - paint::footer(d.scale) {
                    return LRESULT(HTCAPTION as isize);
                }
            }
            return hit;
        }
        WM_NCLBUTTONDBLCLK if wp.0 == HTCAPTION as usize => {
            command(hwnd, CLOSE);
            return LRESULT(0);
        }
        WM_CLOSE => {
            command(hwnd, CLOSE);
            return LRESULT(0);
        }
        WM_DPICHANGED => {
            let suggested = unsafe { *(lp.0 as *const RECT) };
            unsafe {
                let _ = SetWindowPos(
                    hwnd,
                    None,
                    suggested.left,
                    suggested.top,
                    0,
                    0,
                    SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
            if let Some(d) = data(hwnd) {
                let mut d = d.borrow_mut();
                let old = d.font;
                d.scale = (wp.0 & 0xffff) as f32 / 96.0;
                d.font = font(d.scale);
                unsafe {
                    let _ = DeleteObject(old.into());
                }
                let size = available(hwnd, d.scale);
                d.pin.fit(size);
            }
            resize(hwnd);
            return LRESULT(0);
        }
        WM_SETTINGCHANGE | WM_THEMECHANGED => unsafe {
            let _ = InvalidateRect(Some(hwnd), None, false);
        },
        _ => {}
    }
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

/// Невышедший на экран preview, тем же GDI-проходом, что WM_PAINT и WM_DRAWITEM.
pub fn preview(image: &Image, scale: f32) -> Result<Image> {
    paint::preview(image, scale)
}
#[cfg(test)]
mod tests;
