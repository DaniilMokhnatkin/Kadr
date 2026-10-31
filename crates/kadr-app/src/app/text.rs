//! Text from the screen: copy it, or translate it in place. Recognition (and the network call
//! for translation) runs on a worker; the overlay leaves at once, and the bubble (copy) or the
//! lens (translation) shows progress. Blur in the selection is applied before recognition, so
//! blurred text is never read or sent.
use super::*;
use crate::bubble::{self, Content};
use crate::lens;

pub(super) enum TextResult {
    Copied(String),
    /// A QR code in the selection, read instead of the text.
    Qr(String),
    /// What goes over the lens; a narrower picture when Alt+click found its paragraph.
    Lens(lens::Content, Option<Arc<Image>>),
    /// «Скрыть данные»: the search generation and the areas found.
    Smart(u64, std::result::Result<Vec<DocRect>, String>),
    /// The censor's blurs are computed: show them.
    SmartReady(u64, Vec<kadr_doc::annotation::Annotation>),
    Failed(String),
}

#[derive(Clone, Copy, PartialEq)]
enum Job {
    Copy,
    Translate,
}

impl App {
    /// PrtSc, then Alt+click on a paragraph: translate just that paragraph.
    pub(super) fn quick_translate(&mut self, p: DocPoint) {
        let Some(active) = &self.active else { return };
        if active.frames.is_empty() {
            return;
        }
        let Some(monitor) = self.outputs.iter().map(Output::bounds).find(|m| m.contains(p)) else {
            return;
        };
        // A generous band around the click: enough for a wrapped paragraph, small enough to be fast.
        let Some(area) = DocRect::new(p.x - 900, p.y - 320, p.x + 900, p.y + 320).intersect(&monitor) else {
            return;
        };
        let Some(gpu) = self.gpus.first() else { return };
        let image =
            match kadr_render::annotations::flatten(gpu, &self.shared, &active.frames, area, active.drawing.shapes()) {
                Ok(image) => image,
                Err(error) => {
                    log_warn!("подготовка текста для перевода: {error}");
                    return;
                }
            };
        self.run_text_job(image, Job::Translate, Some(p), DocRect::new(p.x, p.y - 10, p.x + 1, p.y + 10));
        self.begin_exit(false, None);
    }

    pub(super) fn translate_selection(&mut self) {
        self.text_from_selection(Job::Translate);
    }

    pub(super) fn copy_text(&mut self) {
        self.text_from_selection(Job::Copy);
    }

    fn text_from_selection(&mut self, job: Job) {
        if let Some(active) = &mut self.active {
            active.drawing.finish_gesture();
            active.drawing.finish_text();
        }
        let Some(selection) = self.active.as_ref().and_then(|a| a.session.selection()) else {
            return;
        };
        let Some(image) = self.selected_image() else {
            return;
        };
        self.run_text_job(image, job, None, selection);
        self.begin_exit(job == Job::Copy, None);
    }

    fn run_text_job(&mut self, image: Image, job: Job, click: Option<DocPoint>, anchor: DocRect) {
        let image = Arc::new(image);
        match job {
            Job::Copy => self.bubble_pending = Some((anchor, Content::Working("Читаю выделение…"))),
            Job::Translate => {
                bubble::close();
                self.lens_pending = Some((image.clone(), lens::Content::Working));
            }
        }
        let (tx, host) = (self.text_tx.clone(), self.hwnd.0 as isize);
        std::thread::spawn(move || {
            unsafe {
                let _ = CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
            }
            let result = match job {
                Job::Copy => copy_job(&image).unwrap_or_else(TextResult::Failed),
                Job::Translate => match translate_job(&image, click) {
                    Ok((content, narrower)) => TextResult::Lens(content, narrower),
                    Err(e) => TextResult::Lens(lens::Content::Failed(e), None),
                },
            };
            let _ = tx.send(result);
            unsafe {
                let _ = PostMessageW(Some(HWND(host as *mut _)), WM_APP_TEXT, WPARAM(0), LPARAM(0));
            }
        });
    }

    pub(super) fn on_text_result(&mut self) {
        while let Ok(result) = self.text_rx.try_recv() {
            let (content, anchor) = match result {
                TextResult::Smart(job, found) => {
                    self.on_smart_found(job, found);
                    continue;
                }
                TextResult::SmartReady(job, shapes) => {
                    self.on_smart_ready(job, shapes);
                    continue;
                }
                TextResult::Lens(content, narrower) => {
                    if let lens::Content::Failed(e) = &content {
                        log_warn!("перевод с экрана: {e}");
                    }
                    match &mut self.lens_pending {
                        // Still leaving: the lens opens with the result already in it.
                        Some((image, c)) => {
                            *c = content;
                            if let Some(narrower) = narrower {
                                *image = narrower;
                            }
                        }
                        None => lens::update(content, narrower),
                    }
                    continue;
                }
                TextResult::Copied(text) => {
                    self.remember_copy(crate::clip_history::Raw::Text(text.clone()));
                    match clipboard::put_text(self.hwnd, &text) {
                        Ok(()) => self.clipboard = None,
                        Err(e) => log_warn!("текст в буфер: {e}"),
                    }
                    log_info!("распознано и скопировано: {} символов", text.chars().count());
                    (Content::Copied(text), None)
                }
                TextResult::Qr(code) => {
                    self.remember_copy(crate::clip_history::Raw::Text(code.clone()));
                    match clipboard::put_text(self.hwnd, &code) {
                        Ok(()) => self.clipboard = None,
                        Err(e) => log_warn!("QR-код в буфер: {e}"),
                    }
                    log_info!("QR-код скопирован: {} символов", code.chars().count());
                    (Content::Qr(code), None)
                }
                TextResult::Failed(e) => {
                    log_warn!("текст с экрана: {e}");
                    (Content::Failed(e), None)
                }
            };
            match &mut self.bubble_pending {
                // Still leaving: the bubble opens with the result already in it.
                Some((a, c)) => {
                    *c = content;
                    if let Some(anchor) = anchor {
                        *a = anchor;
                    }
                }
                None => bubble::update(content, anchor),
            }
        }
    }

    pub(super) fn show_pending_bubble(&mut self) {
        if let Some((anchor, content)) = self.bubble_pending.take() {
            bubble::open(self.instance, anchor, content);
        }
        if let Some((image, content)) = self.lens_pending.take() {
            lens::open(self.instance, image, content);
        }
    }
}

fn copy_job(image: &Image) -> std::result::Result<TextResult, String> {
    if let Some(code) = crate::qr::decode(image) {
        return Ok(TextResult::Qr(code));
    }
    let lines = crate::ocr::recognize(image).map_err(|e| format!("Распознавание: {}", e.message()))?;
    let text = crate::ocr::text(&lines);
    if text.trim().is_empty() {
        return Err("В выделении не нашлось текста".into());
    }
    Ok(TextResult::Copied(text))
}

/// The blocks of the selection (or the paragraph under the Alt+click, with the picture narrowed
/// to it) and their translations.
fn translate_job(
    image: &Image,
    click: Option<DocPoint>,
) -> std::result::Result<(lens::Content, Option<Arc<Image>>), String> {
    let lines = crate::ocr::recognize(image).map_err(|e| format!("Распознавание: {}", e.message()))?;
    let mut blocks = crate::ocr::blocks(&lines);
    let mut narrower = None;
    if let Some(p) = click {
        let around = |b: &crate::ocr::Block, by: i32| {
            DocRect::new(b.rect.left - by, b.rect.top - by, b.rect.right + by, b.rect.bottom + by)
        };
        let half_line = |b: &crate::ocr::Block| (b.line_height / 2.0) as i32;
        let block = blocks
            .iter()
            .find(|b| around(b, half_line(b)).contains(p))
            .cloned()
            .ok_or("Под курсором не нашлось текста")?;
        let area = around(&block, half_line(&block).max(8)).intersect(&image.rect).unwrap_or(image.rect);
        narrower = Some(Arc::new(Image::compose([image], area)));
        blocks = vec![block];
    }
    Ok((lens::translate(blocks)?, narrower))
}
