//! Leaving the overlay. The toolbar folds into the selection, the dimming lifts and an export flashes
//! the captured area once. The work itself (clipboard, file) is already done; only the picture waits.
//! The last frame shows the frozen screen with no chrome, so cloaking it afterwards is invisible.
use super::*;

/// Long enough to read as one motion, short enough that the next action never waits on it.
const DURATION: f64 = 0.2;

/// «Сохранить как…» after the overlay is gone: the image, its draft and where the capture was.
pub(super) type SaveAs = (Arc<Image>, Option<crate::recent_worker::Token>, (i32, i32));

pub(super) struct Exit {
    started: std::time::Instant,
    flash: bool,
    /// «Сохранить как…»: the dialog opens once the overlay is gone (uiAccess windows sit above it).
    then: Option<SaveAs>,
}

impl App {
    pub(super) fn begin_exit(
        &mut self,
        flash: bool,
        save_as: Option<SaveAs>,
    ) {
        if self.active.is_none() || self.exit.is_some() {
            return;
        }
        if !kadr_render::theme::motion_enabled() {
            self.close_session();
            self.show_pending_thumbnail();
            if let Some((image, draft, anchor)) = save_as {
                self.ask_save_as(image, draft, anchor);
            }
            return;
        }
        unsafe {
            let _ = ReleaseCapture();
        }
        if let Some(active) = &mut self.active {
            active.hovered = None;
        }
        self.exit = Some(Exit { started: std::time::Instant::now(), flash, then: save_as });
        self.request_redraw();
    }

    /// Eased 0 → 1 while leaving, and whether the selection flashes.
    pub(super) fn exit_pose(&self) -> Option<(f32, bool)> {
        self.exit.as_ref().map(|e| {
            let t = (e.started.elapsed().as_secs_f64() / DURATION).min(1.0) as f32;
            (1.0 - (1.0 - t).powi(3), e.flash)
        })
    }

    pub fn is_leaving(&self) -> bool {
        self.exit.is_some()
    }

    /// Called every turn of the message loop: keeps frames coming until the motion is done.
    pub(super) fn tick_exit(&mut self) {
        let Some(exit) = &self.exit else { return };
        if exit.started.elapsed().as_secs_f64() >= DURATION {
            self.finish_exit();
        } else {
            self.request_redraw();
        }
    }

    /// Show the final pose, let DWM compose it, then hide the windows and run what was deferred.
    pub(super) fn finish_exit(&mut self) {
        if self.exit.is_none() {
            return;
        }
        if let Some(exit) = &mut self.exit {
            exit.started -= std::time::Duration::from_secs_f64(DURATION);
        }
        self.render_all();
        unsafe {
            let _ = DwmFlush();
        }
        let then = self.exit.take().and_then(|e| e.then);
        self.close_session();
        self.show_pending_thumbnail();
        if let Some((image, draft, anchor)) = then {
            self.ask_save_as(image, draft, anchor);
        }
    }
}
