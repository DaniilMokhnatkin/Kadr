//! Резидентное приложение: скрытое окно (хоткеи, трей, широковещательные сообщения),
//! сессия захвата и ввод в оверлеях. Всё живёт в UI-потоке, поэтому в состоянии нет мьютексов.

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicIsize, AtomicU32, Ordering::Relaxed};
use std::sync::mpsc::{Receiver, Sender, channel};

use windows::Win32::Foundation::{
    ERROR_ALREADY_EXISTS, GetLastError, HANDLE, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM,
};
use windows::Win32::Graphics::Dwm::DwmFlush;
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemInformation::GetTickCount;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::HiDpi::{
    AreDpiAwarenessContextsEqual, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetThreadDpiAwarenessContext,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN, RegisterHotKey,
    ReleaseCapture, SetCapture, UnregisterHotKey, VIRTUAL_KEY, VK_CONTROL, VK_DOWN, VK_ESCAPE, VK_LEFT, VK_RETURN,
    VK_RIGHT, VK_SHIFT, VK_SNAPSHOT, VK_SPACE, VK_UP,
};
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO, NIIF_RESPECT_QUIET_TIME, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NIN_BALLOONUSERCLICK, NOTIFYICONDATAW, Shell_NotifyIconW, ShellExecuteW,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{Error, PCWSTR, Result, w};

use crate::{capture_mode::Mode, config_watch::Watcher};
use kadr_capture::{self as capture, Output};
use kadr_doc::{Image, Point as DocPoint, Rect as DocRect};
use kadr_editor::drawing::Drawing;
use kadr_editor::overlay::{self, Overlay};
use kadr_editor::session::{Action, CursorKind, Key, Mods, Session};
use kadr_export::{clipboard, file};
use kadr_render::gpu::{Gpu, Shared};
use kadr_render::scene::{Hud, HudRow, Magnifier, Marks, Toolbar, ToolbarButton};
use kadr_render::theme::{Theme, dip};
use kadr_store::{
    config::{self, Config},
    hotkey,
};
use kadr_win::perf::{self, Stats, Timing};
use kadr_win::privileges::{self, ProcessInfo};
use kadr_win::{log, log_info, log_warn, wide};

mod drafts;
mod editor_input;
mod settings;

const HK_PRTSC: i32 = 1;
const HK_BACKUP: i32 = 2;
const HK_ESC: i32 = 3;
const HK_FULLSCREEN: i32 = 4;
const HK_WINDOW: i32 = 5;
const HK_LAST_REGION: i32 = 6;
const HK_RECENT: i32 = 7;

const WM_APP_TRAY: u32 = WM_APP + 1;
const WM_APP_HOOK: u32 = WM_APP + 2;
const WM_APP_BENCH: u32 = WM_APP + 3;
/// Фоновое сохранение закончилось — результат ждёт в канале.
const WM_APP_SAVED: u32 = WM_APP + 4;
const WM_APP_CONFIG: u32 = WM_APP + 5;
const WM_APP_RECENT: u32 = WM_APP + 6;
const WM_APP_RECENT_UI: u32 = WM_APP + 7;
const TIMER_HOOK_CHECK: usize = 1;
const TIMER_DRAFT: usize = 2;

const CMD_CAPTURE: usize = 10;
const CMD_WARM: usize = 11;
const CMD_HOOK: usize = 12;
const CMD_LOG: usize = 13;
const CMD_EXIT: usize = 14;
const CMD_FULLSCREEN: usize = 15;
const CMD_LAST_REGION: usize = 16;
const CMD_CONFIG: usize = 17;
const CMD_RECENT: usize = 18;

const HOST_CLASS: PCWSTR = w!("KadrHost");

/// Скрытое окно — адресат для LL-хука (он вызывается вне нашего кода и знает только HWND).
static HOST: AtomicIsize = AtomicIsize::new(0);
static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

/// Открытая сессия захвата: логика выделения, кадры в памяти и монитор с панелью замеров.
struct Active {
    session: Session,
    /// Копии кадров мониторов в памяти — для лупы и экспорта. Появляются сразу после показа.
    frames: Vec<Arc<Image>>,
    hud_overlay: usize,
    /// Кнопка панели под курсором — для подсветки.
    hovered: Option<ToolbarButton>,
    draft: Option<drafts::DraftState>,
    drawing: Drawing,
    text_surrogate: Option<u16>,
}

pub struct App {
    hwnd: HWND,
    instance: HINSTANCE,
    shared: Shared,
    gpus: Vec<Gpu>,
    outputs: Vec<Output>,
    overlays: Vec<Overlay>,
    theme: Theme,
    process: ProcessInfo,
    warm: bool,
    /// Панель замеров на оверлее (флаг `--perf-hud`).
    perf_hud: bool,
    hook: Option<HHOOK>,
    /// LL-хук увидел PrtSc, ждём WM_HOTKEY: передний план в момент нажатия.
    hook_press: Option<String>,
    active: Option<Active>,
    source: &'static str,
    last: Option<Timing>,
    stats: Stats,
    /// Картинка в буфере обмена: из неё по запросу кодируется PNG (отложенный рендеринг).
    clipboard: Option<Arc<Image>>,
    saved_tx: Sender<Saved>,
    saved_rx: Receiver<Saved>,
    last_saved: Option<PathBuf>,
    config: Config,
    config_path: PathBuf,
    watcher: Watcher,
    pending_config: Option<Config>,
    warm_override: Option<bool>,
    /// Последняя вручную выделенная область; быстрый снимок монитора её не заменяет.
    last_region: Option<DocRect>,
    recent: crate::recent_worker::Worker,
    recent_window: Option<crate::recent_window::Window>,
    loading_recent: bool,
    recovery_notice: bool,
    bench_mode: bool,
    icons: crate::icons::Icons,
}

/// Итог фонового сохранения: путь или путь с текстом ошибки.
struct Saved {
    result: std::result::Result<PathBuf, (PathBuf, String)>,
    draft: Option<crate::recent_worker::Token>,
}

enum Export {
    Copy,
    Save,
    SaveAs,
}

/// `bench` — сделать столько захватов подряд, записать p50/p95 в лог и выйти.
pub fn run(warm_override: Option<bool>, perf_hud: bool, bench: Option<u32>) -> Result<()> {
    // Бенчмарку единственный экземпляр не нужен: он работает рядом с установленным Kadr.
    let _instance_lock = match (bench, single_instance()) {
        (None, None) => {
            log_info!("Kadr уже запущен — выхожу");
            return Ok(());
        }
        (_, lock) => lock,
    };
    unsafe {
        // COM нужен диалогу «Сохранить как» и папкам оболочки.
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let instance: HINSTANCE = GetModuleHandleW(None)?.into();
        let icons = crate::icons::Icons::load(instance)?;
        register_class(instance, HOST_CLASS, host_proc, HCURSOR::default(), &icons)?;
        register_class(
            instance,
            overlay::CLASS,
            overlay_proc,
            LoadCursorW(None, IDC_CROSS)?,
            &icons,
        )?;
        // Обычное скрытое окно, а не message-only: только так приходят WM_DISPLAYCHANGE и TaskbarCreated.
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW,
            HOST_CLASS,
            w!("Kadr"),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance),
            None,
        )?;
        HOST.store(hwnd.0 as isize, Relaxed);
        TASKBAR_CREATED.store(RegisterWindowMessageW(w!("TaskbarCreated")), Relaxed);

        let (saved_tx, saved_rx) = channel();
        let config_path = config::path();
        let (config, config_error) = config::load(&config_path);
        let warm = warm_override.unwrap_or(config.capture.warm);
        let watcher = Watcher::start(config_path.clone(), hwnd, WM_APP_CONFIG);
        let recent = crate::recent_worker::Worker::start(hwnd, WM_APP_RECENT, config.recent.max, bench.is_some());
        let mut app = App {
            hwnd,
            instance,
            shared: Shared::new()?,
            gpus: Vec::new(),
            outputs: Vec::new(),
            overlays: Vec::new(),
            theme: Theme::current(),
            process: privileges::current(),
            warm,
            perf_hud,
            hook: None,
            hook_press: None,
            active: None,
            source: "—",
            last: None,
            stats: Stats::default(),
            clipboard: None,
            saved_tx,
            saved_rx,
            last_saved: None,
            config,
            config_path,
            watcher,
            pending_config: None,
            warm_override,
            last_region: None,
            recent,
            recent_window: None,
            loading_recent: false,
            recovery_notice: false,
            bench_mode: bench.is_some(),
            icons,
        };
        app.build_outputs();
        app.log_startup();
        let conflicts = app.register_hotkeys();
        app.set_hook(true);
        app.add_tray_icon();
        if let Some(error) = config_error {
            log_warn!("конфиг: {error}");
            app.notify("Не удалось загрузить настройки", &error);
        } else if !conflicts.is_empty() {
            app.notify(
                "Некоторые хоткеи заняты",
                &format!(
                    "{}\nОбласть: Ctrl+Shift+F12 или значок Kadr в трее",
                    conflicts.join("; ")
                ),
            );
        }
        APP.with(|cell| *cell.borrow_mut() = Some(app));
        if let Some(count) = bench {
            let _ = PostMessageW(Some(hwnd), WM_APP_BENCH, WPARAM(count as usize), LPARAM(0));
        }

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            if crate::recent_window::dialog_message(&msg) {
                continue;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        APP.with(|cell| cell.borrow_mut().take());
    }
    Ok(())
}

impl App {
    fn build_outputs(&mut self) {
        self.overlays.clear();
        self.outputs.clear();
        self.gpus.clear();
        let found = capture::enumerate().and_then(|(adapters, outputs)| {
            let gpus = adapters
                .iter()
                .map(|a| Gpu::new(&self.shared, &a.adapter, a.name.clone()))
                .collect::<Result<Vec<_>>>()?;
            Ok((gpus, outputs))
        });
        match found {
            Ok((gpus, outputs)) => (self.gpus, self.outputs) = (gpus, outputs),
            Err(e) => {
                log_warn!("DXGI: не удалось перечислить мониторы: {e}");
                return;
            }
        }
        for (i, out) in self.outputs.iter().enumerate() {
            match Overlay::new(
                &self.gpus[out.gpu],
                &self.shared,
                i,
                out.rect,
                out.monitor,
                self.instance,
            ) {
                Ok(overlay) => self.overlays.push(overlay),
                Err(e) => log_warn!("оверлей для {}: {e}", out.name),
            }
        }
        self.warm_up();
    }

    /// Всё, что создаётся при первом использовании (глифы, первый Present, композиция DWM,
    /// дубликатор), создаём сейчас — иначе первый захват после запуска в разы медленнее остальных.
    fn warm_up(&mut self) {
        let t = perf::now();
        let hud = self.hud();
        for overlay in &self.overlays {
            let gpu = &self.gpus[self.outputs[overlay.output].gpu];
            let r = overlay.rect;
            let selection = Some(RECT {
                left: r.left + 64,
                top: r.top + 64,
                right: r.left + 128,
                bottom: r.top + 128,
            });
            let marks = Marks {
                selection,
                handles: true,
                magnifier: None,
                toolbar: Some(Toolbar::default()),
                hud: Some(&hud),
                drawing: None,
            };
            if let Err(e) = overlay.present(gpu, &self.shared, &self.theme, marks) {
                log_warn!("прогрев рендера: {e}");
            }
            overlay.warm_up_composition();
        }
        if self.warm {
            for out in &mut self.outputs {
                if let Err(e) = out.capture(&self.gpus[out.gpu].device, &self.gpus[out.gpu].context) {
                    log_warn!("прогрев дубликатора {}: {e}", out.name);
                }
            }
        }
        log_info!("прогрев: {:.1} мс", perf::ms(t, perf::now()));
    }

    fn log_startup(&self) {
        let p = &self.process;
        let pmv2 = unsafe {
            AreDpiAwarenessContextsEqual(
                GetThreadDpiAwarenessContext(),
                DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
            )
        }
        .as_bool();
        log_info!(
            "Kadr M2 запущен: целостность {}, uiAccess {}, админ {}, PerMonitorV2 {}, дубликатор {}",
            p.integrity,
            yes(p.ui_access),
            yes(p.elevated),
            yes(pmv2),
            self.mode()
        );
        for (i, gpu) in self.gpus.iter().enumerate() {
            log_info!("GPU {i}: {}", gpu.name);
        }
        self.log_outputs();
        match privileges::snipping_owns_prtsc() {
            Some(true) => {
                log_warn!("PrtSc забирают «Ножницы» (PrintScreenKeyForSnippingEnabled = 1)")
            }
            Some(false) => log_info!("PrtSc свободен (PrintScreenKeyForSnippingEnabled = 0)"),
            None => log_info!("PrintScreenKeyForSnippingEnabled не задан (в Win11 по умолчанию PrtSc у «Ножниц»)"),
        }
    }

    fn log_outputs(&self) {
        for (i, out) in self.outputs.iter().enumerate() {
            let scale = self.overlays.iter().find(|o| o.output == i).map_or(0.0, |o| o.scale);
            log_info!(
                "монитор {}: {}×{} в ({}, {}), масштаб {:.0}%, GPU {}, поворот {}",
                out.name,
                out.width(),
                out.height(),
                out.rect.left,
                out.rect.top,
                scale * 100.0,
                out.gpu,
                out.rotation.0
            );
        }
    }

    /// LL-хук только наблюдает: пишет в лог, видит ли он PrtSc. Скрин запускает WM_HOTKEY.
    fn set_hook(&mut self, on: bool) {
        match (on, self.hook.take()) {
            (true, None) => {
                match unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook), Some(self.instance), 0) } {
                    Ok(hook) => self.hook = Some(hook),
                    Err(e) => log_warn!("LL-хук не установлен: {e}"),
                }
            }
            (false, Some(hook)) => unsafe {
                let _ = UnhookWindowsHookEx(hook);
            },
            (_, hook) => self.hook = hook,
        }
    }

    fn tray_data(&self) -> NOTIFYICONDATAW {
        NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: 1,
            ..Default::default()
        }
    }

    fn add_tray_icon(&self) {
        let mut data = self.tray_data();
        data.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
        data.uCallbackMessage = WM_APP_TRAY;
        data.hIcon = self.icons.tray();
        let tip = wide("Kadr — снимки экрана; правый клик: меню");
        data.szTip[..tip.len()].copy_from_slice(&tip);
        if !unsafe { Shell_NotifyIconW(NIM_ADD, &data) }.as_bool() {
            log_warn!("иконка в трее не добавилась");
        }
    }

    fn on_hotkey(&mut self, id: i32, queue_ms: u32) {
        match id {
            HK_RECENT => self.show_recent(),
            HK_ESC => self.on_key(VK_ESCAPE.0, true),
            HK_PRTSC | HK_BACKUP | HK_FULLSCREEN | HK_WINDOW | HK_LAST_REGION => {
                let foreground = privileges::foreground_exe();
                let mode = match id {
                    HK_FULLSCREEN => Mode::Fullscreen,
                    HK_WINDOW => Mode::Window,
                    HK_LAST_REGION => Mode::LastRegion,
                    _ => Mode::Region,
                };
                self.capture_mode(mode, queue_ms, &foreground);
                let printscreen = match id {
                    HK_PRTSC => &self.config.hotkeys.region,
                    HK_FULLSCREEN => &self.config.hotkeys.fullscreen,
                    HK_WINDOW => &self.config.hotkeys.window,
                    HK_LAST_REGION => &self.config.hotkeys.last_region,
                    _ => "",
                };
                if hotkey::parse(printscreen)
                    .ok()
                    .flatten()
                    .is_some_and(|key| key.vk == VK_SNAPSHOT.0)
                {
                    self.log_prtsc(&foreground);
                }
            }
            _ => {}
        }
    }

    fn on_theme_change(&mut self) {
        self.theme = Theme::current();
        let mut data = self.tray_data();
        data.uFlags = NIF_ICON;
        data.hIcon = self.icons.tray();
        unsafe {
            let _ = Shell_NotifyIconW(NIM_MODIFY, &data);
        }
    }

    /// Итог эксперимента M0 по одному нажатию PrtSc: кто его увидел.
    fn log_prtsc(&mut self, foreground: &str) {
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_HOOK_CHECK);
        }
        let hook = match (self.hook.is_some(), self.hook_press.take()) {
            (false, _) => "выключен",
            (true, Some(_)) => "видит",
            (true, None) => "НЕ видит",
        };
        log_info!("PrtSc: WM_HOTKEY пришёл, LL-хук {hook}, передний план {foreground}");
    }

    fn on_hook_seen(&mut self) {
        self.hook_press = Some(privileges::foreground_exe());
        unsafe {
            SetTimer(Some(self.hwnd), TIMER_HOOK_CHECK, 400, None);
        }
    }

    fn on_timer(&mut self, id: usize) {
        if id == TIMER_DRAFT {
            unsafe {
                let _ = KillTimer(Some(self.hwnd), TIMER_DRAFT);
            }
            self.flush_draft(true);
            return;
        }
        if id != TIMER_HOOK_CHECK {
            return;
        }
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_HOOK_CHECK);
        }
        if let Some(foreground) = self.hook_press.take() {
            log_warn!("PrtSc: LL-хук видит, но WM_HOTKEY НЕ пришёл, передний план {foreground}");
        }
    }

    fn on_tray(&mut self, event: u32) {
        match event {
            WM_LBUTTONUP => self.show_recent(),
            WM_RBUTTONUP | WM_CONTEXTMENU => self.show_menu(),
            NIN_BALLOONUSERCLICK => {
                if self.recovery_notice {
                    self.recovery_notice = false;
                    self.show_recent();
                } else {
                    self.open_last_saved();
                }
            }
            _ => {}
        }
    }

    fn show_menu(&mut self) {
        let check = |on: bool| if on { MF_CHECKED } else { MF_UNCHECKED };
        let command = unsafe {
            let Ok(menu) = CreatePopupMenu() else { return };
            let label = wide(&format!("Выделить область\t{}", self.config.hotkeys.region));
            let _ = AppendMenuW(menu, MF_STRING, CMD_CAPTURE, PCWSTR(label.as_ptr()));
            let label = wide(&format!("Монитор под курсором\t{}", self.config.hotkeys.fullscreen));
            let _ = AppendMenuW(menu, MF_STRING, CMD_FULLSCREEN, PCWSTR(label.as_ptr()));
            let label = wide(&format!("Повторить область\t{}", self.config.hotkeys.last_region));
            let enabled = if self.last_region.is_some() {
                MF_ENABLED
            } else {
                MF_GRAYED
            };
            let _ = AppendMenuW(menu, MF_STRING | enabled, CMD_LAST_REGION, PCWSTR(label.as_ptr()));
            let _ = AppendMenuW(menu, MF_STRING, CMD_CONFIG, w!("Настройки…"));
            let label = wide(&format!("Недавние…\t{}", self.config.hotkeys.recent));
            let _ = AppendMenuW(menu, MF_STRING, CMD_RECENT, PCWSTR(label.as_ptr()));
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
            let _ = AppendMenuW(menu, MF_STRING | check(self.warm), CMD_WARM, w!("Прогретый дубликатор"));
            let _ = AppendMenuW(
                menu,
                MF_STRING | check(self.hook.is_some()),
                CMD_HOOK,
                w!("Наблюдатель LL-хука"),
            );
            let _ = AppendMenuW(menu, MF_STRING, CMD_LOG, w!("Открыть лог"));
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
            let _ = AppendMenuW(menu, MF_STRING, CMD_EXIT, w!("Выход"));
            let cursor = cursor_pos();
            // Без этого меню не закрывается по клику мимо (особенность TrackPopupMenu).
            let _ = SetForegroundWindow(self.hwnd);
            let flags = TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY;
            let command = TrackPopupMenu(menu, flags, cursor.x, cursor.y, None, self.hwnd, None);
            let _ = DestroyMenu(menu);
            command.0 as usize
        };
        match command {
            CMD_RECENT => self.show_recent(),
            CMD_CAPTURE => self.open_session("трей", 0, "—"),
            CMD_FULLSCREEN => self.capture_mode(Mode::Fullscreen, 0, "трей"),
            CMD_LAST_REGION => self.capture_mode(Mode::LastRegion, 0, "трей"),
            CMD_CONFIG => {
                let path = wide(&format!("\"{}\"", self.config_path.display()));
                unsafe {
                    ShellExecuteW(
                        None,
                        w!("open"),
                        w!("notepad.exe"),
                        PCWSTR(path.as_ptr()),
                        PCWSTR::null(),
                        SW_SHOWNORMAL,
                    );
                }
            }
            CMD_WARM => {
                self.set_warm(!self.warm);
                log_info!("дубликатор: {}", self.mode());
            }
            CMD_HOOK => {
                self.set_hook(self.hook.is_none());
                log_info!(
                    "наблюдатель LL-хука: {}",
                    if self.hook.is_some() {
                        "включён"
                    } else {
                        "выключен"
                    }
                );
            }
            CMD_LOG => {
                if let Some(path) = log::path() {
                    let path = wide(&path.to_string_lossy());
                    unsafe {
                        ShellExecuteW(
                            None,
                            w!("open"),
                            PCWSTR(path.as_ptr()),
                            PCWSTR::null(),
                            PCWSTR::null(),
                            SW_SHOWNORMAL,
                        );
                    }
                }
            }
            CMD_EXIT => unsafe { PostQuitMessage(0) },
            _ => {}
        }
    }

    fn capture_mode(&mut self, mode: Mode, queue_ms: u32, foreground: &str) {
        if self.active.is_some() || self.loading_recent {
            return;
        }
        let monitors: Vec<_> = self.outputs.iter().map(Output::bounds).collect();
        let region = mode.region(
            &monitors,
            doc_point(cursor_pos()),
            capture::foreground_window(),
            self.last_region,
        );
        match region {
            Ok(None) => self.open_session("область", queue_ms, foreground),
            Ok(Some(region)) => {
                self.hide_recent_before_capture();
                let t = perf::now();
                let image = self.capture_image(region);
                if !self.warm {
                    for out in &mut self.outputs {
                        out.release_duplication();
                        out.release_frame();
                    }
                }
                match image {
                    Ok(image) => {
                        // В «Недавние» не пишем: снимок уже в буфере, а десяток быстрых снимков
                        // вытеснил бы несохранённую ручную область, ради которой «Недавние» и есть.
                        self.copy_image(Arc::new(image));
                        log_info!(
                            "быстрый снимок {mode:?}: {}×{}, {:.1} мс",
                            region.width(),
                            region.height(),
                            perf::ms(t, perf::now())
                        );
                    }
                    Err(e) => {
                        log_warn!("быстрый снимок: {e}");
                        self.notify("Не удалось сделать снимок", &e.message());
                    }
                }
            }
            Err(error) => self.notify("Не удалось сделать снимок", error),
        }
    }

    /// Быстрые режимы снимают пиксели до любых оверлеев и не меняют фокус окна.
    fn capture_image(&mut self, region: DocRect) -> Result<Image> {
        let mut frames = Vec::new();
        for out in &mut self.outputs {
            if out.bounds().intersect(&region).is_none() {
                continue;
            }
            let gpu = &self.gpus[out.gpu];
            out.capture(&gpu.device, &gpu.context)?;
            frames.push(out.read_pixels(&gpu.device, &gpu.context)?);
        }
        Ok(Image::compose(&frames, region))
    }

    /// Горячий путь: захват → рендер в замаскированные окна → снятие маскировки.
    fn open_session(&mut self, source: &'static str, queue_ms: u32, foreground: &str) {
        if self.active.is_some() || self.loading_recent || self.overlays.is_empty() {
            return;
        }
        self.hide_recent_before_capture();
        let t0 = perf::now();
        let mut dup_ms = 0.0;
        for out in &mut self.outputs {
            match out.capture(&self.gpus[out.gpu].device, &self.gpus[out.gpu].context) {
                Ok(ms) => dup_ms += ms,
                Err(e) => log_warn!("захват {}: {e}", out.name),
            }
        }
        let t1 = perf::now();

        for overlay in &mut self.overlays {
            let out = &self.outputs[overlay.output];
            overlay.set_frame(&self.gpus[out.gpu], out.frame.as_ref());
        }
        let cursor = cursor_pos();
        let hud_overlay = self.overlay_at(cursor).unwrap_or(0);
        let monitors = self.outputs.iter().map(Output::bounds).collect();
        let session = Session::new(monitors, doc_point(cursor), self.grab(), self.perf_hud);
        self.active = Some(Active {
            session,
            frames: Vec::new(),
            hud_overlay,
            hovered: None,
            draft: None,
            drawing: Drawing::default(),
            text_surrogate: None,
        });
        self.source = source;
        self.render_all();
        let t2 = perf::now();

        unsafe {
            for overlay in &self.overlays {
                let _ = SetWindowPos(
                    overlay.hwnd,
                    Some(HWND_TOPMOST),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                );
                overlay::set_cloaked(overlay.hwnd, false);
            }
        }
        let t3 = perf::now();
        // Только для замера: ждём композицию, в которой окна уже видны. Фокус забираем после —
        // первый SetForegroundWindow стоит ~20 мс, но картинку он не задерживает.
        unsafe {
            let _ = DwmFlush();
        }
        let t4 = perf::now();
        let focused = unsafe {
            let focused = SetForegroundWindow(self.overlays[hud_overlay].hwnd).as_bool();
            // Esc закрывает оверлей, даже если забрать фокус не удалось.
            let _ = RegisterHotKey(Some(self.hwnd), HK_ESC, MOD_NOREPEAT, u32::from(VK_ESCAPE.0));
            focused
        };
        let t5 = perf::now();

        let timing = Timing {
            queue_ms,
            dup_create_ms: dup_ms,
            capture_ms: perf::ms(t0, t1),
            render_ms: perf::ms(t1, t2),
            show_ms: perf::ms(t2, t3),
            composed_ms: perf::ms(t3, t4),
            focus_ms: perf::ms(t4, t5),
        };
        self.stats.push(timing.total());
        self.last = Some(timing);
        log_info!(
            "захват #{}: итого {:.1} мс (захват {:.1}, из них дубликатор {:.1}; рендер {:.1}; показ {:.1}; \
             до экрана {:.1}; потом фокус {:.1}), очередь ~{} мс, {}, дубликатор {}, передний план {}, фокус {}",
            self.stats.len(),
            timing.total(),
            timing.capture_ms,
            timing.dup_create_ms,
            timing.render_ms,
            timing.show_ms,
            timing.composed_ms,
            timing.focus_ms,
            queue_ms,
            source,
            self.mode(),
            foreground,
            if focused {
                "получен"
            } else {
                "НЕ получен"
            }
        );

        self.after_show();
        if !self.warm {
            for out in &mut self.outputs {
                out.release_duplication();
            }
        }
        // Второй кадр — уже с цифрами этого захвата в HUD.
        self.render_all();
    }

    fn close_session(&mut self) {
        self.flush_draft(false);
        let Some(active) = self.active.take() else {
            return;
        };
        if let Some(selection) = active.session.selection() {
            self.last_region = Some(selection);
        }
        unsafe {
            let _ = UnregisterHotKey(Some(self.hwnd), HK_ESC);
            let _ = ReleaseCapture();
        }
        for overlay in &mut self.overlays {
            overlay::set_cloaked(overlay.hwnd, true);
            overlay.clear_frame();
        }
        if !self.warm {
            for out in &mut self.outputs {
                out.release_frame();
            }
        }
        self.apply_pending_config();
    }

    /// После показа: копии кадров в память (лупа, экспорт) и окна для подсветки. Горячий путь их не ждёт.
    fn after_show(&mut self) {
        let t = perf::now();
        let frames: Vec<Arc<Image>> = self
            .outputs
            .iter()
            .filter_map(|out| {
                let gpu = &self.gpus[out.gpu];
                out.read_pixels(&gpu.device, &gpu.context)
                    .map(Arc::new)
                    .map_err(|e| log_warn!("копия кадра {}: {e}", out.name))
                    .ok()
            })
            .collect();
        let t_frames = perf::now();
        let windows = capture::top_level_windows();
        log_info!(
            "после показа: кадры в память {:.1} мс, окна ({} шт.) {:.1} мс",
            perf::ms(t, t_frames),
            windows.len(),
            perf::ms(t_frames, perf::now())
        );
        if let Some(active) = &mut self.active {
            active.frames = frames;
            active.session.set_windows(windows);
        }
        self.start_draft();
    }

    fn render_all(&self) {
        let Some(active) = &self.active else { return };
        let session = &active.session;
        let hud = session.hud_visible().then(|| self.hud());
        let magnifier = (session.show_magnifier()
            && active.drawing.tool == kadr_doc::annotation::Tool::Select
            && active.drawing.selected.is_none())
        .then(|| {
            let at = session.cursor();
            let rgb = active
                .frames
                .iter()
                .find_map(|f| f.pixel(at))
                .map(|[b, g, r, _]| [r, g, b]);
            Magnifier { at: (at.x, at.y), rgb }
        });
        let selection = session.highlight().map(win_rect);
        for (i, overlay) in self.overlays.iter().enumerate() {
            let gpu = &self.gpus[self.outputs[overlay.output].gpu];
            let marks = Marks {
                selection,
                handles: session.show_handles()
                    && active.drawing.tool == kadr_doc::annotation::Tool::Select
                    && active.drawing.selected.is_none(),
                toolbar: (session.show_toolbar() && !active.drawing.busy()).then_some(Toolbar {
                    hovered: active.hovered,
                    editor: Some(editor_bar(&active.drawing)),
                }),
                magnifier: magnifier.as_ref(),
                hud: if i == active.hud_overlay { hud.as_ref() } else { None },
                drawing: session.selection().map(|crop| kadr_render::annotations::View {
                    shapes: active.drawing.shapes(),
                    preview: active.drawing.preview(),
                    frames: &active.frames,
                    crop,
                    selected: active.drawing.selected_bounds(),
                    text_edit: active.drawing.text_selection(),
                }),
            };
            if let Err(e) = overlay.present(gpu, &self.shared, &self.theme, marks) {
                log_warn!("рендер оверлея: {e}");
            }
        }
    }

    fn hud(&self) -> Hud {
        let p = &self.process;
        let ui_access = if p.ui_access { " · uiAccess" } else { "" };
        let scope = format!("M2 · {} · {}{ui_access} · {}", self.source, p.integrity, self.mode());
        let ms = |label: &str, v: f64| HudRow::new(label, format!("{v:.1}"), "мс");
        let (lead, groups) = match &self.last {
            None => (None, Vec::new()),
            Some(t) => {
                let mut stages = vec![ms("захват", t.capture_ms)];
                if t.dup_create_ms > 0.0 {
                    stages.push(ms("из них дубликатор", t.dup_create_ms));
                }
                stages.extend([
                    ms("рендер", t.render_ms),
                    ms("показ", t.show_ms),
                    ms("до экрана", t.composed_ms),
                ]);
                let stats = vec![
                    ms("p50", self.stats.percentile(50.0)),
                    ms("p95", self.stats.percentile(95.0)),
                    HudRow::new("замеров", self.stats.len().to_string(), ""),
                ];
                let after = vec![
                    ms("фокус после показа", t.focus_ms),
                    HudRow::new("очередь сообщений", format!("~{}", t.queue_ms), "мс"),
                ];
                (
                    Some((format!("{:.1}", t.total()), "мс".to_string())),
                    vec![stages, stats, after],
                )
            }
        };
        Hud {
            title: "Kadr".into(),
            scope,
            lead_label: "хоткей → экран".into(),
            lead,
            groups,
            hint: "Enter копировать · Ctrl+S сохранить · Esc закрыть · H панель".into(),
        }
    }

    fn on_pointer(&mut self, hwnd: HWND, msg: u32) {
        let hit = self.toolbar_at(cursor_pos());
        let Some(active) = &mut self.active else {
            return;
        };
        // Кнопки панели перехватывают мышь раньше выделения: нажатие сразу выполняет действие.
        if active.hovered != hit {
            active.hovered = hit;
            self.render_all();
        }
        if let Some(button) = hit {
            if msg == WM_LBUTTONDOWN {
                self.press(button);
            }
            return;
        }
        let p = doc_point(cursor_pos());
        if self.drawing_pointer(hwnd, msg, p) {
            return;
        }
        let Some(active) = &mut self.active else {
            return;
        };
        let action = match msg {
            WM_LBUTTONDOWN => {
                unsafe { SetCapture(hwnd) };
                active.session.pointer_down(p)
            }
            WM_LBUTTONUP => {
                unsafe {
                    let _ = ReleaseCapture();
                }
                active.session.pointer_up(p)
            }
            _ => active.session.pointer_move(p),
        };
        self.apply(action);
    }

    fn on_key(&mut self, vk: u16, down: bool) {
        if down && self.editor_key(vk) {
            return;
        }
        let Some(active) = &mut self.active else {
            return;
        };
        let Some(key) = key_from_vk(vk) else { return };
        let action = if down {
            active.session.key_down(key, mods())
        } else {
            active.session.key_up(key)
        };
        self.apply(action);
    }

    fn apply(&mut self, action: Action) {
        self.schedule_draft();
        match action {
            Action::None => {}
            Action::Redraw => self.render_all(),
            Action::Close => self.close_session(),
            Action::Copy => self.export(Export::Copy),
            Action::Save => self.export(Export::Save),
            Action::SaveAs => self.export(Export::SaveAs),
            Action::CopyColor => self.copy_color(),
        }
    }

    /// Вырезает выделение из кадров в памяти, закрывает оверлей и отдаёт картинку дальше.
    /// Оверлей закрывается до диалога: в режиме uiAccess он лежит выше любых обычных окон.
    fn export(&mut self, how: Export) {
        if let Some(active) = &mut self.active {
            active.drawing.finish_text();
        }
        let Some(image) = self.selected_image() else {
            return;
        };
        self.flush_draft(true);
        let draft = self
            .active
            .as_ref()
            .and_then(|a| a.draft.as_ref().map(|d| d.token.clone()));
        self.close_session();
        match how {
            Export::Copy => self.copy_image(Arc::new(image)),
            Export::Save => {
                let path = file::unique_path(&self.save_folder(), &file::timestamp_name());
                self.save(path, image, draft);
            }
            Export::SaveAs => match file::ask_path(&self.save_folder(), &file::timestamp_name()) {
                Ok(Some(path)) => self.save(path, image, draft),
                Ok(None) => {}
                Err(e) => log_warn!("диалог «Сохранить как»: {e}"),
            },
        }
    }

    fn save_folder(&self) -> PathBuf {
        let folder = self.config.save.folder.trim();
        if folder.is_empty() {
            file::default_folder()
        } else {
            PathBuf::from(folder)
        }
    }

    fn copy_image(&mut self, image: Arc<Image>) {
        match clipboard::put_image(self.hwnd, &image) {
            Ok(()) => {
                log_info!("в буфере обмена: {}×{}", image.width(), image.height());
                self.clipboard = Some(image);
            }
            Err(e) => {
                log_warn!("буфер обмена: {e}");
                self.notify("Не удалось скопировать", &e.message());
            }
        }
    }

    fn selected_image(&self) -> Option<Image> {
        let active = self.active.as_ref()?;
        let selection = active.session.selection()?;
        if active.frames.is_empty() {
            log_warn!("экспорт: копии кадров в памяти нет");
            return None;
        }
        let gpu = self.gpus.first()?;
        match kadr_render::annotations::flatten(gpu, &self.shared, &active.frames, selection, active.drawing.shapes()) {
            Ok(image) => Some(image),
            Err(e) => {
                log_warn!("экспорт аннотаций: {e}");
                self.notify("Не удалось подготовить снимок", &e.message());
                None
            }
        }
    }

    /// Кодирование и запись — в фоне: оверлей уже закрыт, ждать нечего.
    fn save(&self, path: PathBuf, image: Image, draft: Option<crate::recent_worker::Token>) {
        let (tx, host) = (self.saved_tx.clone(), self.hwnd.0 as isize);
        std::thread::spawn(move || {
            let result = file::write_png(&path, &image)
                .map(|()| path.clone())
                .map_err(|e| (path, e.to_string()));
            let _ = tx.send(Saved { result, draft });
            unsafe {
                let _ = PostMessageW(Some(HWND(host as *mut _)), WM_APP_SAVED, WPARAM(0), LPARAM(0));
            }
        });
    }

    fn on_saved(&mut self) {
        while let Ok(saved) = self.saved_rx.try_recv() {
            match saved.result {
                Ok(path) => {
                    log_info!("сохранено: {}", path.display());
                    let name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    if self.config.save.notify {
                        self.notify("Снимок сохранён", &format!("{name}\nНажмите, чтобы открыть папку"));
                    }
                    self.last_saved = Some(path);
                    if let Some(draft) = saved.draft {
                        self.recent.send(crate::recent_worker::Command::Remove(draft));
                    }
                }
                Err((path, e)) => {
                    log_warn!("не удалось сохранить {}: {e}", path.display());
                    self.notify("Не удалось сохранить", &e);
                }
            }
        }
    }

    /// `C` над экраном — HEX цвета под курсором в буфер; оверлей остаётся открытым.
    fn copy_color(&mut self) {
        let Some(active) = &self.active else { return };
        let Some(pixel) = active.frames.iter().find_map(|f| f.pixel(active.session.cursor())) else {
            return;
        };
        let hex = kadr_doc::hex(pixel);
        match clipboard::put_text(self.hwnd, &hex) {
            Ok(()) => {
                log_info!("цвет в буфере: {hex}");
                self.clipboard = None;
            }
            Err(e) => log_warn!("цвет в буфер: {e}"),
        }
    }

    /// Уведомление Windows от иконки в трее. Тихие часы и режим «Не беспокоить» соблюдаются.
    fn notify(&self, title: &str, text: &str) {
        let mut data = self.tray_data();
        data.uFlags = NIF_INFO;
        data.dwInfoFlags = NIIF_INFO | NIIF_RESPECT_QUIET_TIME;
        copy_truncated(&mut data.szInfoTitle, title);
        copy_truncated(&mut data.szInfo, text);
        unsafe {
            let _ = Shell_NotifyIconW(NIM_MODIFY, &data);
        }
    }

    /// Клик по уведомлению о сохранении — Проводник с выделенным файлом.
    fn open_last_saved(&self) {
        let Some(path) = &self.last_saved else { return };
        let args = wide(&format!("/select,\"{}\"", path.display()));
        unsafe {
            ShellExecuteW(
                None,
                w!("open"),
                w!("explorer.exe"),
                PCWSTR(args.as_ptr()),
                PCWSTR::null(),
                SW_SHOWNORMAL,
            );
        }
    }

    /// Кнопка панели под точкой — только когда панель видна.
    fn toolbar_at(&self, p: POINT) -> Option<ToolbarButton> {
        let active = self.active.as_ref()?;
        if !active.session.show_toolbar() || active.drawing.busy() {
            return None;
        }
        let selection = win_rect(active.session.selection()?);
        self.overlays
            .iter()
            .find_map(|o| o.toolbar_hit(&self.shared, selection, p, Some(editor_bar(&active.drawing))))
    }

    fn press(&mut self, button: ToolbarButton) {
        match button {
            ToolbarButton::Copy => self.export(Export::Copy),
            ToolbarButton::Save => self.export(Export::Save),
            ToolbarButton::SaveAs => self.export(Export::SaveAs),
            ToolbarButton::Close => self.close_session(),
            _ => self.editor_button(button),
        }
    }

    /// Курсор над оверлеем: рука над кнопками, крестик, перенос или стрелки размера над маркерами.
    fn cursor_for(&self) -> Option<HCURSOR> {
        if self.toolbar_at(cursor_pos()).is_some() {
            return unsafe { LoadCursorW(None, IDC_HAND) }.ok();
        }
        let active = self.active.as_ref()?;
        if active.drawing.tool != kadr_doc::annotation::Tool::Select {
            return unsafe {
                LoadCursorW(
                    None,
                    if active.drawing.tool == kadr_doc::annotation::Tool::Text {
                        IDC_IBEAM
                    } else {
                        IDC_CROSS
                    },
                )
            }
            .ok();
        }
        let point = doc_point(cursor_pos());
        let shape_cursor = active
            .drawing
            .selected_bounds()
            .and_then(|r| kadr_doc::hit_handle(&r, point, 8))
            .map(|h| {
                use kadr_doc::Handle;
                match h {
                    Handle::Move => CursorKind::Move,
                    Handle::N | Handle::S => CursorKind::SizeNS,
                    Handle::W | Handle::E => CursorKind::SizeWE,
                    Handle::NW | Handle::SE => CursorKind::SizeNWSE,
                    Handle::NE | Handle::SW => CursorKind::SizeNESW,
                }
            });
        let id = match shape_cursor.unwrap_or_else(|| active.session.cursor_kind(point)) {
            CursorKind::Cross => IDC_CROSS,
            CursorKind::Move => IDC_SIZEALL,
            CursorKind::SizeNS => IDC_SIZENS,
            CursorKind::SizeWE => IDC_SIZEWE,
            CursorKind::SizeNWSE => IDC_SIZENWSE,
            CursorKind::SizeNESW => IDC_SIZENESW,
        };
        unsafe { LoadCursorW(None, id) }.ok()
    }

    /// Радиус захвата маркеров в физических пикселях — по самому плотному монитору.
    fn grab(&self) -> i32 {
        let scale = self.overlays.iter().map(|o| o.scale).fold(1.0, f32::max);
        (dip::GRAB * scale).round() as i32
    }

    fn run_bench(&mut self, count: u32) {
        log_info!("бенчмарк: {count} захватов, дубликатор {}", self.mode());
        for _ in 0..count {
            self.open_session("бенчмарк", 0, "—");
            self.close_session();
            // Пауза, чтобы экран успел обновиться и захваты не шли впритык.
            std::thread::sleep(std::time::Duration::from_millis(120));
        }
        log_info!(
            "бенчмарк готов: p50 {:.1} мс, p95 {:.1} мс, n = {}",
            self.stats.percentile(50.0),
            self.stats.percentile(95.0),
            self.stats.len()
        );
        if let Some(dir) = std::env::var_os("KADR_SELFTEST_DIR") {
            self.selftest_export(PathBuf::from(dir));
        }
        unsafe { PostQuitMessage(0) };
    }

    /// Сквозная проверка экспорта на реальных кадрах: монитор под курсором (как Ctrl+A) → PNG в `dir`.
    fn selftest_export(&mut self, dir: PathBuf) {
        self.open_session("самопроверка", 0, "—");
        if let Some(active) = &mut self.active {
            active.session.key_down(
                Key::Char('a'),
                Mods {
                    ctrl: true,
                    shift: false,
                },
            );
        }
        let image = self.selected_image();
        self.close_session();
        let Some(image) = image else {
            log_warn!("самопроверка экспорта: нечего экспортировать");
            return;
        };
        let path = dir.join("selftest.png");
        let t = perf::now();
        match file::write_png(&path, &image) {
            Ok(()) => log_info!(
                "самопроверка экспорта: {}×{} → {} за {:.1} мс",
                image.width(),
                image.height(),
                path.display(),
                perf::ms(t, perf::now())
            ),
            Err(e) => log_warn!("самопроверка экспорта: {e}"),
        }
    }

    fn on_display_change(&mut self) {
        log_info!("конфигурация мониторов изменилась — пересоздаю оверлеи");
        self.close_session();
        self.build_outputs();
        self.log_outputs();
    }

    fn overlay_at(&self, p: POINT) -> Option<usize> {
        self.overlays.iter().position(|o| o.contains(p))
    }

    fn mode(&self) -> &'static str {
        if self.warm { "прогретый" } else { "on-demand" }
    }
}

impl Drop for App {
    fn drop(&mut self) {
        self.close_session();
        self.set_hook(false);
        self.unregister_hotkeys();
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &self.tray_data());
        }
        log_info!("Kadr M2 завершён");
    }
}

/// Вызывает обработчик, если состояние свободно. Во время вложенных вызовов (SetForegroundWindow,
/// TrackPopupMenu, DestroyWindow) оконные процедуры зовутся повторно — тогда сообщение уходит
/// в DefWindowProc, а не роняет RefCell.
fn with_app(f: impl FnOnce(&mut App)) -> bool {
    APP.with(|cell| match cell.try_borrow_mut() {
        Ok(mut slot) => slot.as_mut().map(f).is_some(),
        Err(_) => false,
    })
}

unsafe extern "system" fn host_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let handled = match msg {
        WM_HOTKEY => {
            let queue_ms = unsafe { GetTickCount().wrapping_sub(GetMessageTime() as u32) };
            with_app(|app| app.on_hotkey(wp.0 as i32, queue_ms))
        }
        WM_APP_TRAY => with_app(|app| app.on_tray((lp.0 & 0xffff) as u32)),
        WM_APP_HOOK => with_app(App::on_hook_seen),
        WM_APP_BENCH => with_app(|app| app.run_bench(wp.0 as u32)),
        WM_APP_SAVED => with_app(App::on_saved),
        WM_APP_CONFIG => with_app(App::on_config),
        WM_APP_RECENT => with_app(App::on_recent),
        WM_APP_RECENT_UI => with_app(App::on_recent_ui),
        WM_RENDERFORMAT => with_app(|app| {
            if let Some(image) = &app.clipboard {
                clipboard::render(wp.0 as u32, image);
            }
        }),
        WM_RENDERALLFORMATS => with_app(|app| {
            if let Some(image) = &app.clipboard {
                clipboard::render_all(app.hwnd, image);
            }
        }),
        WM_DESTROYCLIPBOARD => with_app(|app| app.clipboard = None),
        WM_TIMER => with_app(|app| app.on_timer(wp.0)),
        WM_DISPLAYCHANGE => with_app(App::on_display_change),
        WM_SETTINGCHANGE | WM_SYSCOLORCHANGE | WM_THEMECHANGED => with_app(App::on_theme_change),
        m if m != 0 && m == TASKBAR_CREATED.load(Relaxed) => with_app(|app| app.add_tray_icon()),
        _ => false,
    };
    if handled {
        LRESULT(0)
    } else {
        unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
    }
}

unsafe extern "system" fn overlay_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let handled = match msg {
        WM_LBUTTONDOWN | WM_LBUTTONUP | WM_MOUSEMOVE => with_app(|app| app.on_pointer(hwnd, msg)),
        WM_RBUTTONUP => with_app(|app| app.on_key(VK_ESCAPE.0, true)),
        WM_CHAR => with_app(|app| app.on_text(wp.0 as u16)),
        WM_MOUSEWHEEL => with_app(|app| app.editor_wheel(((wp.0 >> 16) as u16 as i16) as i32)),
        WM_LBUTTONDBLCLK => with_app(|app| {
            if let Some(a) = &mut app.active {
                a.drawing.edit_text();
            }
            app.render_all();
        }),
        WM_KEYDOWN => with_app(|app| app.on_key(wp.0 as u16, true)),
        WM_KEYUP => with_app(|app| app.on_key(wp.0 as u16, false)),
        WM_SETCURSOR if (lp.0 & 0xffff) as u32 == HTCLIENT => {
            let mut cursor = None;
            with_app(|app| cursor = app.cursor_for());
            if let Some(cursor) = cursor {
                unsafe { SetCursor(Some(cursor)) };
                return LRESULT(1);
            }
            false
        }
        // Размер окна задаём сами — точно по монитору.
        WM_DPICHANGED => true,
        _ => false,
    };
    if handled {
        LRESULT(0)
    } else {
        unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
    }
}

unsafe extern "system" fn keyboard_hook(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 && (wp.0 as u32 == WM_KEYDOWN || wp.0 as u32 == WM_SYSKEYDOWN) {
        let info = unsafe { &*(lp.0 as *const KBDLLHOOKSTRUCT) };
        if info.vkCode == u32::from(VK_SNAPSHOT.0) {
            let host = HWND(HOST.load(Relaxed) as *mut _);
            unsafe {
                let _ = PostMessageW(Some(host), WM_APP_HOOK, WPARAM(0), LPARAM(0));
            }
        }
    }
    unsafe { CallNextHookEx(None, code, wp, lp) }
}

unsafe fn register_class(
    instance: HINSTANCE,
    name: PCWSTR,
    proc: unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT,
    cursor: HCURSOR,
    icons: &crate::icons::Icons,
) -> Result<()> {
    let class = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        style: CS_DBLCLKS,
        lpfnWndProc: Some(proc),
        hInstance: instance,
        hCursor: cursor,
        hIcon: icons.app(),
        hIconSm: icons.small(),
        lpszClassName: name,
        ..Default::default()
    };
    if unsafe { RegisterClassExW(&class) } == 0 {
        return Err(Error::from_thread());
    }
    Ok(())
}

fn single_instance() -> Option<HANDLE> {
    // Изолированные проверки debug-сборки не завершают установленный экземпляр пользователя.
    let dev = if cfg!(debug_assertions) {
        std::env::var("KADR_DEV_INSTANCE").ok()
    } else {
        None
    };
    let name = dev
        .filter(|s| !s.is_empty() && s.len() <= 32 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
        .map_or_else(|| "Local\\Kadr".to_string(), |s| format!("Local\\Kadr.Dev.{s}"));
    let name = wide(&name);
    let handle = unsafe { CreateMutexW(None, true, PCWSTR(name.as_ptr())) }.ok()?;
    (unsafe { GetLastError() } != ERROR_ALREADY_EXISTS).then_some(handle)
}

fn cursor_pos() -> POINT {
    let mut p = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut p);
    }
    p
}

/// Строка в фиксированный UTF-16-буфер Win32 с обрезкой и нулём на конце.
fn copy_truncated(dst: &mut [u16], s: &str) {
    let src: Vec<u16> = s.encode_utf16().take(dst.len() - 1).collect();
    dst[..src.len()].copy_from_slice(&src);
    dst[src.len()] = 0;
}

fn doc_point(p: POINT) -> DocPoint {
    DocPoint::new(p.x, p.y)
}

fn win_rect(r: DocRect) -> RECT {
    RECT {
        left: r.left,
        top: r.top,
        right: r.right,
        bottom: r.bottom,
    }
}

/// Клавиши по VK-коду: буквы приходят латиницей при любой раскладке, поэтому `C` = `С`.
fn key_from_vk(vk: u16) -> Option<Key> {
    Some(match VIRTUAL_KEY(vk) {
        VK_ESCAPE => Key::Escape,
        VK_RETURN => Key::Enter,
        VK_SPACE => Key::Space,
        VK_LEFT => Key::Left,
        VK_RIGHT => Key::Right,
        VK_UP => Key::Up,
        VK_DOWN => Key::Down,
        _ if (0x41..=0x5A).contains(&vk) => Key::Char((vk as u8 + 32) as char),
        _ => return None,
    })
}

fn mods() -> Mods {
    let down = |key: VIRTUAL_KEY| unsafe { GetKeyState(i32::from(key.0)) } < 0;
    Mods {
        ctrl: down(VK_CONTROL),
        shift: down(VK_SHIFT),
    }
}

fn yes(value: bool) -> &'static str {
    if value { "да" } else { "нет" }
}

fn editor_bar(d: &Drawing) -> kadr_render::scene::EditorBar {
    kadr_render::scene::EditorBar {
        tool: d.tool,
        color: d.color,
        width: d.width,
        strength: d.strength,
        undo: d.can_undo(),
        redo: d.can_redo(),
    }
}
