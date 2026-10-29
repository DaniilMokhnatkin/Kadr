use super::*;
use crate::recent_window::{Request, Window};
use crate::recent_worker::{Command, Event, Token, now_ms};
use kadr_doc::pixels;
use kadr_store::recent::{Document, Draft, from_array, to_array};
use std::sync::atomic::{AtomicU64, Ordering};

pub(super) struct DraftState {
    pub token: Token,
    document: Document,
    /// Изменённая раскладка: выделение переводится обратно в координаты сохранённого кадра.
    view: Option<(DocRect, DocRect)>,
}
impl App {
    pub(super) fn hide_recent_before_capture(&self) {
        if let Some(window) = &self.recent_window
            && unsafe { IsWindowVisible(window.hwnd) }.as_bool()
        {
            window.hide();
            unsafe {
                let _ = DwmFlush();
            }
        }
    }
    pub(super) fn start_draft(&mut self) {
        if !self.config.recent.enabled || self.bench_mode {
            return;
        }
        let Some(active) = &self.active else { return };
        if active.frames.is_empty() {
            return;
        }
        let frames = active.frames.clone();
        let document = document(
            self.outputs.iter().map(Output::bounds).collect(),
            active.session.selection(),
            active.session.is_auto_selected(),
        );
        let token = new_token();
        if !self.recent.send(Command::Create { token: token.clone(), frames, document: document.clone() }) {
            self.notify("Снимок не сохранён в «Недавних»", "Хранилище недоступно. Сохраните снимок в файл.");
            return;
        }
        self.active.as_mut().unwrap().draft = Some(DraftState { token, document, view: None });
    }
    pub(super) fn remember_quick(&self, image: &Arc<Image>) {
        if !self.config.recent.enabled || self.bench_mode {
            return;
        }
        let document = document(self.outputs.iter().map(Output::bounds).collect(), Some(image.rect), false);
        let token = new_token();
        self.recent.send(Command::Create {
            token: token.clone(),
            frames: vec![image.clone()],
            document: document.clone(),
        });
        self.recent.send(Command::State { token, document, open: false });
    }
    pub(super) fn schedule_draft(&self) {
        let Some(active) = &self.active else { return };
        let Some(draft) = &active.draft else { return };
        let selection = archived_selection(active.session.selection(), draft.view).map(to_array);
        if draft.document.selection != selection || draft.document.automatic != active.session.is_auto_selected() {
            unsafe {
                SetTimer(Some(self.hwnd), TIMER_DRAFT, 300, None);
            }
        }
    }
    pub(super) fn flush_draft(&mut self, open: bool) {
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_DRAFT);
        }
        let Some(active) = &mut self.active else { return };
        let Some(draft) = &mut active.draft else { return };
        let selection = archived_selection(active.session.selection(), draft.view).map(to_array);
        let changed =
            selection != draft.document.selection || draft.document.automatic != active.session.is_auto_selected();
        if changed {
            draft.token.revision += 1;
            draft.document.revision = draft.token.revision;
            draft.document.selection = selection;
            draft.document.automatic = active.session.is_auto_selected();
        }
        if changed || !open {
            self.recent.send(Command::State { token: draft.token.clone(), document: draft.document.clone(), open });
        }
    }
    pub(super) fn show_recent(&mut self) {
        if self.loading_recent {
            return;
        }
        self.close_session();
        self.recovery_notice = false;
        if self.recent_window.is_none() {
            match Window::new(self.instance, self.hwnd, WM_APP_RECENT_UI, self.icons.app(), self.icons.small()) {
                Ok(window) => self.recent_window = Some(window),
                Err(e) => {
                    self.notify("Не удалось открыть «Недавние»", &e.message());
                    return;
                }
            }
        }
        self.recent_window.as_ref().unwrap().show(self.config.recent.enabled);
        if !self.recent.send(Command::List) {
            self.recent_window.as_ref().unwrap().error("Хранилище недоступно");
        }
    }
    pub(super) fn on_recent_ui(&mut self) {
        let request = self.recent_window.as_ref().and_then(Window::take_request);
        match request {
            Some(Request::Load(id)) => {
                self.loading_recent = true;
                if !self.recent.send(Command::Load(id)) {
                    self.loading_recent = false;
                }
            }
            Some(Request::Clear) => {
                self.close_session();
                self.recent.send(Command::Clear);
            }
            Some(Request::Close) => self.loading_recent = false,
            Some(Request::Refresh) => {
                self.recent.send(Command::List);
            }
            None => {}
        }
    }
    pub(super) fn on_recent(&mut self) {
        while let Ok(event) = self.recent.events.try_recv() {
            match event {
                Event::Ready { entries, recovered } => {
                    if let Some(window) = &self.recent_window {
                        window.set_entries(entries);
                    }
                    if recovered > 0 {
                        self.notify("Снимок не был сохранён", "Он доступен в «Недавних». Нажмите, чтобы восстановить.");
                        self.recovery_notice = true;
                    }
                }
                Event::List(entries) => {
                    if let Some(window) = &self.recent_window {
                        window.set_entries(entries);
                    }
                }
                Event::Persisted(token) => log_info!("черновик {} записан, ревизия {}", token.id, token.revision),
                Event::Loaded { id, draft } => {
                    if !self.loading_recent {
                        let token = Token { id, revision: draft.document.revision };
                        self.recent.send(Command::State { token, document: draft.document, open: false });
                        continue;
                    }
                    self.loading_recent = false;
                    if let Some(window) = &self.recent_window {
                        window.hide();
                    }
                    if let Err(e) = self.restore_draft(id, draft) {
                        log_warn!("восстановление: {e}");
                        self.notify("Не удалось восстановить снимок", &e.message());
                    }
                }
                Event::Error(error) => {
                    self.loading_recent = false;
                    log_warn!("Недавние: {error}");
                    if let Some(window) = &self.recent_window {
                        window.error(&error);
                    }
                    self.notify("Ошибка «Недавних»", &error);
                }
            }
        }
    }
    fn restore_draft(&mut self, id: String, draft: Draft) -> Result<()> {
        if self.active.is_some() || self.overlays.is_empty() {
            return Ok(());
        }
        let current: Vec<_> = self.outputs.iter().map(Output::bounds).collect();
        let same_layout = draft.document.layout.len() == current.len()
            && draft.document.layout.iter().all(|r| current.contains(&from_array(*r)));
        let mut frames = draft.frames;
        let mut selection = draft.document.selection_rect();
        let mut view = None;
        let monitors = if same_layout {
            current.clone()
        } else {
            let source = pixels::bounds(&frames).unwrap();
            let primary = current.iter().find(|r| r.contains(DocPoint::new(0, 0))).copied().unwrap_or(current[0]);
            let (image, mapped) = pixels::fit_to_monitor(&frames, selection, primary);
            let target = image.rect;
            selection = mapped;
            view = Some((source, target));
            frames = vec![Arc::new(image)];
            vec![target]
        };
        for overlay in &mut self.overlays {
            let image = Image::compose(
                frames.iter().map(Arc::as_ref),
                DocRect::new(overlay.rect.left, overlay.rect.top, overlay.rect.right, overlay.rect.bottom),
            );
            overlay.set_image(&self.gpus[self.outputs[overlay.output].gpu], &image)?;
        }
        let cursor = doc_point(cursor_pos());
        let mut session = Session::new(monitors, cursor, self.grab(), self.perf_hud);
        session.restore_selection(selection, draft.document.automatic);
        let token = Token { id, revision: draft.document.revision };
        let hud_overlay = self.overlay_at(cursor_pos()).unwrap_or(0);
        self.active = Some(Active {
            session,
            frames,
            hud_overlay,
            hovered: None,
            draft: Some(DraftState { token, document: draft.document, view }),
        });
        self.source = "Недавние";
        self.render_all();
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
            let _ = SetForegroundWindow(self.overlays[hud_overlay].hwnd);
            let _ = RegisterHotKey(Some(self.hwnd), HK_ESC, MOD_NOREPEAT, u32::from(VK_ESCAPE.0));
        }
        log_info!("черновик восстановлен, прежняя раскладка {}", same_layout);
        Ok(())
    }
}
fn new_token() -> Token {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    Token {
        id: format!("{:x}-{:x}-{:x}", now_ms(), std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)),
        revision: 1,
    }
}
fn document(layout: Vec<DocRect>, selection: Option<DocRect>, automatic: bool) -> Document {
    Document {
        version: 1,
        revision: 1,
        selection: selection.map(to_array),
        automatic,
        layout: layout.into_iter().map(to_array).collect(),
    }
}
fn archived_selection(selection: Option<DocRect>, view: Option<(DocRect, DocRect)>) -> Option<DocRect> {
    let Some((source, target)) = view else { return selection };
    selection
        .and_then(|r| r.intersect(&target))
        .map(|r| {
            let sx = source.width() as f64 / target.width() as f64;
            let sy = source.height() as f64 / target.height() as f64;
            DocRect::new(
                source.left + ((r.left - target.left) as f64 * sx).round() as i32,
                source.top + ((r.top - target.top) as f64 * sy).round() as i32,
                source.left + ((r.right - target.left) as f64 * sx).round() as i32,
                source.top + ((r.bottom - target.top) as f64 * sy).round() as i32,
            )
        })
        .filter(|r| !r.is_empty())
}
