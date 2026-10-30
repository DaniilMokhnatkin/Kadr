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
    pub(super) fn hide_recent_before_capture(&mut self) {
        self.finish_paused();
        let viewer=crate::viewer::close_now();
        // The corner thumbnail and the text bubble are topmost: they must not end up in the capture.
        let bubble = crate::bubble::is_open();
        if bubble {
            crate::bubble::close();
        }
        if crate::thumbnail::hide_now() || bubble || viewer {
            unsafe {
                let _ = DwmFlush();
            }
        }
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
        if !self.recent.send(Command::Create {
            token: token.clone(),
            frames,
            document: document.clone(),
        }) {
            self.notify(
                "Снимок не сохранён в «Недавних»",
                "Хранилище недоступно. Сохраните снимок в файл.",
            );
            return;
        }
        self.active.as_mut().unwrap().draft = Some(DraftState {
            token,
            document,
            view: None,
        });
    }
    pub(super) fn schedule_draft(&self) {
        let Some(active) = &self.active else { return };
        let Some(draft) = &active.draft else { return };
        let selection = archived_selection(active.session.selection(), draft.view).map(to_array);
        let annotations = archived_shapes(active.drawing.shapes(), draft.view, &draft.document.annotations);
        if draft.document.selection != selection
            || draft.document.automatic != active.session.is_auto_selected()
            || draft.document.annotations != annotations
        {
            unsafe {
                SetTimer(Some(self.hwnd), TIMER_DRAFT, 300, None);
            }
        }
    }
    pub(super) fn flush_draft(&mut self, open: bool) {
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_DRAFT);
        }
        let Some(active) = &mut self.active else {
            return;
        };
        let Some(draft) = &mut active.draft else {
            return;
        };
        let selection = archived_selection(active.session.selection(), draft.view).map(to_array);
        let annotations = archived_shapes(active.drawing.shapes(), draft.view, &draft.document.annotations);
        let changed = selection != draft.document.selection
            || draft.document.automatic != active.session.is_auto_selected()
            || draft.document.annotations != annotations;
        if changed {
            draft.token.revision += 1;
            draft.document.revision = draft.token.revision;
            draft.document.selection = selection;
            draft.document.automatic = active.session.is_auto_selected();
            draft.document.annotations = annotations;
        }
        if changed || !open {
            self.recent.send(Command::State {
                token: draft.token.clone(),
                document: draft.document.clone(),
                open,
            });
        }
    }
    pub(super) fn show_recent(&mut self) {
        if self.loading_recent {
            return;
        }
        self.recovery_notice = false;
        if self.recent_window.is_none() {
            match Window::new(
                self.instance,
                self.hwnd,
                WM_APP_RECENT_UI,
                self.icons.app(),
                self.icons.small(),
            ) {
                Ok(window) => self.recent_window = Some(window),
                Err(e) => {
                    self.notify("Не удалось открыть «Недавние»", &e.message());
                    return;
                }
            }
        }
        self.pause_capture();
        self.recent_window
            .as_ref()
            .unwrap()
            .show(self.config.recent.enabled, self.paused.is_some());
        if !self.recent.send(Command::List) {
            self.recent_window.as_ref().unwrap().error("Хранилище недоступно");
        }
    }
    pub(super) fn on_recent_ui(&mut self) {
        let request = self.recent_window.as_ref().and_then(Window::take_request);
        match request {
            Some(Request::Load(id)) => {
                if self
                    .paused
                    .as_ref()
                    .and_then(|a| a.draft.as_ref())
                    .is_some_and(|d| d.token.id == id)
                {
                    if let Some(window) = &self.recent_window {
                        window.hide();
                    }
                    self.resume_capture();
                    return;
                }
                self.loading_recent = true;
                if !self.recent.send(Command::Load(id)) {
                    self.loading_recent = false;
                }
            }
            Some(Request::Clear) => {
                self.finish_paused();
                self.close_session();
                self.recent.send(Command::Clear);
            }
            Some(Request::Close) => {
                self.loading_recent = false;
                self.resume_capture();
            }
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
                        self.notify(
                            "Снимок не был сохранён",
                            "Он доступен в «Недавних». Нажмите, чтобы восстановить.",
                        );
                        self.recovery_notice = true;
                    }
                }
                Event::List(entries) => {
                    if let Some(window) = &self.recent_window {
                        window.set_entries(entries);
                    }
                }
                Event::Persisted(token) => {
                    log_info!("черновик {} записан, ревизия {}", token.id, token.revision)
                }
                Event::Loaded { id, draft } => {
                    if !self.loading_recent {
                        let token = Token {
                            id,
                            revision: draft.document.revision,
                        };
                        self.recent.send(Command::State {
                            token,
                            document: draft.document,
                            open: false,
                        });
                        continue;
                    }
                    self.loading_recent = false;
                    if let Some(window) = &self.recent_window {
                        window.hide();
                    }
                    self.finish_paused();
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
    fn pause_capture(&mut self) {
        if self.active.is_none() || self.paused.is_some() {
            return;
        }
        if let Some(a) = &mut self.active {
            a.drawing.finish_gesture();
            a.drawing.finish_text();
            a.session.pointer_up(doc_point(cursor_pos()));
        }
        self.flush_draft(true);
        self.paused = self.active.take();
        unsafe {
            let _ = UnregisterHotKey(Some(self.hwnd), HK_ESC);
            let _ = ReleaseCapture();
        }
        for o in &self.overlays {
            overlay::set_cloaked(o.hwnd, true);
        }
    }
    fn resume_capture(&mut self) {
        if !resume_active(&mut self.active, &mut self.paused) {
            return;
        }
        self.render_all();
        unsafe {
            for o in &self.overlays {
                overlay::set_cloaked(o.hwnd, false);
            }
            if let Some(a) = &self.active {
                let _ = SetForegroundWindow(self.overlays[a.hud_overlay].hwnd);
            }
            let _ = RegisterHotKey(Some(self.hwnd), HK_ESC, MOD_NOREPEAT, u32::from(VK_ESCAPE.0));
        }
    }
    pub(super) fn finish_paused(&mut self) {
        if self.paused.is_none() {
            return;
        }
        let active = self.active.take();
        self.active = self.paused.take();
        self.close_session();
        self.active = active;
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
            let primary = current
                .iter()
                .find(|r| r.contains(DocPoint::new(0, 0)))
                .copied()
                .unwrap_or(current[0]);
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
                DocRect::new(
                    overlay.rect.left,
                    overlay.rect.top,
                    overlay.rect.right,
                    overlay.rect.bottom,
                ),
            );
            overlay.set_image(&self.gpus[self.outputs[overlay.output].gpu], &image)?;
        }
        let cursor = doc_point(cursor_pos());
        let mut session = Session::new(monitors, cursor, self.grab(), self.perf_hud);
        session.restore_selection(selection, draft.document.automatic);
        let mut drawing = Drawing::default();
        let shapes = if let Some((from, to)) = view {
            draft
                .document
                .annotations
                .iter()
                .map(|s| s.transformed(from, to))
                .collect()
        } else {
            draft.document.annotations.clone()
        };
        drawing.restore(shapes);
        let token = Token {
            id,
            revision: draft.document.revision,
        };
        let hud_overlay = self.overlay_at(cursor_pos()).unwrap_or(0);
        self.active = Some(Active {
            session,
            frames,
            hud_overlay,
            hovered: None,
            draft: Some(DraftState {
                token,
                document: draft.document,
                view,
            }),
            drawing,
            text_surrogate: None,
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
fn resume_active(active: &mut Option<Active>, paused: &mut Option<Active>) -> bool {
    if active.is_some() {
        return false;
    }
    *active = paused.take();
    active.is_some()
}
fn new_token() -> Token {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    Token {
        id: format!(
            "{:x}-{:x}-{:x}",
            now_ms(),
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ),
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
        annotations: Vec::new(),
    }
}
fn archived_selection(selection: Option<DocRect>, view: Option<(DocRect, DocRect)>) -> Option<DocRect> {
    let Some((source, target)) = view else {
        return selection;
    };
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

fn archived_shapes(
    shapes: &[kadr_doc::annotation::Annotation],
    view: Option<(DocRect, DocRect)>,
    originals: &[kadr_doc::annotation::Annotation],
) -> Vec<kadr_doc::annotation::Annotation> {
    if let Some((source, target)) = view {
        shapes
            .iter()
            .map(|s| {
                let original = originals.iter().find(|old| old.id == s.id);
                let displayed = original.map(|s| s.transformed(source, target));
                if displayed.as_ref() == Some(s) {
                    return original.unwrap().clone();
                }
                let mut mapped = s.transformed(target, source);
                if let Some(old) = original
                    && displayed.as_ref().is_some_and(|d| d.width == s.width)
                {
                    mapped.width = old.width;
                }
                mapped
            })
            .collect()
    } else {
        shapes.to_vec()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn returning_from_recent_keeps_selection_shapes_and_undo_history() {
        let crop = DocRect::new(0, 0, 800, 600);
        let mut session = Session::new(vec![crop], DocPoint::new(100, 100), 8, false);
        session.restore_selection(Some(crop), false);
        let mut drawing = Drawing::default();
        drawing.set_tool(kadr_doc::annotation::Tool::Arrow);
        drawing.down(DocPoint::new(100, 100), crop, 1);
        drawing.up(DocPoint::new(200, 200), crop, false);
        let mut paused = Some(Active {
            session,
            drawing,
            frames: Vec::new(),
            hud_overlay: 0,
            hovered: None,
            draft: None,
            text_surrogate: None,
        });
        let mut active = None;
        assert!(resume_active(&mut active, &mut paused));
        assert!(paused.is_none());
        let a = active.as_mut().unwrap();
        assert_eq!(a.session.selection(), Some(crop));
        assert_eq!(a.drawing.shapes().len(), 1);
        a.drawing.undo();
        assert!(a.drawing.shapes().is_empty());
        a.drawing.redo();
        assert_eq!(a.drawing.shapes().len(), 1);
        assert!(!resume_active(&mut active, &mut paused));
    }
    #[test]
    fn restoring_on_a_smaller_monitor_does_not_rewrite_unedited_coordinates_or_width() {
        use kadr_doc::annotation::{Annotation, Tool};
        let source = DocRect::new(-1000, 0, 1000, 1000);
        let target = DocRect::new(0, 0, 1000, 500);
        let mut original = Annotation::new(1, Tool::Arrow, DocPoint::new(-801, 101));
        original.points[1] = [301, 401];
        let displayed = original.transformed(source, target);
        assert_eq!(displayed.width, 2);
        assert_eq!(
            archived_shapes(
                std::slice::from_ref(&displayed),
                Some((source, target)),
                std::slice::from_ref(&original)
            ),
            vec![original.clone()]
        );
        let mut moved = displayed;
        moved.points[0][0] += 10;
        moved.points[1][0] += 10;
        assert_eq!(
            archived_shapes(&[moved], Some((source, target)), &[original])[0].width,
            3
        );
    }
}
