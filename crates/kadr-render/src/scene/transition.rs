//! Toolbar choreography. The panel grows out of the selection edge with a soft pop; one selection
//! pill and one swatch ring glide between items instead of cross-fading; hover lights up at once
//! and lets go slowly; a new parameter row replaces the old one as a short cascade.
use super::*;
use crate::motion::{Curve, Spring};

/// Delay between neighbours of an entering row.
const STAGGER: f64 = 0.014;
/// Leaving parameters are gone (opacity < 0.02 with `FADE_OUT`) before different glyphs take their slots.
const HANDOVER: f64 = 0.075;

/// CSS `cubic-bezier()` timing, for the motions the reference defines by duration rather than by spring.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Ease(f32, f32, f32, f32);
/// The reference's `--ease`: Apple's quick start and long, quiet settle.
const EASE: Ease = Ease(0.32, 0.72, 0.0, 1.0);
/// `--ease-out`, for opacity.
const EASE_OUT: Ease = Ease(0.2, 0.8, 0.2, 1.0);
impl Ease {
    fn at(self, x: f32) -> f32 {
        let Ease(x1, y1, x2, y2) = self;
        let bezier =
            |t: f32, a: f32, b: f32| 3.0 * a * t * (1.0 - t) * (1.0 - t) + 3.0 * b * t * t * (1.0 - t) + t * t * t;
        let x = x.clamp(0.0, 1.0);
        let (mut lo, mut hi) = (0.0f32, 1.0f32);
        for _ in 0..24 {
            let mid = (lo + hi) / 2.0;
            if bezier(mid, x1, x2) < x {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        bezier((lo + hi) / 2.0, y1, y2)
    }
}
/// A timed transition; retargeting starts the new one from wherever the old one is.
#[derive(Clone, Copy, Debug)]
struct Tween {
    from: f32,
    to: f32,
    start: f64,
    duration: f64,
    ease: Ease,
}
impl Tween {
    fn at(value: f32) -> Self {
        Self { from: value, to: value, start: 0.0, duration: 0.0, ease: EASE }
    }
    fn value(&self, now: f64) -> f32 {
        let t = if self.duration > 0.0 { ((now - self.start) / self.duration) as f32 } else { 1.0 };
        if t >= 1.0 {
            self.to
        } else if t <= 0.0 {
            self.from
        } else {
            self.from + (self.to - self.from) * self.ease.at(t)
        }
    }
    /// Toward `to` after `delay`, over `duration` seconds.
    fn target(&mut self, to: f32, now: f64, (delay, duration): (f64, f64), ease: Ease) {
        if self.to == to {
            return;
        }
        self.from = self.value(now);
        self.to = to;
        self.start = now + delay;
        self.duration = duration;
        self.ease = ease;
    }
    fn active(&self, now: f64) -> bool {
        now < self.start + self.duration && self.from != self.to
    }
}

/// The censor drawer, after the reference's `.drawer`: its row grows from nothing (content clipped
/// to it), the rows after it slide along, and the content fades in rising from −8 px at 98.5 %.
/// Closing is quicker than opening.
#[derive(Clone, Copy)]
struct Drawer {
    /// What the drawer adds to the panel when open: its row and the gap after it.
    height: f32,
    /// The row alone.
    row: f32,
    /// Where it opens: from the panel's top edge, or from its bottom edge when the panel grows upward.
    offset: f32,
    up: bool,
    /// 0 closed → 1 open: the row's height.
    open: Tween,
    /// Content opacity.
    fade: Tween,
    /// Content rise and scale.
    rise: Tween,
}
impl Drawer {
    fn new(open: bool) -> Self {
        let v = f32::from(open);
        Self {
            height: 0.0,
            row: 0.0,
            offset: 0.0,
            up: false,
            open: Tween::at(v),
            fade: Tween::at(v),
            rise: Tween::at(v),
        }
    }
    fn toward(&mut self, open: bool, now: f64) {
        if open {
            self.open.target(1.0, now, (0.0, 0.42), EASE);
            self.rise.target(1.0, now, (0.0, 0.42), EASE);
            self.fade.target(1.0, now, (0.06, 0.24), EASE_OUT);
        } else {
            self.open.target(0.0, now, (0.0, 0.26), EASE);
            self.rise.target(0.0, now, (0.0, 0.26), EASE);
            self.fade.target(0.0, now, (0.0, 0.16), EASE_OUT);
        }
    }
    fn active(&self, now: f64) -> bool {
        self.open.active(now) || self.fade.active(now) || self.rise.active(now)
    }
    /// How far the panel has grown, in pixels.
    fn extent(&self, now: f64) -> f32 {
        self.open.value(now) * self.height
    }
}
/// The censor row and the eye beside it fold away together.
fn in_drawer(b: ToolbarButton) -> bool {
    matches!(b, ToolbarButton::Censor(_) | ToolbarButton::CensorOff | ToolbarButton::Original)
}

/// A popover grows out of its trigger, after the reference's `.popover`: 96 % → 100 % and a fade,
/// its transform origin on the trigger's centre; it leaves faster than it came.
#[derive(Clone, Copy)]
struct Popover {
    /// Rect in the panel's folded coordinates, and how the drawer moves it.
    rect: D2D_RECT_F,
    shift: f32,
    menu: Option<BarMenu>,
    origin: f32,
    below: bool,
    grow: Tween,
    fade: Tween,
}
impl Popover {
    fn close(&mut self, now: f64) {
        self.grow.target(0.0, now, (0.0, 0.16), EASE);
        self.fade.target(0.0, now, (0.0, 0.12), EASE_OUT);
    }
    fn active(&self, now: f64) -> bool {
        self.grow.active(now) || self.fade.active(now)
    }
}
/// A popover as drawn this frame: its surface and controls scale by `scale` around `origin`,
/// then move by `dy`, at `opacity`.
#[derive(Clone, Copy)]
pub(super) struct PopoverPose {
    pub rect: D2D_RECT_F,
    pub menu: Option<BarMenu>,
    pub opacity: f32,
    pub scale: f32,
    pub origin: (f32, f32),
    pub dy: f32,
}
/// The drawer as drawn this frame: content is clipped to `clip`, scaled by `scale` around `origin`
/// (its opacity is already in the controls').
#[derive(Clone, Copy)]
pub(super) struct DrawerPose {
    pub clip: D2D_RECT_F,
    pub scale: f32,
    pub origin: (f32, f32),
    pub opacity: f32,
}

#[derive(Clone)]
struct Bounds([Spring; 4]);
impl Bounds {
    fn new(r: D2D_RECT_F, curve: Curve) -> Self {
        Self([r.left, r.top, r.right, r.bottom].map(|v| Spring::with(v, curve)))
    }
    fn target(&mut self, r: D2D_RECT_F, now: f64, start: f64) {
        for (s, v) in self.0.iter_mut().zip([r.left, r.top, r.right, r.bottom]) {
            s.target_at(v, now, start);
        }
    }
    fn value(&self, now: f64) -> D2D_RECT_F {
        let [l, t, r, b] = self.0.map(|s| s.value(now));
        rect(l, t, r, b)
    }
    fn active(&self, now: f64) -> bool {
        self.0.iter().any(|s| s.active(now))
    }
    fn liquid_target(&mut self, r: D2D_RECT_F, now: f64) {
        if self.0.iter().zip([r.left, r.top, r.right, r.bottom]).all(|(s, v)| s.goal() == v) {
            return;
        }
        let at = self.value(now);
        let dx = r.left + r.right - at.left - at.right;
        let dy = r.top + r.bottom - at.top - at.bottom;
        for (i, (spring, value)) in self.0.iter_mut().zip([r.left, r.top, r.right, r.bottom]).enumerate() {
            let lead = match i {
                0 => dx < 0.0,
                1 => dy < 0.0,
                2 => dx > 0.0,
                _ => dy > 0.0,
            };
            let distance = if i % 2 == 0 { dx } else { dy };
            let (k, c): (f32, f32) = if lead || distance.abs() < 1.0 { (640.0, 40.0) } else { (230.0, 26.0) };
            spring.target_with(
                value,
                Curve { response: std::f32::consts::TAU / k.sqrt(), damping: c / (2.0 * k.sqrt()) },
                now,
            );
        }
    }
    fn liquid_value(&self, now: f64) -> D2D_RECT_F {
        let mut r = self.value(now);
        let width = self.0[2].goal() - self.0[0].goal();
        let height = self.0[3].goal() - self.0[1].goal();
        let thin = (height * 0.16).min(((r.right - r.left) - width).max(0.0) * 0.045);
        let narrow = (width * 0.16).min(((r.bottom - r.top) - height).max(0.0) * 0.045);
        r.top += thin;
        r.bottom -= thin;
        r.left += narrow;
        r.right -= narrow;
        r
    }
}
struct Control {
    id: usize,
    button: ToolbarButton,
    bounds: Bounds,
    destination: D2D_RECT_F,
    label: Text,
    opacity: Spring,
    hover: Spring,
    selected: Spring,
    /// A tap's give: kicked on press, springs back with a small rebound.
    press: Spring,
    /// When the pointer arrived; tooltips wait for a resting pointer.
    hover_since: f64,
    live: bool,
    exiting: bool,
    enabled: bool,
    property: bool,
    /// How the drawer moves it: +1 after a drawer that grows down, −1 before one that grows up.
    shift: f32,
    drawer: bool,
    popup: bool,
}
/// A resting pointer gets a tooltip after this long; moving to a neighbour while one is shown does not wait.
const TIP_DELAY: f64 = 0.5;
const TIP_WARM: f64 = 0.3;
pub(super) struct Button {
    pub button: ToolbarButton,
    pub rect: D2D_RECT_F,
    pub label: Text,
    pub opacity: f32,
    pub hover: f32,
    pub selected: f32,
    /// How far the control gives under a tap: 0 at rest, about 1 at the deepest, below 0 on the rebound.
    pub press: f32,
    pub live: bool,
    pub enabled: bool,
    pub property: bool,
    /// Part of the censor drawer: drawn inside `Layout::drawer`.
    pub drawer: bool,
}
pub(super) struct Layout {
    /// Open or closing popovers, the leaving one first.
    pub popovers: Vec<PopoverPose>,
    pub drawer: Option<DrawerPose>,
    pub radios: [Option<D2D_RECT_F>; 3],
    pub panel: D2D_RECT_F,
    pub opacity: f32,
    /// Appearance transform around `anchor`: uniform scale, then vertical lift in pixels.
    pub scale: f32,
    pub lift: f32,
    pub anchor: (f32, f32),
    /// Pill under the selected tool of the main row.
    pub indicator: Option<D2D_RECT_F>,
    /// Slot of the selected colour; the ring is drawn there with that swatch's opacity.
    pub swatch: Option<D2D_RECT_F>,
    /// Tooltip target in panel coordinates and its opacity.
    pub tip: Option<(ToolbarButton, D2D_RECT_F, f32)>,
    pub buttons: Vec<Button>,
    pub context: Option<Text>,
}
impl Layout {
    pub fn property_clip(&self) -> Option<D2D_RECT_F> {
        let top = self
            .buttons
            .iter()
            .filter(|c| c.live && !c.property && matches!(c.button, ToolbarButton::Tool(_)))
            .map(|c| c.rect.bottom)
            .max_by(f32::total_cmp)?;
        let bottom = self
            .buttons
            .iter()
            .filter(|c| c.live && matches!(c.button, ToolbarButton::Copy | ToolbarButton::Undo | ToolbarButton::Recent))
            .map(|c| c.rect.top)
            .min_by(f32::total_cmp)?;
        let gap = self.buttons.first().map_or(4.0, |c| (c.rect.bottom - c.rect.top) / 10.0);
        (bottom > top + 2.0 * gap).then_some(rect(self.panel.left, top + gap, self.panel.right, bottom - gap))
    }
    /// Screen point → panel coordinates (inverse of the appearance transform).
    pub fn unproject(&self, x: f32, y: f32) -> (f32, f32) {
        let (ax, ay) = self.anchor;
        (ax + (x - ax) / self.scale, ay + (y - self.lift - ay) / self.scale)
    }
    /// Panel coordinates → screen; used for the tooltip, which is drawn untransformed.
    pub fn project(&self, r: D2D_RECT_F) -> D2D_RECT_F {
        let (ax, ay) = self.anchor;
        let p = |x: f32, y: f32| (ax + (x - ax) * self.scale, ay + (y - ay) * self.scale + self.lift);
        let (l, t) = p(r.left, r.top);
        let (rr, b) = p(r.right, r.bottom);
        rect(l, t, rr, b)
    }
}
#[derive(Default)]
pub(super) struct State {
    radios: [Option<Bounds>; 3],
    panel: Option<Bounds>,
    opacity: Option<Spring>,
    appear: Option<Spring>,
    indicator: Option<Bounds>,
    swatch: Option<Bounds>,
    controls: Vec<Control>,
    hovered: Option<ToolbarButton>,
    last_tip: Option<f64>,
    drawer: Option<Drawer>,
    popover: Option<Popover>,
    leaving: Option<Popover>,
}
fn shifted(mut r: D2D_RECT_F, dy: f32) -> D2D_RECT_F {
    r.top += dy;
    r.bottom += dy;
    r
}
fn code(b: ToolbarButton) -> usize {
    match b {
        ToolbarButton::Surface => 0,
        ToolbarButton::Copy => 1,
        ToolbarButton::Save => 2,
        ToolbarButton::SaveAs => 3,
        ToolbarButton::Pin => 4,
        ToolbarButton::Recent => 5,
        ToolbarButton::Close => 6,
        ToolbarButton::Text => 7,
        ToolbarButton::Translate => 8,
        ToolbarButton::Smart => 9,
        ToolbarButton::Record => 10,
        ToolbarButton::Tool(t) => 16 + t as usize,
        ToolbarButton::TextPlain => 52,
        ToolbarButton::TextFilled => 53,
        ToolbarButton::TextOutline => 54,
        ToolbarButton::TextCallout => 55,
        ToolbarButton::Color(i) => 40 + i,
        ToolbarButton::Width => 48,
        ToolbarButton::Strength => 49,
        ToolbarButton::StrengthStep(false) => 78,
        ToolbarButton::StrengthStep(true) => 79,
        ToolbarButton::Undo => 50,
        ToolbarButton::Redo => 51,
        ToolbarButton::Crop => 56,
        ToolbarButton::Menu(m) => 64 + m as usize,
        ToolbarButton::Censor(c) => 70 + c as usize,
        ToolbarButton::CensorOff => 74,
        ToolbarButton::WidthValue(w) => 80 + w as usize,
        ToolbarButton::Original => 90,
        ToolbarButton::About => 91,
        ToolbarButton::PinWindow => 92,
        ToolbarButton::RecentClear => 93,
        ToolbarButton::SystemAudio => 96,
        ToolbarButton::Microphone => 97,
        ToolbarButton::RecordCursor => 99,
        ToolbarButton::StartRecord => 98,
        ToolbarButton::RecentOpen(i) => 128 + i * 3,
        ToolbarButton::RecentPin(i) => 129 + i * 3,
        ToolbarButton::RecentDelete(i) => 130 + i * 3,
    }
}
fn property(b: ToolbarButton, occurrence: usize) -> bool {
    matches!(
        b,
        ToolbarButton::TextPlain
            | ToolbarButton::TextFilled
            | ToolbarButton::TextOutline
            | ToolbarButton::TextCallout
            | ToolbarButton::Color(_)
            | ToolbarButton::Width
            | ToolbarButton::Strength
            | ToolbarButton::Tool(Tool::BlurEllipse | Tool::BlurBrush | Tool::Spotlight)
    ) || (b == ToolbarButton::Tool(Tool::Blur) && occurrence > 0)
}
fn enabled(b: ToolbarButton, e: Option<EditorBar>) -> bool {
    e.is_none_or(|e| match b {
        ToolbarButton::Undo => e.undo,
        ToolbarButton::Redo => e.redo,
        ToolbarButton::StrengthStep(false) => e.strength > 0,
        ToolbarButton::StrengthStep(true) => e.strength < 2,
        _ => true,
    })
}
fn selected(b: ToolbarButton, property: bool, e: Option<EditorBar>) -> bool {
    match b {
        ToolbarButton::Tool(t) => e.is_some_and(|e| e.tool == t || (t == Tool::Blur && e.tool.is_blur() && !property)),
        ToolbarButton::Color(i) => e.is_some_and(|e| e.color == i),
        ToolbarButton::TextPlain => {
            e.is_some_and(|e| !e.callout && e.text_style == kadr_doc::annotation::TextStyle::Plain)
        }
        ToolbarButton::TextFilled => {
            e.is_some_and(|e| !e.callout && e.text_style == kadr_doc::annotation::TextStyle::Filled)
        }
        ToolbarButton::TextOutline => {
            e.is_some_and(|e| !e.callout && e.text_style == kadr_doc::annotation::TextStyle::Outline)
        }
        ToolbarButton::TextCallout => e.is_some_and(|e| e.callout),
        ToolbarButton::Smart => e.is_some_and(|e| e.smart != SmartUi::Off),
        ToolbarButton::Pin => e.is_some_and(|e| e.ui.pinned),
        ToolbarButton::Original => e.is_some_and(|e| e.ui.original),
        ToolbarButton::SystemAudio => e.is_some_and(|e| e.ui.system_audio),
        ToolbarButton::Microphone => e.is_some_and(|e| e.ui.microphone),
        ToolbarButton::RecordCursor => e.is_some_and(|e| e.ui.cursor),
        ToolbarButton::WidthValue(w) => e.is_some_and(|e| e.width == w),
        ToolbarButton::Censor(level) => {
            e.is_some_and(|e| matches!(e.smart, SmartUi::On(c, _) | SmartUi::Scanning(c) if c == level))
        }
        ToolbarButton::CensorOff => e.is_some_and(|e| e.smart == SmartUi::Open),
        ToolbarButton::Menu(BarMenu::Shapes) => {
            e.is_some_and(|e| matches!(e.tool, Tool::Line | Tool::Rectangle | Tool::Ellipse))
        }
        ToolbarButton::Menu(BarMenu::Blur) => {
            e.is_some_and(|e| matches!(e.tool, Tool::Blur | Tool::BlurEllipse | Tool::BlurBrush))
        }
        ToolbarButton::Menu(BarMenu::More) => {
            e.is_some_and(|e| matches!(e.tool, Tool::Eraser | Tool::Lens | Tool::Spotlight | Tool::Redact))
        }
        _ => false,
    }
}
/// Ids follow the order of the target layout; a second occurrence of a button gets its own id.
fn ids(target: &ToolbarLayout) -> Vec<usize> {
    let mut occurrences = [0usize; 4096];
    target
        .buttons
        .iter()
        .map(|(b, _, _)| {
            let base = code(*b);
            occurrences[base] += 1;
            base + (occurrences[base] - 1) * 4096
        })
        .collect()
}
/// Grow out of the selection edge the panel is attached to.
fn anchor(panel: D2D_RECT_F, sel: D2D_RECT_F) -> ((f32, f32), f32) {
    let x = (panel.left + panel.right) / 2.0;
    if panel.top >= sel.bottom - 1.0 {
        ((x, panel.top), -1.0)
    } else if panel.bottom <= sel.top + 1.0 {
        ((x, panel.bottom), 1.0)
    } else {
        ((x, (panel.top + panel.bottom) / 2.0), 0.0)
    }
}
/// Slot of the first item matching `pick` in a pose.
fn slot(
    buttons: &[(ToolbarButton, D2D_RECT_F, bool)],
    pick: impl Fn(ToolbarButton, bool) -> bool,
) -> Option<D2D_RECT_F> {
    buttons.iter().find(|(b, _, p)| pick(*b, *p)).map(|(_, r, _)| *r)
}
fn glide(state: &mut Option<Bounds>, goal: Option<D2D_RECT_F>, now: f64) -> Option<D2D_RECT_F> {
    match goal {
        None => *state = None,
        Some(r) => match state {
            Some(b) => b.target(r, now, now),
            None => *state = Some(Bounds::new(r, Curve::GLIDE)),
        },
    }
    state.as_ref().map(|b| b.value(now))
}
fn liquid(state: &mut Option<Bounds>, goal: Option<D2D_RECT_F>, now: f64) -> Option<D2D_RECT_F> {
    let Some(r) = goal else {
        *state = None;
        return None;
    };
    let state = state.get_or_insert_with(|| Bounds::new(r, Curve::GLIDE));
    state.liquid_target(r, now);
    Some(state.liquid_value(now))
}

impl State {
    pub fn clear(&mut self) {
        *self = Self::default();
    }
    pub fn active(&self, now: f64) -> bool {
        self.panel.as_ref().is_some_and(|p| p.active(now))
            || self.opacity.is_some_and(|p| p.active(now))
            || self.appear.is_some_and(|p| p.active(now))
            || self.indicator.as_ref().is_some_and(|p| p.active(now))
            || self.radios.iter().flatten().any(|p| p.active(now))
            || self.swatch.as_ref().is_some_and(|p| p.active(now))
            || self.drawer.is_some_and(|d| d.active(now))
            || self.popover.is_some_and(|p| p.active(now))
            || self.leaving.is_some_and(|p| p.active(now))
            || self.controls.iter().any(|c| {
                c.bounds.active(now)
                    || c.opacity.active(now)
                    || c.hover.active(now)
                    || c.selected.active(now)
                    || c.press.active(now)
                    || (c.live && c.hover.goal() > 0.5 && now - c.hover_since < TIP_DELAY + 0.2)
            })
    }
    /// A tap on `button`: it gives a little and springs back, so the press is felt, not only seen.
    pub fn press(&mut self, button: ToolbarButton, now: f64) {
        for c in self.controls.iter_mut().filter(|c| c.live && c.enabled && c.button == button) {
            c.press.kick(38.0, now);
        }
    }
    pub fn sample(
        &mut self,
        target: &ToolbarLayout,
        e: Option<EditorBar>,
        hover: Option<Option<ToolbarButton>>,
        now: f64,
        s: f32,
        animate: bool,
    ) -> Layout {
        if let Some(hover) = hover {
            self.hovered = hover;
        }
        let ids = ids(target);
        let poses: Vec<_> =
            target.buttons.iter().zip(&ids).map(|((b, r, _), id)| (*b, *r, property(*b, id / 4096))).collect();
        let tool_goal = slot(&poses, |b, p| {
            !p && matches!(
                b,
                ToolbarButton::Tool(_) | ToolbarButton::Menu(BarMenu::Shapes | BarMenu::Blur | BarMenu::More)
            ) && selected(b, p, e)
        });
        let swatch_goal = slot(&poses, |b, p| matches!(b, ToolbarButton::Color(_)) && selected(b, p, e));
        let radio_goals = [
            slot(&poses, |b, p| matches!(b, ToolbarButton::Censor(_) | ToolbarButton::CensorOff) && selected(b, p, e)),
            slot(&poses, |b, p| matches!(b, ToolbarButton::WidthValue(_)) && selected(b, p, e)),
            slot(&poses, |b, p| {
                matches!(
                    b,
                    ToolbarButton::TextPlain
                        | ToolbarButton::TextFilled
                        | ToolbarButton::TextOutline
                        | ToolbarButton::TextCallout
                ) && selected(b, p, e)
            }),
        ];
        let (anchor_point, direction) = anchor(target.panel, target.sel);
        let menu = e.and_then(|e| e.ui.menu);
        let band = drawer_band(target);
        let gap = 8.0 * s;
        if !animate {
            self.drawer = None;
            self.popover = None;
            self.leaving = None;
            return Layout {
                radios: radio_goals,
                popovers: target
                    .popup
                    .map(|p| PopoverPose {
                        rect: p,
                        menu,
                        opacity: 1.0,
                        scale: 1.0,
                        origin: ((p.left + p.right) / 2.0, p.bottom),
                        dy: 0.0,
                    })
                    .into_iter()
                    .collect(),
                drawer: band.map(|(top, row)| DrawerPose {
                    clip: rect(target.panel.left, top, target.panel.right, top + row),
                    scale: 1.0,
                    origin: ((target.panel.left + target.panel.right) / 2.0, top),
                    opacity: 1.0,
                }),
                panel: target.panel,
                opacity: 1.0,
                scale: 1.0,
                lift: 0.0,
                anchor: anchor_point,
                indicator: tool_goal,
                swatch: swatch_goal,
                tip: self
                    .hovered
                    .filter(|h| enabled(*h, e))
                    .and_then(|h| poses.iter().find(|(b, _, _)| *b == h))
                    .map(|(b, r, _)| (*b, *r, 1.0)),
                context: target.context.clone(),
                buttons: target
                    .buttons
                    .iter()
                    .zip(&ids)
                    .map(|((b, r, t), id)| Button {
                        button: *b,
                        rect: *r,
                        label: t.clone(),
                        opacity: 1.0,
                        hover: f32::from(self.hovered == Some(*b)),
                        selected: f32::from(selected(*b, property(*b, id / 4096), e)),
                        press: 0.0,
                        live: true,
                        enabled: enabled(*b, e),
                        property: property(*b, id / 4096),
                        drawer: in_drawer(*b),
                    })
                    .collect(),
            };
        }

        // First pose: the panel appears as one object; its controls do not slide in on their own.
        let fresh = self.panel.is_none();

        // The drawer folds: springs work in the panel's folded coordinates (as if the drawer were
        // closed), and its opening is added on top, so every row moves with one timing.
        let up = direction > 0.5;
        let mut folded = target.panel;
        if let Some((top, row)) = band {
            let height = row + gap;
            if up {
                folded.top += height;
            } else {
                folded.bottom -= height;
            }
            let drawer = self.drawer.get_or_insert_with(|| Drawer::new(fresh));
            drawer.height = height;
            drawer.row = row;
            drawer.up = up;
            drawer.offset = if up { target.panel.bottom - (top + height) } else { top - target.panel.top };
            drawer.toward(true, now);
        } else if let Some(drawer) = &mut self.drawer {
            drawer.toward(false, now);
        }
        let fold = self.drawer.map(|d| {
            let edge = if d.up { folded.bottom - d.offset } else { folded.top + d.offset };
            (d, edge)
        });
        // Target rect → (folded rect, how the drawer moves it).
        let place = |r: D2D_RECT_F, drawer: bool| -> (D2D_RECT_F, f32) {
            match (band, fold) {
                (Some((top, row)), _) => {
                    let height = row + gap;
                    if up {
                        if drawer || r.bottom <= top + 0.5 { (shifted(r, height), -1.0) } else { (r, 0.0) }
                    } else if !drawer && r.top >= top + height - 0.5 {
                        (shifted(r, -height), 1.0)
                    } else {
                        (r, 0.0)
                    }
                }
                (None, Some((d, edge))) if d.up => (r, if r.bottom <= edge + 0.5 { -1.0 } else { 0.0 }),
                (None, Some((_, edge))) => (r, if r.top >= edge - 0.5 { 1.0 } else { 0.0 }),
                (None, None) => (r, 0.0),
            }
        };
        let extent = self.drawer.map_or(0.0, |d| d.extent(now));
        let rise = self.drawer.map_or(0.0, |d| -8.0 * s * (1.0 - d.rise.value(now)));
        let show =
            |r: D2D_RECT_F, shift: f32, drawer: bool| shifted(r, shift * extent + if drawer { rise } else { 0.0 });

        let panel = self.panel.get_or_insert_with(|| Bounds::new(folded, Curve::LAYOUT));
        panel.target(folded, now, now);
        let opacity = self.opacity.get_or_insert_with(|| Spring::with(0.0, Curve::FADE).precise(0.002));
        opacity.target(1.0, now);
        let appear = self.appear.get_or_insert_with(|| Spring::with(0.0, Curve::POP).precise(0.0005));
        appear.target(1.0, now);
        let mut panel_now = panel.value(now);
        if self.drawer.is_some_and(|d| d.up) {
            panel_now.top -= extent;
        } else {
            panel_now.bottom += extent;
        }

        // Popovers: a new one grows from its trigger; the old one shrinks away at the same time.
        let popup = target.popup.map(|p| {
            let (r, shift) = place(p, false);
            let below = p.top >= target.panel.bottom - 1.0;
            let origin = target.trigger.map_or((p.left + p.right) / 2.0, |t| (t.left + t.right) / 2.0);
            (r, shift, below, origin)
        });
        match (popup, &mut self.popover) {
            (Some((r, shift, below, origin)), Some(p)) if p.menu == menu => {
                (p.rect, p.shift, p.below, p.origin) = (r, shift, below, origin);
            }
            (Some((rect, shift, below, origin)), _) => {
                if let Some(mut old) = self.popover.take() {
                    old.close(now);
                    self.leaving = Some(old);
                }
                let mut grow = Tween::at(f32::from(fresh));
                let mut fade = Tween::at(f32::from(fresh));
                grow.target(1.0, now, (0.0, 0.32), EASE);
                fade.target(1.0, now, (0.0, 0.18), EASE_OUT);
                self.popover = Some(Popover { rect, shift, menu, origin, below, grow, fade });
            }
            (None, Some(_)) => {
                let mut old = self.popover.take().unwrap();
                old.close(now);
                self.leaving = Some(old);
            }
            (None, None) => {}
        }
        if self.leaving.is_some_and(|p| !p.active(now) && p.fade.value(now) <= 0.0) {
            self.leaving = None;
        }
        let popovers: Vec<PopoverPose> = self
            .leaving
            .iter()
            .chain(self.popover.iter())
            .map(|p| {
                let rect = show(p.rect, p.shift, false);
                let grow = p.grow.value(now);
                PopoverPose {
                    rect,
                    menu: p.menu,
                    opacity: p.fade.value(now).clamp(0.0, 1.0),
                    scale: 0.96 + 0.04 * grow,
                    origin: (p.origin, if p.below { rect.top } else { rect.bottom }),
                    dy: (1.0 - grow) * 6.0 * s * if p.below { -1.0 } else { 1.0 },
                }
            })
            .collect();

        // A different parameter family hands over: the old one leaves first, the new one cascades in.
        let handover = self
            .controls
            .iter()
            .any(|c| c.property && !ids.contains(&c.id) && (!c.exiting || c.opacity.value(now) > 0.02));
        for c in &mut self.controls {
            c.live = false;
        }
        let mut entering = 0;
        for ((b, target_rect, label), &id) in target.buttons.iter().zip(&ids) {
            let drawer = in_drawer(*b);
            let (r, shift) = place(*target_rect, drawer);
            let r = &r;
            // Popover and drawer contents move with their container, not on their own.
            let contained = drawer || target.popup.is_some_and(|p| contains(p, *target_rect));
            let index = self.controls.iter().position(|c| c.id == id).unwrap_or_else(|| {
                let is_property = property(*b, id / 4096);
                let still = fresh || contained;
                let mut control = Control {
                    id,
                    button: *b,
                    bounds: Bounds::new(if still { *r } else { shifted(*r, 6.0 * s) }, Curve::LAYOUT),
                    destination: *r,
                    label: label.clone(),
                    opacity: Spring::with(if still { 1.0 } else { 0.0 }, Curve::FADE_IN).precise(0.002),
                    hover: Spring::with(0.0, Curve::HOVER_IN).precise(0.002),
                    selected: Spring::with(0.0, Curve::FADE).precise(0.002),
                    press: Spring::with(0.0, Curve::PRESS).precise(0.001),
                    hover_since: now,
                    live: true,
                    exiting: false,
                    enabled: enabled(*b, e),
                    property: is_property,
                    shift,
                    drawer,
                    popup: !drawer && contained,
                };
                if !still {
                    let start = now + if handover && is_property { HANDOVER } else { 0.0 } + STAGGER * entering as f64;
                    entering += 1;
                    control.bounds.target(*r, now, start);
                    control.opacity.target_at(1.0, now, start);
                }
                self.controls.push(control);
                self.controls.len() - 1
            });
            let c = &mut self.controls[index];
            c.live = true;
            c.enabled = enabled(*b, e);
            c.destination = *r;
            c.label = label.clone();
            c.shift = shift;
            c.popup = !drawer && contained;
            if c.exiting {
                // Came back while leaving: continue from the current pose, no restart.
                c.exiting = false;
                c.opacity.target_with(1.0, Curve::FADE, now);
            }
            // Same goal keeps a pending staggered start; a new one moves now.
            c.bounds.target(*r, now, now);
            let hovered = f32::from(c.enabled && self.hovered == Some(*b));
            if hovered > c.hover.goal() {
                c.hover_since = now;
            }
            let curve = if hovered > c.hover.goal() { Curve::HOVER_IN } else { Curve::HOVER_OUT };
            c.hover.target_with(hovered, curve, now);
            c.selected.target(f32::from(selected(*b, c.property, e)), now);
        }
        for c in &mut self.controls {
            if !c.live && !c.exiting {
                c.exiting = true;
                if c.popup {
                    // The popover's own fade carries its contents out.
                    c.opacity.target_at(0.0, now, now + 0.16);
                } else if !c.drawer {
                    c.opacity.target_with(0.0, Curve::FADE_OUT, now);
                    c.bounds.target(shifted(c.destination, -3.0 * s), now, now);
                }
            }
        }
        let drawer_open = self.drawer.is_some();
        self.controls.retain(|c| {
            c.live
                || (c.drawer && drawer_open)
                || (!c.drawer && (c.opacity.active(now) || c.opacity.value(now) > 0.001))
        });

        let tool = tool_goal.map(|r| place(r, false));
        let indicator =
            liquid(&mut self.indicator, tool.map(|t| t.0), now).map(|r| show(r, tool.map_or(0.0, |t| t.1), false));
        let radios = std::array::from_fn(|i| {
            if i == 0 {
                // The level blob stays with its drawer while it folds away.
                let goal = radio_goals[0].map(|r| place(r, true).0);
                if goal.is_none()
                    && let Some(d) = self.drawer
                {
                    return self.radios[0]
                        .as_ref()
                        .map(|b| show(b.liquid_value(now), if d.up { -1.0 } else { 0.0 }, true));
                }
                let shift = radio_goals[0].map_or(0.0, |r| place(r, true).1);
                return liquid(&mut self.radios[0], goal, now).map(|r| show(r, shift, true));
            }
            let goal = radio_goals[i].map(|r| place(r, false));
            liquid(&mut self.radios[i], goal.map(|g| g.0), now).map(|r| show(r, goal.map_or(0.0, |g| g.1), false))
        });
        // The ring appears with its row instead of flying in, and stays in place while the row
        // leaves: it fades with its swatch rather than vanishing a frame early.
        let swatch_goal = swatch_goal.map(|r| place(r, false).0);
        let swatch_shift = target.popup.map_or(0.0, |p| place(p, false).1);
        let swatch = match (&self.swatch, swatch_goal) {
            (None, Some(r)) => {
                self.swatch = Some(Bounds::new(r, Curve::GLIDE));
                Some(r)
            }
            (_, None) => self.swatch.as_ref().map(|b| b.value(now)),
            _ => glide(&mut self.swatch, swatch_goal, now),
        }
        .map(|r| show(r, swatch_shift, false));
        let drawer = self.drawer.map(|d| {
            let top = if d.up { panel_now.bottom - d.offset - extent } else { panel_now.top + d.offset };
            DrawerPose {
                clip: rect(panel_now.left, top, panel_now.right, top + extent * d.row / d.height.max(1.0)),
                scale: 0.985 + 0.015 * d.rise.value(now),
                origin: ((panel_now.left + panel_now.right) / 2.0, top),
                opacity: d.fade.value(now).clamp(0.0, 1.0),
            }
        });
        if self.drawer.is_some_and(|d| band.is_none() && !d.active(now) && d.open.value(now) <= 0.0) {
            self.drawer = None;
            self.radios[0] = None;
        }
        let appear = appear.value(now);
        let warm = self.last_tip.is_some_and(|t| now - t < TIP_WARM);
        let tip = self
            .controls
            .iter()
            .filter(|c| c.live && c.enabled && c.hover.goal() > 0.5)
            .max_by(|a, b| a.hover_since.total_cmp(&b.hover_since))
            .map(|c| {
                let wait = if warm { 0.0 } else { TIP_DELAY };
                let shown = (((now - c.hover_since - wait) / 0.12) as f32).clamp(0.0, 1.0);
                (c.button, show(c.bounds.value(now), c.shift, c.drawer), shown * c.opacity.value(now).clamp(0.0, 1.0))
            })
            .filter(|(_, _, a)| *a > 0.001);
        if tip.is_some_and(|(_, _, a)| a > 0.5) {
            self.last_tip = Some(now);
        }
        let content = drawer.map_or(1.0, |d| d.opacity);
        Layout {
            radios,
            tip,
            popovers,
            drawer,
            panel: panel_now,
            opacity: opacity.value(now).clamp(0.0, 1.0),
            scale: 0.94 + 0.06 * appear,
            lift: (1.0 - appear) * 8.0 * s * direction,
            anchor: anchor_point,
            indicator,
            swatch,
            context: target.context.clone(),
            buttons: self
                .controls
                .iter()
                .map(|c| Button {
                    button: c.button,
                    rect: show(c.bounds.value(now), c.shift, c.drawer),
                    label: c.label.clone(),
                    opacity: c.opacity.value(now).clamp(0.0, 1.0) * if c.drawer { content } else { 1.0 },
                    hover: c.hover.value(now).clamp(0.0, 1.0),
                    selected: c.selected.value(now).clamp(0.0, 1.0),
                    press: c.press.value(now),
                    live: c.live,
                    enabled: c.enabled,
                    property: c.property,
                    drawer: c.drawer,
                })
                .collect(),
        }
    }
}

/// The drawer row in a target layout: its top and height.
fn drawer_band(target: &ToolbarLayout) -> Option<(f32, f32)> {
    let rows = target.buttons.iter().filter(|b| in_drawer(b.0)).map(|b| b.1);
    let top = rows.clone().map(|r| r.top).min_by(f32::total_cmp)?;
    let bottom = rows.map(|r| r.bottom).max_by(f32::total_cmp)?;
    Some((top, bottom - top))
}
fn contains(outer: D2D_RECT_F, inner: D2D_RECT_F) -> bool {
    inner.left >= outer.left - 0.5
        && inner.right <= outer.right + 0.5
        && inner.top >= outer.top - 0.5
        && inner.bottom <= outer.bottom + 0.5
}

/// Overlay-wide motion outside the toolbar: the screen dims in once per session, and the window
/// highlight glides between windows while the user is still picking one.
#[derive(Default)]
pub(super) struct Chrome {
    dim: Option<Spring>,
    highlight: Option<Bounds>,
}
impl Chrome {
    pub fn clear(&mut self) {
        *self = Self::default();
    }
    pub fn active(&self, now: f64) -> bool {
        self.dim.is_some_and(|s| s.active(now)) || self.highlight.as_ref().is_some_and(|b| b.active(now))
    }
    /// 0 → 1 over the first frames after the overlay is shown.
    pub fn dim(&mut self, now: f64, animate: bool) -> f32 {
        if !animate {
            return 1.0;
        }
        let spring = self.dim.get_or_insert_with(|| {
            let mut s = Spring::with(0.0, Curve::DIM).precise(0.002);
            s.target(1.0, now);
            s
        });
        spring.value(now)
    }
    /// Gliding only while hovering windows; a drawn or dragged selection follows the hand exactly.
    pub fn highlight(&mut self, goal: Option<D2D_RECT_F>, glide: bool, now: f64, animate: bool) -> Option<D2D_RECT_F> {
        match goal {
            Some(r) if animate && glide => {
                match &mut self.highlight {
                    Some(b) => b.target(r, now, now),
                    None => self.highlight = Some(Bounds::new(r, Curve::TRACK)),
                }
                // Whole pixels: four scrim rectangles meet at this edge without seams.
                self.highlight.as_ref().map(|b| {
                    let r = b.value(now);
                    rect(r.left.round(), r.top.round(), r.right.round(), r.bottom.round())
                })
            }
            _ => {
                self.highlight = goal.map(|r| Bounds::new(r, Curve::TRACK));
                goal
            }
        }
    }
}

/// The hint card: appears after the pointer has rested a moment, leaves at once. A new kind
/// (no selection → selection) first lets the old card go, then brings the new one.
#[derive(Default)]
pub(super) struct Hint {
    shown: Option<Spring>,
    kind: Option<HintKind>,
}
impl Hint {
    pub fn clear(&mut self) {
        *self = Self::default();
    }
    pub fn active(&self, now: f64) -> bool {
        self.shown.is_some_and(|s| s.active(now))
    }
    /// Opacity and which card to draw.
    pub fn sample(&mut self, want: Option<HintKind>, now: f64, animate: bool) -> (f32, Option<HintKind>) {
        if !animate {
            return (f32::from(want.is_some()), want);
        }
        let s = self.shown.get_or_insert_with(|| Spring::with(0.0, Curve::FADE).precise(0.002));
        if want != self.kind && s.value(now) <= 0.002 {
            // The previous card is gone: switch, and wait a moment before the next one.
            self.kind = want;
            s.snap(0.0);
            if want.is_some() {
                s.target_at(1.0, now, now + 0.45);
            }
        } else if want != self.kind || want.is_none() {
            s.target_with(0.0, Curve::FADE_OUT, now);
        } else if s.goal() < 0.5 {
            s.target_at(1.0, now, now + 0.45);
        }
        (s.value(now), self.kind)
    }
}

#[cfg(test)]
mod concept_motion_tests {
    use super::*;
    #[test]
    fn liquid_selection_stretches_and_can_be_reversed_without_a_jump() {
        let mut b = Bounds::new(rect(0.0, 0.0, 80.0, 44.0), Curve::GLIDE);
        b.liquid_target(rect(240.0, 0.0, 320.0, 44.0), 0.0);
        let frame = b.liquid_value(0.07);
        assert!(frame.right - frame.left > 80.0);
        assert!(frame.top > 0.0 && frame.bottom < 44.0);
        let before = b.value(0.07);
        b.liquid_target(rect(-80.0, 0.0, 0.0, 44.0), 0.07);
        let after = b.value(0.07);
        for (x, y) in [before.left, before.top, before.right, before.bottom].into_iter().zip([
            after.left,
            after.top,
            after.right,
            after.bottom,
        ]) {
            assert!((x - y).abs() < 0.001, "retargeting jumped from {before:?} to {after:?}");
        }
        let end = b.liquid_value(2.0);
        assert!((end.left + 80.0).abs() < 0.01 && end.right.abs() < 0.01);
        let mut state = Some(b);
        assert!(liquid(&mut state, None, 2.0).is_none());
        assert!(state.is_none());
    }
}
