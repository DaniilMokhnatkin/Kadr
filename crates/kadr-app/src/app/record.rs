//! Screen recording from the editor: «Подготовить запись» leaves the overlay and the selected area
//! gets a frame with a panel under it, but nothing records yet — start the video you want first,
//! then «Пуск» (or PrtSc) begins. The worker records the area with the computer's sound; «Стоп» or
//! PrtSc finishes: the MP4 goes to «Видео\Kadr», onto the clipboard as a file (paste it into a
//! chat) and into the corner thumbnail. «Отмена» throws it away. «Курсор» on the panel hides or
//! shows the mouse pointer in the video, before and during the recording.
use super::*;
use crate::recorder::{self, Event, Recording};
use std::sync::mpsc::{Receiver, Sender};

/// What a recording is about to capture: the area, the computer's sound, the microphone.
type Plan = (DocRect, bool, bool);

pub(super) struct RecordState {
    /// Chosen in the editor; framed once the overlay has left.
    pending: Option<Plan>,
    /// Framed on screen, waiting for «Пуск».
    armed: Option<Plan>,
    running: Option<(Recording, DocRect)>,
    /// The mouse pointer is drawn into the video.
    cursor: bool,
    tx: Sender<Event>,
    rx: Receiver<Event>,
}

impl Default for RecordState {
    fn default() -> Self {
        let (tx, rx) = channel();
        Self { pending: None, armed: None, running: None, cursor: true, tx, rx }
    }
}

impl App {
    /// From the editor: leave the overlay, then frame the selection and wait for «Пуск».
    pub(super) fn start_record_from_selection(&mut self) {
        let Some(region) = self.active.as_ref().and_then(|a| a.session.selection()) else { return };
        if region.width() < 16 || region.height() < 16 {
            return;
        }
        let (system_audio, microphone, cursor) = self
            .active
            .as_ref()
            .map(|a| (a.bar.system_audio, a.bar.microphone, a.bar.cursor))
            .unwrap_or((true, self.config.capture.microphone, self.config.capture.cursor));
        self.record.pending = Some((region, system_audio, microphone));
        self.record.cursor = cursor;
        self.begin_exit(false, None);
    }

    /// Once the overlay has gone: the frame and the panel, still not recording.
    pub(super) fn start_pending_record(&mut self) {
        let Some(plan) = self.record.pending.take() else { return };
        if self.record.running.is_some() || self.record.armed.is_some() {
            return;
        }
        crate::record_window::open(self.instance, self.hwnd, WM_APP_RECORD, plan.0, self.record.cursor);
        self.record.armed = Some(plan);
        log_info!("запись: область {}×{} ждёт «Пуск»", plan.0.width(), plan.0.height());
    }

    /// «Пуск»: the worker starts recording the framed area.
    fn begin_recording(&mut self) {
        let Some((region, system_audio, microphone)) = self.record.armed.take() else { return };
        if self.record.running.is_some() {
            return;
        }
        // The worker duplicates the screen itself; ours lets go meanwhile.
        for out in &mut self.outputs {
            out.release_duplication();
            out.release_frame();
        }
        let folder = kadr_export::file::video_folder();
        if let Err(e) = std::fs::create_dir_all(&folder) {
            crate::record_window::close();
            self.notify("Запись не началась", &format!("Папка {}: {e}", folder.display()));
            return;
        }
        let name = kadr_export::file::timestamp_name().replacen("Screenshot", "Video", 1);
        let path = folder.join(format!("{name}.{}", self.config.capture.video.name()));
        let (tx, host) = (self.record.tx.clone(), self.hwnd.0 as isize);
        let recording =
            recorder::start_with_audio(region, path, system_audio, microphone, self.record.cursor, move |event| {
                let _ = tx.send(event);
                unsafe {
                    let _ = PostMessageW(Some(HWND(host as *mut _)), WM_APP_RECORD, WPARAM(0), LPARAM(0));
                }
            });
        crate::record_window::recording();
        self.record.running = Some((recording, region));
        log_info!(
            "запись: {}×{}, курсор {}",
            region.width(),
            region.height(),
            if self.record.cursor { "виден" } else { "скрыт" }
        );
    }

    /// The framed area waits: drop it without recording anything.
    fn disarm(&mut self) {
        if self.record.armed.take().is_some() {
            crate::record_window::close();
            log_info!("запись отменена до начала");
        }
    }

    /// A capture key while recording: PrtSc starts a framed recording and ends a running one.
    /// False when there is nothing to start or stop.
    pub(super) fn stop_recording(&mut self) -> bool {
        if self.record.armed.is_some() {
            self.begin_recording();
            return true;
        }
        match &self.record.running {
            Some((recording, _)) => {
                recording.stop();
                true
            }
            None => false,
        }
    }

    /// The worker's ticks and result, and the panel's buttons.
    pub(super) fn on_record(&mut self) {
        match crate::record_window::take_request() {
            Some(crate::record_window::Request::Start) => self.begin_recording(),
            Some(crate::record_window::Request::Stop) => {
                self.stop_recording();
            }
            Some(crate::record_window::Request::Cancel) => {
                if let Some((recording, _)) = &self.record.running {
                    recording.cancel();
                } else {
                    self.disarm();
                }
            }
            Some(crate::record_window::Request::Cursor(on)) => {
                self.record.cursor = on;
                if let Some((recording, _)) = &self.record.running {
                    recording.set_cursor(on);
                }
            }
            None => {}
        }
        while let Ok(event) = self.record.rx.try_recv() {
            match event {
                Event::Tick(elapsed) => crate::record_window::update(elapsed),
                Event::Done(result) => self.finish_recording(result),
            }
        }
    }

    fn restore_warm(&mut self) {
        if self.warm {
            self.warm_up();
        }
    }

    fn finish_recording(&mut self, result: std::result::Result<recorder::Finished, String>) {
        crate::record_window::close();
        let Some((_, region)) = self.record.running.take() else { return };
        self.restore_warm();
        match result {
            Ok(done) if done.path.exists() => {
                if let Err(e) = kadr_export::clipboard::put_file(self.hwnd, &done.path) {
                    log_warn!("видео в буфер: {e}");
                }
                self.clipboard = None;
                log_info!("видео {:.1} с в буфере: {}", done.seconds, done.path.display());
                let anchor = ((region.left + region.right) / 2, (region.top + region.bottom) / 2);
                self.offer_thumbnail(Arc::new(done.preview), Some(done.path), anchor);
            }
            Ok(_) => log_info!("запись отменена"),
            Err(e) => {
                log_warn!("запись: {e}");
                self.notify("Запись не удалась", &e);
            }
        }
    }
}
