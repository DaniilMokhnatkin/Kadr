//! Long screenshot: the overlay leaves, the selected area stays framed, and while the user scrolls
//! the page underneath, the area is captured ~15 times a second and glued (kadr_doc::stitch).
//! «Готово» or Enter copies the tall image and shows it in the corner thumbnail; Esc cancels.
use super::*;
use crate::scroll_window::{self, Status};
use kadr_doc::stitch::{Step, Stitcher};

const TIMER_SCROLL: usize = 7;
const PERIOD_MS: u32 = 66;
const HK_SCROLL_DONE: i32 = 20;
const HK_SCROLL_CANCEL: i32 = 21;

pub(super) struct Scroll {
    region: DocRect,
    stitcher: Stitcher,
    status: Status,
    /// Steps by kind (grew, same, back, lost) for the log: tells a still page from a lost match.
    steps: [usize; 4],
}

impl App {
    /// From the editor: leave the overlay, then frame the selection and start capturing.
    pub(super) fn start_scroll_from_selection(&mut self) {
        let Some(region) = self.active.as_ref().and_then(|a| a.session.selection()) else { return };
        if region.height() < 40 || region.width() < 40 {
            return;
        }
        self.scroll_pending = Some(region);
        self.begin_exit(false, None);
    }

    pub(super) fn start_pending_scroll(&mut self) {
        let Some(region) = self.scroll_pending.take() else { return };
        // The editor has only just been hidden: let two compositions pass so the first frame is
        // the page itself and not the editor's last faded picture.
        unsafe {
            let _ = DwmFlush();
            let _ = DwmFlush();
        }
        let first = match self.capture_image(region) {
            Ok(image) => image,
            Err(e) => {
                log_warn!("длинный снимок: {e}");
                self.notify("Не удалось начать длинный снимок", &e.message());
                return;
            }
        };
        self.scroll = Some(Scroll { region, stitcher: Stitcher::new(&first), status: Status::Scrolling, steps: [0; 4] });
        scroll_window::open(self.instance, self.hwnd, WM_APP_SCROLL, region);
        unsafe {
            SetTimer(Some(self.hwnd), TIMER_SCROLL, PERIOD_MS, None);
            let _ = RegisterHotKey(Some(self.hwnd), HK_SCROLL_DONE, MOD_NOREPEAT, u32::from(VK_RETURN.0));
            let _ = RegisterHotKey(Some(self.hwnd), HK_SCROLL_CANCEL, MOD_NOREPEAT, u32::from(VK_ESCAPE.0));
        }
        log_info!("длинный снимок: {}×{}", region.width(), region.height());
    }

    /// Timer tick: one capture, one gluing step.
    pub(super) fn tick_scroll(&mut self) {
        let Some(region) = self.scroll.as_ref().map(|s| s.region) else { return };
        let frame = match self.capture_image(region) {
            Ok(f) => f,
            Err(e) => {
                log_warn!("длинный снимок, кадр: {e}");
                return;
            }
        };
        let Some(scroll) = &mut self.scroll else { return };
        let (kind, status) = match scroll.stitcher.push(&frame) {
            Step::Grew(_) => (0, Status::Scrolling),
            Step::Same => (1, Status::Scrolling),
            Step::Back => (2, Status::Scrolling),
            Step::Lost => (3, Status::Lost),
            Step::Full => (0, Status::Full),
        };
        scroll.steps[kind] += 1;
        scroll.status = status;
        scroll_window::update(scroll.stitcher.height(), status);
    }

    pub(super) fn on_scroll_window(&mut self) {
        match scroll_window::take_request() {
            Some(scroll_window::Request::Done) => self.finish_scroll(true),
            Some(scroll_window::Request::Cancel) => self.finish_scroll(false),
            None => {}
        }
    }

    pub(super) fn scroll_hotkey(&mut self, id: i32) -> bool {
        match id {
            HK_SCROLL_DONE => self.finish_scroll(true),
            HK_SCROLL_CANCEL => self.finish_scroll(false),
            _ => return false,
        }
        true
    }

    pub(super) fn scroll_timer(&mut self, id: usize) -> bool {
        if id != TIMER_SCROLL {
            return false;
        }
        self.tick_scroll();
        true
    }

    pub(super) fn finish_scroll(&mut self, keep: bool) {
        let Some(scroll) = self.scroll.take() else { return };
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_SCROLL);
            let _ = UnregisterHotKey(Some(self.hwnd), HK_SCROLL_DONE);
            let _ = UnregisterHotKey(Some(self.hwnd), HK_SCROLL_CANCEL);
        }
        scroll_window::close();
        let [grew, same, back, lost] = scroll.steps;
        log_info!("длинный снимок, шаги: выросло {grew}, на месте {same}, назад {back}, сбился {lost}");
        if !self.warm {
            for out in &mut self.outputs {
                out.release_duplication();
                out.release_frame();
            }
        }
        if !keep {
            log_info!("длинный снимок отменён");
            return;
        }
        let r = scroll.region;
        let mut image = scroll.stitcher.finish();
        image.rect = DocRect::new(r.left, r.top, r.left + image.rect.width(), r.top + image.rect.height());
        log_info!("длинный снимок: {}×{}", image.width(), image.height());
        let image = Arc::new(image);
        self.copy_image(image.clone());
        let anchor = ((r.left + r.right) / 2, (r.top + r.bottom) / 2);
        self.offer_thumbnail(image, None, None, anchor);
    }
}
