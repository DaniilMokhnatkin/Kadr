//! Отрисовка сцены оверлея. Одна функция и для окна, и для `--snapshot`.
//! Координаты — физические пиксели поверхности; размеры из `theme::dip` умножаются на масштаб.

use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct2D::Common::{D2D_RECT_F, D2D1_COLOR_F};
use windows::Win32::Graphics::Direct2D::{
    D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR, D2D1_ROUNDED_RECT, ID2D1Bitmap1,
    ID2D1StrokeStyle,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT, DWRITE_FONT_WEIGHT_REGULAR,
    DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_LINE_METRICS, DWRITE_TEXT_METRICS, DWRITE_TEXT_RANGE,
    DWRITE_WORD_WRAPPING_NO_WRAP, IDWriteFontCollection, IDWriteTextFormat, IDWriteTextLayout,
};
use windows::core::{PCWSTR, Result, w};
use windows_numerics::Vector2;

use crate::annotations::{self, Cache, View};
use crate::gpu::{Gpu, Shared};
use crate::theme::{Theme, dip};
use kadr_doc::annotation::{COLORS, Tool};

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
        })
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
            Self::Undo => "Назад Z",
            Self::Redo => "Повтор Y",
        }
    }
}

/// Панель кнопок у выделения; `hovered` — кнопка под курсором.
#[derive(Clone, Copy, Default)]
pub struct Toolbar {
    pub hovered: Option<ToolbarButton>,
    pub editor: Option<EditorBar>,
}
#[derive(Clone, Copy)]
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

    if let Some(drawing) = scene.marks.drawing {
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

    match selection.and_then(|s| intersect(s, full)) {
        None => fill(gpu, theme.scrim, full),
        Some(c) => {
            fill(gpu, theme.scrim, rect(0.0, 0.0, full.right, c.top));
            fill(gpu, theme.scrim, rect(0.0, c.bottom, full.right, full.bottom));
            fill(gpu, theme.scrim, rect(0.0, c.top, c.left, c.bottom));
            fill(gpu, theme.scrim, rect(c.right, c.top, full.right, c.bottom));
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
            draw_toolbar(gpu, shared, text, theme, scene, sel, toolbar)?;
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

struct ToolbarLayout {
    panel: D2D_RECT_F,
    buttons: Vec<(ToolbarButton, D2D_RECT_F, Text)>,
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
    let (w, h) = size;
    let (corner_x, corner_y) = (sel.right - 1.0, sel.bottom - 1.0);
    if corner_x < 0.0 || corner_y < 0.0 || corner_x >= w || corner_y >= h {
        return Ok(None);
    }
    let s = scale;
    let (pad, gap) = (dip::SPACE_1 * s, dip::SPACE_1 * s);
    let (button_x, button_y) = (dip::SPACE_3 * s, dip::SPACE_2 * s);
    let offset = (dip::SPACE_2 + dip::FRAME + dip::FRAME_OUTLINE) * s;

    let labels = ToolbarButton::ALL
        .iter()
        .map(|b| Ok((*b, measure(shared, &text.button, b.label())?)))
        .collect::<Result<Vec<_>>>()?;
    let button_h = (labels.iter().map(|(_, t)| t.h).fold(0.0, f32::max) + button_y * 2.0).ceil();
    let widths: Vec<f32> = labels.iter().map(|(_, t)| (t.w + button_x * 2.0).ceil()).collect();
    let export_w = pad * 2.0 + widths.iter().copied().fold(0.0, f32::max) * 3.0 + gap * 2.0;
    let columns = 7usize;
    let editor_w = if editor.is_some() {
        (660.0 * s).min(w - 8.0 * s)
    } else {
        0.0
    };
    let panel_w = export_w.max(editor_w).min(w);
    let extra = if editor.is_some() {
        3.0 * (button_h + gap) + 24.0 * s
    } else {
        0.0
    };
    let panel_h = button_h * 2.0 + gap + pad * 2.0 + extra;

    let mut y = sel.bottom + offset;
    if y + panel_h > h {
        y = sel.top - offset - panel_h;
    }
    if y < 0.0 {
        y = sel.bottom - offset - panel_h;
    }
    let x = (sel.right - panel_w).clamp(0.0, (w - panel_w).max(0.0)).round();
    let y = y.clamp(0.0, (h - panel_h).max(0.0)).round();

    let cell = (panel_w - pad * 2.0 - gap * 2.0) / 3.0;
    let mut buttons: Vec<_> = labels
        .into_iter()
        .enumerate()
        .map(|(i, (button, label))| {
            let left = x + pad + (i % 3) as f32 * (cell + gap);
            let top = y + pad + extra + (i / 3) as f32 * (button_h + gap);
            let r = rect(left, top, left + cell, top + button_h);
            (button, r, label)
        })
        .collect();
    if let Some(editor) = editor {
        let cell = (panel_w - 2.0 * pad - gap * (columns - 1) as f32) / columns as f32;
        for (i, tool) in Tool::ALL.into_iter().enumerate() {
            let l = x + pad + (i % columns) as f32 * (cell + gap);
            let t = y + pad + (i / columns) as f32 * (button_h + gap);
            buttons.push((
                ToolbarButton::Tool(tool),
                rect(l, t, l + cell, t + button_h),
                measure(shared, &text.label, tool.label())?,
            ));
        }
        let t = y + pad + 2.0 * (button_h + gap);
        let mut l = x + pad;
        for i in 0..COLORS.len() {
            let cw = 26.0 * s;
            buttons.push((
                ToolbarButton::Color(i),
                rect(l, t, l + cw, t + button_h),
                measure(shared, &text.button, "")?,
            ));
            l += cw + gap;
        }
        for (button, label) in [
            (ToolbarButton::Width, format!("{} px", editor.width)),
            (ToolbarButton::Strength, format!("Сила {}", editor.strength + 1)),
            (ToolbarButton::Undo, "Назад Z".into()),
            (ToolbarButton::Redo, "Повтор Y".into()),
        ] {
            let label = measure(shared, &text.button, &label)?;
            let bw = label.w + button_x * 2.0;
            buttons.push((button, rect(l, t, l + bw, t + button_h), label));
            l += bw + gap;
        }
    }
    Ok(Some(ToolbarLayout {
        panel: rect(x, y, x + panel_w, y + panel_h),
        buttons,
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
    let layout = toolbar_layout(shared, text, size, scale, sel, editor).ok()??;
    let (x, y) = ((at.0 - ox) as f32, (at.1 - oy) as f32);
    layout
        .buttons
        .into_iter()
        .find(|(_, r, _)| x >= r.left && x < r.right && y >= r.top && y < r.bottom)
        .map(|(b, _, _)| b)
}

/// Основное действие — белая заливка (editorial: «filled white on black»), остальные — текстом,
/// с тоновой подложкой при наведении.
fn draw_toolbar(
    gpu: &Gpu,
    shared: &Shared,
    text: &TextStyles,
    theme: &Theme,
    scene: &Scene,
    sel: D2D_RECT_F,
    toolbar: Toolbar,
) -> Result<()> {
    let size = (scene.size.0 as f32, scene.size.1 as f32);
    let Some(layout) = toolbar_layout(shared, text, size, scene.scale, sel, toolbar.editor)? else {
        return Ok(());
    };
    let s = scene.scale;
    panel(gpu, theme, layout.panel, dip::RADIUS_MD * s, s);
    for (button, r, label) in &layout.buttons {
        let hovered = toolbar.hovered == Some(*button);
        if let ToolbarButton::Color(index) = button {
            let c = COLORS[*index];
            let ink = D2D1_COLOR_F {
                r: c[0] as f32 / 255.0,
                g: c[1] as f32 / 255.0,
                b: c[2] as f32 / 255.0,
                a: 1.0,
            };
            let swatch = rect(r.left + 5.0 * s, r.top + 7.0 * s, r.right - 5.0 * s, r.bottom - 7.0 * s);
            fill(gpu, ink, swatch);
            ring(gpu, theme.text_muted, swatch, s);
            if toolbar.editor.is_some_and(|e| e.color == *index) {
                ring(gpu, theme.text, *r, 2.0 * s);
            }
            continue;
        }
        let selected = matches!(button,ToolbarButton::Tool(tool) if toolbar.editor.is_some_and(|e|e.tool==*tool));
        if selected {
            fill_rounded(gpu, theme.border, *r, dip::RADIUS_SM * s);
            ring(gpu, theme.text, *r, s);
        }
        let enabled = toolbar.editor.is_none_or(|e| match button {
            ToolbarButton::Undo => e.undo,
            ToolbarButton::Redo => e.redo,
            _ => true,
        });
        let ink = if !enabled {
            theme.text_muted
        } else if *button == ToolbarButton::Copy {
            fill_rounded(
                gpu,
                if hovered { theme.text } else { theme.frame },
                *r,
                dip::RADIUS_SM * s,
            );
            theme.surface
        } else {
            if hovered {
                fill_rounded(gpu, theme.border, *r, dip::RADIUS_SM * s);
            }
            theme.text
        };
        draw_text(
            gpu,
            label,
            r.left + (r.right - r.left - label.w) / 2.0,
            r.top + (r.bottom - r.top - label.h) / 2.0,
            ink,
        );
    }
    if let Some(editor) = toolbar.editor {
        let hint = if editor.tool.is_blur() {
            "Размытие скрывает детали · Для паролей используйте «Скрыть»"
        } else if editor.tool == Tool::Text {
            "Введите текст · Enter — готово · Shift+Enter — строка · Esc — отменить"
        } else {
            "Ctrl+D — закрепить · Ctrl+H — недавние · Tab — инструмент · 1–6 — цвет"
        };
        let hint = measure(shared, &text.label, hint)?;
        let export_top = layout.buttons[0].1.top;
        draw_text(
            gpu,
            &hint,
            layout.panel.left + 8.0 * s,
            export_top - 22.0 * s,
            theme.text_muted,
        );
    }
    Ok(())
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

/// Непрозрачная панель с рамкой в 1 px — глубина только рамкой, без теней (editorial).
fn panel(gpu: &Gpu, theme: &Theme, r: D2D_RECT_F, radius: f32, scale: f32) {
    let stroke = px(dip::BORDER, scale);
    let half = stroke / 2.0;
    let inner = rect(r.left + half, r.top + half, r.right - half, r.bottom - half);
    let rounded = |r| D2D1_ROUNDED_RECT {
        rect: r,
        radiusX: radius,
        radiusY: radius,
    };
    unsafe {
        gpu.brush.SetColor(&theme.surface);
        gpu.d2d.FillRoundedRectangle(&rounded(r), &gpu.brush);
        gpu.brush.SetColor(&theme.border);
        gpu.d2d
            .DrawRoundedRectangle(&rounded(inner), &gpu.brush, stroke, None::<&ID2D1StrokeStyle>);
    }
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
