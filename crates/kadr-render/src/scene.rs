//! Отрисовка сцены оверлея. Одна функция и для окна, и для `--snapshot`.
//! Координаты — физические пиксели поверхности; размеры из `theme::dip` умножаются на масштаб.

use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct2D::Common::{D2D_RECT_F, D2D1_BORDER_MODE_HARD, D2D1_COLOR_F, D2D1_COMPOSITE_MODE_SOURCE_OVER};
use windows::Win32::Graphics::Direct2D::{
    CLSID_D2D1GaussianBlur, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
    D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_ELLIPSE, D2D1_GAUSSIANBLUR_PROP_BORDER_MODE,
    D2D1_GAUSSIANBLUR_PROP_STANDARD_DEVIATION, D2D1_INTERPOLATION_MODE_LINEAR, D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR,
    D2D1_LAYER_OPTIONS1_NONE, D2D1_LAYER_PARAMETERS1, D2D1_PROPERTY_TYPE_ENUM, D2D1_PROPERTY_TYPE_FLOAT, D2D1_ROUNDED_RECT,
    ID2D1Bitmap1, ID2D1Effect, ID2D1Geometry, ID2D1Layer,
    ID2D1StrokeStyle,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT, DWRITE_FONT_WEIGHT_REGULAR,
    DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_LINE_METRICS, DWRITE_TEXT_METRICS, DWRITE_TEXT_RANGE,
    DWRITE_WORD_WRAPPING_NO_WRAP, IDWriteFontCollection, IDWriteTextFormat, IDWriteTextLayout,
};
use std::mem::ManuallyDrop;
use windows::core::{Interface, PCWSTR, Result, w};
use windows_numerics::{Matrix3x2, Vector2};

use crate::annotations::{self, Cache, View};
use crate::gpu::{Gpu, Shared};
use crate::theme::{Theme, dip};
use kadr_doc::annotation::{COLORS, Tool};
mod glyphs;
mod transition;

struct Style {
    format: IDWriteTextFormat,
    /// Табличные цифры: числа стоят разряд под разрядом.
    tabular: bool,
}

/// Четыре роли текста в одном семействе: подписи, заголовок, цифры и главная цифра.
pub struct TextStyles {
    label: Style,
    title: Style,
    figure: Style,
    lead: Style,
    button: Style,
    toolbar: std::cell::RefCell<Option<(ToolbarKey, Option<ToolbarLayout>)>>,
    motion: std::cell::RefCell<transition::State>,
    chrome: std::cell::RefCell<transition::Chrome>,
    /// Frosted backdrop of the toolbar; created on first use by this surface's device context.
    backdrop: std::cell::RefCell<Option<ID2D1Effect>>,
    animate: std::cell::Cell<bool>,
    epoch: std::time::Instant,
    time_override: std::cell::Cell<Option<f64>>,
}

impl TextStyles {
    pub fn new(shared: &Shared, scale: f32) -> Result<Self> {
        let style = |weight, size: f32, tabular| -> Result<Style> {
            Ok(Style {
                format: text_format(shared, weight, size * scale)?,
                tabular,
            })
        };
        Ok(Self {
            label: style(DWRITE_FONT_WEIGHT_REGULAR, dip::TEXT, false)?,
            title: style(DWRITE_FONT_WEIGHT_SEMI_BOLD, dip::TITLE, false)?,
            figure: style(DWRITE_FONT_WEIGHT_REGULAR, dip::TEXT, true)?,
            lead: style(DWRITE_FONT_WEIGHT_REGULAR, dip::LEAD, true)?,
            button: style(DWRITE_FONT_WEIGHT_REGULAR, dip::TITLE, false)?,
            toolbar: Default::default(),
            motion: Default::default(),
            chrome: Default::default(),
            backdrop: Default::default(),
            animate: std::cell::Cell::new(false),
            epoch: std::time::Instant::now(),
            time_override: std::cell::Cell::new(None),
        })
    }
    /// Static by default for exports/fixtures; only interactive surfaces enable motion.
    pub fn enable_motion(&self, enabled: bool) {
        if self.animate.replace(enabled) != enabled {
            self.reset_motion();
        }
    }
    /// New session or closed overlay: every transition starts over, including the dim-in.
    pub fn reset_motion(&self) {
        self.motion.borrow_mut().clear();
        self.chrome.borrow_mut().clear();
    }
    fn reset_toolbar_motion(&self) {
        self.motion.borrow_mut().clear();
    }
    pub fn animations_pending(&self) -> bool {
        let now = self.animation_time();
        self.animate.get() && (self.motion.borrow().active(now) || self.chrome.borrow().active(now))
    }
    pub fn set_animation_time(&self, seconds: Option<f64>) {
        self.time_override.set(seconds);
    }
    fn animation_time(&self) -> f64 {
        self.time_override
            .get()
            .unwrap_or_else(|| self.epoch.elapsed().as_secs_f64())
    }
    fn displayed(
        &self,
        target: &ToolbarLayout,
        e: Option<EditorBar>,
        hover: Option<Option<ToolbarButton>>,
        s: f32,
    ) -> transition::Layout {
        self.motion
            .borrow_mut()
            .sample(target, e, hover, self.animation_time(), s, self.animate.get())
    }
}

/// Строка таблицы HUD: подпись, число (по правому краю) и единица серым суффиксом.
pub struct HudRow {
    pub label: String,
    pub value: String,
    pub unit: String,
}

impl HudRow {
    pub fn new(label: &str, value: String, unit: &str) -> Self {
        Self {
            label: label.into(),
            value,
            unit: unit.into(),
        }
    }
}

/// Панель замеров. Её вопрос — «сколько от хоткея до экрана», и ответ на него — самая крупная цифра.
pub struct Hud {
    /// Имя продукта.
    pub title: String,
    /// Контекст справа от имени: этап, источник, уровень целостности, режим.
    pub scope: String,
    pub lead_label: String,
    /// Главная цифра и её единица; `None` — замера ещё нет, рисуется серый прочерк.
    pub lead: Option<(String, String)>,
    /// Группы строк, разделённые линиями.
    pub groups: Vec<Vec<HudRow>>,
    /// Горячие клавиши всегда на виду: видимых кнопок у оверлея нет.
    pub hint: String,
}

/// Лупа у курсора: увеличенные пиксели, координаты и цвет под курсором.
pub struct Magnifier {
    /// Курсор в координатах экрана.
    pub at: (i32, i32),
    /// Цвет пикселя под курсором (RGB); `None`, пока копия кадра в памяти не готова.
    pub rgb: Option<[u8; 3]>,
}

/// Кнопки панели у выделения. Основное действие — первое и единственное с заливкой.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolbarButton {
    /// Padding/disabled controls consume input so the image behind the panel is never edited.
    Surface,
    Copy,
    Save,
    SaveAs,
    Pin,
    Recent,
    Close,
    Tool(Tool),
    Color(usize),
    Width,
    Strength,
    Undo,
    Redo,
}

impl ToolbarButton {
    pub const ALL: [ToolbarButton; 6] = [
        Self::Copy,
        Self::Save,
        Self::SaveAs,
        Self::Pin,
        Self::Recent,
        Self::Close,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Surface => "",
            Self::Copy => "Копировать",
            Self::Save => "Сохранить",
            Self::SaveAs => "Сохранить как…",
            Self::Pin => "Закрепить",
            Self::Recent => "Недавние",
            Self::Close => "Закрыть",
            Self::Tool(tool) => tool.label(),
            Self::Color(_) => "Цвет",
            Self::Width => "Толщина",
            Self::Strength => "Сила",
            Self::Undo => "Отменить · Ctrl+Z",
            Self::Redo => "Повторить · Ctrl+Y",
        }
    }
}

/// Панель кнопок у выделения; `hovered` — кнопка под курсором.
#[derive(Clone, Copy, Default)]
pub struct Toolbar {
    pub hovered: Option<ToolbarButton>,
    pub editor: Option<EditorBar>,
}
#[derive(Clone, Copy, PartialEq)]
pub struct EditorBar {
    pub tool: Tool,
    pub color: usize,
    pub width: u32,
    pub strength: u8,
    pub undo: bool,
    pub redo: bool,
}

/// Что нарисовать поверх кадра. Одинаково для всех мониторов: каждый рисует свою часть.
#[derive(Clone, Copy, Default)]
pub struct Marks<'a> {
    /// В координатах экрана; может выходить за пределы этой поверхности.
    pub selection: Option<RECT>,
    /// Маркеры на рамке — только у готового выделения.
    pub handles: bool,
    pub magnifier: Option<&'a Magnifier>,
    pub toolbar: Option<Toolbar>,
    pub hud: Option<&'a Hud>,
    pub drawing: Option<View<'a>>,
    /// The highlight is a hovered window, not a selection: let it glide between windows.
    pub glide: bool,
}

pub struct Scene<'a> {
    pub frame: Option<&'a ID2D1Bitmap1>,
    /// Левый верхний угол поверхности в координатах экрана.
    pub origin: (i32, i32),
    pub size: (u32, u32),
    pub scale: f32,
    pub marks: Marks<'a>,
}

pub fn draw(
    gpu: &Gpu,
    shared: &Shared,
    text: &TextStyles,
    theme: &Theme,
    scene: &Scene,
    cache: &mut Cache,
) -> Result<()> {
    if scene.marks.toolbar.is_none() {
        text.reset_toolbar_motion();
    }
    let now = text.animation_time();
    let animate = text.animate.get();
    let dim = text.chrome.borrow_mut().dim(now, animate);
    let scrim = fade(theme.scrim, dim);
    let full = rect(0.0, 0.0, scene.size.0 as f32, scene.size.1 as f32);
    unsafe {
        match scene.frame {
            Some(frame) => gpu.d2d.DrawBitmap(
                frame,
                Some(&raw const full),
                1.0,
                D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR,
                None,
                None,
            ),
            None => gpu.d2d.Clear(Some(&raw const theme.surface)),
        }
    }

    if let Some(drawing) = scene.marks.drawing
        && intersect(
            rect(
                (drawing.crop.left - scene.origin.0) as f32,
                (drawing.crop.top - scene.origin.1) as f32,
                (drawing.crop.right - scene.origin.0) as f32,
                (drawing.crop.bottom - scene.origin.1) as f32,
            ),
            full,
        )
        .is_some()
    {
        annotations::draw(
            gpu,
            shared,
            kadr_doc::Point::new(scene.origin.0, scene.origin.1),
            drawing,
            cache,
        )?;
    }

    let (ox, oy) = (scene.origin.0, scene.origin.1);
    let selection = scene.marks.selection.map(|s| {
        rect(
            (s.left - ox) as f32,
            (s.top - oy) as f32,
            (s.right - ox) as f32,
            (s.bottom - oy) as f32,
        )
    });
    let selection = text.chrome.borrow_mut().highlight(selection, scene.marks.glide, now, animate);

    match selection.and_then(|s| intersect(s, full)) {
        None => fill(gpu, scrim, full),
        Some(c) => {
            fill(gpu, scrim, rect(0.0, 0.0, full.right, c.top));
            fill(gpu, scrim, rect(0.0, c.bottom, full.right, full.bottom));
            fill(gpu, scrim, rect(0.0, c.top, c.left, c.bottom));
            fill(gpu, scrim, rect(c.right, c.top, full.right, c.bottom));
        }
    }

    if let (Some(sel), Some(screen)) = (selection, scene.marks.selection) {
        let line = px(dip::FRAME, scene.scale);
        let outline = px(dip::FRAME_OUTLINE, scene.scale);
        // Рамка лежит снаружи выделения и не закрывает выбранные пиксели.
        ring(gpu, theme.frame_outline, inflate(sel, line), outline);
        ring(gpu, theme.frame, sel, line);
        if scene.marks.handles {
            draw_handles(gpu, theme, sel, line, scene.scale);
        }
        if let Some(bounds) = scene.marks.drawing.and_then(|v| v.selected) {
            let selected = rect(
                (bounds.left - ox) as f32,
                (bounds.top - oy) as f32,
                (bounds.right - ox) as f32,
                (bounds.bottom - oy) as f32,
            );
            ring(gpu, theme.frame, selected, line);
            draw_handles(gpu, theme, selected, line, scene.scale);
        }
        let label = format!("{} × {}", screen.right - screen.left, screen.bottom - screen.top);
        size_label(gpu, shared, text, theme, scene, sel, line + outline, &label)?;
        if let Some(toolbar) = scene.marks.toolbar {
            draw_toolbar(gpu, shared, text, theme, scene, sel, toolbar, scrim)?;
        }
    }

    if let Some(hud) = scene.marks.hud {
        draw_hud(gpu, shared, text, theme, scene, hud, selection)?;
    }
    if let Some(magnifier) = scene.marks.magnifier {
        draw_magnifier(gpu, shared, text, theme, scene, magnifier)?;
    }
    Ok(())
}

#[derive(Clone)]
struct ToolbarLayout {
    panel: D2D_RECT_F,
    /// Selection the panel is attached to: the panel grows out of that edge.
    sel: D2D_RECT_F,
    buttons: Vec<(ToolbarButton, D2D_RECT_F, Text)>,
    context: Option<Text>,
}
#[derive(Clone, PartialEq)]
struct ToolbarKey {
    size: (f32, f32),
    scale: f32,
    sel: (f32, f32, f32, f32),
    editor: Option<EditorBar>,
}

/// Панель под выделением, по правому краю; нет места снизу — сверху, нет и там — внутри.
/// Рисует её та поверхность, где лежит правый нижний угол выделения.
fn toolbar_layout(
    shared: &Shared,
    text: &TextStyles,
    size: (f32, f32),
    scale: f32,
    sel: D2D_RECT_F,
    editor: Option<EditorBar>,
) -> Result<Option<ToolbarLayout>> {
    let key = ToolbarKey {
        size,
        scale,
        sel: (sel.left, sel.top, sel.right, sel.bottom),
        editor,
    };
    if let Some((old, layout)) = &*text.toolbar.borrow()
        && *old == key
    {
        return Ok(layout.clone());
    }
    let layout = build_toolbar_layout(shared, text, size, scale, sel, editor)?;
    *text.toolbar.borrow_mut() = Some((key, layout.clone()));
    Ok(layout)
}
fn build_toolbar_layout(
    shared: &Shared,
    text: &TextStyles,
    size: (f32, f32),
    scale: f32,
    sel: D2D_RECT_F,
    editor: Option<EditorBar>,
) -> Result<Option<ToolbarLayout>> {
    let (w, h) = size;
    let (corner_x, corner_y) = (sel.right - 1.0, sel.bottom - 1.0);
    if corner_x < 0.0 || corner_y < 0.0 || corner_x >= w || corner_y >= h {
        return Ok(None);
    }
    let s = scale;
    let pad = 12.0 * s;
    let gap = 4.0 * s;
    let side = 40.0 * s;
    let panel_w = (544.0 * s).min((w - 16.0 * s).max(1.0));
    let content_w = (panel_w - 2.0 * pad).max(side);
    let mut buttons = Vec::new();
    let empty = measure(shared, &text.button, "")?;
    let mut top = pad;
    let mut row = |items: Vec<(ToolbarButton, String, f32)>, row_gap: f32| -> Result<()> {
        let mut left = pad;
        for (button, value, width) in items {
            if left + width > panel_w - pad + 0.1 && left > pad {
                left = pad;
                top += side + gap;
            }
            if button == ToolbarButton::Copy {
                left = panel_w - pad - width;
            }
            buttons.push((
                button,
                rect(left, top, left + width, top + side),
                if value.is_empty() {
                    empty.clone()
                } else {
                    measure(shared, &text.button, &value)?
                },
            ));
            left += width + row_gap;
        }
        top += side + 8.0 * s;
        Ok(())
    };
    if let Some(e) = editor {
        // Four blur variants share one entry until the user selects the blur family.
        let tools = [
            Tool::Select,
            Tool::Arrow,
            Tool::Line,
            Tool::Rectangle,
            Tool::Ellipse,
            Tool::Pen,
            Tool::Marker,
            Tool::Text,
            Tool::Counter,
            Tool::Blur,
            Tool::Redact,
        ];
        row(
            tools
                .into_iter()
                .map(|t| (ToolbarButton::Tool(t), String::new(), side))
                .collect(),
            8.0 * s,
        )?;
        if e.tool != Tool::Select {
            let mut properties = Vec::new();
            if e.tool.is_blur() {
                for tool in [Tool::Blur, Tool::BlurEllipse, Tool::BlurBrush, Tool::Spotlight] {
                    properties.push((ToolbarButton::Tool(tool), String::new(), side));
                }
                properties.push((ToolbarButton::Strength, format!("Сила {}", e.strength + 1), 88.0 * s));
                if e.tool == Tool::BlurBrush {
                    properties.push((ToolbarButton::Width, format!("{} px", e.width), 72.0 * s));
                }
            } else {
                for i in 0..COLORS.len() {
                    properties.push((ToolbarButton::Color(i), String::new(), 32.0 * s));
                }
                if e.tool != Tool::Redact {
                    properties.push((ToolbarButton::Width, format!("{} px", e.width), 76.0 * s));
                }
            }
            row(properties, gap)?;
        }
    }
    let actions = if editor.is_some() {
        vec![
            ToolbarButton::Undo,
            ToolbarButton::Redo,
            ToolbarButton::Recent,
            ToolbarButton::Pin,
            ToolbarButton::SaveAs,
            ToolbarButton::Save,
            ToolbarButton::Close,
            ToolbarButton::Copy,
        ]
    } else {
        vec![
            ToolbarButton::Recent,
            ToolbarButton::Pin,
            ToolbarButton::SaveAs,
            ToolbarButton::Save,
            ToolbarButton::Close,
            ToolbarButton::Copy,
        ]
    };
    row(
        actions
            .into_iter()
            .map(|b| {
                (
                    b,
                    if b == ToolbarButton::Copy {
                        "Копировать".into()
                    } else {
                        String::new()
                    },
                    if b == ToolbarButton::Copy {
                        (120.0 * s).min(content_w)
                    } else {
                        side
                    },
                )
            })
            .collect(),
        gap,
    )?;
    let panel_h = top - 8.0 * s + pad;
    let offset = 12.0 * s;
    let mut y = sel.bottom + offset;
    if y + panel_h > h {
        y = sel.top - offset - panel_h;
    }
    if y < 8.0 * s {
        y = sel.bottom - offset - panel_h;
    }
    let x = ((sel.left + sel.right - panel_w) / 2.0)
        .clamp(8.0 * s, (w - panel_w - 8.0 * s).max(8.0 * s))
        .round();
    let y = y.clamp(8.0 * s, (h - panel_h - 8.0 * s).max(8.0 * s)).round();
    for (_, r, _) in &mut buttons {
        r.left += x;
        r.right += x;
        r.top += y;
        r.bottom += y;
    }
    let context = editor
        .filter(|e| e.tool != Tool::Select)
        .map(|e| measure(shared, &text.label, e.tool.label()))
        .transpose()?;
    Ok(Some(ToolbarLayout {
        panel: rect(x, y, x + panel_w, y + panel_h),
        sel,
        buttons,
        context,
    }))
}

/// Какая кнопка панели под точкой `at` (координаты экрана) — по той же раскладке, что и отрисовка.
pub fn toolbar_hit(
    shared: &Shared,
    text: &TextStyles,
    surface: RECT,
    scale: f32,
    selection: RECT,
    at: (i32, i32),
    editor: Option<EditorBar>,
) -> Option<ToolbarButton> {
    let (ox, oy) = (surface.left, surface.top);
    let size = (
        (surface.right - surface.left) as f32,
        (surface.bottom - surface.top) as f32,
    );
    let sel = rect(
        (selection.left - ox) as f32,
        (selection.top - oy) as f32,
        (selection.right - ox) as f32,
        (selection.bottom - oy) as f32,
    );
    let target = toolbar_layout(shared, text, size, scale, sel, editor).ok()??;
    let layout = text.displayed(&target, editor, None, scale);
    let (x, y) = layout.unproject((at.0 - ox) as f32, (at.1 - oy) as f32);
    let clip = layout.property_clip();
    if let Some(c) = layout.buttons.iter().rev().find(|c| {
        c.opacity > 0.02
            && (!c.property || clip.is_some_and(|r| x >= r.left && x < r.right && y >= r.top && y < r.bottom))
            && x >= c.rect.left
            && x < c.rect.right
            && y >= c.rect.top
            && y < c.rect.bottom
    }) {
        return Some(if c.live && c.enabled {
            c.button
        } else {
            ToolbarButton::Surface
        });
    }
    let r = layout.panel;
    (x >= r.left && x < r.right && y >= r.top && y < r.bottom).then_some(ToolbarButton::Surface)
}

/// Floating glass panel: the screenshot shows through it blurred and tinted, under a soft shadow and a
/// hairline edge. It grows out of the selection edge; one pill and one ring glide between choices.
/// The primary action keeps the only saturated fill.
#[allow(clippy::too_many_arguments)]
fn draw_toolbar(
    gpu: &Gpu,
    shared: &Shared,
    text: &TextStyles,
    theme: &Theme,
    scene: &Scene,
    sel: D2D_RECT_F,
    toolbar: Toolbar,
    scrim: D2D1_COLOR_F,
) -> Result<()> {
    let size = (scene.size.0 as f32, scene.size.1 as f32);
    let Some(target) = toolbar_layout(shared, text, size, scene.scale, sel, toolbar.editor)? else {
        text.reset_toolbar_motion();
        return Ok(());
    };
    let s = scene.scale;
    let layout = text.displayed(&target, toolbar.editor, Some(toolbar.hovered), s);
    let a0 = layout.opacity;
    let (ax, ay) = layout.anchor;
    let k = layout.scale;
    let mut previous = Matrix3x2::default();
    unsafe {
        gpu.d2d.GetTransform(&mut previous);
        gpu.d2d.SetTransform(&Matrix3x2 {
            M11: k,
            M12: 0.0,
            M21: 0.0,
            M22: k,
            M31: ax * (1.0 - k),
            M32: ay * (1.0 - k) + layout.lift,
        });
    }
    let p = layout.panel;
    let inside = p.left >= sel.left && p.top >= sel.top && p.right <= sel.right && p.bottom <= sel.bottom;
    let painted = glass(gpu, shared, text, theme, scene.frame, p, 16.0 * s, s, a0, (!inside).then_some(scrim));
    unsafe {
        gpu.d2d.PushAxisAlignedClip(&p, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
    }
    if let Some(r) = layout.indicator {
        fill_rounded(gpu, fade(theme.selected, a0), r, 10.0 * s);
    }
    let property_clip = layout.property_clip();
    let selected_color = toolbar.editor.map(|e| e.color);
    // Parameters fade with the opening row: no sliver of a swatch shows through a two-pixel gap.
    let openness = |r: &D2D_RECT_F| {
        property_clip.map_or(0.0, |clip| {
            let open = (clip.bottom - clip.top) / (r.bottom - r.top).max(1.0);
            ((open - 0.4) / 0.6).clamp(0.0, 1.0)
        })
    };
    for c in &layout.buttons {
        let (button, r, label) = (&c.button, &c.rect, &c.label);
        let a = a0 * c.opacity * if c.property { openness(r) } else { 1.0 };
        if a < 0.001 {
            continue;
        }
        if c.property {
            let Some(clip) = property_clip else {
                continue;
            };
            unsafe {
                gpu.d2d.PushAxisAlignedClip(&clip, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
            }
        }
        if let ToolbarButton::Color(i) = button {
            let rgb = COLORS[*i];
            let ink = D2D1_COLOR_F {
                r: rgb[0] as f32 / 255.0,
                g: rgb[1] as f32 / 255.0,
                b: rgb[2] as f32 / 255.0,
                a,
            };
            // The swatch itself swells a little under the pointer instead of getting a box around it.
            let radius = (9.0 + 1.5 * c.hover) * s;
            unsafe {
                let e = D2D1_ELLIPSE {
                    point: Vector2::new((r.left + r.right) / 2.0, (r.top + r.bottom) / 2.0),
                    radiusX: radius,
                    radiusY: radius,
                };
                gpu.brush.SetColor(&ink);
                gpu.d2d.FillEllipse(&e, &gpu.brush);
                gpu.brush.SetColor(&fade(theme.hairline, a));
                gpu.d2d.DrawEllipse(&e, &gpu.brush, s, None::<&ID2D1StrokeStyle>);
            }
        } else {
            let ink = if !c.enabled {
                fade(theme.disabled, a)
            } else if *button == ToolbarButton::Copy {
                fill_rounded(gpu, fade(mix(theme.accent, theme.on_accent, 0.12 * c.hover), a), *r, 10.0 * s);
                fade(theme.on_accent, a)
            } else {
                // Main-row tools take their tint from how much of the gliding pill covers them, so
                // the colour travels with the pill instead of jumping ahead of it.
                let tint = match (c.property, layout.indicator) {
                    (false, Some(p)) if matches!(button, ToolbarButton::Tool(_)) => {
                        ((p.right.min(r.right) - p.left.max(r.left)) / (r.right - r.left).max(1.0)).clamp(0.0, 1.0)
                    }
                    (false, None) if matches!(button, ToolbarButton::Tool(_)) => 0.0,
                    _ => c.selected,
                };
                if c.hover > 0.01 {
                    fill_rounded(gpu, fade(theme.control, a * c.hover * (1.0 - tint)), *r, 10.0 * s);
                }
                if c.property && c.selected > 0.01 {
                    fill_rounded(gpu, fade(theme.selected, a * c.selected), *r, 10.0 * s);
                }
                fade(mix(theme.text, theme.on_selected, tint), a)
            };
            if label.w > 0.0 {
                draw_text(
                    gpu,
                    label,
                    r.left + (r.right - r.left - label.w) / 2.0,
                    r.top + (r.bottom - r.top - label.h) / 2.0,
                    ink,
                );
            } else {
                glyphs::draw(gpu, *button, *r, ink, s);
            }
        }
        if c.property {
            unsafe {
                gpu.d2d.PopAxisAlignedClip();
            }
        }
    }
    if let (Some(r), Some(clip)) = (layout.swatch, property_clip) {
        let ring = layout
            .buttons
            .iter()
            .find(|c| matches!(c.button, ToolbarButton::Color(i) if Some(i) == selected_color))
            .map_or(0.0, |c| c.opacity * openness(&c.rect));
        if ring > 0.001 {
            unsafe {
                gpu.d2d.PushAxisAlignedClip(&clip, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
                let e = D2D1_ELLIPSE {
                    point: Vector2::new((r.left + r.right) / 2.0, (r.top + r.bottom) / 2.0),
                    radiusX: 13.0 * s,
                    radiusY: 13.0 * s,
                };
                gpu.brush.SetColor(&fade(theme.text, a0 * ring));
                gpu.d2d.DrawEllipse(&e, &gpu.brush, 1.75 * s, None::<&ID2D1StrokeStyle>);
                gpu.d2d.PopAxisAlignedClip();
            }
        }
    }
    if let Some(context) = &layout.context
        && let Some(clip) = property_clip
    {
        let properties: Vec<_> = layout
            .buttons
            .iter()
            .filter(|c| {
                c.live && matches!(c.button, ToolbarButton::Color(_) | ToolbarButton::Width | ToolbarButton::Strength)
            })
            .collect();
        if let Some(c) = properties.first() {
            let end = properties.iter().map(|c| c.rect.right).fold(0.0, f32::max);
            let x = layout.panel.right - 16.0 * s - context.w;
            let context_opacity = properties.iter().map(|c| c.opacity).fold(1.0, f32::min);
            if x > end + 16.0 * s && clip.bottom - clip.top > context.h + 8.0 * s {
                draw_text(
                    gpu,
                    context,
                    x,
                    c.rect.top + (c.rect.bottom - c.rect.top - context.h) / 2.0,
                    fade(theme.text_muted, a0 * context_opacity),
                );
            }
        }
    }
    unsafe {
        gpu.d2d.PopAxisAlignedClip();
        gpu.d2d.SetTransform(&previous);
    }
    painted?;
    // A resting pointer gets its tooltip; drawn untransformed, next to where the button is on screen.
    if let Some((hovered, r, tip_opacity)) = layout.tip {
        let tip_opacity = tip_opacity * a0;
        let value = match hovered {
            ToolbarButton::Copy => "Копировать · Enter / Ctrl+C".into(),
            ToolbarButton::Save => "Сохранить · Ctrl+S".into(),
            ToolbarButton::SaveAs => "Сохранить как… · Ctrl+Shift+S".into(),
            ToolbarButton::Pin => "Закрепить · Ctrl+D".into(),
            ToolbarButton::Recent => "Недавние · Ctrl+H".into(),
            ToolbarButton::Close => "Закрыть · Esc".into(),
            ToolbarButton::Width => "Толщина · [ / ] · колесо".into(),
            ToolbarButton::Strength => "Сила размытия · колесо".into(),
            ToolbarButton::Color(i) => format!("Цвет · {}", i + 1),
            _ => hovered.label().into(),
        };
        let tip = measure(shared, &text.label, &value)?;
        let rw = tip.w + 20.0 * s;
        let rh = tip.h + 12.0 * s;
        let shown = layout.project(layout.panel);
        let target = layout.project(r);
        let x = ((target.left + target.right - rw) / 2.0).clamp(8.0 * s, (size.0 - rw - 8.0 * s).max(8.0 * s));
        let y = if shown.top > rh + 16.0 * s { shown.top - rh - 8.0 * s } else { shown.bottom + 8.0 * s };
        if y + rh <= size.1 {
            let y = y + 3.0 * s * (1.0 - tip_opacity);
            let mut tip_theme = *theme;
            tip_theme.surface = fade(theme.surface, tip_opacity);
            tip_theme.hairline = fade(theme.hairline, tip_opacity);
            panel(gpu, &tip_theme, rect(x, y, x + rw, y + rh), 8.0 * s, s);
            draw_text(gpu, &tip, x + 10.0 * s, y + 6.0 * s, fade(theme.text, tip_opacity));
        }
    }
    Ok(())
}

/// Frosted material: blurred screenshot, the scrim it sits on, then a light tint. Falls back to an
/// opaque surface without a frame (fixtures) or in high contrast.
#[allow(clippy::too_many_arguments)]
fn glass(
    gpu: &Gpu,
    shared: &Shared,
    text: &TextStyles,
    theme: &Theme,
    frame: Option<&ID2D1Bitmap1>,
    r: D2D_RECT_F,
    radius: f32,
    s: f32,
    opacity: f32,
    scrim: Option<D2D1_COLOR_F>,
) -> Result<()> {
    shadow(gpu, theme.shadow, r, radius, s, opacity, true);
    let rounded = D2D1_ROUNDED_RECT { rect: r, radiusX: radius, radiusY: radius };
    match frame.filter(|_| theme.glass.a < 1.0) {
        Some(frame) => unsafe {
            let effect = backdrop(gpu, text, frame, s)?;
            let geometry: ID2D1Geometry = shared.d2d.CreateRoundedRectangleGeometry(&rounded)?.cast()?;
            let mut params = D2D1_LAYER_PARAMETERS1 {
                contentBounds: r,
                geometricMask: ManuallyDrop::new(Some(geometry)),
                maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
                maskTransform: Matrix3x2::identity(),
                opacity,
                opacityBrush: ManuallyDrop::new(None),
                layerOptions: D2D1_LAYER_OPTIONS1_NONE,
            };
            gpu.d2d.PushLayer(&params, None::<&ID2D1Layer>);
            gpu.d2d.DrawImage(
                &effect.GetOutput()?,
                Some(&Vector2 { X: r.left, Y: r.top }),
                Some(&r),
                D2D1_INTERPOLATION_MODE_LINEAR,
                D2D1_COMPOSITE_MODE_SOURCE_OVER,
            );
            if let Some(scrim) = scrim {
                fill(gpu, scrim, r);
            }
            fill(gpu, theme.glass, r);
            gpu.d2d.PopLayer();
            ManuallyDrop::drop(&mut params.geometricMask);
            ManuallyDrop::drop(&mut params.opacityBrush);
        },
        None => fill_rounded(gpu, fade(theme.surface, opacity), r, radius),
    }
    edge(gpu, theme, r, radius, s, opacity);
    Ok(())
}

/// Blur of this surface's frozen frame, reused across frames; only its input changes.
fn backdrop(gpu: &Gpu, text: &TextStyles, frame: &ID2D1Bitmap1, s: f32) -> Result<ID2D1Effect> {
    let mut slot = text.backdrop.borrow_mut();
    if slot.is_none() {
        unsafe {
            let effect = gpu.d2d.CreateEffect(&CLSID_D2D1GaussianBlur)?;
            effect.SetValue(
                D2D1_GAUSSIANBLUR_PROP_STANDARD_DEVIATION.0 as u32,
                D2D1_PROPERTY_TYPE_FLOAT,
                &(20.0 * s).to_le_bytes(),
            )?;
            // Hard edges: the monitor border does not fade the material to transparent.
            effect.SetValue(
                D2D1_GAUSSIANBLUR_PROP_BORDER_MODE.0 as u32,
                D2D1_PROPERTY_TYPE_ENUM,
                &(D2D1_BORDER_MODE_HARD.0 as u32).to_le_bytes(),
            )?;
            *slot = Some(effect);
        }
    }
    let effect = slot.clone().expect("created above");
    unsafe {
        effect.SetInput(0, frame, true);
    }
    Ok(effect)
}

/// Soft shadow from stacked translucent rings: a wide ambient one and, for floating panels, a tight
/// contact shade. Cheaper than a shadow effect and identical at every refresh rate.
fn shadow(gpu: &Gpu, color: D2D1_COLOR_F, r: D2D_RECT_F, radius: f32, s: f32, opacity: f32, floating: bool) {
    if color.a <= 0.0 || opacity <= 0.0 {
        return;
    }
    let layers: &[(f32, f32, f32, usize)] =
        if floating { &[(28.0, 12.0, 0.20, 14), (3.0, 1.0, 0.10, 3)] } else { &[(8.0, 3.0, 0.12, 6)] };
    for &(spread, drop, alpha, steps) in layers {
        for i in 0..steps {
            let t = (i + 1) as f32 / steps as f32;
            let grow = spread * s * t;
            let dy = drop * s * t;
            let ring = rect(r.left - grow, r.top - grow + dy, r.right + grow, r.bottom + grow + dy);
            fill_rounded(gpu, fade(color, opacity * alpha / steps as f32 * (1.0 - t * 0.5)), ring, radius + grow);
        }
    }
}

/// Hairline outside and a faint light rim inside give the slab its thickness.
fn edge(gpu: &Gpu, theme: &Theme, r: D2D_RECT_F, radius: f32, s: f32, opacity: f32) {
    let line = px(dip::BORDER, s);
    let half = line / 2.0;
    let rounded = |r: D2D_RECT_F, radius: f32| D2D1_ROUNDED_RECT { rect: r, radiusX: radius, radiusY: radius };
    unsafe {
        gpu.brush.SetColor(&fade(theme.hairline, opacity));
        gpu.d2d.DrawRoundedRectangle(&rounded(inflate(r, -half), radius - half), &gpu.brush, line, None::<&ID2D1StrokeStyle>);
        gpu.brush.SetColor(&fade(theme.sheen, opacity));
        gpu.d2d.DrawRoundedRectangle(
            &rounded(inflate(r, -line - half), radius - line - half),
            &gpu.brush,
            line,
            None::<&ID2D1StrokeStyle>,
        );
    }
}

fn fade(mut c: D2D1_COLOR_F, a: f32) -> D2D1_COLOR_F {
    c.a *= a.clamp(0.0, 1.0);
    c
}
fn mix(a: D2D1_COLOR_F, b: D2D1_COLOR_F, t: f32) -> D2D1_COLOR_F {
    let t = t.clamp(0.0, 1.0);
    D2D1_COLOR_F {
        r: a.r + (b.r - a.r) * t,
        g: a.g + (b.g - a.g) * t,
        b: a.b + (b.b - a.b) * t,
        a: a.a + (b.a - a.a) * t,
    }
}

fn fill_rounded(gpu: &Gpu, color: D2D1_COLOR_F, r: D2D_RECT_F, radius: f32) {
    unsafe {
        gpu.brush.SetColor(&color);
        gpu.d2d.FillRoundedRectangle(
            &D2D1_ROUNDED_RECT {
                rect: r,
                radiusX: radius,
                radiusY: radius,
            },
            &gpu.brush,
        );
    }
}

/// Квадраты на углах и серединах сторон рамки: белые с тёмным контуром, видны на любом фоне.
/// У маленького выделения — только углы, чтобы маркеры не слиплись.
fn draw_handles(gpu: &Gpu, theme: &Theme, sel: D2D_RECT_F, line: f32, scale: f32) {
    let size = (px(dip::HANDLE, scale) / 2.0).round() * 2.0;
    let half = size / 2.0;
    let (l, t) = (sel.left - line / 2.0, sel.top - line / 2.0);
    let (r, b) = (sel.right + line / 2.0, sel.bottom + line / 2.0);
    let (mx, my) = ((l + r) / 2.0, (t + b) / 2.0);
    let mut points = vec![(l, t), (r, t), (l, b), (r, b)];
    if r - l > size * 4.0 {
        points.extend([(mx, t), (mx, b)]);
    }
    if b - t > size * 4.0 {
        points.extend([(l, my), (r, my)]);
    }
    for (x, y) in points {
        let (x, y) = ((x - half).round(), (y - half).round());
        let square = rect(x, y, x + size, y + size);
        fill(gpu, theme.frame_outline, inflate(square, 1.0));
        fill(gpu, theme.frame, square);
    }
}

/// Лупа рядом с курсором: 15×15 пикселей крупно, центральный обведён; под ними координаты
/// и цвет. Встаёт по другую сторону курсора, если не помещается у края монитора.
fn draw_magnifier(
    gpu: &Gpu,
    shared: &Shared,
    text: &TextStyles,
    theme: &Theme,
    scene: &Scene,
    magnifier: &Magnifier,
) -> Result<()> {
    const SPAN: i32 = 15;
    let (w, h) = (scene.size.0 as f32, scene.size.1 as f32);
    let (cx, cy) = (magnifier.at.0 - scene.origin.0, magnifier.at.1 - scene.origin.1);
    let Some(frame) = scene.frame else {
        return Ok(());
    };
    if cx < 0 || cy < 0 || cx as f32 >= w || cy as f32 >= h {
        return Ok(());
    }
    let s = scene.scale;
    let cell = (10.0 * s).round();
    let zoom = cell * SPAN as f32;
    let border = px(dip::BORDER, s);
    let pad = dip::SPACE_2 * s;
    let gap = (dip::SPACE_4 + dip::SPACE_2) * s;

    let coords = measure(shared, &text.figure, &format!("{}, {}", magnifier.at.0, magnifier.at.1))?;
    let hex_text = magnifier
        .rgb
        .map_or("—".to_string(), |c| format!("#{:02X}{:02X}{:02X}", c[0], c[1], c[2]));
    let hex = measure(shared, &text.figure, &hex_text)?;
    let info_h = (coords.h + pad * 2.0).ceil();
    let (pw, ph) = (zoom + border * 2.0, zoom + border * 3.0 + info_h);

    let mut x = cx as f32 + gap;
    if x + pw > w {
        x = cx as f32 - gap - pw;
    }
    let mut y = cy as f32 + gap;
    if y + ph > h {
        y = cy as f32 - gap - ph;
    }
    let (x, y) = (x.max(0.0).round(), y.max(0.0).round());

    // Панель: рамка заливкой, внутри — поверхность. Пиксели квадратные, и углы у лупы тоже.
    fill(gpu, theme.border, rect(x, y, x + pw, y + ph));
    let inner = rect(x + border, y + border, x + pw - border, y + ph - border);
    fill(gpu, theme.surface, inner);

    let zoom_rect = rect(inner.left, inner.top, inner.left + zoom, inner.top + zoom);
    let half = SPAN / 2;
    let source = rect(
        (cx - half) as f32,
        (cy - half) as f32,
        (cx + half + 1) as f32,
        (cy + half + 1) as f32,
    );
    if let Some(visible) = intersect(source, rect(0.0, 0.0, w, h)) {
        let dest = rect(
            zoom_rect.left + (visible.left - source.left) * cell,
            zoom_rect.top + (visible.top - source.top) * cell,
            zoom_rect.left + (visible.right - source.left) * cell,
            zoom_rect.top + (visible.bottom - source.top) * cell,
        );
        unsafe {
            gpu.d2d.DrawBitmap(
                frame,
                Some(&raw const dest),
                1.0,
                D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR,
                Some(&raw const visible),
                None,
            );
        }
    }
    let center_left = zoom_rect.left + half as f32 * cell;
    let center_top = zoom_rect.top + half as f32 * cell;
    let center = rect(center_left, center_top, center_left + cell, center_top + cell);
    ring(gpu, theme.frame_outline, inflate(center, border), border);
    ring(gpu, theme.frame, center, border);

    fill(
        gpu,
        theme.border,
        rect(inner.left, zoom_rect.bottom, inner.right, zoom_rect.bottom + border),
    );
    let text_y = zoom_rect.bottom + border + pad;
    draw_text(gpu, &coords, inner.left + pad, text_y, theme.text_muted);
    let hex_x = inner.right - pad - hex.w;
    draw_text(gpu, &hex, hex_x, text_y, theme.text);
    if let Some([r, g, b]) = magnifier.rgb {
        // Образец цвета — это данные, а не хром: единственное место хрома, где есть цвет.
        let side = (hex.h * 0.6).round();
        let (sx, sy) = (
            (hex_x - side - dip::SPACE_2 * s).round(),
            (text_y + (hex.h - side) / 2.0).round(),
        );
        fill(
            gpu,
            theme.border,
            rect(sx - border, sy - border, sx + side + border, sy + side + border),
        );
        let color = D2D1_COLOR_F {
            r: r as f32 / 255.0,
            g: g as f32 / 255.0,
            b: b as f32 / 255.0,
            a: 1.0,
        };
        fill(gpu, color, rect(sx, sy, sx + side, sy + side));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn size_label(
    gpu: &Gpu,
    shared: &Shared,
    text: &TextStyles,
    theme: &Theme,
    scene: &Scene,
    sel: D2D_RECT_F,
    frame_width: f32,
    label: &str,
) -> Result<()> {
    let (w, h) = (scene.size.0 as f32, scene.size.1 as f32);
    // Подпись рисует только та поверхность, где лежит левый верхний угол выделения.
    if sel.left < 0.0 || sel.top < 0.0 || sel.left >= w || sel.top >= h {
        return Ok(());
    }
    let s = scene.scale;
    let content = measure(shared, &text.figure, label)?;
    let (pad_x, pad_y, gap) = (dip::SPACE_2 * s, dip::SPACE_1 * s, dip::SPACE_2 * s);
    let (bw, bh) = ((content.w + pad_x * 2.0).ceil(), (content.h + pad_y * 2.0).ceil());

    // Над рамкой, по её внешнему краю; если сверху нет места — внутри выделения.
    let (mut x, mut y) = (sel.left - frame_width, sel.top - frame_width - gap - bh);
    if y < gap {
        x = sel.left + gap;
        y = sel.top + gap;
    }
    let x = x.clamp(0.0, (w - bw).max(0.0)).round();
    let y = y.round();

    panel(gpu, theme, rect(x, y, x + bw, y + bh), dip::RADIUS_SM * s, s);
    draw_text(gpu, &content, x + pad_x, y + pad_y, theme.text);
    Ok(())
}

struct Row {
    label: Text,
    value: Text,
    unit: Text,
}

impl Row {
    fn height(&self) -> f32 {
        self.label.h.max(self.value.h)
    }

    fn baseline(&self) -> f32 {
        self.label.baseline.max(self.value.baseline)
    }
}

fn draw_hud(
    gpu: &Gpu,
    shared: &Shared,
    text: &TextStyles,
    theme: &Theme,
    scene: &Scene,
    hud: &Hud,
    selection: Option<D2D_RECT_F>,
) -> Result<()> {
    let s = scene.scale;
    let pad = dip::SPACE_3 * s;
    let column_gap = dip::SPACE_4 * s;
    let unit_gap = dip::SPACE_1 * s;
    let row_gap = dip::SPACE_1 * s;
    let around_rule = dip::SPACE_2 * s;
    let block_gap = dip::SPACE_3 * s;
    let margin = dip::SPACE_4 * s;
    let rule = px(dip::BORDER, s);

    let title = measure(shared, &text.title, &hud.title)?;
    let scope = measure(shared, &text.label, &hud.scope)?;
    let lead_label = measure(shared, &text.label, &hud.lead_label)?;
    let (lead_value, lead_unit) = match &hud.lead {
        Some((value, unit)) => (measure(shared, &text.lead, value)?, measure(shared, &text.label, unit)?),
        None => (measure(shared, &text.lead, "—")?, measure(shared, &text.label, "")?),
    };
    let hint = measure(shared, &text.label, &hud.hint)?;
    let groups = hud
        .groups
        .iter()
        .map(|group| {
            group
                .iter()
                .map(|row| {
                    Ok(Row {
                        label: measure(shared, &text.label, &row.label)?,
                        value: measure(shared, &text.figure, &row.value)?,
                        unit: measure(shared, &text.label, &row.unit)?,
                    })
                })
                .collect::<Result<Vec<_>>>()
        })
        .collect::<Result<Vec<_>>>()?;
    let rows = || groups.iter().flatten();

    let label_w = rows().map(|r| r.label.w).fold(0.0, f32::max);
    let value_w = rows().map(|r| r.value.w).fold(0.0, f32::max);
    let unit_w = rows().map(|r| r.unit.w).fold(0.0, f32::max);
    let table_w = label_w + column_gap + value_w + unit_gap + unit_w;
    let header_w = title.w + column_gap + scope.w;
    let lead_w = (lead_value.w + unit_gap + lead_unit.w).max(lead_label.w);
    let content_w = table_w.max(header_w).max(lead_w).max(hint.w);

    let section = |rows_h: f32| around_rule + rule + around_rule + rows_h;
    let group_h = |group: &Vec<Row>| group.iter().map(|r| r.height() + row_gap).sum::<f32>() - row_gap;
    let panel_w = (pad * 2.0 + content_w).ceil();
    let panel_h = (pad
        + title.h.max(scope.h)
        + block_gap
        + lead_label.h
        + row_gap
        + lead_value.h
        + groups.iter().map(|g| section(group_h(g))).sum::<f32>()
        + section(hint.h)
        + pad)
        .ceil();

    // Панель не должна закрывать выделение (ui-ux-pro-max: focus-not-obscured): первый свободный угол.
    let (w, h) = (scene.size.0 as f32, scene.size.1 as f32);
    let corners = [
        (margin, margin),
        (w - margin - panel_w, margin),
        (margin, h - margin - panel_h),
        (w - margin - panel_w, h - margin - panel_h),
    ];
    let (x, y) = corners
        .into_iter()
        .find(|&(x, y)| {
            selection.is_none_or(|sel| intersect(inflate(sel, margin), rect(x, y, x + panel_w, y + panel_h)).is_none())
        })
        .unwrap_or(corners[0]);
    let (x, y) = (x.round(), y.round());
    panel(gpu, theme, rect(x, y, x + panel_w, y + panel_h), dip::RADIUS_MD * s, s);

    let left = x + pad;
    let right = x + panel_w - pad;
    let mut top = y + pad;

    // Имя продукта и контекст — на одной базовой линии.
    let header_base = title.baseline.max(scope.baseline);
    draw_text(gpu, &title, left, top + header_base - title.baseline, theme.text);
    draw_text(
        gpu,
        &scope,
        right - scope.w,
        top + header_base - scope.baseline,
        theme.text_muted,
    );
    top += title.h.max(scope.h) + block_gap;

    // Ответ на вопрос панели.
    draw_text(gpu, &lead_label, left, top, theme.text_muted);
    top += lead_label.h + row_gap;
    let lead_color = if hud.lead.is_some() {
        theme.text
    } else {
        theme.text_muted
    };
    draw_text(gpu, &lead_value, left, top, lead_color);
    draw_text(
        gpu,
        &lead_unit,
        left + lead_value.w + unit_gap,
        top + lead_value.baseline - lead_unit.baseline,
        theme.text_muted,
    );
    top += lead_value.h;

    // Таблица: подписи слева, числа на одном правом крае с панелью, единицы серым после них.
    let value_right = right - unit_gap - unit_w;
    for group in &groups {
        top += around_rule;
        fill(gpu, theme.border, rect(left, top, right, top + rule));
        top += rule + around_rule;
        for row in group {
            let base = row.baseline();
            draw_text(gpu, &row.label, left, top + base - row.label.baseline, theme.text_muted);
            draw_text(
                gpu,
                &row.value,
                value_right - row.value.w,
                top + base - row.value.baseline,
                theme.text,
            );
            draw_text(
                gpu,
                &row.unit,
                value_right + unit_gap,
                top + base - row.unit.baseline,
                theme.text_muted,
            );
            top += row.height() + row_gap;
        }
        top -= row_gap;
    }

    top += around_rule;
    fill(gpu, theme.border, rect(left, top, right, top + rule));
    top += rule + around_rule;
    draw_text(gpu, &hint, left, top, theme.text_muted);
    Ok(())
}

#[derive(Clone)]
struct Text {
    layout: IDWriteTextLayout,
    w: f32,
    h: f32,
    /// Расстояние от верха строки до базовой линии — чтобы ровнять разные шрифты и размеры.
    baseline: f32,
}

fn measure(shared: &Shared, style: &Style, s: &str) -> Result<Text> {
    let utf16: Vec<u16> = s.encode_utf16().collect();
    unsafe {
        let layout = shared
            .dwrite
            .CreateTextLayout(&utf16, &style.format, 10_000.0, 10_000.0)?;
        if style.tabular {
            let all = DWRITE_TEXT_RANGE {
                startPosition: 0,
                length: utf16.len() as u32,
            };
            layout.SetTypography(&shared.tabular, all)?;
        }
        let mut metrics = DWRITE_TEXT_METRICS::default();
        layout.GetMetrics(&mut metrics)?;
        let mut lines = [DWRITE_LINE_METRICS::default(); 1];
        let mut count = 0;
        layout.GetLineMetrics(Some(&mut lines), &mut count)?;
        Ok(Text {
            layout,
            w: metrics.widthIncludingTrailingWhitespace,
            h: metrics.height,
            baseline: lines[0].baseline,
        })
    }
}

fn draw_text(gpu: &Gpu, text: &Text, x: f32, y: f32, color: D2D1_COLOR_F) {
    unsafe {
        gpu.brush.SetColor(&color);
        gpu.d2d.DrawTextLayout(
            Vector2 {
                X: x.round(),
                Y: y.round(),
            },
            &text.layout,
            &gpu.brush,
            D2D1_DRAW_TEXT_OPTIONS_NONE,
        );
    }
}

/// Small opaque card (size label, tooltip, HUD): a short soft shadow, the surface and a hairline.
/// Opacity comes from `theme.surface.a`, so a fading card fades its shadow too.
fn panel(gpu: &Gpu, theme: &Theme, r: D2D_RECT_F, radius: f32, scale: f32) {
    let opacity = theme.surface.a;
    shadow(gpu, theme.shadow, r, radius, scale, opacity, false);
    fill_rounded(gpu, theme.surface, r, radius);
    edge(gpu, theme, r, radius, scale, opacity);
}

/// Кольцо толщиной `width` вокруг `inner`. Заливкой прямоугольников, а не обводкой, —
/// так линии остаются чёткими без полупикселей.
fn ring(gpu: &Gpu, color: D2D1_COLOR_F, inner: D2D_RECT_F, width: f32) {
    let D2D_RECT_F {
        left: l,
        top: t,
        right: r,
        bottom: b,
    } = inner;
    fill(gpu, color, rect(l - width, t - width, r + width, t));
    fill(gpu, color, rect(l - width, b, r + width, b + width));
    fill(gpu, color, rect(l - width, t, l, b));
    fill(gpu, color, rect(r, t, r + width, b));
}

fn fill(gpu: &Gpu, color: D2D1_COLOR_F, r: D2D_RECT_F) {
    if r.right <= r.left || r.bottom <= r.top {
        return;
    }
    unsafe {
        gpu.brush.SetColor(&color);
        gpu.d2d.FillRectangle(&r, &gpu.brush);
    }
}

fn text_format(shared: &Shared, weight: DWRITE_FONT_WEIGHT, size: f32) -> Result<IDWriteTextFormat> {
    unsafe {
        let format = shared.dwrite.CreateTextFormat(
            PCWSTR(shared.ui_family.as_ptr()),
            None::<&IDWriteFontCollection>,
            weight,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            size,
            w!("ru-ru"),
        )?;
        format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
        Ok(format)
    }
}

/// DIP → целые физические пиксели, не меньше одного.
fn px(value: f32, scale: f32) -> f32 {
    (value * scale).round().max(1.0)
}

fn rect(left: f32, top: f32, right: f32, bottom: f32) -> D2D_RECT_F {
    D2D_RECT_F {
        left,
        top,
        right,
        bottom,
    }
}

fn inflate(r: D2D_RECT_F, by: f32) -> D2D_RECT_F {
    rect(r.left - by, r.top - by, r.right + by, r.bottom + by)
}

fn intersect(a: D2D_RECT_F, b: D2D_RECT_F) -> Option<D2D_RECT_F> {
    let r = rect(
        a.left.max(b.left),
        a.top.max(b.top),
        a.right.min(b.right),
        a.bottom.min(b.bottom),
    );
    (r.right > r.left && r.bottom > r.top).then_some(r)
}
#[cfg(test)]
mod toolbar_tests {
    use super::*;
    #[test]
    fn replacing_palette_with_blur_controls_does_not_ghost_two_different_glyphs() {
        let shared = Shared::new().unwrap();
        let text = TextStyles::new(&shared, 1.0).unwrap();
        text.enable_motion(true);
        let e = EditorBar {
            tool: Tool::Rectangle,
            color: 0,
            width: 3,
            strength: 1,
            undo: true,
            redo: false,
        };
        let sel = rect(100.0, 100.0, 900.0, 500.0);
        let palette = toolbar_layout(&shared, &text, (1280.0, 720.0), 1.0, sel, Some(e))
            .unwrap()
            .unwrap();
        text.set_animation_time(Some(0.0));
        text.displayed(&palette, Some(e), Some(None), 1.0);
        text.set_animation_time(Some(0.7));
        text.displayed(&palette, Some(e), Some(None), 1.0);
        let next_e = EditorBar {
            tool: Tool::BlurBrush,
            ..e
        };
        let blur = toolbar_layout(&shared, &text, (1280.0, 720.0), 1.0, sel, Some(next_e))
            .unwrap()
            .unwrap();
        for i in 0..40 {
            text.set_animation_time(Some(1.0 + i as f64 / 170.0));
            let pose = text.displayed(&blur, Some(next_e), Some(None), 1.0);
            let old = pose
                .buttons
                .iter()
                .any(|c| matches!(c.button, ToolbarButton::Color(_)) && c.opacity > 0.02);
            let new = pose
                .buttons
                .iter()
                .any(|c| c.button == ToolbarButton::Tool(Tool::BlurBrush) && c.opacity > 0.02);
            assert!(!(old && new), "unrelated property controls overlap on frame {i}");
        }
    }
    #[test]
    fn animated_panel_opens_between_frames_and_hit_testing_uses_its_visible_buttons() {
        let shared = Shared::new().unwrap();
        let text = TextStyles::new(&shared, 1.0).unwrap();
        text.enable_motion(true);
        let sel = rect(100.0, 100.0, 900.0, 500.0);
        let e = EditorBar {
            tool: Tool::Select,
            color: 0,
            width: 3,
            strength: 1,
            undo: true,
            redo: false,
        };
        let first = toolbar_layout(&shared, &text, (1280.0, 720.0), 1.0, sel, Some(e))
            .unwrap()
            .unwrap();
        text.set_animation_time(Some(0.0));
        text.displayed(&first, Some(e), Some(None), 1.0);
        text.set_animation_time(Some(0.7));
        let before = text.displayed(&first, Some(e), Some(None), 1.0);
        let next_e = EditorBar {
            tool: Tool::Rectangle,
            ..e
        };
        let next = toolbar_layout(&shared, &text, (1280.0, 720.0), 1.0, sel, Some(next_e))
            .unwrap()
            .unwrap();
        text.set_animation_time(Some(1.0));
        let start = text.displayed(&next, Some(next_e), Some(None), 1.0);
        assert!((start.panel.bottom - before.panel.bottom).abs() < 0.01);
        text.set_animation_time(Some(1.0 + 1.0 / 170.0));
        let moving = text.displayed(&next, Some(next_e), Some(None), 1.0);
        assert!(moving.panel.bottom > before.panel.bottom && moving.panel.bottom < next.panel.bottom);
        let copy = moving
            .buttons
            .iter()
            .find(|c| c.button == ToolbarButton::Copy)
            .unwrap()
            .rect;
        assert_eq!(
            toolbar_hit(
                &shared,
                &text,
                RECT {
                    left: 0,
                    top: 0,
                    right: 1280,
                    bottom: 720
                },
                1.0,
                RECT {
                    left: 100,
                    top: 100,
                    right: 900,
                    bottom: 500
                },
                ((copy.left + copy.right) as i32 / 2, (copy.top + copy.bottom) as i32 / 2),
                Some(next_e)
            ),
            Some(ToolbarButton::Copy)
        );
        text.enable_motion(false);
        let reduced = text.displayed(&next, Some(next_e), Some(None), 1.0);
        assert_eq!(reduced.panel.bottom, next.panel.bottom);
        assert!(!text.animations_pending());
    }
    #[test]
    fn changing_tool_while_a_property_is_hovered_does_not_crash_the_next_frame() {
        use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1};
        let shared = Shared::new().unwrap();
        let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1().unwrap() };
        let gpu = Gpu::new(
            &shared,
            &unsafe { factory.EnumAdapters1(0).unwrap() },
            "stale hover".into(),
        )
        .unwrap();
        let text = TextStyles::new(&shared, 1.0).unwrap();
        let scene = Scene {
            frame: None,
            origin: (0, 0),
            size: (1280, 720),
            scale: 1.0,
            marks: Marks::default(),
        };
        let bar = Toolbar {
            hovered: Some(ToolbarButton::Width),
            editor: Some(EditorBar {
                tool: Tool::Select,
                color: 0,
                width: 3,
                strength: 1,
                undo: false,
                redo: false,
            }),
        };
        unsafe {
            let target = gpu
                .d2d
                .CreateBitmap(
                    windows::Win32::Graphics::Direct2D::Common::D2D_SIZE_U {
                        width: 1280,
                        height: 720,
                    },
                    None,
                    0,
                    &crate::gpu::bitmap_props(windows::Win32::Graphics::Direct2D::D2D1_BITMAP_OPTIONS_TARGET),
                )
                .unwrap();
            gpu.d2d.SetTarget(&target);
            gpu.d2d.BeginDraw();
            draw_toolbar(
                &gpu,
                &shared,
                &text,
                &Theme::current(),
                &scene,
                rect(100.0, 100.0, 900.0, 500.0),
                bar,
                Theme::current().scrim,
            )
            .unwrap();
            gpu.d2d.EndDraw(None, None).unwrap();
        }
    }
    #[test]
    fn background_and_disabled_controls_consume_pointer_input() {
        let shared = Shared::new().unwrap();
        let text = TextStyles::new(&shared, 1.0).unwrap();
        let e = Some(EditorBar {
            tool: Tool::Select,
            color: 0,
            width: 3,
            strength: 1,
            undo: false,
            redo: false,
        });
        let r = RECT {
            left: 0,
            top: 0,
            right: 1280,
            bottom: 720,
        };
        let sel = RECT {
            left: 100,
            top: 100,
            right: 900,
            bottom: 500,
        };
        let l = toolbar_layout(
            &shared,
            &text,
            (1280.0, 720.0),
            1.0,
            rect(100.0, 100.0, 900.0, 500.0),
            e,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            toolbar_hit(
                &shared,
                &text,
                r,
                1.0,
                sel,
                (l.panel.left as i32 + 5, l.panel.top as i32 + 5),
                e
            ),
            Some(ToolbarButton::Surface)
        );
        let undo = l.buttons.iter().find(|b| b.0 == ToolbarButton::Undo).unwrap().1;
        assert_eq!(
            toolbar_hit(
                &shared,
                &text,
                r,
                1.0,
                sel,
                ((undo.left + undo.right) as i32 / 2, (undo.top + undo.bottom) as i32 / 2),
                e
            ),
            Some(ToolbarButton::Surface)
        );
    }
    #[test]
    fn properties_are_contextual_and_selection_toolbar_is_compact() {
        let shared = Shared::new().unwrap();
        let text = TextStyles::new(&shared, 1.0).unwrap();
        let editor = EditorBar {
            tool: Tool::Select,
            color: 0,
            width: 3,
            strength: 1,
            undo: false,
            redo: false,
        };
        let layout = toolbar_layout(
            &shared,
            &text,
            (1280.0, 720.0),
            1.0,
            rect(100.0, 100.0, 900.0, 500.0),
            Some(editor),
        )
        .unwrap()
        .unwrap();
        assert!(
            layout.panel.bottom - layout.panel.top <= 116.0,
            "selection UI must leave the image room"
        );
        assert!(!layout.buttons.iter().any(|b| matches!(
            b.0,
            ToolbarButton::Color(_) | ToolbarButton::Width | ToolbarButton::Strength
        )));
        let blur = toolbar_layout(
            &shared,
            &text,
            (1280.0, 720.0),
            1.0,
            rect(100.0, 100.0, 900.0, 500.0),
            Some(EditorBar {
                tool: Tool::Blur,
                ..editor
            }),
        )
        .unwrap()
        .unwrap();
        assert!(blur.buttons.iter().any(|b| b.0 == ToolbarButton::Strength));
        assert!(
            !blur
                .buttons
                .iter()
                .any(|b| matches!(b.0, ToolbarButton::Color(_) | ToolbarButton::Width))
        );
    }

    #[test]
    fn toolbar_targets_stay_inside_the_panel_on_small_surfaces_and_at_two_hundred_percent() {
        let shared = Shared::new().unwrap();
        for s in [1.0, 1.5, 2.0] {
            let text = TextStyles::new(&shared, s).unwrap();
            for width in [360.0, 640.0, 1280.0] {
                for tool in Tool::ALL {
                    let editor = EditorBar {
                        tool,
                        color: 0,
                        width: 3,
                        strength: 1,
                        undo: true,
                        redo: false,
                    };
                    let layout = toolbar_layout(
                        &shared,
                        &text,
                        (width * s, 720.0 * s),
                        s,
                        rect(20.0 * s, 20.0 * s, 320.0 * s, 680.0 * s),
                        Some(editor),
                    )
                    .unwrap()
                    .unwrap();
                    for (_, r, _) in &layout.buttons {
                        assert!(
                            r.left >= layout.panel.left
                                && r.right <= layout.panel.right + 0.1
                                && r.top >= layout.panel.top
                                && r.bottom <= layout.panel.bottom,
                            "{tool:?} control escaped its panel at {width} DIP"
                        );
                        assert!(r.right - r.left >= 28.0 * s && r.bottom - r.top >= 28.0 * s);
                    }
                }
            }
        }
    }
    #[test]
    fn all_actions_have_a_visible_non_overlapping_hit_target_at_each_dpi() {
        let shared = Shared::new().unwrap();
        for s in [1.0, 1.5, 2.0] {
            let text = TextStyles::new(&shared, s).unwrap();
            let size = (1280.0 * s, 720.0 * s);
            let selection = rect(200.0 * s, 100.0 * s, 1100.0 * s, 500.0 * s);
            let editor = Some(EditorBar {
                tool: Tool::Select,
                color: 0,
                width: 3,
                strength: 1,
                undo: false,
                redo: false,
            });
            let layout = toolbar_layout(&shared, &text, size, s, selection, editor)
                .unwrap()
                .unwrap();
            assert!(
                layout.panel.left >= 0.0
                    && layout.panel.right <= size.0
                    && layout.panel.top >= 0.0
                    && layout.panel.bottom <= size.1
            );
            for button in ToolbarButton::ALL {
                let (_, r, label) = layout.buttons.iter().find(|b| b.0 == button).unwrap();
                assert!(label.w <= r.right - r.left);
                let hit = toolbar_hit(
                    &shared,
                    &text,
                    RECT {
                        left: 0,
                        top: 0,
                        right: size.0 as i32,
                        bottom: size.1 as i32,
                    },
                    s,
                    RECT {
                        left: selection.left as i32,
                        top: selection.top as i32,
                        right: selection.right as i32,
                        bottom: selection.bottom as i32,
                    },
                    (((r.left + r.right) / 2.0) as i32, ((r.top + r.bottom) / 2.0) as i32),
                    editor,
                );
                assert_eq!(hit, Some(button));
            }
            for (i, (_, r, _)) in layout.buttons.iter().enumerate() {
                for (_, other, _) in layout.buttons.iter().skip(i + 1) {
                    assert!(intersect(*r, *other).is_none());
                }
            }
        }
    }
}
