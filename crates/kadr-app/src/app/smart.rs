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
const REVEAL: f64 = 0.64;
const STAGGER: f64 = 0.04;
const GLOW: f64 = 0.84;

#[derive(Default)]
pub(super) struct Smart {
    original: bool,
    /// The level being searched right now.
    scanning: Option<Censor>,
    /// Results of an older search (the session closed or the user pressed again) are ignored.
    job: u64,
    /// The level whose result is on screen (maybe nothing found).
    level: Option<Censor>,
    /// The level to search once the current blurs have dissolved away.
    then: Option<Censor>,
    /// «Скрывать данные при копировании»: the export waiting for the search and its blurs.
    after: Option<Export>,
    anim: Option<Anim>,
    /// Blurs found and planned, waiting for their pixels to be computed on a worker.
    pending: Option<Vec<kadr_doc::annotation::Annotation>>,
    source: Option<Source>,
    readings: Arc<std::sync::Mutex<Option<Readings>>>,
    /// The hidden area under the pointer, shown through its blur: where, and how much.
    peek: Option<(DocRect, Peek)>,
}

#[derive(Clone)]
struct Source {
    crop: DocRect,
    frames: Vec<Arc<Image>>,
    shapes: Vec<kadr_doc::annotation::Annotation>,
}

struct Readings {
    source: Source,
    lines: Vec<crate::ocr::Line>,
}
impl Source {
    fn same(&self, other: &Self) -> bool {
        self.crop == other.crop
            && self.shapes == other.shapes
            && self.frames.len() == other.frames.len()
            && self.frames.iter().zip(&other.frames).all(|(a, b)| Arc::ptr_eq(a, b))
    }
}

fn contains(outer: DocRect, inner: DocRect) -> bool {
    outer.left <= inner.left && outer.top <= inner.top && outer.right >= inner.right && outer.bottom >= inner.bottom
}
/// Stay within the document limit without silently leaving text after the last region uncovered.
fn bounded_regions(mut regions: Vec<DocRect>, capacity: usize) -> Vec<DocRect> {
    if capacity == 0 {
        return Vec::new();
    }
    regions = reading_order(regions);
    if regions.len() > capacity {
        let rest = regions.split_off(capacity - 1);
        if let Some(area) = rest.into_iter().reduce(|a, b| a.union(&b)) {
            regions.push(area);
        }
    }
    regions
}

/// The peek's fade: towards 1 while the pointer rests on a blur, back to 0 when it leaves.
struct Peek {
    from: f32,
    to: f32,
    at: Instant,
}

impl Peek {
    const SECONDS: f32 = 0.24;
    fn value(&self) -> f32 {
        let t = (self.at.elapsed().as_secs_f32() / Self::SECONDS).clamp(0.0, 1.0);
        let e = ease(t as f64);
        self.from + (self.to - self.from) * e
    }
    fn settled(&self) -> bool {
        self.at.elapsed().as_secs_f32() >= Self::SECONDS
    }
    fn toward(&mut self, to: f32) {
        if self.to != to {
            *self = Peek { from: self.value(), to, at: Instant::now() };
        }
    }
}

/// Recognition and blurring are heavy: they yield to the frames being drawn.
fn yield_to_ui() {
    use windows::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_BELOW_NORMAL};
    unsafe {
        let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL);
    }
}

struct Anim {
    started: Instant,
    areas: Vec<DocRect>,
    showing: bool,
}

impl Anim {
    fn stagger(&self) -> f64 {
        STAGGER.min(0.45 / self.areas.len().saturating_sub(1).max(1) as f64)
    }
    fn duration(&self) -> f64 {
        REVEAL + self.stagger() * self.areas.len().saturating_sub(1) as f64 + if self.showing { GLOW } else { 0.0 }
    }
}

fn ease(t: f64) -> f32 {
    let t = t.clamp(0.0, 1.0);
    // Zero velocity and acceleration at both ends: a quiet dissolve without a sudden start.
    (t * t * t * (t * (t * 6.0 - 15.0) + 10.0)) as f32
}

impl App {
    pub(super) fn smart_needs_frames(&self) -> bool {
        self.smart.scanning.is_some()
            || self.smart.anim.is_some()
            || self.smart.peek.as_ref().is_some_and(|(_, p)| !p.settled())
    }

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

    pub(super) fn smart_scan_time(&self) -> Option<f64> {
        self.smart.anim.as_ref().filter(|a| a.showing).map(|a| a.started.elapsed().as_secs_f64() / a.duration() * 1.6)
    }

    /// Per area: rect, how much original shows over the blur, and the outline strength.
    pub(super) fn smart_fx(&self) -> (bool, Vec<(RECT, f32, f32)>) {
        if self.smart.original {
            return (
                false,
                self.active
                    .as_ref()
                    .map(|a| a.drawing.smart_shapes().into_iter().map(|r| (win_rect(r), 1.0, 0.1)).collect())
                    .unwrap_or_default(),
            );
        }
        // Play the sweep only after the complete censor layer has been prepared.
        // OCR can take seconds; it must not compete with the reveal animation.
        let scanning = self.smart.anim.as_ref().is_some_and(|a| a.showing);
        // A peek: the original over the blur, with a faint outline saying it is only shown here.
        let peek = self
            .smart
            .peek
            .as_ref()
            .map(|(r, p)| (win_rect(*r), p.value() * 0.96, p.value() * 0.55))
            .filter(|(_, c, _)| *c > 0.01);
        let Some(anim) = &self.smart.anim else {
            return (scanning, peek.into_iter().collect());
        };
        let elapsed = anim.started.elapsed().as_secs_f64();
        let areas = anim
            .areas
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let local = elapsed - i as f64 * anim.stagger();
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
    /// Copy with «Скрывать данные при копировании» on: search first (the beam hides the wait),
    /// copy once the blurs are in. False when there is nothing to wait for.
    pub(super) fn censor_before(&mut self, how: Export) -> bool {
        if !self.config.capture.auto_censor || self.smart.level.is_some() || self.smart.anim.is_some() {
            return false;
        }
        self.smart.after = Some(how);
        if self.smart.scanning.is_none() {
            self.scan(Censor::Light);
        }
        if self.smart.scanning.is_none() {
            self.smart.after = None;
            return false;
        }
        true
    }

    /// The search behind a waiting copy is over: copy now.
    fn export_after(&mut self) {
        if let Some(how) = self.smart.after.take() {
            self.export(how);
        }
    }

    pub(super) fn toggle_original(&mut self) {
        self.smart.original = !self.smart.original;
        self.request_redraw();
    }
    pub(super) fn original_visible(&self) -> bool {
        self.smart.original
    }

    pub(super) fn set_censor(&mut self, to: Option<Censor>) {
        if self.smart.anim.is_some() {
            return;
        }
        let Some(active) = &mut self.active else {
            return;
        };
        self.smart.original = false;
        // A search still running belongs to the level being left.
        self.smart.job += 1;
        self.smart.scanning = None;
        self.smart.pending = None;
        self.smart.source = None;
        self.smart.peek = None;
        self.smart.level = None;
        self.smart.after = None;
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
        let Some(active) = &mut self.active else {
            return;
        };
        active.drawing.finish_gesture();
        active.drawing.finish_text();
        let Some(crop) = active.session.selection() else { return };
        if active.frames.is_empty() {
            return;
        }
        let source = Source { crop, frames: active.frames.clone(), shapes: active.drawing.shapes().to_vec() };
        self.smart.source = Some(source.clone());
        self.smart.job += 1;
        self.smart.scanning = Some(level);
        let (job, tx, host) = (self.smart.job, self.text_tx.clone(), self.hwnd.0 as isize);
        let readings = self.smart.readings.clone();
        std::thread::spawn(move || {
            yield_to_ui();
            unsafe {
                let _ = CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
            }
            let started = Instant::now();
            let found = (|| -> windows::core::Result<Vec<DocRect>> {
                if level == Censor::Full {
                    let image =
                        kadr_render::annotations::flatten_background(&source.frames, source.crop, &source.shapes)?;
                    return crate::ocr::text_regions(&image);
                }
                let cached = readings
                    .lock()
                    .ok()
                    .and_then(|cache| cache.as_ref().filter(|c| c.source.same(&source)).map(|c| c.lines.clone()));
                let lines = if let Some(lines) = cached {
                    lines
                } else {
                    let image =
                        kadr_render::annotations::flatten_background(&source.frames, source.crop, &source.shapes)?;
                    let lines = crate::ocr::recognize(&image)?;
                    if let Ok(mut cache) = readings.lock() {
                        *cache = Some(Readings { source: source.clone(), lines: lines.clone() });
                    }
                    lines
                };
                let found = match level {
                    Censor::Light => crate::sensitive::find(&lines),
                    Censor::Hard => crate::sensitive::find_strict(&lines),
                    Censor::Full => unreachable!(),
                };
                Ok(found.into_iter().map(|(_, r)| r).collect())
            })()
            .map_err(|e| e.message());
            log_info!("цензура: фоновая подготовка и OCR {:.1} мс", started.elapsed().as_secs_f64() * 1000.0);
            let _ = tx.send(text::TextResult::Smart(job, found));
            unsafe {
                let _ = PostMessageW(Some(HWND(host as *mut _)), WM_APP_TEXT, WPARAM(0), LPARAM(0));
            }
        });
        self.request_redraw();
    }

    fn source_is_current(&self) -> bool {
        let (Some(source), Some(active)) = (&self.smart.source, &self.active) else { return false };
        active.session.selection() == Some(source.crop)
            && active.drawing.shapes() == source.shapes
            && active.frames.len() == source.frames.len()
            && active.frames.iter().zip(&source.frames).all(|(a, b)| Arc::ptr_eq(a, b))
    }

    fn discard_stale_scan(&mut self) -> bool {
        if self.source_is_current() {
            return false;
        }
        self.smart.scanning = None;
        self.smart.pending = None;
        self.smart.source = None;
        self.request_redraw();
        // The selection changed under a waiting copy: search what it is now.
        self.export_after();
        true
    }

    pub(super) fn on_smart_found(&mut self, job: u64, found: std::result::Result<Vec<DocRect>, String>) {
        let Some(level) = self.smart.scanning else {
            return;
        };
        if job != self.smart.job {
            return;
        }
        if self.discard_stale_scan() {
            return;
        }
        let Some(active) = &mut self.active else {
            return;
        };
        // Only what lies inside the selection: it is what will be exported.
        let Some(crop) = active.session.selection() else {
            return;
        };
        let areas: Vec<DocRect> = match found {
            Ok(rects) => rects.into_iter().filter_map(|r| r.intersect(&crop)).collect(),
            Err(e) => {
                log_warn!("цензура: {e}");
                Vec::new()
            }
        };
        log_info!("цензура {}: найдено {}", level.name(), areas.len());
        if areas.is_empty() {
            self.smart.scanning = None;
            self.smart.level = Some(level);
            self.request_redraw();
            self.export_after();
            return;
        }
        // The blurs are computed on a worker while the search glint keeps running; the frame
        // that shows them then only pastes ready pixels, however many there are.
        let capacity = 256usize.saturating_sub(active.drawing.shapes().len());
        let mut shapes = active.drawing.plan_smart(&bounded_regions(areas, capacity), seed);
        for shape in &mut shapes {
            shape.strength = shape.strength.max(match level {
                Censor::Light => 0,
                Censor::Hard => 1,
                Censor::Full => 2,
            });
        }
        let frames = active.frames.clone();
        self.smart.pending = Some(shapes.clone());
        let mut all_shapes = active.drawing.shapes().to_vec();
        all_shapes.extend(shapes.clone());
        let existing = active.drawing.shapes().len();
        let (tx, host) = (self.text_tx.clone(), self.hwnd.0 as isize);
        std::thread::spawn(move || {
            yield_to_ui();
            // The verification pass may fall back to Windows OCR: initialise this worker too.
            let com = unsafe { CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED) };
            let started = Instant::now();
            let mut shapes = shapes;
            if level == Censor::Full {
                match kadr_render::annotations::flatten_background(&frames, crop, &all_shapes)
                    .and_then(|image| crate::ocr::text_regions(&image))
                {
                    Ok(residual) => {
                        let missing: Vec<_> = residual
                            .into_iter()
                            .filter(|r| !shapes.iter().any(|shape| contains(shape.rect(), *r)))
                            .collect();
                        let room = 256usize.saturating_sub(existing + shapes.len());
                        if room == 0
                            && let Some(extra) = missing.iter().copied().reduce(|a, b| a.union(&b))
                            && let Some(last) = shapes.last_mut()
                        {
                            let area = last.rect().union(&extra);
                            last.points[0] = [area.left, area.top];
                            last.points[1] = [area.right - 1, area.bottom - 1];
                            if let Some(last_all) = all_shapes.last_mut() {
                                *last_all = last.clone();
                            }
                        }
                        let corrections = bounded_regions(missing, room);
                        for r in corrections {
                            let mut shape = kadr_doc::annotation::Annotation::new(
                                0,
                                kadr_doc::annotation::Tool::Blur,
                                DocPoint::new(r.left, r.top),
                            );
                            shape.points[1] = [r.right - 1, r.bottom - 1];
                            shape.strength = 2;
                            shape.seed = seed();
                            all_shapes.push(shape.clone());
                            shapes.push(shape);
                        }
                    }
                    Err(e) => log_warn!("цензура: дополнительная проверка: {e}"),
                }
            }
            kadr_render::annotations::prewarm(&frames, crop, &all_shapes);
            log_info!("цензура: готовый слой в фоне {:.1} мс", started.elapsed().as_secs_f64() * 1000.0);
            if com.is_ok() {
                unsafe {
                    windows::Win32::System::Com::CoUninitialize();
                }
            }
            let _ = tx.send(text::TextResult::SmartReady(job, shapes));
            unsafe {
                let _ = PostMessageW(Some(HWND(host as *mut _)), WM_APP_TEXT, WPARAM(0), LPARAM(0));
            }
        });
        self.request_redraw();
    }

    /// The blurs are computed: in they come, in reading order.
    pub(super) fn on_smart_ready(&mut self, job: u64, shapes: Vec<kadr_doc::annotation::Annotation>) {
        let Some(level) = self.smart.scanning else {
            return;
        };
        if job != self.smart.job {
            return;
        }
        if self.discard_stale_scan() {
            return;
        }
        let Some(_) = self.smart.pending.take() else {
            return;
        };
        let Some(active) = &mut self.active else {
            return;
        };
        self.smart.scanning = None;
        self.smart.level = Some(level);
        let areas: Vec<DocRect> = shapes.iter().map(|s| s.rect()).collect();
        active.drawing.add_smart_shapes(shapes);
        // Upload each surface before the animation clock starts, not during its first frame.
        if let Some(crop) = active.session.selection() {
            let started = Instant::now();
            for overlay in &self.overlays {
                let gpu = &self.gpus[self.outputs[overlay.output].gpu];
                if let Err(e) =
                    overlay.prepare_annotations(gpu, &self.shared, &active.frames, crop, active.drawing.shapes())
                {
                    log_warn!("цензура: загрузка готового слоя: {e}");
                }
            }
            log_info!("цензура: загрузка слоя перед анимацией {:.1} мс", started.elapsed().as_secs_f64() * 1000.0);
        }
        self.smart.anim = Some(Anim { started: Instant::now(), areas, showing: true });
        self.schedule_draft();
        self.request_redraw();
    }

    /// The pointer moved over the selection: a blur under it shows what it hides, softly.
    pub(super) fn update_peek(&mut self, p: Option<DocPoint>) {
        let Some(active) = &self.active else { return };
        let over = p
            .filter(|_| !active.drawing.busy() && self.smart.anim.is_none())
            .and_then(|p| active.drawing.smart_shapes().into_iter().find(|r| r.contains(p)));
        match (&mut self.smart.peek, over) {
            (Some((rect, peek)), Some(r)) if *rect == r => peek.toward(1.0),
            (_, Some(r)) => {
                // A different blur: start its peek from nothing.
                self.smart.peek = Some((r, Peek { from: 0.0, to: 1.0, at: Instant::now() }));
            }
            (Some((_, peek)), None) => peek.toward(0.0),
            (None, None) => return,
        }
        self.request_redraw();
    }

    /// Called every turn of the message loop, like the closing motion.
    pub(super) fn tick_smart(&mut self) {
        if self.active.is_none() {
            self.smart = Smart { job: self.smart.job, ..Default::default() };
            return;
        }
        if let Some((_, peek)) = &self.smart.peek
            && peek.settled()
            && peek.to == 0.0
        {
            self.smart.peek = None;
        }
        if let Some(anim) = &self.smart.anim
            && anim.started.elapsed().as_secs_f64() >= anim.duration()
        {
            let showing = anim.showing;
            self.smart.anim = None;
            if showing {
                // The blurs have arrived: a copy waiting for them goes now.
                self.export_after();
            }
            if !showing && let Some(active) = &mut self.active {
                active.drawing.remove_smart();
                self.schedule_draft();
                if let Some(level) = self.smart.then.take() {
                    self.scan(level);
                }
            }
            self.request_redraw();
        }
        // The frame scheduler retains each animated surface's next DXGI slot. Re-requesting
        // every monitor here would make an untouched monitor redraw at the fastest one's rate.
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
    fn dissolve_has_soft_endpoints_and_moves_monotonically() {
        assert_eq!(ease(-1.0), 0.0);
        assert_eq!(ease(2.0), 1.0);
        assert!(ease(0.01) < 0.00002);
        assert!(1.0 - ease(0.99) < 0.00002);
        assert!((1..100).all(|i| ease(i as f64 / 100.0) > ease((i - 1) as f64 / 100.0)));
    }
    #[test]
    fn region_limit_merges_overflow_instead_of_dropping_text() {
        let regions: Vec<_> = (0..300).map(|i| DocRect::new(10, i * 10, 80, i * 10 + 8)).collect();
        let bounded = bounded_regions(regions.clone(), 256);
        assert_eq!(bounded.len(), 256);
        assert!(regions.into_iter().all(|r| bounded.iter().any(|b| contains(*b, r))));
    }
    #[test]
    fn a_dense_censor_finishes_its_reveal_without_a_long_unresponsive_cascade() {
        let animation = Anim { started: Instant::now(), areas: vec![DocRect::new(0, 0, 30, 20); 256], showing: true };
        assert!(animation.duration() < 2.0);
        assert!(animation.stagger() < STAGGER);
    }
    #[test]
    fn the_censor_button_turns_off_any_level_and_restores_the_last_choice() {
        for level in [Censor::Light, Censor::Hard, Censor::Full] {
            assert_eq!(SmartUi::Off.toggled(level), Some(level));
            assert_eq!(SmartUi::On(level, 3).toggled(level), None);
            assert_eq!(SmartUi::Scanning(level).toggled(level), None);
        }
    }
}
