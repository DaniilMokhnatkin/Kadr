//! Censor: each press of the pill steps the level — off → light → hard → full → off. A level finds
//! what to hide in the selection and blurs it; the next press dissolves those blurs away and
//! searches again at the next level. The search runs on a worker (local OCR, nothing leaves the
//! computer). Areas appear in reading order, each dissolving into its blur with a brief accent
//! outline; on the way out the original dissolves back before the blur is removed.
use super::*;
use kadr_render::scene::{Censor, SmartUi};
use std::time::Instant;
use windows::Win32::Security::Cryptography::{BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom};

/// One area's dissolve, the delay between neighbours, and how long the outline lingers.
const REVEAL: f64 = 0.42;
const STAGGER: f64 = 0.055;
const GLOW: f64 = 0.95;

#[derive(Default)]
pub(super) struct Smart {
    /// The level being searched right now.
    scanning: Option<Censor>,
    /// Results of an older search (the session closed or the user pressed again) are ignored.
    job: u64,
    /// The level whose result is on screen (maybe nothing found).
    level: Option<Censor>,
    /// The level to search once the current blurs have dissolved away.
    then: Option<Censor>,
    anim: Option<Anim>,
}

struct Anim {
    started: Instant,
    areas: Vec<DocRect>,
    showing: bool,
}

impl Anim {
    fn duration(&self) -> f64 {
        REVEAL + STAGGER * self.areas.len().saturating_sub(1) as f64 + if self.showing { GLOW } else { 0.0 }
    }
}

fn ease(t: f64) -> f32 {
    let t = t.clamp(0.0, 1.0);
    (1.0 - (1.0 - t).powi(3)) as f32
}

fn next(level: Option<Censor>) -> Option<Censor> {
    match level {
        None => Some(Censor::Light),
        Some(Censor::Light) => Some(Censor::Hard),
        Some(Censor::Hard) => Some(Censor::Full),
        Some(Censor::Full) => None,
    }
}

impl App {
    pub(super) fn smart_ui(&self, d: &Drawing) -> SmartUi {
        if let Some(level) = self.smart.scanning.or(self.smart.then) {
            return SmartUi::Scanning(level);
        }
        if self.smart.anim.as_ref().is_some_and(|a| !a.showing) {
            return SmartUi::Off;
        }
        match self.smart.level {
            Some(level) => SmartUi::On(level, d.smart_shapes().len().min(u16::MAX as usize) as u16),
            None => SmartUi::Off,
        }
    }

    /// Per area: rect, how much original shows over the blur, and the outline strength.
    pub(super) fn smart_fx(&self) -> (bool, Vec<(RECT, f32, f32)>) {
        let scanning = self.smart.scanning.is_some();
        let Some(anim) = &self.smart.anim else { return (scanning, Vec::new()) };
        let elapsed = anim.started.elapsed().as_secs_f64();
        let areas = anim
            .areas
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let local = elapsed - i as f64 * STAGGER;
                let e = ease(local / REVEAL);
                let (cover, glow) = if anim.showing {
                    let rise = (local / 0.12).clamp(0.0, 1.0) as f32;
                    let fall = (1.0 - (local - REVEAL * 0.5) / GLOW).clamp(0.0, 1.0) as f32;
                    (1.0 - e, rise * fall)
                } else {
                    (e, 0.0)
                };
                (win_rect(*r), cover, glow)
            })
            .collect();
        (scanning, areas)
    }

    /// The pill: one step to the next level.
    pub(super) fn toggle_smart(&mut self) {
        if self.smart.anim.is_some() {
            return;
        }
        let Some(active) = &mut self.active else { return };
        let to = next(self.smart.scanning.or(self.smart.level));
        // A search still running belongs to the level being left.
        self.smart.job += 1;
        self.smart.scanning = None;
        self.smart.level = None;
        let found = active.drawing.smart_shapes();
        if !found.is_empty() {
            // Out: the original dissolves back over the blur, the blurs go, then the next level.
            self.smart.then = to;
            self.smart.anim = Some(Anim { started: Instant::now(), areas: reading_order(found), showing: false });
        } else if let Some(level) = to {
            self.scan(level);
        }
        self.request_redraw();
    }

    fn scan(&mut self, level: Censor) {
        let Some(active) = &mut self.active else { return };
        active.drawing.finish_gesture();
        active.drawing.finish_text();
        let Some(image) = self.selected_image() else { return };
        self.smart.job += 1;
        self.smart.scanning = Some(level);
        let (job, tx, host) = (self.smart.job, self.text_tx.clone(), self.hwnd.0 as isize);
        std::thread::spawn(move || {
            unsafe {
                let _ = CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
            }
            let found = crate::ocr::recognize(&image)
                .map(|lines| {
                    let found = match level {
                        Censor::Light => crate::sensitive::find(&lines),
                        Censor::Hard => crate::sensitive::find_strict(&lines),
                        Censor::Full => crate::sensitive::find_all(&lines),
                    };
                    found.into_iter().map(|(_, r)| r).collect()
                })
                .map_err(|e| e.message());
            let _ = tx.send(text::TextResult::Smart(job, found));
            unsafe {
                let _ = PostMessageW(Some(HWND(host as *mut _)), WM_APP_TEXT, WPARAM(0), LPARAM(0));
            }
        });
        self.request_redraw();
    }

    pub(super) fn on_smart_found(&mut self, job: u64, found: std::result::Result<Vec<DocRect>, String>) {
        let Some(level) = self.smart.scanning else { return };
        if job != self.smart.job {
            return;
        }
        self.smart.scanning = None;
        self.smart.level = Some(level);
        let Some(active) = &mut self.active else { return };
        // Only what lies inside the selection: it is what will be exported.
        let crop = active.session.selection();
        let areas: Vec<DocRect> = match found {
            Ok(rects) => rects.into_iter().filter_map(|r| crop.and_then(|c| r.intersect(&c))).collect(),
            Err(e) => {
                log_warn!("цензура: {e}");
                Vec::new()
            }
        };
        log_info!("цензура {}: найдено {}", level.name(), areas.len());
        if !areas.is_empty() {
            let areas = reading_order(areas);
            active.drawing.add_smart(&areas, seed);
            self.smart.anim = Some(Anim { started: Instant::now(), areas, showing: true });
            self.schedule_draft();
        }
        self.request_redraw();
    }

    /// Called every turn of the message loop, like the closing motion.
    pub(super) fn tick_smart(&mut self) {
        if self.active.is_none() {
            self.smart = Smart { job: self.smart.job, ..Default::default() };
            return;
        }
        let mut busy = self.smart.scanning.is_some();
        if let Some(anim) = &self.smart.anim {
            if anim.started.elapsed().as_secs_f64() >= anim.duration() {
                let showing = anim.showing;
                self.smart.anim = None;
                if !showing && let Some(active) = &mut self.active {
                    active.drawing.remove_smart();
                    self.schedule_draft();
                    if let Some(level) = self.smart.then.take() {
                        self.scan(level);
                    }
                }
                self.request_redraw();
            } else {
                busy = true;
            }
        }
        if busy {
            self.request_redraw();
        }
    }
}

/// Top to bottom, then left to right: the cascade reads like the text it hides.
fn reading_order(mut areas: Vec<DocRect>) -> Vec<DocRect> {
    areas.sort_by_key(|r| (r.top / 8, r.left));
    areas
}

fn seed() -> u64 {
    let mut bytes = [0; 8];
    unsafe {
        let _ = BCryptGenRandom(None, &mut bytes, BCRYPT_USE_SYSTEM_PREFERRED_RNG);
    }
    u64::from_le_bytes(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn the_pill_steps_off_light_hard_full_and_back_to_off() {
        let mut level = None;
        let mut seen = Vec::new();
        for _ in 0..4 {
            level = next(level);
            seen.push(level);
        }
        assert_eq!(seen, [Some(Censor::Light), Some(Censor::Hard), Some(Censor::Full), None]);
    }
}
