//! Native implementation of Claude's concept 04. Geometry is in Windows DIP, not CSS pixels.
use super::*;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BarMenu {
    Shapes,
    Blur,
    More,
    Properties,
    Recent,
    Record,
}
impl BarMenu {
    pub fn label(self) -> &'static str {
        match self {
            Self::Shapes => "Фигуры",
            Self::Blur => "Размытие",
            Self::More => "Ещё",
            Self::Properties => "Параметры разметки",
            Self::Recent => "Недавние",
            Self::Record => "Запись экрана",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BarState {
    pub menu: Option<BarMenu>,
    pub shape: Tool,
    pub blur: Tool,
    pub pinned: bool,
    pub original: bool,
    pub system_audio: bool,
    pub microphone: bool,
    pub recent_count: usize,
    pub page: usize,
    pub focus: Option<ToolbarButton>,
}
impl Default for BarState {
    fn default() -> Self {
        Self {
            menu: None,
            shape: Tool::Rectangle,
            blur: Tool::Blur,
            pinned: false,
            original: false,
            system_audio: true,
            microphone: false,
            recent_count: 0,
            page: 0,
            focus: None,
        }
    }
}
impl BarState {
    pub fn toggle(&mut self, menu: BarMenu) {
        self.menu = (self.menu != Some(menu)).then_some(menu);
        self.focus = None;
        self.page = 0;
    }
    pub fn select_tool(&mut self, tool: Tool) {
        if matches!(tool, Tool::Line | Tool::Rectangle | Tool::Ellipse) {
            self.shape = tool;
        }
        if matches!(tool, Tool::Blur | Tool::BlurEllipse | Tool::BlurBrush) {
            self.blur = tool;
        }
        self.menu = None;
        self.focus = None;
    }
}
pub struct RecentItem {
    pub preview: Option<Arc<kadr_doc::Image>>,
    pub title: String,
    pub caption: String,
    pub pinned: bool,
}
const SIDE: f32 = 44.0;
const GAP: f32 = 8.0;

/// CSS field-label line heights and padding from the reference, in DIP.
struct Fields {
    color: Option<f32>,
    width: f32,
    style: Option<f32>,
    strength: Option<f32>,
    height: f32,
}
fn fields(e: EditorBar) -> Fields {
    let colored = !e.tool.is_blur() && e.tool != Tool::Lens;
    let mut top = 8.0;
    let color = colored.then(|| {
        top += 26.8;
        let row = top;
        top += 44.0;
        row
    });
    top += if colored { 32.8 } else { 26.8 };
    let width = top;
    top += 44.0;
    let style = (e.tool == Tool::Text).then(|| {
        top += 32.8;
        let row = top;
        top += 44.0;
        row
    });
    let strength = (e.tool.is_blur() || e.tool == Tool::Lens).then(|| {
        top += 32.8;
        let row = top;
        top += 44.0;
        row
    });
    Fields { color, width, style, strength, height: top + 8.0 }
}

pub(super) fn build(
    shared: &Shared,
    text: &TextStyles,
    size: (f32, f32),
    s: f32,
    sel: D2D_RECT_F,
    e: EditorBar,
) -> Result<Option<ToolbarLayout>> {
    let (w, h) = size;
    if sel.right - 1.0 < 0.0 || sel.bottom - 1.0 < 0.0 || sel.right - 1.0 >= w || sel.bottom - 1.0 >= h {
        return Ok(None);
    }
    let actions = [
        (ToolbarButton::Record, "Запись"),
        (ToolbarButton::Recent, "Недавние"),
        (ToolbarButton::Pin, "Закрепить"),
        (ToolbarButton::Text, "Текст"),
        (ToolbarButton::Translate, "Перевести"),
        (ToolbarButton::Save, "Сохранить"),
        (ToolbarButton::SaveAs, ""),
        (ToolbarButton::Copy, "Копировать"),
    ];
    let widths: Vec<f32> = actions
        .iter()
        .map(|(b, label)| {
            let style = if *b == ToolbarButton::Copy { &text.dock_strong } else { &text.dock_label };
            let label = measure(shared, style, label)?.w / s;
            Ok(match b {
                ToolbarButton::SaveAs => 44.0,
                ToolbarButton::Copy => label + 84.0,
                ToolbarButton::Recent => label + 78.0,
                ToolbarButton::Save => label + 48.0,
                _ => label + 54.0,
            })
        })
        .collect::<Result<_>>()?;
    let preferred = (widths.iter().sum::<f32>() + 6.0 * GAP + 16.0).max(927.8594);
    let pw = (preferred * s).min((w - 16.0 * s).max(60.0 * s));
    let content = pw / s - 16.0;
    let mut buttons = Vec::new();
    let mut y = 8.0;
    let empty = measure(shared, &text.dock_label, "")?;
    let mut add = |b, x: f32, y: f32, width: f32, label: &str| -> Result<()> {
        buttons.push((
            b,
            rect(x * s, y * s, (x + width) * s, (y + SIDE) * s),
            if label.is_empty() {
                empty.clone()
            } else {
                measure(shared, if b == ToolbarButton::Copy { &text.dock_strong } else { &text.dock_label }, label)?
            },
        ));
        Ok(())
    };
    // 8 tools share the neutral track. Utilities follow at the same 44 DIP height.
    let tools = [
        ToolbarButton::Tool(Tool::Select),
        ToolbarButton::Tool(Tool::Arrow),
        ToolbarButton::Menu(BarMenu::Shapes),
        ToolbarButton::Tool(Tool::Pen),
        ToolbarButton::Tool(Tool::Text),
        ToolbarButton::Tool(Tool::Counter),
        ToolbarButton::Menu(BarMenu::Blur),
        ToolbarButton::Menu(BarMenu::More),
    ];
    let utilities = [
        (ToolbarButton::Smart, measure(shared, &text.dock_label, "Цензура")?.w / s + 54.0, "Цензура"),
        (ToolbarButton::Undo, 44.0, ""),
        (ToolbarButton::Redo, 44.0, ""),
        (ToolbarButton::Menu(BarMenu::Properties), 78.0, ""),
    ];
    let narrow = content < 700.0;
    let track_width = if narrow { content } else { content - (utilities.iter().map(|u| u.1).sum::<f32>() + 4.0 * GAP) };
    let columns = if narrow && content < SIDE * 8.0 { 4 } else { 8 };
    let cell = track_width / columns as f32;
    for (i, b) in tools.into_iter().enumerate() {
        add(b, 8.0 + (i % columns) as f32 * cell, y + (i / columns) as f32 * SIDE, cell, "")?;
    }
    let track = rect(8.0 * s, y * s, (8.0 + track_width) * s, (y + SIDE * (8 / columns) as f32) * s);
    if narrow {
        y += SIDE * (8 / columns) as f32 + GAP;
    }
    let mut x = if narrow { 8.0 } else { 8.0 + track_width + GAP };
    for (b, width, label) in utilities {
        if x + width > pw / s - 8.0 + 0.1 {
            x = 8.0;
            y += SIDE + GAP;
        }
        add(b, x, y, width, label)?;
        x += width + GAP;
    }
    y += SIDE + GAP;
    if e.smart != SmartUi::Off {
        let levels = [(Censor::Light, "Лёгкая"), (Censor::Hard, "Жёсткая"), (Censor::Full, "Полная")];
        let info = if content >= 700.0 { 148.0 } else { 0.0 };
        let level_width = (content - SIDE - GAP - info) / 3.0;
        for (i, (level, label)) in levels.into_iter().enumerate() {
            add(ToolbarButton::Censor(level), 8.0 + i as f32 * level_width, y, level_width, label)?;
        }
        add(ToolbarButton::Original, pw / s - 8.0 - SIDE, y, SIDE, "")?;
        y += SIDE + GAP;
    }
    // One dense action row; no icon-only pin/text and no gap stretched across the middle.
    let extra = ((content - (widths.iter().sum::<f32>() + 6.0 * GAP)) / 7.0).max(0.0);
    x = 8.0;
    for ((b, label), width) in actions.into_iter().zip(widths) {
        let width = (width + if b == ToolbarButton::SaveAs { 0.0 } else { extra }).min(content);
        if x + width > pw / s - 8.0 + 0.1 && x > 8.0 {
            x = 8.0;
            y += SIDE + GAP;
        }
        add(b, x, y, width, label)?;
        x += width + if b == ToolbarButton::Save { 0.0 } else { GAP };
    }
    let ph = (y + SIDE + GAP) * s;
    let px = ((sel.left + sel.right - pw) / 2.0).clamp(8.0 * s, (w - pw - 8.0 * s).max(8.0 * s)).round();
    let below = sel.bottom + 12.0 * s;
    let py = if below + ph <= h - 8.0 * s { below } else { sel.top - 12.0 * s - ph };
    let py = py.clamp(8.0 * s, (h - ph - 8.0 * s).max(8.0 * s)).round();
    for (_, r, _) in &mut buttons {
        shift(r, px, py);
    }
    let mut track = track;
    shift(&mut track, px, py);
    let mut panel = rect(px, py, px + pw, py + ph);
    let main_count = buttons.len();
    let mut popup = if let Some(menu) = e.ui.menu {
        Some(popover(shared, text, (size.0, size.1, s), panel, e, menu, &mut buttons)?)
    } else {
        None
    };
    // On a narrow monitor the dock wraps. Move it toward the bottom before putting a tall menu
    // above it, rather than letting a menu cover the actions it belongs to.
    if popup.is_some_and(|p| intersect(p, panel).is_some()) {
        let dy = (h - 8.0 * s - panel.bottom).max(0.0);
        shift(&mut panel, 0.0, dy);
        shift(&mut track, 0.0, dy);
        buttons.truncate(main_count);
        for (_, r, _) in &mut buttons {
            shift(r, 0.0, dy);
        }
        popup = Some(popover(shared, text, (w, h, s), panel, e, e.ui.menu.unwrap(), &mut buttons)?);
    }
    Ok(Some(ToolbarLayout { panel, sel, buttons, context: None, track: Some(track), popup }))
}
fn shift(r: &mut D2D_RECT_F, x: f32, y: f32) {
    r.left += x;
    r.right += x;
    r.top += y;
    r.bottom += y;
}

fn popover(
    shared: &Shared,
    text: &TextStyles,
    viewport: (f32, f32, f32),
    panel: D2D_RECT_F,
    e: EditorBar,
    menu: BarMenu,
    buttons: &mut Vec<(ToolbarButton, D2D_RECT_F, Text)>,
) -> Result<D2D_RECT_F> {
    let (w, h, s) = viewport;
    let size = (w, h);
    let mut rows: Vec<(ToolbarButton, String)> = match menu {
        BarMenu::Shapes => [Tool::Line, Tool::Rectangle, Tool::Ellipse]
            .into_iter()
            .map(|t| {
                (
                    ToolbarButton::Tool(t),
                    match t {
                        Tool::Line => "Линия",
                        Tool::Rectangle => "Рамка",
                        _ => "Овал",
                    }
                    .into(),
                )
            })
            .collect(),
        BarMenu::Blur => [Tool::Blur, Tool::BlurEllipse, Tool::BlurBrush]
            .into_iter()
            .map(|t| {
                (
                    ToolbarButton::Tool(t),
                    match t {
                        Tool::Blur => "Область",
                        Tool::BlurEllipse => "Овал",
                        _ => "Кисть",
                    }
                    .into(),
                )
            })
            .collect(),
        BarMenu::More => vec![
            (ToolbarButton::Tool(Tool::Eraser), "Ластик".into()),
            (ToolbarButton::Tool(Tool::Lens), "Лупа".into()),
            (ToolbarButton::Tool(Tool::Spotlight), "Размытие вокруг".into()),
            (ToolbarButton::Tool(Tool::Redact), "Скрыть вручную".into()),
            (ToolbarButton::PinWindow, "Поверх окон".into()),
            (ToolbarButton::About, "О программе".into()),
            (ToolbarButton::Close, "Закрыть снимок".into()),
        ],
        BarMenu::Properties => Vec::new(),
        BarMenu::Recent => Vec::new(),
        BarMenu::Record => vec![
            (ToolbarButton::SystemAudio, "Звук компьютера".into()),
            (ToolbarButton::Microphone, "Микрофон".into()),
            (ToolbarButton::StartRecord, "Начать запись".into()),
        ],
    };
    let pw = ((if menu == BarMenu::Recent {
        456.0
    } else if menu == BarMenu::Properties {
        304.0
    } else if menu == BarMenu::Shapes || menu == BarMenu::Blur {
        280.0
    } else if menu == BarMenu::Record {
        288.0
    } else {
        264.0
    }) * s)
        .min(panel.right - panel.left - 16.0 * s);
    let fields = fields(e);
    let grid = menu == BarMenu::Recent;
    let count = e.ui.recent_count.saturating_sub(e.ui.page * 6).min(6);
    let card_width = (pw / s - 32.0) / 3.0;
    let card_height = card_width * 0.75 + 46.2;
    let rows_count = count.max(1).div_ceil(3);
    let ph = if grid && count == 0 {
        132.8 * s
    } else if grid {
        (60.0 + rows_count as f32 * card_height + (rows_count - 1) as f32 * 8.0) * s
    } else if menu == BarMenu::Shapes || menu == BarMenu::Blur {
        124.8 * s
    } else if menu == BarMenu::Record {
        188.8 * s
    } else if menu == BarMenu::Properties {
        fields.height * s
    } else {
        (25.0 + rows.len() as f32 * 44.0) * s
    };
    let anchor = buttons
        .iter()
        .find(|b| {
            b.0 == if grid {
                ToolbarButton::Recent
            } else if menu == BarMenu::Record {
                ToolbarButton::Record
            } else {
                ToolbarButton::Menu(menu)
            }
        })
        .map_or(panel, |b| b.1);
    let x = ((anchor.left + anchor.right - pw) / 2.0)
        .clamp(panel.left + 8.0 * s, (panel.right - pw - 8.0 * s).max(panel.left + 8.0 * s));
    let y = if panel.top - ph - 8.0 * s >= 0.0 {
        panel.top - ph - 8.0 * s
    } else {
        (panel.bottom + 8.0 * s).min(size.1 - ph - 8.0 * s).max(8.0 * s)
    };
    let popup = rect(x, y, x + pw, y + ph);
    let mut push = |b, r, label: &str| -> Result<()> {
        let style = if b == ToolbarButton::StartRecord {
            &text.dock_strong
        } else if matches!(
            b,
            ToolbarButton::WidthValue(_)
                | ToolbarButton::TextPlain
                | ToolbarButton::TextFilled
                | ToolbarButton::TextOutline
                | ToolbarButton::TextCallout
        ) || matches!(menu, BarMenu::Shapes | BarMenu::Blur)
        {
            &text.dock_option
        } else {
            &text.dock_label
        };
        buttons.push((b, r, measure(shared, style, label)?));
        Ok(())
    };
    if grid {
        let clear_width = measure(shared, &text.dock_label, "Очистить всё")?.w + 28.0 * s;
        push(
            ToolbarButton::RecentClear,
            rect(x + pw - 8.0 * s - clear_width, y + 8.0 * s, x + pw - 8.0 * s, y + 52.0 * s),
            "Очистить всё",
        )?;
        let cell = (pw - 32.0 * s) / 3.0;
        for n in 0..count {
            let i = e.ui.page * 6 + n;
            let left = x + 8.0 * s + (n % 3) as f32 * (cell + 8.0 * s);
            let top = y + 52.0 * s + (n / 3) as f32 * (card_height + 8.0) * s;
            push(ToolbarButton::RecentOpen(i), rect(left, top, left + cell, top + card_height * s), "")?;
            push(ToolbarButton::RecentPin(i), rect(left, top, left + 44.0 * s, top + 44.0 * s), "")?;
            push(ToolbarButton::RecentDelete(i), rect(left + cell - 44.0 * s, top, left + cell, top + 44.0 * s), "")?;
        }
        if e.ui.recent_count > 6 {
            // Keep long histories reachable without growing the reference's popup over the dock.
            for (next, left) in [(false, x + 100.0 * s), (true, x + 144.0 * s)] {
                push(ToolbarButton::RecentPage(next), rect(left, y + 8.0 * s, left + 44.0 * s, y + 52.0 * s), "")?;
            }
        }
    } else if menu == BarMenu::Shapes || menu == BarMenu::Blur {
        let cell = (pw - 32.0 * s) / 3.0;
        for (i, (b, label)) in rows.into_iter().enumerate() {
            let left = x + (8.0 + i as f32 * 8.0) * s + i as f32 * cell;
            push(b, rect(left, y + 40.8 * s, left + cell, y + 116.8 * s), &label)?;
        }
    } else if menu == BarMenu::Record {
        for (i, (b, label)) in rows.into_iter().enumerate() {
            let top = y + (40.8 + i as f32 * 44.0 + if i == 2 { 8.0 } else { 0.0 }) * s;
            push(b, rect(x + 8.0 * s, top, x + pw - 8.0 * s, top + 44.0 * s), &label)?;
        }
    } else if menu == BarMenu::Properties {
        if let Some(top) = fields.color.map(|top| y + top * s) {
            let cell = (pw - 16.0 * s) / COLORS.len() as f32;
            for i in 0..COLORS.len() {
                let left = x + 8.0 * s + i as f32 * cell;
                push(ToolbarButton::Color(i), rect(left, top, left + cell, top + 44.0 * s), "")?;
            }
        }
        let top = y + fields.width * s;
        let cell = (pw - 16.0 * s) / 4.0;
        for (i, n) in [2, 3, 5, 8].into_iter().enumerate() {
            let left = x + 8.0 * s + i as f32 * cell;
            push(ToolbarButton::WidthValue(n), rect(left, top, left + cell, top + 44.0 * s), &format!("{n} px"))?;
        }
        if let Some(top) = fields.strength.map(|top| y + top * s) {
            push(ToolbarButton::StrengthStep(false), rect(x + 8.0 * s, top, x + 52.0 * s, top + 44.0 * s), "")?;
            push(
                ToolbarButton::StrengthStep(true),
                rect(x + pw - 52.0 * s, top, x + pw - 8.0 * s, top + 44.0 * s),
                "",
            )?;
        }
        if let Some(top) = fields.style.map(|top| y + top * s) {
            for (i, b) in [
                ToolbarButton::TextPlain,
                ToolbarButton::TextFilled,
                ToolbarButton::TextOutline,
                ToolbarButton::TextCallout,
            ]
            .into_iter()
            .enumerate()
            {
                let left = x + 8.0 * s + i as f32 * cell;
                push(
                    b,
                    rect(left, top, left + cell, top + 44.0 * s),
                    if b == ToolbarButton::TextFilled { "Фон" } else { b.label() },
                )?;
            }
        }
    } else {
        for (i, (b, label)) in rows.drain(..).enumerate() {
            push(
                b,
                rect(
                    x + 8.0 * s,
                    y + (8.0 + i as f32 * 44.0 + if i >= 4 { 9.0 } else { 0.0 }) * s,
                    x + pw - 8.0 * s,
                    y + (52.0 + i as f32 * 44.0 + if i >= 4 { 9.0 } else { 0.0 }) * s,
                ),
                &label,
            )?;
        }
    }
    Ok(popup)
}

pub(super) fn draw(
    gpu: &Gpu,
    shared: &Shared,
    text: &TextStyles,
    theme: &Theme,
    scene: &Scene,
    sel: D2D_RECT_F,
    toolbar: Toolbar,
) -> Result<()> {
    let e = toolbar.editor.unwrap();
    let mut appearance = *theme;
    // Use the reference's roles here; other native windows retain their own materials.
    // System high-contrast colors take precedence over the reference palette.
    if appearance.shadow.a > 0.0 {
        let rgb = |value: u32, alpha| D2D1_COLOR_F {
            r: ((value >> 16) & 255) as f32 / 255.0,
            g: ((value >> 8) & 255) as f32 / 255.0,
            b: (value & 255) as f32 / 255.0,
            a: alpha,
        };
        appearance.text_muted = rgb(if appearance.dark { 0xa1a1a6 } else { 0x636366 }, 1.0);
        appearance.selected =
            rgb(if appearance.dark { 0x0a84ff } else { 0x0071e3 }, if appearance.dark { 0.2 } else { 0.12 });
        appearance.on_selected = rgb(if appearance.dark { 0x4da3ff } else { 0x0062c4 }, 1.0);
        appearance.track = rgb(if appearance.dark { 0x48484b } else { 0xd6d6db }, 1.0);
        appearance.border = rgb(if appearance.dark { 0xffffff } else { 0x000000 }, 0.1);
    }
    let theme = &appearance;
    let s = scene.scale;
    let Some(target) = toolbar_layout(shared, text, (scene.size.0 as f32, scene.size.1 as f32), s, sel, Some(e))?
    else {
        return Ok(());
    };
    let l = text.displayed(&target, Some(e), Some(toolbar.hovered), s);
    let a = l.opacity * (1.0 - scene.marks.exit * 1.5).clamp(0.0, 1.0);
    let k = l.scale * (1.0 - 0.06 * scene.marks.exit);
    let (ax, ay) = l.anchor;
    let mut previous = Matrix3x2::default();
    unsafe {
        gpu.d2d.GetTransform(&mut previous);
        gpu.d2d.SetTransform(&Matrix3x2 {
            M11: k,
            M12: 0.0,
            M21: 0.0,
            M22: k,
            M31: ax * (1.0 - k),
            M32: ay * (1.0 - k) + l.lift,
        });
    }
    fill_rounded(gpu, fade(theme.surface, a), l.panel, 30.0 * s);
    edge(gpu, theme, l.panel, 30.0 * s, s, a);
    if let Some(mut track) = target.track {
        track.left += l.panel.left - target.panel.left;
        track.right += l.panel.right - target.panel.right;
        track.top += l.panel.top - target.panel.top;
        track.bottom += l.panel.top - target.panel.top;
        fill_rounded(gpu, fade(theme.control, a), track, 22.0 * s);
    }
    if let Some(r) = l.indicator {
        fill_rounded(gpu, fade(theme.accent, a), inflate(r, -4.0 * s), 18.0 * s);
    }
    // The reference has one neutral track per segmented group, with one moving blue blob.
    for group in 0..1 {
        let mut track = None;
        for (b, r, _) in &target.buttons {
            if radio_group(*b) == Some(group) {
                track = Some(track.map_or(*r, |p: D2D_RECT_F| {
                    rect(p.left.min(r.left), p.top.min(r.top), p.right.max(r.right), p.bottom.max(r.bottom))
                }));
            }
        }
        if let Some(track) = track {
            fill_rounded(gpu, fade(theme.control, a), track, 22.0 * s);
        }
        if let Some(r) = l.radios[group] {
            fill_rounded(gpu, fade(theme.accent, a), inflate(r, -4.0 * s), 18.0 * s);
        }
    }
    if let Some(p) = target.popup {
        shadow(gpu, theme.shadow, p, 22.0 * s, s, a, true);
        fill_rounded(gpu, fade(theme.surface, a), p, 22.0 * s);
        edge(gpu, theme, p, 22.0 * s, s, a);
        if e.ui.menu == Some(BarMenu::Recent) {
            caption(
                gpu,
                shared,
                &text.dock_heading,
                "Недавние",
                p.left + 20.0 * s,
                p.top + 23.0 * s,
                fade(theme.text_muted, a),
            )?;
            if (1..=6).contains(&e.ui.recent_count) {
                caption(
                    gpu,
                    shared,
                    &text.dock_option,
                    &count_label(e.ui.recent_count, ["снимок", "снимка", "снимков"]),
                    p.right - 184.0 * s,
                    p.top + 23.0 * s,
                    fade(theme.text_muted, a),
                )?;
            }
            if e.ui.recent_count == 0 {
                let r = rect(p.left + 8.0 * s, p.top + 52.0 * s, p.right - 8.0 * s, p.bottom - 8.0 * s);
                fill_rounded(gpu, fade(theme.control, a), r, 14.0 * s);
                let t = measure(shared, &text.dock_label, "Недавних снимков нет")?;
                draw_text(
                    gpu,
                    &t,
                    (r.left + r.right - t.w) / 2.0,
                    (r.top + r.bottom - t.h) / 2.0,
                    fade(theme.text_muted, a),
                );
            }
        } else if matches!(e.ui.menu, Some(BarMenu::Shapes | BarMenu::Blur | BarMenu::Record)) {
            let title = if e.ui.menu == Some(BarMenu::Record) {
                "Запись области"
            } else {
                e.ui.menu.unwrap().label()
            };
            caption(
                gpu,
                shared,
                &text.dock_heading,
                title,
                p.left + 20.0 * s,
                p.top + 14.0 * s,
                fade(theme.text_muted, a),
            )?;
        } else if e.ui.menu == Some(BarMenu::Properties) {
            let f = fields(e);
            for (row, label) in [
                (f.color, "Цвет"),
                (Some(f.width), "Толщина"),
                (f.style, "Оформление текста"),
                (f.strength, if e.tool == Tool::Lens { "Увеличение" } else { "Сила эффекта" }),
            ] {
                if let Some(top) = row {
                    caption(
                        gpu,
                        shared,
                        &text.dock_option,
                        label,
                        p.left + 20.0 * s,
                        p.top + (top - 22.8) * s,
                        fade(theme.text_muted, a),
                    )?;
                }
            }
            if let Some(top) = f.strength {
                let r = rect(p.left + 8.0 * s, p.top + top * s, p.right - 8.0 * s, p.top + (top + 44.0) * s);
                fill_rounded(gpu, fade(theme.control, a), r, 22.0 * s);
                let t = measure(shared, &text.dock_strong, &(e.strength + 1).to_string())?;
                draw_text(gpu, &t, (r.left + r.right - t.w) / 2.0, (r.top + r.bottom - t.h) / 2.0, fade(theme.text, a));
            }
        } else if e.ui.menu == Some(BarMenu::More) {
            fill(
                gpu,
                fade(theme.border, a),
                rect(p.left + 20.0 * s, p.top + 188.0 * s, p.right - 20.0 * s, p.top + 189.0 * s),
            );
        }
    }
    // Properties tracks are inside the popup surface, so paint them after that surface.
    if e.ui.menu == Some(BarMenu::Properties) {
        for group in 1..3 {
            let row: Vec<_> = target.buttons.iter().filter(|b| radio_group(b.0) == Some(group)).collect();
            if let (Some(first), Some(last)) = (row.first(), row.last()) {
                fill_rounded(
                    gpu,
                    fade(theme.control, a),
                    rect(first.1.left, first.1.top, last.1.right, last.1.bottom),
                    22.0 * s,
                );
                if let Some(r) = l.radios[group] {
                    fill_rounded(gpu, fade(theme.accent, a), inflate(r, -4.0 * s), 18.0 * s);
                }
            }
        }
    }
    for c in &l.buttons {
        let b = c.button;
        let r = inflate(c.rect, -s * c.press);
        let alpha = a * c.opacity;
        if alpha < 0.002 || (!c.live && !inside_rect(l.panel, r) && target.popup.is_none()) {
            continue;
        }
        let is_main = matches!(
            b,
            ToolbarButton::Tool(Tool::Select | Tool::Arrow | Tool::Pen | Tool::Text | Tool::Counter)
                | ToolbarButton::Menu(BarMenu::Shapes | BarMenu::Blur | BarMenu::More)
        );
        let is_tile = !is_main
            && matches!(b, ToolbarButton::Tool(_))
            && matches!(e.ui.menu, Some(BarMenu::Shapes | BarMenu::Blur));
        let group = radio_group(b);
        let selected = c.selected;
        if let ToolbarButton::RecentOpen(i) = b {
            if let Some(item) = toolbar.recent.get(i) {
                draw_recent(gpu, shared, text, theme, r, item, (s, alpha, c.hover))?;
            }
            continue;
        }
        if let ToolbarButton::RecentPin(i) | ToolbarButton::RecentDelete(i) = b {
            let pinned =
                matches!(b, ToolbarButton::RecentPin(_)) && toolbar.recent.get(i).is_some_and(|item| item.pinned);
            let near = |button: Option<ToolbarButton>| matches!(button,Some(ToolbarButton::RecentOpen(j)|ToolbarButton::RecentPin(j)|ToolbarButton::RecentDelete(j)) if j==i);
            if pinned || near(toolbar.hovered) || near(e.ui.focus) {
                let circle = inflate(r, -8.0 * s);
                let color = if pinned {
                    theme.accent
                } else {
                    D2D1_COLOR_F { r: 20.0 / 255.0, g: 20.0 / 255.0, b: 22.0 / 255.0, a: 0.72 + 0.18 * c.hover }
                };
                fill_rounded(gpu, fade(color, alpha), circle, 14.0 * s);
                glyphs::v4(
                    gpu,
                    if matches!(b, ToolbarButton::RecentPin(_)) { "pin" } else { "close" },
                    r,
                    fade(theme.on_accent, alpha),
                    16.0 * s,
                );
            }
            continue;
        }
        if is_tile {
            fill_rounded(gpu, fade(if selected > 0.5 { theme.accent } else { theme.control }, alpha), r, 14.0 * s);
        } else if b == ToolbarButton::Copy || b == ToolbarButton::StartRecord {
            fill_rounded(gpu, fade(theme.accent, alpha), r, 22.0 * s);
        } else if !is_main
            && group.is_none()
            && !matches!(b, ToolbarButton::Color(_) | ToolbarButton::SystemAudio | ToolbarButton::Microphone)
            && selected > 0.001
        {
            fill_rounded(
                gpu,
                fade(if matches!(b, ToolbarButton::Tool(_)) { theme.accent } else { theme.selected }, alpha * selected),
                r,
                if e.ui.menu == Some(BarMenu::More) { 14.0 * s } else { 22.0 * s },
            );
        } else if c.hover > 0.001 {
            fill_rounded(
                gpu,
                fade(theme.control, alpha * c.hover),
                if is_main || group.is_some() { inflate(r, -4.0 * s) } else { r },
                if is_main || group.is_some() { 18.0 * s } else { 22.0 * s },
            );
        }
        if e.ui.focus == Some(b) {
            edge(gpu, theme, inflate(r, -2.0 * s), 20.0 * s, s, alpha);
        }
        if let ToolbarButton::Color(i) = b {
            let rgb = COLORS[i];
            let center = (r.left + r.right) / 2.0;
            let circle = rect(center - 12.0 * s, r.top + 10.0 * s, center + 12.0 * s, r.bottom - 10.0 * s);
            fill_rounded(
                gpu,
                D2D1_COLOR_F { r: rgb[0] as f32 / 255.0, g: rgb[1] as f32 / 255.0, b: rgb[2] as f32 / 255.0, a: alpha },
                circle,
                12.0 * s,
            );
            if e.color == i {
                outline(gpu, fade(theme.text, alpha), inflate(circle, 3.0 * s), 15.0 * s, 2.0 * s);
            }
            continue;
        }
        let ink = fade(if !c.enabled { theme.disabled } else { theme.text }, alpha);
        if matches!(b, ToolbarButton::SystemAudio | ToolbarButton::Microphone) {
            glyph(gpu, b, rect(r.left + 12.0 * s, r.top, r.left + 32.0 * s, r.bottom), ink, s, e);
            draw_text(gpu, &c.label, r.left + 42.0 * s, (r.top + r.bottom - c.label.h) / 2.0, ink);
            let track = rect(r.right - 52.0 * s, r.top + 10.0 * s, r.right - 12.0 * s, r.bottom - 10.0 * s);
            fill_rounded(gpu, fade(if selected > 0.5 { theme.accent } else { theme.track }, alpha), track, 12.0 * s);
            let x = if selected > 0.5 { track.right - 22.0 * s } else { track.left + 2.0 * s };
            fill_rounded(
                gpu,
                fade(theme.on_accent, alpha),
                rect(x, track.top + 2.0 * s, x + 20.0 * s, track.bottom - 2.0 * s),
                10.0 * s,
            );
            continue;
        }
        if b == ToolbarButton::Menu(BarMenu::Properties) {
            let rgb = COLORS[e.color.min(COLORS.len() - 1)];
            fill_rounded(
                gpu,
                D2D1_COLOR_F { r: rgb[0] as f32 / 255.0, g: rgb[1] as f32 / 255.0, b: rgb[2] as f32 / 255.0, a: alpha },
                rect(r.left + 14.0 * s, r.top + 15.0 * s, r.left + 28.0 * s, r.top + 29.0 * s),
                7.0 * s,
            );
            let t = measure(shared, &text.dock_label, &format!("{} px", e.width))?;
            draw_text(gpu, &t, r.left + 34.0 * s, (r.top + r.bottom - t.h) / 2.0, ink);
            continue;
        }
        let primary = b == ToolbarButton::Copy
            || b == ToolbarButton::StartRecord
            || (is_tile && selected > 0.5)
            || (e.ui.menu == Some(BarMenu::More) && selected > 0.5);
        let ink = if primary {
            fade(theme.on_accent, alpha)
        } else if b == ToolbarButton::RecentClear || (selected > 0.5 && !is_main && group.is_none()) {
            fade(theme.on_selected, alpha)
        } else {
            ink
        };
        let paint = |ink: D2D1_COLOR_F, muted: D2D1_COLOR_F| -> Result<()> {
            if let ToolbarButton::Censor(level) = b {
                let title = measure(shared, &text.dock_strong, level.name())?;
                let desc = measure(
                    shared,
                    &text.dock_small,
                    match level {
                        Censor::Light => "Контакты и коды",
                        Censor::Hard => "Плюс имена и адреса",
                        Censor::Full => "Весь текст",
                    },
                )?;
                draw_text(gpu, &title, (r.left + r.right - title.w) / 2.0, r.top + 7.0 * s, ink);
                if desc.w < r.right - r.left - 8.0 * s {
                    draw_text(gpu, &desc, (r.left + r.right - desc.w) / 2.0, r.top + 24.0 * s, muted);
                }
            } else if is_tile {
                glyphs::v4(gpu, tool_icon(b), rect(r.left, r.top + 16.0 * s, r.right, r.top + 38.0 * s), ink, 22.0 * s);
                let t = &c.label;
                draw_text(gpu, t, (r.left + r.right - t.w) / 2.0, r.top + 44.0 * s, ink);
            } else if e.ui.menu == Some(BarMenu::More) && !inside_rect(l.panel, r) {
                glyph(gpu, b, rect(r.left + 12.0 * s, r.top, r.left + 32.0 * s, r.bottom), ink, s, e);
                draw_text(gpu, &c.label, r.left + 42.0 * s, (r.top + r.bottom - c.label.h) / 2.0, ink);
                let key = match b {
                    ToolbarButton::Tool(Tool::Eraser) => "M",
                    ToolbarButton::Tool(Tool::Lens) => "G",
                    ToolbarButton::Tool(Tool::Spotlight) => "I",
                    ToolbarButton::Tool(Tool::Redact) => "X",
                    ToolbarButton::Close => "Esc",
                    _ => "",
                };
                let key = measure(shared, &text.dock_option, key)?;
                draw_text(gpu, &key, r.right - 12.0 * s - key.w, (r.top + r.bottom - key.h) / 2.0, muted);
            } else if group.is_some()
                || matches!(b, ToolbarButton::RecentClear | ToolbarButton::RecentPage(_) | ToolbarButton::StartRecord)
            {
                draw_text(
                    gpu,
                    &c.label,
                    (r.left + r.right - c.label.w) / 2.0,
                    (r.top + r.bottom - c.label.h) / 2.0,
                    ink,
                );
            } else if c.label.w > 0.0 {
                let badge = if b == ToolbarButton::Recent { 24.0 * s } else { 0.0 };
                let trailing = if b == ToolbarButton::Copy { 28.0 * s } else { 0.0 };
                let width = 26.0 * s + c.label.w + badge + trailing;
                let left = r.left + (r.right - r.left - width) / 2.0;
                glyph(
                    gpu,
                    b,
                    rect(left, r.top, left + 20.0 * s, r.bottom),
                    if b == ToolbarButton::Record {
                        D2D1_COLOR_F { r: 1.0, g: 69.0 / 255.0, b: 58.0 / 255.0, a: alpha }
                    } else {
                        ink
                    },
                    s,
                    e,
                );
                draw_text(gpu, &c.label, left + 26.0 * s, (r.top + r.bottom - c.label.h) / 2.0, ink);
                if badge > 0.0 {
                    let t = measure(shared, &text.dock_small, &e.ui.recent_count.to_string())?;
                    let x = left + 26.0 * s + c.label.w + 6.0 * s;
                    fill_rounded(
                        gpu,
                        fade(theme.control, alpha),
                        rect(x, r.top + 13.0 * s, x + 18.0 * s, r.top + 31.0 * s),
                        9.0 * s,
                    );
                    draw_text(gpu, &t, x + (18.0 * s - t.w) / 2.0, (r.top + r.bottom - t.h) / 2.0, muted);
                }
                if trailing > 0.0 {
                    glyphs::v4(
                        gpu,
                        "return",
                        rect(r.right - 36.0 * s, r.top, r.right - 16.0 * s, r.bottom),
                        fade(ink, 0.75),
                        20.0 * s,
                    );
                }
            } else {
                glyph(gpu, b, r, ink, s, e);
            }
            Ok(())
        };
        paint(ink, fade(theme.text_muted, alpha))?;
        let blob = if is_main { l.indicator } else { group.and_then(|i| l.radios[i]) };
        if let Some(clip) = blob.and_then(|p| intersect(inflate(p, -4.0 * s), r)) {
            unsafe {
                gpu.d2d.PushAxisAlignedClip(&clip, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
            }
            let result = paint(fade(theme.on_accent, alpha), fade(theme.on_accent, alpha));
            unsafe {
                gpu.d2d.PopAxisAlignedClip();
            }
            result?;
        }
    }
    if e.smart != SmartUi::Off
        && let Some((_, r, _)) = target.buttons.iter().find(|b| b.0 == ToolbarButton::Original)
        && target.panel.right - target.panel.left >= 700.0 * s
    {
        let value = match e.smart {
            SmartUi::On(_, n) => count_label(n as usize, ["фрагмент", "фрагмента", "фрагментов"]),
            SmartUi::Scanning(_) => "Поиск данных…".into(),
            SmartUi::Off => String::new(),
        };
        caption(gpu, shared, &text.dock_strong, &value, r.left - 142.0 * s, r.top + 7.0 * s, fade(theme.text, a))?;
        caption(
            gpu,
            shared,
            &text.dock_small,
            "скрыто на снимке",
            r.left - 142.0 * s,
            r.top + 24.0 * s,
            fade(theme.text_muted, a),
        )?;
    }
    if let Some((_, r, _)) = target.buttons.iter().find(|b| b.0 == ToolbarButton::SaveAs) {
        fill(gpu, fade(theme.border, a), rect(r.left, r.top + 14.0 * s, r.left + s, r.bottom - 14.0 * s));
    }
    unsafe {
        gpu.d2d.SetTransform(&previous);
    }
    if let Some((button, r, opacity)) = l.tip.filter(|_| e.ui.menu.is_none()) {
        let label = button.label();
        if !label.is_empty() {
            let t = measure(shared, &text.dock_label, label)?;
            let r = l.project(r);
            let width = t.w + 20.0 * s;
            let x =
                ((r.left + r.right - width) / 2.0).clamp(8.0 * s, (scene.size.0 as f32 - width - 8.0 * s).max(8.0 * s));
            let y = (l.project(l.panel).top - t.h - 20.0 * s).max(8.0 * s);
            let mut tip_theme = *theme;
            tip_theme.surface = fade(theme.surface, a * opacity);
            panel(gpu, &tip_theme, rect(x, y, x + width, y + t.h + 12.0 * s), 8.0 * s, s);
            draw_text(gpu, &t, x + 10.0 * s, y + 6.0 * s, fade(theme.text, a * opacity));
        }
    }
    Ok(())
}
fn inside_rect(outer: D2D_RECT_F, inner: D2D_RECT_F) -> bool {
    inner.left >= outer.left - 0.1
        && inner.right <= outer.right + 0.1
        && inner.top >= outer.top - 0.1
        && inner.bottom <= outer.bottom + 0.1
}
fn count_label(n: usize, words: [&str; 3]) -> String {
    let index = if (11..=14).contains(&(n % 100)) {
        2
    } else {
        match n % 10 {
            1 => 0,
            2..=4 => 1,
            _ => 2,
        }
    };
    format!("{n} {}", words[index])
}
fn radio_group(b: ToolbarButton) -> Option<usize> {
    match b {
        ToolbarButton::Censor(_) => Some(0),
        ToolbarButton::WidthValue(_) => Some(1),
        ToolbarButton::TextPlain
        | ToolbarButton::TextFilled
        | ToolbarButton::TextOutline
        | ToolbarButton::TextCallout => Some(2),
        _ => None,
    }
}
fn outline(gpu: &Gpu, ink: D2D1_COLOR_F, r: D2D_RECT_F, radius: f32, width: f32) {
    unsafe {
        gpu.brush.SetColor(&ink);
        gpu.d2d.DrawRoundedRectangle(
            &D2D1_ROUNDED_RECT { rect: r, radiusX: radius, radiusY: radius },
            &gpu.brush,
            width,
            None,
        );
    }
}
fn tool_icon(b: ToolbarButton) -> &'static str {
    match b {
        ToolbarButton::Tool(t) => match t {
            Tool::Select => "cursor",
            Tool::Arrow => "arrow",
            Tool::Line => "line",
            Tool::Rectangle => "rect",
            Tool::Ellipse => "ellipse",
            Tool::Pen => "pen",
            Tool::Text => "text",
            Tool::Counter => "counter",
            Tool::Blur => "blur",
            Tool::BlurEllipse => "blur-ellipse",
            Tool::BlurBrush => "blur-brush",
            Tool::Spotlight => "spotlight",
            Tool::Redact => "redact",
            Tool::Eraser | Tool::Marker => "eraser",
            Tool::Lens => "lens",
        },
        _ => "info",
    }
}
fn caption(gpu: &Gpu, shared: &Shared, style: &Style, value: &str, x: f32, y: f32, ink: D2D1_COLOR_F) -> Result<()> {
    draw_text(gpu, &measure(shared, style, value)?, x, y, ink);
    Ok(())
}
fn glyph(gpu: &Gpu, b: ToolbarButton, r: D2D_RECT_F, ink: D2D1_COLOR_F, s: f32, e: EditorBar) {
    let icon = match b {
        ToolbarButton::Menu(BarMenu::Shapes) => ToolbarButton::Tool(e.ui.shape),
        ToolbarButton::Menu(BarMenu::Blur) => ToolbarButton::Tool(e.ui.blur),
        ToolbarButton::RecentPin(_) | ToolbarButton::PinWindow => ToolbarButton::Pin,
        ToolbarButton::RecentDelete(_) => ToolbarButton::Close,
        _ => b,
    };
    let name = match icon {
        ToolbarButton::Tool(_) => tool_icon(icon),
        ToolbarButton::Copy => "copy",
        ToolbarButton::Save => "save",
        ToolbarButton::SaveAs => "chevron",
        ToolbarButton::Pin => "pin",
        ToolbarButton::Recent => "history",
        ToolbarButton::Text => "ocr",
        ToolbarButton::Translate => "translate",
        ToolbarButton::Smart => "shield",
        ToolbarButton::Record => "record",
        ToolbarButton::Close => "close",
        ToolbarButton::Undo => "undo",
        ToolbarButton::Redo => "redo",
        ToolbarButton::Menu(BarMenu::More) => "more",
        ToolbarButton::About => "info",
        ToolbarButton::Original => "eye",
        ToolbarButton::SystemAudio => "volume",
        ToolbarButton::Microphone => "mic",
        ToolbarButton::StrengthStep(false) => "minus",
        ToolbarButton::StrengthStep(true) => "plus",
        ToolbarButton::RecentPage(false) => "back",
        ToolbarButton::RecentPage(true) => "forward",
        _ => "info",
    };
    if matches!(b, ToolbarButton::Menu(BarMenu::Shapes | BarMenu::Blur)) {
        let x = (r.left + r.right) / 2.0;
        glyphs::v4(gpu, name, rect(x - 16.0 * s, r.top, x + 4.0 * s, r.bottom), ink, 20.0 * s);
        glyphs::v4(gpu, "chevron-small", rect(x + 6.0 * s, r.top, x + 16.0 * s, r.bottom), fade(ink, 0.7), 10.0 * s);
    } else {
        glyphs::v4(gpu, name, r, ink, 20.0 * s);
    }
}
fn draw_recent(
    gpu: &Gpu,
    shared: &Shared,
    text: &TextStyles,
    theme: &Theme,
    r: D2D_RECT_F,
    item: &RecentItem,
    style: (f32, f32, f32),
) -> Result<()> {
    let (s, a, hover) = style;
    fill_rounded(gpu, fade(theme.control, a * hover), r, 14.0 * s);
    let image_rect = rect(r.left, r.top, r.right, r.top + (r.right - r.left) * 0.75);
    fill_rounded(gpu, fade(theme.tile, a), image_rect, 14.0 * s);
    if let Some(image) = &item.preview {
        let mut cache = text.recent_bitmaps.borrow_mut();
        if !cache.iter().any(|(p, _)| Arc::ptr_eq(p, image)) {
            let bitmap = unsafe {
                gpu.d2d.CreateBitmap(
                    windows::Win32::Graphics::Direct2D::Common::D2D_SIZE_U {
                        width: image.width(),
                        height: image.height(),
                    },
                    Some(image.bgra.as_ptr().cast()),
                    image.width() * 4,
                    &crate::gpu::bitmap_props(windows::Win32::Graphics::Direct2D::D2D1_BITMAP_OPTIONS_NONE),
                )
            }?;
            if cache.len() >= 64 {
                cache.clear();
            }
            cache.push((image.clone(), bitmap));
        }
        if let Some((_, bitmap)) = cache.iter().find(|(p, _)| Arc::ptr_eq(p, image)) {
            let fit = ((image_rect.right - image_rect.left) / image.width() as f32)
                .min((image_rect.bottom - image_rect.top) / image.height() as f32);
            let (w, h) = (image.width() as f32 * fit, image.height() as f32 * fit);
            let dest = rect(
                (image_rect.left + image_rect.right - w) / 2.0,
                (image_rect.top + image_rect.bottom - h) / 2.0,
                (image_rect.left + image_rect.right + w) / 2.0,
                (image_rect.top + image_rect.bottom + h) / 2.0,
            );
            unsafe {
                let geometry = gpu.d2d.GetFactory()?.CreateRoundedRectangleGeometry(&D2D1_ROUNDED_RECT {
                    rect: image_rect,
                    radiusX: 14.0 * s,
                    radiusY: 14.0 * s,
                })?;
                let mut mask = D2D1_LAYER_PARAMETERS1 {
                    contentBounds: image_rect,
                    geometricMask: ManuallyDrop::new(Some(geometry.into())),
                    maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
                    maskTransform: Matrix3x2::identity(),
                    opacity: 1.0,
                    opacityBrush: ManuallyDrop::new(None),
                    layerOptions: D2D1_LAYER_OPTIONS1_NONE,
                };
                gpu.d2d.PushLayer(&mask, None::<&ID2D1Layer>);
                gpu.d2d.DrawBitmap(bitmap, Some(&raw const dest), a, D2D1_INTERPOLATION_MODE_LINEAR, None, None);
                gpu.d2d.PopLayer();
                ManuallyDrop::drop(&mut mask.geometricMask);
            }
        }
    }
    // Clip captions rather than painting them over the neighbouring capture.
    unsafe {
        gpu.d2d.PushAxisAlignedClip(&r, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
    }
    caption(
        gpu,
        shared,
        &text.dock_option,
        &item.title,
        r.left + 8.0 * s,
        image_rect.bottom + 6.0 * s,
        fade(theme.text, a),
    )?;
    caption(
        gpu,
        shared,
        &text.dock_small,
        &item.caption,
        r.left + 8.0 * s,
        image_rect.bottom + 25.0 * s,
        fade(theme.text_muted, a),
    )?;
    unsafe {
        gpu.d2d.PopAxisAlignedClip();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn editor(menu: Option<BarMenu>) -> EditorBar {
        EditorBar {
            ui: BarState { menu, recent_count: 7, ..Default::default() },
            text_style: Default::default(),
            callout: false,
            tool: Tool::Text,
            color: 0,
            width: 3,
            strength: 1,
            undo: true,
            redo: false,
            smart: SmartUi::Off,
        }
    }
    #[test]
    fn menus_match_the_measured_html_geometry_at_each_dpi() {
        let shared = Shared::new().unwrap();
        for s in [1.0, 1.5, 2.0] {
            let text = TextStyles::new(&shared, s).unwrap();
            for (menu, width, height) in [
                (BarMenu::Shapes, 280.0, 124.8),
                (BarMenu::Record, 288.0, 188.8),
                (BarMenu::Recent, 456.0, 372.4),
                (BarMenu::Properties, 304.0, 240.4),
            ] {
                let mut e = editor(Some(menu));
                e.ui.recent_count = 6;
                let l = build(
                    &shared,
                    &text,
                    (1600.0 * s, 900.0 * s),
                    s,
                    rect(100.0 * s, 100.0 * s, 1100.0 * s, 600.0 * s),
                    e,
                )
                .unwrap()
                .unwrap();
                let p = l.popup.unwrap();
                assert!(((p.right - p.left) / s - width).abs() < 0.05, "{menu:?} width");
                assert!(((p.bottom - p.top) / s - height).abs() < 0.05, "{menu:?} height: {p:?}");
                if menu == BarMenu::Recent {
                    let card = l.buttons.iter().find(|b| b.0 == ToolbarButton::RecentOpen(0)).unwrap().1;
                    assert!(((card.top - p.top) / s - 52.0).abs() < 0.05);
                    assert!(((card.right - card.left) / s - 141.3333).abs() < 0.05);
                    assert!(((card.bottom - card.top) / s - 152.2).abs() < 0.05);
                    assert!((p.left - l.panel.left - 8.0 * s).abs() < 0.05);
                }
            }
        }
    }
    #[test]
    fn choosing_a_group_item_remembers_it_and_closes_the_group() {
        let mut ui = BarState::default();
        ui.toggle(BarMenu::Shapes);
        ui.select_tool(Tool::Ellipse);
        assert_eq!(ui.menu, None);
        assert_eq!(ui.shape, Tool::Ellipse);
        ui.toggle(BarMenu::Blur);
        ui.select_tool(Tool::BlurBrush);
        assert_eq!(ui.blur, Tool::BlurBrush);
        assert_eq!(ui.shape, Tool::Ellipse);
        ui.toggle(BarMenu::More);
        ui.toggle(BarMenu::More);
        assert_eq!(ui.menu, None);
    }
    #[test]
    fn all_fifteen_tools_are_reachable_through_the_dock_and_its_groups() {
        let shared = Shared::new().unwrap();
        let text = TextStyles::new(&shared, 1.0).unwrap();
        let mut tools = Vec::new();
        for menu in [None, Some(BarMenu::Shapes), Some(BarMenu::Blur), Some(BarMenu::More)] {
            let l = build(&shared, &text, (1440.0, 900.0), 1.0, rect(100.0, 100.0, 1100.0, 600.0), editor(menu))
                .unwrap()
                .unwrap();
            tools
                .extend(l.buttons.iter().filter_map(|b| if let ToolbarButton::Tool(t) = b.0 { Some(t) } else { None }));
        }
        for tool in Tool::ALL {
            assert!(tools.contains(&tool), "{tool:?} is missing");
        }
    }
    #[test]
    fn every_popup_fits_and_its_pin_and_delete_buttons_win_hit_testing_at_each_dpi() {
        let shared = Shared::new().unwrap();
        for scale in [1.0, 1.5, 2.0] {
            let text = TextStyles::new(&shared, scale).unwrap();
            for width in [360.0, 640.0, 1280.0] {
                for menu in [
                    BarMenu::Shapes,
                    BarMenu::Blur,
                    BarMenu::More,
                    BarMenu::Properties,
                    BarMenu::Recent,
                    BarMenu::Record,
                ] {
                    let e = editor(Some(menu));
                    let surface = RECT {
                        left: -2000,
                        top: -200,
                        right: -2000 + (width * scale) as i32,
                        bottom: -200 + (720.0 * scale) as i32,
                    };
                    let selection = RECT {
                        left: surface.left + 20,
                        top: surface.top + 400,
                        right: surface.left + (width * scale) as i32 - 20,
                        bottom: surface.top + 650,
                    };
                    let local = rect(20.0, 400.0, width * scale - 20.0, 650.0);
                    let l = build(&shared, &text, (width * scale, 720.0 * scale), scale, local, e).unwrap().unwrap();
                    let popup = l.popup.unwrap();
                    assert!(
                        popup.left >= 0.0
                            && popup.right <= width * scale
                            && popup.top >= 0.0
                            && popup.bottom <= 720.0 * scale,
                        "{menu:?} at {width} / {scale}"
                    );
                    assert!(
                        intersect(popup, l.panel).is_none(),
                        "{menu:?} overlaps the dock at {width} / {scale}: {:?} / {:?}",
                        popup,
                        l.panel
                    );
                    let keys = toolbar_buttons(&shared, &text, surface, scale, selection, e);
                    assert!(keys.iter().all(|b| !matches!(b, ToolbarButton::Menu(_) | ToolbarButton::Copy)));
                    for (b, r, _) in l
                        .buttons
                        .iter()
                        .filter(|b| matches!(b.0, ToolbarButton::RecentPin(_) | ToolbarButton::RecentDelete(_)))
                    {
                        let hit = toolbar_hit(
                            &shared,
                            &text,
                            surface,
                            scale,
                            selection,
                            (
                                surface.left + ((r.left + r.right) / 2.0) as i32,
                                surface.top + ((r.top + r.bottom) / 2.0) as i32,
                            ),
                            Some(e),
                        );
                        assert_eq!(hit, Some(*b));
                    }
                }
            }
        }
    }
}
