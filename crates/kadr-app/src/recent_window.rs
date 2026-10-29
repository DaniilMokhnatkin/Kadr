//! Нативная лента восстановления. Настоящие кнопки дают Tab/Space и имена для UI Automation.
use kadr_render::theme::Theme;
use kadr_store::recent::Entry;
use kadr_win::wide;
use std::cell::RefCell;
use windows::Win32::Foundation::{COLORREF, FILETIME, HINSTANCE, HWND, LPARAM, LRESULT, RECT, SYSTEMTIME, WPARAM};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};
use windows::Win32::UI::Controls::{DRAWITEMSTRUCT, ODS_FOCUS, ODS_SELECTED};
use windows::Win32::UI::HiDpi::{GetDpiForSystem, GetDpiForWindow};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetFocus, SetFocus};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, Result, w};

pub enum Request {
    Load(String),
    Clear,
    Close,
    Refresh,
}
struct Data {
    hwnd: HWND,
    host: HWND,
    message: u32,
    entries: Vec<Entry>,
    buttons: Vec<HWND>,
    loading: bool,
    enabled: bool,
    scale: f32,
    font: HFONT,
    title: HFONT,
    error: Option<String>,
}
thread_local! {
    static DATA: RefCell<Option<Data>> = const { RefCell::new(None) };
    static REQUEST: RefCell<Option<Request>> = const { RefCell::new(None) };
}
pub struct Window {
    pub hwnd: HWND,
}
const CLEAR: usize = 200;
const FIRST: usize = 100;
impl Window {
    pub fn new(instance: HINSTANCE, host: HWND, message: u32, app_icon: HICON, small_icon: HICON) -> Result<Self> {
        unsafe {
            let class = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                hInstance: instance,
                lpfnWndProc: Some(proc),
                lpszClassName: w!("KadrRecent"),
                hCursor: LoadCursorW(None, IDC_ARROW)?,
                hIcon: app_icon,
                hIconSm: small_icon,
                ..Default::default()
            };
            RegisterClassExW(&class);
            let scale = GetDpiForSystem() as f32 / 96.0;
            let width = (1000.0 * scale) as i32;
            let height = (520.0 * scale) as i32;
            let hwnd = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_CONTROLPARENT,
                w!("KadrRecent"),
                w!("Недавние — Kadr"),
                WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU,
                (GetSystemMetrics(SM_CXSCREEN) - width) / 2,
                (GetSystemMetrics(SM_CYSCREEN) - height) / 2,
                width,
                height,
                None,
                None,
                Some(instance),
                None,
            )?;
            let dark = windows::core::BOOL::from(Theme::current().surface.r < 0.5);
            let _ = windows::Win32::Graphics::Dwm::DwmSetWindowAttribute(
                hwnd,
                windows::Win32::Graphics::Dwm::DWMWA_USE_IMMERSIVE_DARK_MODE,
                &dark as *const _ as *const _,
                size_of_val(&dark) as u32,
            );
            let font = make_font(13.0, scale, 400);
            let title = make_font(24.0, scale, 600);
            DATA.with(|d| {
                *d.borrow_mut() = Some(Data {
                    hwnd,
                    host,
                    message,
                    entries: Vec::new(),
                    buttons: Vec::new(),
                    loading: true,
                    enabled: true,
                    scale,
                    font,
                    title,
                    error: None,
                })
            });
            Ok(Self { hwnd })
        }
    }
    pub fn show(&self, enabled: bool) {
        DATA.with(|d| {
            if let Some(d) = d.borrow_mut().as_mut() {
                d.loading = true;
                d.enabled = enabled;
                d.error = None;
            }
        });
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_SHOW);
            let _ = SetForegroundWindow(self.hwnd);
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }
    pub fn set_entries(&self, entries: Vec<Entry>) {
        let old = DATA.with(|d| {
            let mut state = d.borrow_mut();
            let d = state.as_mut().unwrap();
            d.entries = entries;
            d.loading = false;
            d.error = None;
            std::mem::take(&mut d.buttons)
        });
        unsafe {
            for button in old {
                let _ = DestroyWindow(button);
            }
        }
        let (scale, labels, font) = DATA.with(|d| {
            let mut state = d.borrow_mut();
            let d = state.as_mut().unwrap();
            let scale = unsafe { GetDpiForWindow(self.hwnd) } as f32 / 96.0;
            if scale != d.scale {
                unsafe {
                    let _ = DeleteObject(d.font.into());
                    let _ = DeleteObject(d.title.into());
                    d.font = make_font(13.0, scale, 400);
                    d.title = make_font(24.0, scale, 600);
                }
                d.scale = scale;
            }
            (
                d.scale,
                d.entries.iter().map(|e| format!("Восстановить снимок {}", stamp(e.created_ms))).collect::<Vec<_>>(),
                d.font,
            )
        });
        let mut client = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut client);
        }
        let at = |n: f32| (n * scale).round() as i32;
        let rows = labels.len().div_ceil(5).max(1) as f32;
        let desired = at(96.0 + rows * 156.0 + 56.0);
        let mut outer = RECT::default();
        unsafe {
            let _ = GetWindowRect(self.hwnd, &mut outer);
        }
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                None,
                0,
                0,
                outer.right - outer.left,
                desired + (outer.bottom - outer.top - client.bottom),
                SWP_NOMOVE | SWP_NOZORDER,
            );
            let _ = GetClientRect(self.hwnd, &mut client);
        }
        let margin = at(24.0);
        let gap = at(16.0);
        let cell = (client.right - 2 * margin - 4 * gap) / 5;
        let h = at(140.0);
        let mut buttons = Vec::new();
        for (i, label) in labels.iter().enumerate() {
            let text = wide(label);
            let result = unsafe {
                CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    w!("BUTTON"),
                    PCWSTR(text.as_ptr()),
                    WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(BS_OWNERDRAW as u32),
                    margin + (i as i32 % 5) * (cell + gap),
                    at(96.0) + (i as i32 / 5) * (h + gap),
                    cell,
                    h,
                    Some(self.hwnd),
                    Some(HMENU((FIRST + i) as *mut _)),
                    None,
                    None,
                )
            };
            if let Ok(button) = result {
                buttons.push(button);
            }
        }
        let clear = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("BUTTON"),
                w!("Очистить"),
                WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(BS_OWNERDRAW as u32),
                client.right - at(132.0),
                at(25.0),
                at(108.0),
                at(36.0),
                Some(self.hwnd),
                Some(HMENU(CLEAR as *mut _)),
                None,
                None,
            )
        };
        if let Ok(button) = clear {
            unsafe {
                let _ = SendMessageW(button, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(0)));
                let _ = windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow(button, !labels.is_empty());
            }
            buttons.push(button);
        }
        let focus = buttons.first().copied();
        DATA.with(|d| d.borrow_mut().as_mut().unwrap().buttons = buttons);
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
            if IsWindowVisible(self.hwnd).as_bool()
                && let Some(button) = focus
            {
                let _ = SetFocus(Some(button));
            }
        }
    }
    pub fn hide(&self) {
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
    }
    pub fn error(&self, message: &str) {
        DATA.with(|d| {
            if let Some(d) = d.borrow_mut().as_mut() {
                d.loading = false;
                d.error = Some(message.to_string());
            }
        });
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }
    pub fn take_request(&self) -> Option<Request> {
        REQUEST.with(|r| r.borrow_mut().take())
    }
}
impl Drop for Window {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
        if let Some(d) = DATA.with(|d| d.borrow_mut().take()) {
            unsafe {
                let _ = DeleteObject(d.font.into());
                let _ = DeleteObject(d.title.into());
            }
        }
    }
}
pub fn dialog_message(msg: &MSG) -> bool {
    let hwnd = DATA.with(|d| d.borrow().as_ref().map(|d| d.hwnd));
    hwnd.is_some_and(|hwnd| unsafe { IsWindowVisible(hwnd).as_bool() && IsDialogMessageW(hwnd, msg).as_bool() })
}
fn request(value: Request) {
    REQUEST.with(|r| *r.borrow_mut() = Some(value));
    let target = DATA.with(|d| d.borrow().as_ref().map(|d| (d.host, d.message)));
    if let Some((host, msg)) = target {
        unsafe {
            let _ = PostMessageW(Some(host), msg, WPARAM(0), LPARAM(0));
        }
    }
}
unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_ERASEBKGND => return LRESULT(1),
        WM_PAINT => {
            let mut paint = PAINTSTRUCT::default();
            let dc = unsafe { BeginPaint(hwnd, &mut paint) };
            DATA.with(|data| {
                if let Ok(data) = data.try_borrow()
                    && let Some(data) = data.as_ref()
                {
                    draw(dc, data);
                }
            });
            unsafe {
                let _ = EndPaint(hwnd, &paint);
            }
            return LRESULT(0);
        }
        WM_DRAWITEM => {
            let item = unsafe { &*(lp.0 as *const DRAWITEMSTRUCT) };
            DATA.with(|data| {
                if let Ok(data) = data.try_borrow()
                    && let Some(data) = data.as_ref()
                {
                    draw_button(item, data);
                }
            });
            return LRESULT(1);
        }
        WM_COMMAND => {
            if wp.0 >> 16 != 0 {
                return LRESULT(0);
            }
            let mut id = wp.0 & 0xffff;
            if id == 2 {
                unsafe {
                    let _ = ShowWindow(hwnd, SW_HIDE);
                }
                request(Request::Close);
                return LRESULT(0);
            }
            if id == 1 {
                id = unsafe { GetDlgCtrlID(GetFocus()) } as usize;
            }
            if id == CLEAR {
                let any = DATA.with(|d| d.borrow().as_ref().is_some_and(|d| !d.entries.is_empty()));
                if any
                    && unsafe {
                        MessageBoxW(
                            Some(hwnd),
                            w!("Удалить все несохранённые снимки из «Недавних»?"),
                            w!("Очистить недавние"),
                            MB_YESNO | MB_DEFBUTTON2 | MB_ICONQUESTION,
                        )
                    } == IDYES
                {
                    request(Request::Clear);
                }
            } else if (FIRST..FIRST + 10).contains(&id) {
                let id =
                    DATA.with(|d| d.borrow().as_ref().and_then(|d| d.entries.get(id - FIRST).map(|e| e.id.clone())));
                if let Some(id) = id {
                    request(Request::Load(id));
                }
            }
            return LRESULT(0);
        }
        WM_CLOSE => {
            unsafe {
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
            request(Request::Close);
            return LRESULT(0);
        }
        WM_KEYDOWN if wp.0 == 27 => {
            unsafe {
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
            request(Request::Close);
            return LRESULT(0);
        }
        WM_DPICHANGED => {
            let r = unsafe { &*(lp.0 as *const RECT) };
            unsafe {
                let _ = SetWindowPos(hwnd, None, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER);
            }
            request(Request::Refresh);
        }
        _ => {}
    }
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}
fn color(c: windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F) -> COLORREF {
    COLORREF((c.r * 255.0) as u32 | (((c.g * 255.0) as u32) << 8) | (((c.b * 255.0) as u32) << 16))
}
fn fill(dc: HDC, r: RECT, c: COLORREF) {
    unsafe {
        let brush = CreateSolidBrush(c);
        FillRect(dc, &r, brush);
        let _ = DeleteObject(brush.into());
    }
}
fn text(dc: HDC, r: RECT, value: &str, font: HFONT, c: COLORREF) {
    let mut chars: Vec<u16> = value.encode_utf16().collect();
    let mut r = r;
    unsafe {
        let old = SelectObject(dc, font.into());
        SetTextColor(dc, c);
        SetBkMode(dc, TRANSPARENT);
        DrawTextW(dc, &mut chars, &mut r, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX);
        SelectObject(dc, old);
    }
}
fn draw(dc: HDC, d: &Data) {
    let theme = Theme::current();
    let at = |n: f32| (n * d.scale).round() as i32;
    let mut r = RECT::default();
    unsafe {
        let _ = GetClientRect(d.hwnd, &mut r);
    }
    fill(dc, r, color(theme.surface));
    text(
        dc,
        RECT { left: at(24.0), top: at(18.0), right: r.right - at(150.0), bottom: at(54.0) },
        "Недавние",
        d.title,
        color(theme.text),
    );
    let subtitle = if d.loading {
        "Загружаем снимки…".into()
    } else {
        format!("Kadr · Несохранённых снимков: {}", d.entries.len())
    };
    text(
        dc,
        RECT { left: at(24.0), top: at(57.0), right: r.right - at(24.0), bottom: at(80.0) },
        &subtitle,
        d.font,
        color(theme.text_muted),
    );
    if d.entries.is_empty() && !d.loading {
        text(
            dc,
            RECT { left: at(24.0), top: at(155.0), right: r.right - at(24.0), bottom: at(185.0) },
            "Несохранённых снимков пока нет",
            d.title,
            color(theme.text),
        );
        text(
            dc,
            RECT { left: at(24.0), top: at(200.0), right: r.right - at(24.0), bottom: at(230.0) },
            "После захвата снимок останется здесь, пока вы не сохраните его в файл.",
            d.font,
            color(theme.text_muted),
        );
    }
    let hint = if let Some(error) = &d.error {
        error.as_str()
    } else if d.enabled {
        "Выберите снимок, чтобы продолжить · Tab — выбор · Esc — закрыть"
    } else {
        "Автосохранение выключено в настройках · прежние снимки доступны"
    };
    text(
        dc,
        RECT { left: at(24.0), top: r.bottom - at(42.0), right: r.right - at(24.0), bottom: r.bottom - at(16.0) },
        hint,
        d.font,
        color(theme.text_muted),
    );
}
fn draw_button(item: &DRAWITEMSTRUCT, d: &Data) {
    let t = Theme::current();
    let r = item.rcItem;
    let at = |n: f32| (n * d.scale).round() as i32;
    let focused = item.itemState.0 & (ODS_FOCUS.0 | ODS_SELECTED.0) != 0;
    fill(item.hDC, r, color(if focused { t.text } else { t.border }));
    let inset = if focused { at(2.0) } else { at(1.0) };
    let inner = RECT { left: r.left + inset, top: r.top + inset, right: r.right - inset, bottom: r.bottom - inset };
    fill(item.hDC, inner, color(t.surface));
    if item.CtlID as usize == CLEAR {
        text(item.hDC, RECT { left: inner.left + at(16.0), ..inner }, "Очистить", d.font, color(t.text));
        return;
    }
    let Some(entry) = d.entries.get(item.CtlID as usize - FIRST) else { return };
    let pic = RECT {
        left: inner.left + at(8.0),
        top: inner.top + at(8.0),
        right: inner.right - at(8.0),
        bottom: inner.bottom - at(36.0),
    };
    if let Some(image) = &entry.preview {
        let scale = ((pic.right - pic.left) as f64 / image.width() as f64)
            .min((pic.bottom - pic.top) as f64 / image.height() as f64);
        let w = (image.width() as f64 * scale) as i32;
        let h = (image.height() as f64 * scale) as i32;
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: image.width() as i32,
                biHeight: -(image.height() as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        unsafe {
            SetStretchBltMode(item.hDC, HALFTONE);
            StretchDIBits(
                item.hDC,
                pic.left + (pic.right - pic.left - w) / 2,
                pic.top + (pic.bottom - pic.top - h) / 2,
                w,
                h,
                0,
                0,
                image.width() as i32,
                image.height() as i32,
                Some(image.bgra.as_ptr() as *const _),
                &info,
                DIB_RGB_COLORS,
                SRCCOPY,
            );
        }
    } else {
        text(item.hDC, pic, "Снимок", d.font, color(t.text_muted));
    }
    text(
        item.hDC,
        RECT {
            left: inner.left + at(10.0),
            top: inner.bottom - at(31.0),
            right: inner.right - at(8.0),
            bottom: inner.bottom - at(7.0),
        },
        &stamp(entry.created_ms),
        d.font,
        color(t.text),
    );
}
fn stamp(ms: u64) -> String {
    let ticks = ms.saturating_mul(10000).saturating_add(116444736000000000);
    let ft = FILETIME { dwLowDateTime: ticks as u32, dwHighDateTime: (ticks >> 32) as u32 };
    let mut utc = SYSTEMTIME::default();
    let mut local = SYSTEMTIME::default();
    unsafe {
        if FileTimeToSystemTime(&ft, &mut utc).is_ok()
            && SystemTimeToTzSpecificLocalTime(None, &utc, &mut local).is_ok()
        {
            return format!("{:02}.{:02} · {:02}:{:02}", local.wDay, local.wMonth, local.wHour, local.wMinute);
        }
    }
    "Время неизвестно".into()
}
unsafe fn make_font(size: f32, scale: f32, weight: i32) -> HFONT {
    unsafe {
        CreateFontW(
            -(size * scale).round() as i32,
            0,
            0,
            0,
            weight,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            0,
            w!("Segoe UI"),
        )
    }
}
