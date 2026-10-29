//! Text from the screen: copy it, or translate it into Russian. Recognition (and the network call
//! for translation) runs on a worker; the overlay leaves at once and the bubble shows progress.
//! Blur in the selection is applied before recognition, so blurred text is never read or sent.
use super::*;
use crate::bubble::{self, Content};

pub(super) enum TextResult {
    Copied(String),
    Translated { text: String, from: String, anchor: Option<DocRect> },
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
        let Some(monitor) = self.outputs.iter().map(Output::bounds).find(|m| m.contains(p)) else { return };
        // A generous band around the click: enough for a wrapped paragraph, small enough to be fast.
        let Some(area) = DocRect::new(p.x - 900, p.y - 320, p.x + 900, p.y + 320).intersect(&monitor) else { return };
        let image = Image::compose(active.frames.iter().map(Arc::as_ref), area);
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
        let Some(selection) = self.active.as_ref().and_then(|a| a.session.selection()) else { return };
        let Some(image) = self.selected_image() else { return };
        self.run_text_job(image, job, None, selection);
        self.begin_exit(job == Job::Copy, None);
    }

    fn run_text_job(&mut self, image: Image, job: Job, click: Option<DocPoint>, anchor: DocRect) {
        let working = match job {
            Job::Copy => "Распознаю текст…",
            Job::Translate => "Перевожу…",
        };
        self.bubble_pending = Some((anchor, Content::Working(working)));
        let (tx, host) = (self.text_tx.clone(), self.hwnd.0 as isize);
        std::thread::spawn(move || {
            unsafe {
                let _ = CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
            }
            let result = (|| -> std::result::Result<TextResult, String> {
                let lines = crate::ocr::recognize(&image).map_err(|e| format!("Распознавание: {}", e.message()))?;
                let (text, rect) = match click {
                    Some(p) => crate::ocr::paragraph_at(&lines, p).ok_or("Под курсором не нашлось текста")?,
                    None => {
                        let text = crate::ocr::text(&lines);
                        if text.trim().is_empty() {
                            return Err("В выделении не нашлось текста".into());
                        }
                        (text, anchor)
                    }
                };
                Ok(match job {
                    Job::Copy => TextResult::Copied(text),
                    Job::Translate => {
                        let t = crate::translate::translate(&text, "ru")?;
                        TextResult::Translated { text: t.text, from: t.from, anchor: click.map(|_| rect) }
                    }
                })
            })()
            .unwrap_or_else(TextResult::Failed);
            let _ = tx.send(result);
            unsafe {
                let _ = PostMessageW(Some(HWND(host as *mut _)), WM_APP_TEXT, WPARAM(0), LPARAM(0));
            }
        });
    }

    pub(super) fn on_text_result(&mut self) {
        while let Ok(result) = self.text_rx.try_recv() {
            let (content, anchor) = match result {
                TextResult::Copied(text) => {
                    match clipboard::put_text(self.hwnd, &text) {
                        Ok(()) => self.clipboard = None,
                        Err(e) => log_warn!("текст в буфер: {e}"),
                    }
                    log_info!("распознано и скопировано: {} символов", text.chars().count());
                    (Content::Copied(text), None)
                }
                TextResult::Translated { text, from, anchor } => {
                    log_info!("перевод с «{from}»: {} символов", text.chars().count());
                    (Content::Translated { text, from }, anchor)
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
    }
}
