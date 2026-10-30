//! Kadr's Win+V: the clipboard history is fed from every clipboard change, opened by Win+V (caught
//! by a keyboard hook, since Windows keeps that combination for itself) and pastes into the window
//! that was in front.
use super::*;
use crate::clip_history::{History, Raw};
use kadr_store::clips::Data;
use std::sync::atomic::AtomicBool;
use windows::Win32::System::DataExchange::{
    AddClipboardFormatListener, GetClipboardOwner, RemoveClipboardFormatListener,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput,
    VK_LWIN, VK_RWIN,
};

/// Win+V's V went down and was kept from Windows: its key-up is kept too.
static SWALLOWED: AtomicBool = AtomicBool::new(false);

impl App {
    /// Starts or stops the history to match the settings.
    pub(super) fn sync_clipboard(&mut self) {
        let on = self.config.clipboard.enabled && !self.bench_mode;
        match (on, self.clips.is_some()) {
            (true, false) => {
                self.clips = Some(History::start(self.hwnd, WM_APP_CLIPS, self.clip_limits()));
                unsafe {
                    if let Err(e) = AddClipboardFormatListener(self.hwnd) {
                        log_warn!("история буфера: не слежу за буфером: {e}");
                    }
                    match SetWindowsHookExW(WH_KEYBOARD_LL, Some(win_v_hook), Some(self.instance), 0) {
                        Ok(hook) => self.clip_hook = Some(hook),
                        Err(e) => log_warn!("история буфера: Win+V не перехвачен: {e}"),
                    }
                }
                log_info!("история буфера включена (Win+V)");
            }
            (false, true) => {
                self.clips = None;
                unsafe {
                    let _ = RemoveClipboardFormatListener(self.hwnd);
                    if let Some(hook) = self.clip_hook.take() {
                        let _ = UnhookWindowsHookEx(hook);
                    }
                }
                log_info!("история буфера выключена");
            }
            (true, true) => {
                if let Some(h) = &self.clips {
                    h.set_limits(self.clip_limits());
                }
            }
            (false, false) => {}
        }
    }

    fn clip_limits(&self) -> kadr_store::clips::Limits {
        kadr_store::clips::Limits { entries: self.config.clipboard.max, images: self.config.clipboard.images }
    }

    /// WM_CLIPBOARDUPDATE: something was copied somewhere.
    pub(super) fn on_clipboard_update(&mut self) {
        let Some(history) = &self.clips else { return };
        // Kadr's own copies are recorded where they are made (its pictures are rendered late, and
        // a paste from the history must not come back as a new entry).
        if unsafe { GetClipboardOwner() }.is_ok_and(|owner| owner == self.hwnd) {
            return;
        }
        if let Some(raw) = crate::clip_history::read(self.hwnd) {
            history.add(raw);
        }
    }

    /// Kadr copied this itself.
    pub(super) fn remember_copy(&self, raw: Raw) {
        if let Some(history) = &self.clips {
            history.add(raw);
        }
    }

    /// The list changed on the worker.
    pub(super) fn on_clips_changed(&mut self) {
        if let Some(history) = &self.clips
            && crate::clip_window::is_open()
        {
            crate::clip_window::update(history.entries());
        }
    }

    /// Win+V (or the tray): open the history by the caret, or by the pointer without one.
    pub(super) fn show_clipboard(&mut self) {
        let Some(history) = &self.clips else { return };
        if crate::clip_window::is_open() {
            crate::clip_window::close();
            return;
        }
        let entries = history.entries();
        let front = unsafe { GetForegroundWindow() };
        self.clip_target = (!front.is_invalid() && front != self.hwnd).then_some(front);
        let at = caret(front).unwrap_or_else(cursor_pos);
        crate::clip_window::show(self.instance, self.hwnd, WM_APP_CLIPWIN, entries, at);
    }

    /// What the history window asked for.
    pub(super) fn on_clip_window(&mut self) {
        for request in crate::clip_window::take_requests() {
            let Some(history) = &self.clips else { continue };
            match request {
                crate::clip_window::Request::Paste(id) => self.paste_clip(id),
                crate::clip_window::Request::Pin(id, on) => history.pin(id, on),
                crate::clip_window::Request::Remove(id) => history.remove(id),
                crate::clip_window::Request::Clear => history.clear(),
                crate::clip_window::Request::Recent => self.show_recent(),
            }
        }
    }

    /// Back onto the clipboard, then Ctrl+V into the window that was in front.
    fn paste_clip(&mut self, id: u64) {
        let Some(history) = &self.clips else { return };
        let Some(data) = history.data(id) else { return };
        history.touch(id);
        let put = match data {
            Data::Text(t) => clipboard::put_text(self.hwnd, &t),
            Data::Files(f) => clipboard::put_files(self.hwnd, &f),
            Data::Image { width, height, png } => {
                match kadr_doc::pixels::decode_png(&png, kadr_doc::Rect::new(0, 0, width as i32, height as i32)) {
                    Ok(image) => {
                        let image = Arc::new(image);
                        let put = clipboard::put_image(self.hwnd, &image);
                        if put.is_ok() {
                            self.clipboard = Some(image);
                        }
                        put
                    }
                    Err(e) => {
                        log_warn!("история буфера: картинка не читается: {e}");
                        return;
                    }
                }
            }
        };
        if let Err(e) = put {
            log_warn!("история буфера: в буфер не положилось: {e}");
            return;
        }
        let Some(target) = self.clip_target.take() else { return };
        unsafe {
            let _ = SetForegroundWindow(target);
        }
        // A moment for the window to take focus back before the keys arrive.
        std::thread::spawn(|| {
            std::thread::sleep(std::time::Duration::from_millis(60));
            press_ctrl_v();
        });
    }
}

/// The caret of the window in front, in screen pixels, when it has one.
fn caret(front: HWND) -> Option<POINT> {
    let thread = unsafe { GetWindowThreadProcessId(front, None) };
    let mut info = GUITHREADINFO { cbSize: size_of::<GUITHREADINFO>() as u32, ..Default::default() };
    unsafe { GetGUIThreadInfo(thread, &mut info) }.ok()?;
    if info.hwndCaret.is_invalid() || (info.rcCaret.right == 0 && info.rcCaret.bottom == 0) {
        return None;
    }
    let mut p = POINT { x: info.rcCaret.left, y: info.rcCaret.bottom };
    unsafe { windows::Win32::Graphics::Gdi::ClientToScreen(info.hwndCaret, &mut p) }.ok().ok()?;
    Some(p)
}

fn key(vk: u16, up: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk),
                dwFlags: if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) },
                ..Default::default()
            },
        },
    }
}

fn press_ctrl_v() {
    let keys = [key(VK_CONTROL.0, false), key(0x56, false), key(0x56, true), key(VK_CONTROL.0, true)];
    unsafe {
        SendInput(&keys, size_of::<INPUT>() as i32);
    }
}

/// Win+V belongs to Windows: the V is kept from it here, and an unassigned key is pressed so that
/// letting go of Win does not open Start.
unsafe extern "system" fn win_v_hook(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let info = unsafe { &*(lp.0 as *const KBDLLHOOKSTRUCT) };
        let injected = info.flags.0 & LLKHF_INJECTED.0 != 0;
        if info.vkCode == 0x56 && !injected {
            let down = matches!(wp.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
            let held = |vk: VIRTUAL_KEY| unsafe { GetAsyncKeyState(vk.0 as i32) } < 0;
            if down {
                let win = held(VK_LWIN) || held(VK_RWIN);
                if win && !held(VK_CONTROL) && !held(VK_MENU) && !held(VK_SHIFT) {
                    // Held down, V repeats: open once.
                    if !SWALLOWED.swap(true, Relaxed) {
                        unsafe {
                            SendInput(&[key(0xE8, false), key(0xE8, true)], size_of::<INPUT>() as i32);
                            let host = HWND(HOST.load(Relaxed) as *mut _);
                            let _ = PostMessageW(Some(host), WM_APP_WINV, WPARAM(0), LPARAM(0));
                        }
                    }
                    return LRESULT(1);
                }
            } else if SWALLOWED.swap(false, Relaxed) {
                return LRESULT(1);
            }
        }
    }
    unsafe { CallNextHookEx(None, code, wp, lp) }
}
