//! «Скрыть данные»: one press finds personal data in the selection and blurs it; another press
//! takes exactly those blurs away. The search runs on a worker (local OCR, nothing leaves the
//! computer). Areas appear in reading order, each dissolving into its blur with a brief accent
//! outline; on the way out the original dissolves back before the blur is removed.
use super::*;
use kadr_render::scene::SmartUi;
use std::time::{Duration, Instant};
use windows::Win32::Security::Cryptography::{BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom};

/// One area's dissolve, the delay between neighbours, and how long the outline lingers.
const REVEAL: f64 = 0.42;
const STAGGER: f64 = 0.055;
const GLOW: f64 = 0.95;
const EMPTY: Duration = Duration::from_millis(1600);

#[derive(Default)]
pub(super) struct Smart {
    scanning: bool,
    /// Results of an older search (the session closed or the user pressed again) are ignored.
    job: u64,
    empty_until: Option<Instant>,
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

impl App {
    pub(super) fn smart_ui(&self, d: &Drawing) -> SmartUi {
        if self.smart.scanning {
            return SmartUi::Scanning;
        }
        if self.smart.empty_until.is_some_and(|t| Instant::now() < t) {
            return SmartUi::Empty;
        }
        if self.smart.anim.as_ref().is_some_and(|a| !a.showing) {
            return SmartUi::Off;
        }
        match d.smart_shapes().len() {
            0 => SmartUi::Off,
            n => SmartUi::On(n.min(u16::MAX as usize) as u16),
        }
    }

    /// Per area: rect, how much original shows over the blur, and the outline strength.
    pub(super) fn smart_fx(&self) -> (bool, Vec<(RECT, f32, f32)>) {
        let Some(anim) = &self.smart.anim else { return (self.smart.scanning, Vec::new()) };
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
        (self.smart.scanning, areas)
    }

    pub(super) fn toggle_smart(&mut self) {
        if self.smart.scanning || self.smart.anim.is_some() {
            return;
        }
        let Some(active) = &mut self.active else { return };
        let found = active.drawing.smart_shapes();
        if !found.is_empty() {
            // Out: the original dissolves back over the blur, then the blurs go (one undo step).
            self.smart.anim = Some(Anim { started: Instant::now(), areas: reading_order(found), showing: false });
            self.request_redraw();
            return;
        }
        active.drawing.finish_gesture();
        active.drawing.finish_text();
        let Some(image) = self.selected_image() else { return };
        self.smart.job += 1;
        self.smart.scanning = true;
        self.smart.empty_until = None;
        let (job, tx, host) = (self.smart.job, self.text_tx.clone(), self.hwnd.0 as isize);
        let strict = self.config.smart.mode == kadr_store::config::SmartMode::Strict;
        std::thread::spawn(move || {
            unsafe {
                let _ = CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
            }
            let found = crate::ocr::recognize(&image)
                .map(|lines| {
                    let found = if strict { crate::sensitive::find_strict(&lines) } else { crate::sensitive::find(&lines) };
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
        if job != self.smart.job || !self.smart.scanning {
            return;
        }
        self.smart.scanning = false;
        let Some(active) = &mut self.active else { return };
        // Only what lies inside the selection: it is what will be exported.
        let crop = active.session.selection();
        let areas: Vec<DocRect> = match found {
            Ok(rects) => rects.into_iter().filter_map(|r| crop.and_then(|c| r.intersect(&c))).collect(),
            Err(e) => {
                log_warn!("скрытие данных: {e}");
                Vec::new()
            }
        };
        log_info!("скрытие данных: найдено {}", areas.len());
        if areas.is_empty() {
            self.smart.empty_until = Some(Instant::now() + EMPTY);
        } else {
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
        let mut busy = self.smart.scanning;
        if let Some(until) = self.smart.empty_until {
            if Instant::now() >= until {
                self.smart.empty_until = None;
                self.request_redraw();
            } else {
                busy = true;
            }
        }
        if let Some(anim) = &self.smart.anim {
            if anim.started.elapsed().as_secs_f64() >= anim.duration() {
                let showing = anim.showing;
                self.smart.anim = None;
                if !showing && let Some(active) = &mut self.active {
                    active.drawing.remove_smart();
                    self.schedule_draft();
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
