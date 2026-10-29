use super::*;
use crate::motion::Spring;

#[derive(Clone)]
struct Bounds([Spring; 4]);
impl Bounds {
    fn new(r: D2D_RECT_F) -> Self {
        Self([r.left, r.top, r.right, r.bottom].map(|v| Spring::new(v, 32.0)))
    }
    fn target(&mut self, r: D2D_RECT_F, now: f64) {
        for (s, v) in self.0.iter_mut().zip([r.left, r.top, r.right, r.bottom]) {
            s.target(v, now);
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
    live: bool,
    exiting: bool,
    enabled: bool,
    property: bool,
}
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
            .filter(|c| {
                c.live
                    && matches!(
                        c.button,
                        ToolbarButton::Copy | ToolbarButton::Undo | ToolbarButton::Recent
                    )
            })
            .map(|c| c.rect.top)
            .min_by(f32::total_cmp)?;
        let gap = self
            .buttons
            .first()
            .map_or(4.0, |c| (c.rect.bottom - c.rect.top) / 10.0);
        (bottom > top + 2.0 * gap).then_some(rect(self.panel.left, top + gap, self.panel.right, bottom - gap))
    }
}
#[derive(Default)]
pub(super) struct State {
    panel: Option<Bounds>,
    opacity: Option<Spring>,
    controls: Vec<Control>,
    hovered: Option<ToolbarButton>,
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
        ToolbarButton::Tool(t) => 16 + t as usize,
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
        ToolbarButton::Color(_)
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
        _ => false,
    }
}

impl State {
    pub fn clear(&mut self) {
        *self = Self::default();
    }
    pub fn active(&self, now: f64) -> bool {
        self.panel.as_ref().is_some_and(|p| p.active(now))
            || self.opacity.is_some_and(|p| p.active(now))
            || self
                .controls
                .iter()
                .any(|c| c.bounds.active(now) || c.opacity.active(now) || c.hover.active(now) || c.selected.active(now))
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
        if !animate {
            let mut counts = [0usize; 64];
            return Layout {
                panel: target.panel,
                opacity: 1.0,
                context: target.context.clone(),
                buttons: target
                    .buttons
                    .iter()
                    .map(|(b, r, t)| Button {
                        button: *b,
                        rect: *r,
                        label: t.clone(),
                        opacity: 1.0,
                        hover: f32::from(self.hovered == Some(*b)),
                        selected: f32::from(selected(*b, property(*b, counts[code(*b)]), e)),
                        live: true,
                        enabled: enabled(*b, e),
                        property: {
                            let n = counts[code(*b)];
                            counts[code(*b)] += 1;
                            property(*b, n)
                        },
                    })
                    .collect(),
            };
        }
        let panel = self
            .panel
            .get_or_insert_with(|| Bounds::new(shifted(target.panel, 8.0 * s)));
        panel.target(target.panel, now);
        let opacity = self.opacity.get_or_insert_with(|| Spring::new(0.0, 35.0));
        opacity.target(1.0, now);
        for c in &mut self.controls {
            c.live = false;
        }
        let mut occurrences = [0usize; 64];
        for (b, r, label) in &target.buttons {
            let base = code(*b);
            let id = base + occurrences[base] * 64;
            occurrences[base] += 1;
            let index = self.controls.iter().position(|c| c.id == id).unwrap_or_else(|| {
                self.controls.push(Control {
                    id,
                    button: *b,
                    bounds: Bounds::new(shifted(*r, 8.0 * s)),
                    destination: *r,
                    label: label.clone(),
                    opacity: Spring::new(0.0, 35.0),
                    hover: Spring::new(0.0, 48.0),
                    selected: Spring::new(0.0, 40.0),
                    live: true,
                    exiting: false,
                    enabled: enabled(*b, e),
                    property: property(*b, id / 64),
                });
                self.controls.len() - 1
            });
            let c = &mut self.controls[index];
            c.live = true;
            c.exiting = false;
            c.enabled = enabled(*b, e);
            c.destination = *r;
            c.label = label.clone();
            c.bounds.target(*r, now);
            c.opacity.target(1.0, now);
            c.hover.target(f32::from(c.enabled && self.hovered == Some(*b)), now);
            c.selected.target(f32::from(selected(*b, c.property, e)), now);
        }
        for c in &mut self.controls {
            if !c.live && !c.exiting {
                c.exiting = true;
                c.opacity.target(0.0, now);
                c.bounds.target(shifted(c.destination, -6.0 * s), now);
            }
        }
        self.controls
            .retain(|c| c.live || c.opacity.active(now) || c.opacity.value(now) > 0.001);
        Layout {
            panel: panel.value(now),
            opacity: opacity.value(now).clamp(0.0, 1.0),
            context: target.context.clone(),
            buttons: self
                .controls
                .iter()
                .map(|c| Button {
                    button: c.button,
                    rect: c.bounds.value(now),
                    label: c.label.clone(),
                    // Fade the old property family before revealing the new glyphs in the same slots.
                    opacity: if c.property {
                        (c.opacity.value(now) * 2.0 - 1.0).clamp(0.0, 1.0)
                    } else {
                        c.opacity.value(now).clamp(0.0, 1.0)
                    },
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
