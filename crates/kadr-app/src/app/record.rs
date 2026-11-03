//! Screen recording from the editor: «Запись» leaves the overlay, the selected area gets a red
//! frame, and the worker records it with the computer's sound. «Стоп» or PrtSc finishes: the MP4
//! goes to «Видео\Kadr», onto the clipboard as a file (paste it into a chat) and into the corner
//! thumbnail. «Отмена» throws it away.
use super::*;
use crate::recorder::{self, Event, Recording};
use std::sync::mpsc::{Receiver, Sender};

pub(super) struct RecordState {
    pending: Option<(DocRect, bool, bool)>,
    running: Option<(Recording, DocRect)>,
    tx: Sender<Event>,
    rx: Receiver<Event>,
}

impl Default for RecordState {
    fn default() -> Self {
        let (tx, rx) = channel();
        Self { pending: None, running: None, tx, rx }
    }
}

impl App {
    /// From the editor: leave the overlay, then frame the selection and start.
    pub(super) fn start_record_from_selection(&mut self) {
        let Some(region) = self.active.as_ref().and_then(|a| a.session.selection()) else { return };
        if region.width() < 16 || region.height() < 16 {
            return;
        }
        let audio = self
            .active
            .as_ref()
            .map(|a| (a.bar.system_audio, a.bar.microphone))
            .unwrap_or((true, self.config.capture.microphone));
        self.record.pending = Some((region, audio.0, audio.1));
        self.begin_exit(false, None);
    }

    pub(super) fn start_pending_record(&mut self) {
        let Some((region, system_audio, microphone)) = self.record.pending.take() else { return };
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
            self.notify("Запись не началась", &format!("Папка {}: {e}", folder.display()));
            return;
        }
        let name = kadr_export::file::timestamp_name().replacen("Screenshot", "Video", 1);
        let path = folder.join(format!("{name}.{}", self.config.capture.video.name()));
        let (tx, host) = (self.record.tx.clone(), self.hwnd.0 as isize);
        let recording = recorder::start_with_audio(region, path, system_audio, microphone, move |event| {
            let _ = tx.send(event);
            unsafe {
                let _ = PostMessageW(Some(HWND(host as *mut _)), WM_APP_RECORD, WPARAM(0), LPARAM(0));
            }
        });
        crate::record_window::open(self.instance, self.hwnd, WM_APP_RECORD, region);
        self.record.running = Some((recording, region));
        log_info!("запись: {}×{}", region.width(), region.height());
    }

    /// Stops a running recording, keeping the file; false when nothing records.
    pub(super) fn stop_recording(&mut self) -> bool {
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
            Some(crate::record_window::Request::Stop) => {
                self.stop_recording();
            }
            Some(crate::record_window::Request::Cancel) => {
                if let Some((recording, _)) = &self.record.running {
                    recording.cancel();
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

    fn finish_recording(&mut self, result: std::result::Result<recorder::Finished, String>) {
        crate::record_window::close();
        let Some((_, region)) = self.record.running.take() else { return };
        if self.warm {
            self.warm_up();
        }
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
