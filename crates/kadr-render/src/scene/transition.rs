//! Toolbar choreography. The panel grows out of the selection edge with a soft pop; one selection
//! pill and one swatch ring glide between items instead of cross-fading; hover lights up at once
//! and lets go slowly; a new parameter row replaces the old one as a short cascade.
use super::*;
use crate::motion::{Curve, Spring};

/// Delay between neighbours of an entering row.
const STAGGER: f64 = 0.014;
/// Leaving parameters are gone (opacity < 0.02 with `FADE_OUT`) before different glyphs take their slots.
const HANDOVER: f64 = 0.075;

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
    /// When the pointer arrived; tooltips wait for a resting pointer.
    hover_since: f64,
    live: bool,
    exiting: bool,
    enabled: bool,
    property: bool,
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
    pub live: bool,
    pub enabled: bool,
    pub property: bool,
}
pub(super) struct Layout {
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
    panel: Option<Bounds>,
    opacity: Option<Spring>,
    appear: Option<Spring>,
    indicator: Option<Bounds>,
    swatch: Option<Bounds>,
    controls: Vec<Control>,
    hovered: Option<ToolbarButton>,
    last_tip: Option<f64>,
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
        ToolbarButton::Undo => 50,
        ToolbarButton::Redo => 51,
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
        _ => true,
    })
}
fn selected(b: ToolbarButton, property: bool, e: Option<EditorBar>) -> bool {
    match b {
        ToolbarButton::Tool(t) => e.is_some_and(|e| e.tool == t || (t == Tool::Blur && e.tool.is_blur() && !property)),
        ToolbarButton::Color(i) => e.is_some_and(|e| e.color == i),
        ToolbarButton::TextPlain => e.is_some_and(|e| e.text_style == kadr_doc::annotation::TextStyle::Plain),
        ToolbarButton::TextFilled => e.is_some_and(|e| e.text_style == kadr_doc::annotation::TextStyle::Filled),
        ToolbarButton::TextOutline => e.is_some_and(|e| e.text_style == kadr_doc::annotation::TextStyle::Outline),
        ToolbarButton::TextCallout => e.is_some_and(|e| e.callout),
        ToolbarButton::Smart => e.is_some_and(|e| e.smart != SmartUi::Off),
        _ => false,
    }
}
/// Ids follow the order of the target layout; a second occurrence of a button gets its own id.
fn ids(target: &ToolbarLayout) -> Vec<usize> {
    let mut occurrences = [0usize; 64];
    target
        .buttons
        .iter()
        .map(|(b, _, _)| {
            let base = code(*b);
            occurrences[base] += 1;
            base + (occurrences[base] - 1) * 64
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

impl State {
    pub fn clear(&mut self) {
        *self = Self::default();
    }
    pub fn active(&self, now: f64) -> bool {
        self.panel.as_ref().is_some_and(|p| p.active(now))
            || self.opacity.is_some_and(|p| p.active(now))
            || self.appear.is_some_and(|p| p.active(now))
            || self.indicator.as_ref().is_some_and(|p| p.active(now))
            || self.swatch.as_ref().is_some_and(|p| p.active(now))
            || self.controls.iter().any(|c| {
                c.bounds.active(now)
                    || c.opacity.active(now)
                    || c.hover.active(now)
                    || c.selected.active(now)
                    || (c.live && c.hover.goal() > 0.5 && now - c.hover_since < TIP_DELAY + 0.2)
            })
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
            target.buttons.iter().zip(&ids).map(|((b, r, _), id)| (*b, *r, property(*b, id / 64))).collect();
        let tool_goal = slot(&poses, |b, p| !p && matches!(b, ToolbarButton::Tool(_)) && selected(b, p, e));
        let swatch_goal = slot(&poses, |b, p| matches!(b, ToolbarButton::Color(_)) && selected(b, p, e));
        let (anchor_point, direction) = anchor(target.panel, target.sel);
        if !animate {
            return Layout {
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
                        selected: f32::from(selected(*b, property(*b, id / 64), e)),
                        live: true,
                        enabled: enabled(*b, e),
                        property: property(*b, id / 64),
                    })
                    .collect(),
            };
        }

        // First pose: the panel appears as one object; its controls do not slide in on their own.
        let fresh = self.panel.is_none();
        let panel = self.panel.get_or_insert_with(|| Bounds::new(target.panel, Curve::LAYOUT));
        panel.target(target.panel, now, now);
        let opacity = self.opacity.get_or_insert_with(|| Spring::with(0.0, Curve::FADE).precise(0.002));
        opacity.target(1.0, now);
        let appear = self.appear.get_or_insert_with(|| Spring::with(0.0, Curve::POP).precise(0.0005));
        appear.target(1.0, now);

        // A different parameter family hands over: the old one leaves first, the new one cascades in.
        let handover = self
            .controls
            .iter()
            .any(|c| c.property && !ids.contains(&c.id) && (!c.exiting || c.opacity.value(now) > 0.02));
        for c in &mut self.controls {
            c.live = false;
        }
        let mut entering = 0;
        for ((b, r, label), &id) in target.buttons.iter().zip(&ids) {
            let index = self.controls.iter().position(|c| c.id == id).unwrap_or_else(|| {
                let is_property = property(*b, id / 64);
                let mut control = Control {
                    id,
                    button: *b,
                    bounds: Bounds::new(if fresh { *r } else { shifted(*r, 6.0 * s) }, Curve::LAYOUT),
                    destination: *r,
                    label: label.clone(),
                    opacity: Spring::with(if fresh { 1.0 } else { 0.0 }, Curve::FADE_IN).precise(0.002),
                    hover: Spring::with(0.0, Curve::HOVER_IN).precise(0.002),
                    selected: Spring::with(0.0, Curve::FADE).precise(0.002),
                    hover_since: now,
                    live: true,
                    exiting: false,
                    enabled: enabled(*b, e),
                    property: is_property,
                };
                if !fresh {
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
                c.opacity.target_with(0.0, Curve::FADE_OUT, now);
                c.bounds.target(shifted(c.destination, -3.0 * s), now, now);
            }
        }
        self.controls.retain(|c| c.live || c.opacity.active(now) || c.opacity.value(now) > 0.001);

        let indicator = glide(&mut self.indicator, tool_goal, now);
        // The ring appears with its row instead of flying in, and stays in place while the row
        // leaves: it fades with its swatch rather than vanishing a frame early.
        let swatch = match (&self.swatch, swatch_goal) {
            (None, Some(r)) => {
                self.swatch = Some(Bounds::new(r, Curve::GLIDE));
                Some(r)
            }
            (_, None) => self.swatch.as_ref().map(|b| b.value(now)),
            _ => glide(&mut self.swatch, swatch_goal, now),
        };
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
                (c.button, c.bounds.value(now), shown * c.opacity.value(now).clamp(0.0, 1.0))
            })
            .filter(|(_, _, a)| *a > 0.001);
        if tip.is_some_and(|(_, _, a)| a > 0.5) {
            self.last_tip = Some(now);
        }
        Layout {
            tip,
            panel: panel.value(now),
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
                    rect: c.bounds.value(now),
                    label: c.label.clone(),
                    opacity: c.opacity.value(now).clamp(0.0, 1.0),
                    hover: c.hover.value(now).clamp(0.0, 1.0),
                    selected: c.selected.value(now).clamp(0.0, 1.0),
                    live: c.live,
                    enabled: c.enabled,
                    property: c.property,
                })
                .collect(),
        }
    }
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
