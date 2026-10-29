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
    returning: bool,
}
thread_local! {
    static DATA: RefCell<Option<Data>> = const { RefCell::new(None) };
    static REQUEST: RefCell<Option<Request>> = const { RefCell::new(None) };
}
pub struct Window {
    pub hwnd: HWND,
}
const CLEAR: usize = 200;
const BACK: usize = 201;
const FIRST: usize = 100;
fn columns(r: RECT, scale: f32) -> usize {
    (((r.right - r.left) as f32 / scale - 32.0) / 224.0)
        .floor()
        .clamp(2.0, 4.0) as usize
}
fn card_rects(r: RECT, scale: f32, count: usize) -> Vec<RECT> {
    let at = |v: f32| (v * scale).round() as i32;
    let cols = columns(r, scale);
    let rows = count.div_ceil(cols).max(1) as i32;
    let margin = at(24.0);
    let gap = at(16.0);
    let width = (r.right - r.left - 2 * margin - (cols as i32 - 1) * gap) / cols as i32;
    let height = ((r.bottom - at(96.0) - at(56.0) - (rows - 1) * gap) / rows).min(at(180.0));
    (0..count)
        .map(|i| {
            let left = margin + (i % cols) as i32 * (width + gap);
            let top = at(96.0) + (i / cols) as i32 * (height + gap);
            RECT {
                left,
                top,
                right: left + width,
                bottom: top + height,
            }
        })
        .collect()
}
fn count_label(n: usize) -> String {
    let word = if (11..=14).contains(&(n % 100)) {
        "снимков"
    } else {
        match n % 10 {
            1 => "снимок",
            2..=4 => "снимка",
            _ => "снимков",
        }
    };
    format!("{n} {word}")
}
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
            let mut work = RECT {
                left: 0,
                top: 0,
                right: GetSystemMetrics(SM_CXSCREEN),
                bottom: GetSystemMetrics(SM_CYSCREEN),
            };
            let _ = SystemParametersInfoW(
                SPI_GETWORKAREA,
                0,
                Some(&mut work as *mut _ as *mut _),
                SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
            );
            let width = ((1000.0 * scale) as i32).min(work.right - work.left - 32);
            let height = ((520.0 * scale) as i32).min(work.bottom - work.top - 32);
            let hwnd = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_CONTROLPARENT,
                w!("KadrRecent"),
                w!("Недавние — Kadr"),
                WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU,
                work.left + (work.right - work.left - width) / 2,
                work.top + (work.bottom - work.top - height) / 2,
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
            let font = make_font(14.0, scale, 400);
            let title = make_font(26.0, scale, 600);
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
                    returning: false,
                })
            });
            Ok(Self { hwnd })
        }
    }
    pub fn show(&self, enabled: bool, returning: bool) {
        DATA.with(|d| {
            if let Some(d) = d.borrow_mut().as_mut() {
                d.loading = true;
                d.enabled = enabled;
                d.error = None;
                d.returning = returning;
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
                    d.font = make_font(14.0, scale, 400);
                    d.title = make_font(26.0, scale, 600);
                }
                d.scale = scale;
            }
            (
                d.scale,
                d.entries
                    .iter()
                    .map(|e| format!("Восстановить снимок {}", stamp(e.created_ms)))
                    .collect::<Vec<_>>(),
                d.font,
            )
        });
        let mut client = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut client);
        }
        let at = |n: f32| (n * scale).round() as i32;
        let rows = labels.len().div_ceil(columns(client, scale)).max(1) as f32;
        let desired = at(96.0 + rows * 196.0 + 56.0);
        let monitor = unsafe { MonitorFromWindow(self.hwnd, MONITOR_DEFAULTTONEAREST) };
        let mut mi = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        unsafe {
            let _ = GetMonitorInfoW(monitor, &mut mi);
        }
        let desired = desired.min((mi.rcWork.bottom - mi.rcWork.top - at(64.0)).max(at(300.0)));
        let mut outer = RECT::default();
        unsafe {
            let _ = GetWindowRect(self.hwnd, &mut outer);
        }
        unsafe {
            let width = outer.right - outer.left;
            let height = desired + (outer.bottom - outer.top - client.bottom);
            let _ = SetWindowPos(
                self.hwnd,
                None,
                outer
                    .left
                    .clamp(mi.rcWork.left, (mi.rcWork.right - width).max(mi.rcWork.left)),
                outer
                    .top
                    .clamp(mi.rcWork.top, (mi.rcWork.bottom - height).max(mi.rcWork.top)),
                width,
                height,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            let _ = GetClientRect(self.hwnd, &mut client);
        }
        let mut buttons = Vec::new();
        for (i, (label, r)) in labels.iter().zip(card_rects(client, scale, labels.len())).enumerate() {
            let text = wide(label);
            let result = unsafe {
                CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    w!("BUTTON"),
                    PCWSTR(text.as_ptr()),
                    WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(BS_OWNERDRAW as u32),
                    r.left,
                    r.top,
                    r.right - r.left,
                    r.bottom - r.top,
                    Some(self.hwnd),
                    Some(HMENU((FIRST + i) as *mut _)),
                    None,
                    None,
                )
            };
            if let Ok(button) = result {
                crate::chrome::track(button);
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
            crate::chrome::track(button);
            buttons.push(button);
        }
        let returning = DATA.with(|d| d.borrow().as_ref().is_some_and(|d| d.returning));
        let back = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("BUTTON"),
                if returning {
                    w!("К снимку")
                } else {
                    w!("Закрыть")
                },
                WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(BS_OWNERDRAW as u32),
                client.right - at(264.0),
                at(25.0),
                at(120.0),
                at(36.0),
                Some(self.hwnd),
                Some(HMENU(BACK as *mut _)),
                None,
                None,
            )
        };
        let first = buttons.first().copied().filter(|_| !labels.is_empty());
        if let Ok(back) = back {
            crate::chrome::track(back);
            buttons.push(back);
        }
        let focus = first.or_else(|| buttons.last().copied());
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
    hwnd.is_some_and(|hwnd| unsafe {
        GetAncestor(msg.hwnd, GA_ROOT) == hwnd
            && IsWindowVisible(hwnd).as_bool()
            && IsDialogMessageW(hwnd, msg).as_bool()
    })
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
            if id == 2 || id == BACK {
                unsafe {
                    let _ = ShowWindow(hwnd, SW_HIDE);
                }
                request(Request::Close);
                return LRESULT(0);
            }
            if id == 1 {
                id = unsafe { GetDlgCtrlID(GetFocus()) } as usize;
            }
            if id == BACK {
                unsafe {
                    let _ = ShowWindow(hwnd, SW_HIDE);
                }
                request(Request::Close);
                return LRESULT(0);
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
                let id = DATA.with(|d| {
                    d.borrow()
                        .as_ref()
                        .and_then(|d| d.entries.get(id - FIRST).map(|e| e.id.clone()))
                });
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
                let _ = SetWindowPos(
                    hwnd,
                    None,
                    r.left,
                    r.top,
                    r.right - r.left,
                    r.bottom - r.top,
                    SWP_NOZORDER,
                );
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
        DrawTextW(
            dc,
            &mut chars,
            &mut r,
            DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX,
        );
        SelectObject(dc, old);
    }
}
fn draw(dc: HDC, d: &Data) {
    let mut r = RECT::default();
    unsafe {
        let _ = GetClientRect(d.hwnd, &mut r);
    }
    draw_at(dc, r, d);
}
fn draw_at(dc: HDC, r: RECT, d: &Data) {
    let theme = Theme::current();
    let at = |n: f32| (n * d.scale).round() as i32;
    fill(dc, r, color(theme.surface));
    text(
        dc,
        RECT {
            left: at(24.0),
            top: at(18.0),
            right: r.right - at(150.0),
            bottom: at(54.0),
        },
        "Ваши снимки",
        d.title,
        color(theme.text),
    );
    let subtitle = if d.loading {
        "Загружаем снимки…".into()
    } else if d.error.is_some() {
        "Хранилище снимков недоступно".into()
    } else if d.entries.is_empty() {
        "Несохранённые снимки".into()
    } else {
        format!("{} · можно продолжить редактирование", count_label(d.entries.len()))
    };
    text(
        dc,
        RECT {
            left: at(24.0),
            top: at(57.0),
            right: r.right - at(24.0),
            bottom: at(80.0),
        },
        &subtitle,
        d.font,
        color(theme.text_muted),
    );
    if d.entries.is_empty() && !d.loading {
        text(
            dc,
            RECT {
                left: at(24.0),
                top: at(155.0),
                right: r.right - at(24.0),
                bottom: at(185.0),
            },
            if d.error.is_some() {
                "Не удалось открыть снимки"
            } else {
                "Здесь будут ваши снимки"
            },
            d.title,
            color(theme.text),
        );
        text(
            dc,
            RECT {
                left: at(24.0),
                top: at(200.0),
                right: r.right - at(24.0),
                bottom: at(230.0),
            },
            d.error
                .as_deref()
                .unwrap_or("Сделайте скриншот — к нему можно будет вернуться здесь."),
            d.font,
            color(theme.text_muted),
        );
    }
    let hint = if d.error.is_some() {
        "Закройте это окно и откройте «Недавние» ещё раз"
    } else if d.enabled {
        if d.returning {
            "Esc — вернуться к текущему снимку"
        } else {
            "Нажмите на снимок, чтобы продолжить редактирование"
        }
    } else {
        "Автосохранение выключено в настройках · прежние снимки доступны"
    };
    text(
        dc,
        RECT {
            left: at(24.0),
            top: r.bottom - at(42.0),
            right: r.right - at(24.0),
            bottom: r.bottom - at(16.0),
        },
        hint,
        d.font,
        color(theme.text_muted),
    );
}

pub fn preview(
    entries: Vec<Entry>,
    scale: f32,
    returning: bool,
    loading: bool,
    error: Option<String>,
) -> Result<kadr_doc::Image> {
    let at = |v: f32| (v * scale).round() as i32;
    let rows = entries.len().div_ceil(4).max(1);
    let h = at((96.0 + rows as f32 * 196.0 + 56.0).min(680.0));
    let font = unsafe { make_font(14.0, scale, 400) };
    let title = unsafe { make_font(26.0, scale, 600) };
    let d = Data {
        hwnd: HWND::default(),
        host: HWND::default(),
        message: 0,
        entries,
        buttons: Vec::new(),
        loading,
        enabled: true,
        scale,
        font,
        title,
        error,
        returning,
    };
    let result = crate::chrome::snapshot(at(1000.0), h, |dc, r| {
        draw_at(dc, r, &d);
        for (i, card) in card_rects(r, scale, d.entries.len()).into_iter().enumerate() {
            let item = DRAWITEMSTRUCT {
                CtlID: (FIRST + i) as u32,
                hDC: dc,
                rcItem: card,
                itemState: if i == 0 { ODS_FOCUS } else { Default::default() },
                ..Default::default()
            };
            draw_button(&item, &d);
        }
        for (id, left, width) in [
            (BACK, r.right - at(264.0), at(120.0)),
            (CLEAR, r.right - at(132.0), at(108.0)),
        ] {
            let item = DRAWITEMSTRUCT {
                CtlID: id as u32,
                hDC: dc,
                itemState: if id == CLEAR && d.entries.is_empty() {
                    windows::Win32::UI::Controls::ODS_DISABLED
                } else {
                    Default::default()
                },
                rcItem: RECT {
                    left,
                    top: at(25.0),
                    right: left + width,
                    bottom: at(61.0),
                },
                ..Default::default()
            };
            draw_button(&item, &d);
        }
    });
    unsafe {
        let _ = DeleteObject(font.into());
        let _ = DeleteObject(title.into());
    }
    result
}
fn draw_button(item: &DRAWITEMSTRUCT, d: &Data) {
    let t = Theme::current();
    let r = item.rcItem;
    let at = |n: f32| (n * d.scale).round() as i32;
    let focused = item.itemState.0 & ODS_FOCUS.0 != 0;
    fill(item.hDC, r, color(t.surface));
    let is_action = item.CtlID as usize == CLEAR || item.CtlID as usize == BACK;
    let hover = crate::chrome::hovered(item.hwndItem);
    let bg = if item.itemState.0 & ODS_SELECTED.0 != 0 {
        t.selected
    } else if focused || hover {
        t.control
    } else {
        t.surface
    };
    if !is_action {
        crate::chrome::round(
            item.hDC,
            r,
            color(bg),
            color(if focused { t.accent } else { bg }),
            at(12.0),
            if focused { at(2.0) } else { 1 },
        );
    }
    let inner = RECT {
        left: r.left + at(3.0),
        top: r.top + at(3.0),
        right: r.right - at(3.0),
        bottom: r.bottom - at(3.0),
    };
    if item.CtlID as usize == CLEAR || item.CtlID as usize == BACK {
        let label = if item.CtlID as usize == CLEAR {
            "Очистить"
        } else if d.returning {
            "К снимку"
        } else {
            "Закрыть"
        };
        let fg = crate::chrome::button(
            item.hDC,
            inner,
            item.CtlID as usize == BACK,
            focused,
            item.itemState.0 & ODS_SELECTED.0 != 0,
            hover,
            d.scale,
        );
        text(
            item.hDC,
            RECT {
                left: inner.left + at(16.0),
                ..inner
            },
            label,
            d.font,
            if item.itemState.0 & windows::Win32::UI::Controls::ODS_DISABLED.0 != 0 {
                color(t.disabled)
            } else {
                fg
            },
        );
        return;
    }
    let Some(entry) = d.entries.get(item.CtlID as usize - FIRST) else {
        return;
    };
    let pic = RECT {
        left: inner.left + at(8.0),
        top: inner.top + at(8.0),
        right: inner.right - at(8.0),
        bottom: inner.bottom - at(42.0),
    };
    crate::chrome::round(item.hDC, pic, color(t.control), color(t.control), at(8.0), 1);
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
    let ft = FILETIME {
        dwLowDateTime: ticks as u32,
        dwHighDateTime: (ticks >> 32) as u32,
    };
    let mut utc = SYSTEMTIME::default();
    let mut local = SYSTEMTIME::default();
    unsafe {
        if FileTimeToSystemTime(&ft, &mut utc).is_ok()
            && SystemTimeToTzSpecificLocalTime(None, &utc, &mut local).is_ok()
        {
            return format!(
                "{:02}.{:02} · {:02}:{:02}",
                local.wDay, local.wMonth, local.wHour, local.wMinute
            );
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recent_grid_fits_all_ten_previews_at_large_dpi() {
        for s in [1.0, 1.5, 2.0] {
            let r = RECT {
                left: 0,
                top: 0,
                right: (960.0 * s) as i32,
                bottom: (640.0 * s) as i32,
            };
            let cards = card_rects(r, s, 10);
            assert_eq!(cards.len(), 10);
            for c in &cards {
                assert!(c.left >= 0 && c.right <= r.right && c.top >= 96 && c.bottom <= r.bottom - (56.0 * s) as i32);
                assert!(c.bottom - c.top > 100);
            }
        }
        assert_eq!(count_label(1), "1 снимок");
        assert_eq!(count_label(3), "3 снимка");
        assert_eq!(count_label(10), "10 снимков");
    }
    #[test]
    fn recent_has_an_accessible_return_button_without_showing_the_window() {
        unsafe {
            let before = GetForegroundWindow();
            let instance: HINSTANCE = windows::Win32::System::LibraryLoader::GetModuleHandleW(None)
                .unwrap()
                .into();
            let window = Window::new(
                instance,
                HWND::default(),
                WM_APP + 90,
                HICON::default(),
                HICON::default(),
            )
            .unwrap();
            DATA.with(|d| d.borrow_mut().as_mut().unwrap().returning = true);
            window.set_entries(Vec::new());
            let button = GetDlgItem(Some(window.hwnd), BACK as i32).unwrap();
            let mut name = [0u16; 64];
            let len = GetWindowTextW(button, &mut name);
            assert_eq!(String::from_utf16_lossy(&name[..len as usize]), "К снимку");
            assert!(!IsWindowVisible(window.hwnd).as_bool());
            assert_eq!(GetForegroundWindow(), before);
            let _ = SendMessageW(window.hwnd, WM_COMMAND, Some(WPARAM(BACK)), Some(LPARAM(0)));
            assert!(matches!(Window::take_request(&window), Some(Request::Close)));
            drop(window);
            assert_eq!(GetForegroundWindow(), before);
        }
    }
}
