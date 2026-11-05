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
    /// The pointer is drawn into screen recordings.
    pub cursor: bool,
    pub recent_count: usize,
    /// How far the «Недавние» grid is scrolled, in quarters of a row (the cards glide between them).
    pub scroll: u32,
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
            cursor: true,
            recent_count: 0,
            scroll: 0,
            focus: None,
        }
    }
}
impl BarState {
    pub fn toggle(&mut self, menu: BarMenu) {
        self.menu = (self.menu != Some(menu)).then_some(menu);
        self.focus = None;
        self.scroll = 0;
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
/// Rows of «Недавние» cards the dock's popup shows at once; the rest scroll under its edge.
const RECENT_ROWS: usize = 2;
/// How far the grid of `count` captures can scroll, in quarters of a row.
pub fn recent_scroll_limit(count: usize) -> u32 {
    count.div_ceil(3).saturating_sub(RECENT_ROWS) as u32 * 4
}
/// A part of a «Недавние» card: these scroll and are clipped to the popup's grid.
pub(super) fn is_recent_card(b: ToolbarButton) -> bool {
    matches!(b, ToolbarButton::RecentOpen(_) | ToolbarButton::RecentPin(_) | ToolbarButton::RecentDelete(_))
}
/// Where the cards of the «Недавние» popup `popup` are visible: under its header, inside its padding.
pub(super) fn recent_view(popup: D2D_RECT_F, s: f32) -> D2D_RECT_F {
    rect(popup.left, popup.top + 52.0 * s, popup.right, popup.bottom - 8.0 * s)
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
                // 16 + icon 20 + 6 + label + 6 + 2 + return 20 + 16.
                ToolbarButton::Copy => label + 86.0,
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
        // «Без цензуры» first: opening the drawer recognises nothing until a level is chosen.
        let levels = [
            ToolbarButton::CensorOff,
            ToolbarButton::Censor(Censor::Light),
            ToolbarButton::Censor(Censor::Hard),
            ToolbarButton::Censor(Censor::Full),
        ];
        let info = if content >= 700.0 { 148.0 } else { 0.0 };
        let level_width = (content - SIDE - GAP - info) / levels.len() as f32;
        for (i, level) in levels.into_iter().enumerate() {
            add(level, 8.0 + i as f32 * level_width, y, level_width, "")?;
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
    if popup.is_some_and(|p| intersect(p.0, panel).is_some()) {
        let dy = (h - 8.0 * s - panel.bottom).max(0.0);
        shift(&mut panel, 0.0, dy);
        shift(&mut track, 0.0, dy);
        buttons.truncate(main_count);
        for (_, r, _) in &mut buttons {
            shift(r, 0.0, dy);
        }
        popup = Some(popover(shared, text, (w, h, s), panel, e, e.ui.menu.unwrap(), &mut buttons)?);
    }
    Ok(Some(ToolbarLayout {
        panel,
        sel,
        buttons,
        context: None,
        track: Some(track),
        popup: popup.map(|p| p.0),
        trigger: popup.map(|p| p.1),
    }))
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
) -> Result<(D2D_RECT_F, D2D_RECT_F)> {
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
            (ToolbarButton::RecordCursor, "Курсор на видео".into()),
            (ToolbarButton::StartRecord, "Подготовить запись".into()),
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
    let count = e.ui.recent_count;
    let card_width = (pw / s - 32.0) / 3.0;
    let card_height = card_width * 0.75 + 46.2;
    let rows_count = count.max(1).div_ceil(3).min(RECENT_ROWS);
    let ph = if grid && count == 0 {
        132.8 * s
    } else if grid {
        (60.0 + rows_count as f32 * card_height + (rows_count - 1) as f32 * 8.0) * s
    } else if menu == BarMenu::Shapes || menu == BarMenu::Blur {
        124.8 * s
    } else if menu == BarMenu::Record {
        232.8 * s
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
        // Every capture is laid out; the grid scrolls by whole pixels of its row pitch and the popup
        // keeps two rows in view, so a long history never grows the popup over the dock.
        let cell = (pw - 32.0 * s) / 3.0;
        let pitch = (card_height + 8.0) * s;
        let scroll = e.ui.scroll.min(recent_scroll_limit(count)) as f32 / 4.0 * pitch;
        for i in 0..count {
            let left = x + 8.0 * s + (i % 3) as f32 * (cell + 8.0 * s);
            let top = y + 52.0 * s + (i / 3) as f32 * pitch - scroll;
            push(ToolbarButton::RecentOpen(i), rect(left, top, left + cell, top + card_height * s), "")?;
            push(ToolbarButton::RecentPin(i), rect(left, top, left + 44.0 * s, top + 44.0 * s), "")?;
            push(ToolbarButton::RecentDelete(i), rect(left + cell - 44.0 * s, top, left + cell, top + 44.0 * s), "")?;
        }
    } else if menu == BarMenu::Shapes || menu == BarMenu::Blur {
        let cell = (pw - 32.0 * s) / 3.0;
        for (i, (b, label)) in rows.into_iter().enumerate() {
            let left = x + (8.0 + i as f32 * 8.0) * s + i as f32 * cell;
            push(b, rect(left, y + 40.8 * s, left + cell, y + 116.8 * s), &label)?;
        }
    } else if menu == BarMenu::Record {
        for (i, (b, label)) in rows.into_iter().enumerate() {
            let top = y + (40.8 + i as f32 * 44.0 + if i == 3 { 8.0 } else { 0.0 }) * s;
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
    Ok((popup, anchor))
}

/// Reference colours the theme has no role for.
struct Extra {
    /// `--hover`: a translucent wash under the liquid blob, never over it.
    hover: D2D1_COLOR_F,
    /// `--press`: a tile's hover.
    press: D2D1_COLOR_F,
    /// `--red`: the record icon.
    red: D2D1_COLOR_F,
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
    let rgb = |value: u32, alpha| D2D1_COLOR_F {
        r: ((value >> 16) & 255) as f32 / 255.0,
        g: ((value >> 8) & 255) as f32 / 255.0,
        b: (value & 255) as f32 / 255.0,
        a: alpha,
    };
    let mut extra = Extra {
        hover: theme.tint,
        press: theme.tint_strong,
        red: rgb(if theme.dark { 0xff453a } else { 0xe5342a }, 1.0),
    };
    // Use the reference's roles here; other native windows retain their own materials.
    // System high-contrast colors take precedence over the reference palette.
    if appearance.shadow.a > 0.0 {
        let dark = appearance.dark;
        appearance.surface = rgb(if dark { 0x1c1c1e } else { 0xfbfbfd }, 1.0);
        appearance.control = rgb(if dark { 0x28282b } else { 0xececf0 }, 1.0);
        appearance.text = rgb(if dark { 0xf5f5f7 } else { 0x1d1d1f }, 1.0);
        appearance.text_muted = rgb(if dark { 0xa1a1a6 } else { 0x636366 }, 1.0);
        appearance.accent = rgb(0x0071e3, 1.0);
        appearance.on_accent = rgb(0xffffff, 1.0);
        appearance.selected = rgb(if dark { 0x0a84ff } else { 0x0071e3 }, if dark { 0.2 } else { 0.12 });
        appearance.on_selected = rgb(if dark { 0x4da3ff } else { 0x0062c4 }, 1.0);
        appearance.track = rgb(if dark { 0x48484b } else { 0xd6d6db }, 1.0);
        appearance.border = rgb(if dark { 0xffffff } else { 0x000000 }, 0.1);
        extra.hover = rgb(if dark { 0xffffff } else { 0x000000 }, if dark { 0.07 } else { 0.05 });
        extra.press = rgb(if dark { 0xffffff } else { 0x000000 }, if dark { 0.12 } else { 0.09 });
    } else {
        extra.red = theme.text;
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
    let dock = Matrix3x2 { M11: k, M12: 0.0, M21: 0.0, M22: k, M31: ax * (1.0 - k), M32: ay * (1.0 - k) + l.lift };
    unsafe {
        gpu.d2d.GetTransform(&mut previous);
        gpu.d2d.SetTransform(&dock);
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
    let p = Painter { gpu, shared, text, theme, extra, e, toolbar, s };
    // A scrolled «Недавние» card belongs to its popup even when it has slid past the popup's edge.
    let in_popover = |c: &super::transition::Button| {
        if is_recent_card(c.button) {
            l.popovers.iter().rposition(|p| p.menu == Some(BarMenu::Recent))
        } else {
            l.popovers.iter().rposition(|p| inside_rect(p.rect, c.rect))
        }
    };
    let main: Vec<&super::transition::Button> = l
        .buttons
        .iter()
        .filter(|c| !c.drawer && in_popover(c).is_none() && (c.live || inside_rect(l.panel, c.rect)))
        .collect();
    p.layer(&main, None, &[(None, l.indicator)], a)?;

    // The drawer: clipped to its growing row, its content rising into place.
    if let Some(d) = l.drawer
        && d.clip.bottom - d.clip.top > 0.01
    {
        let items: Vec<&super::transition::Button> = l.buttons.iter().filter(|c| c.drawer).collect();
        let (ox, oy) = d.origin;
        let grow = Matrix3x2 {
            M11: d.scale,
            M12: 0.0,
            M21: 0.0,
            M22: d.scale,
            M31: ox * (1.0 - d.scale),
            M32: oy * (1.0 - d.scale),
        };
        unsafe {
            gpu.d2d.PushAxisAlignedClip(&d.clip, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
            gpu.d2d.SetTransform(&(grow * dock));
        }
        let alpha = a * d.opacity;
        let result = (|| -> Result<()> {
            if let Some(track) = union(items.iter().filter(|c| radio_group(c.button) == Some(0)).map(|c| c.rect)) {
                fill_rounded(gpu, fade(theme.control, alpha), track, 22.0 * s);
            }
            p.layer(&items, None, &[(Some(0), l.radios[0])], alpha)?;
            if let Some(r) = items.iter().find(|c| c.button == ToolbarButton::Original).map(|c| c.rect)
                && target.panel.right - target.panel.left >= 700.0 * s
            {
                let (value, note) = match e.smart {
                    SmartUi::On(_, n) => {
                        (count_label(n as usize, ["фрагмент", "фрагмента", "фрагментов"]), "скрыто на снимке")
                    }
                    SmartUi::Scanning(_) => ("Поиск данных…".into(), "скрыто на снимке"),
                    SmartUi::Open | SmartUi::Off => ("Ничего не скрыто".into(), "выберите уровень"),
                };
                caption(
                    gpu,
                    shared,
                    &text.dock_strong,
                    &value,
                    r.left - 142.0 * s,
                    r.top + 7.0 * s,
                    fade(theme.text, alpha),
                )?;
                caption(
                    gpu,
                    shared,
                    &text.dock_small,
                    note,
                    r.left - 142.0 * s,
                    r.top + 24.0 * s,
                    fade(theme.text_muted, alpha),
                )?;
            }
            Ok(())
        })();
        unsafe {
            gpu.d2d.SetTransform(&dock);
            gpu.d2d.PopAxisAlignedClip();
        }
        result?;
    }
    if let Some(r) = l.buttons.iter().find(|c| c.button == ToolbarButton::SaveAs && c.live).map(|c| c.rect) {
        fill(gpu, fade(theme.border, a), rect(r.left, r.top + 14.0 * s, r.left + s, r.bottom - 14.0 * s));
    }

    // Popovers grow out of their trigger and shrink back into it.
    for (i, pose) in l.popovers.iter().enumerate() {
        let (ox, oy) = pose.origin;
        let grow = Matrix3x2 {
            M11: pose.scale,
            M12: 0.0,
            M21: 0.0,
            M22: pose.scale,
            M31: ox * (1.0 - pose.scale),
            M32: oy * (1.0 - pose.scale) + pose.dy,
        };
        unsafe {
            gpu.d2d.SetTransform(&(grow * dock));
        }
        let alpha = a * pose.opacity;
        let items: Vec<&super::transition::Button> =
            l.buttons.iter().filter(|c| !c.drawer && in_popover(c) == Some(i)).collect();
        let result = (|| -> Result<()> {
            p.popover(pose.rect, pose.menu, alpha)?;
            let mut blobs = Vec::new();
            if pose.menu == Some(BarMenu::Properties) {
                // Properties tracks are inside the popup surface, so paint them after that surface.
                for group in 1..3 {
                    if let Some(track) =
                        union(items.iter().filter(|c| radio_group(c.button) == Some(group)).map(|c| c.rect))
                    {
                        fill_rounded(gpu, fade(theme.control, alpha), track, 22.0 * s);
                        blobs.push((Some(group), l.radios[group]));
                    }
                }
            }
            if pose.menu != Some(BarMenu::Recent) {
                return p.layer(&items, pose.menu, &blobs, alpha);
            }
            // «Недавние»: the header stays put; the cards scroll inside the grid, clipped to it.
            let (cards, header): (Vec<_>, Vec<_>) = items.into_iter().partition(|c| is_recent_card(c.button));
            p.layer(&header, pose.menu, &blobs, alpha)?;
            let view = recent_view(pose.rect, s);
            unsafe {
                gpu.d2d.PushAxisAlignedClip(&view, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
            }
            let drawn = p.layer(&cards, pose.menu, &[], alpha);
            unsafe {
                gpu.d2d.PopAxisAlignedClip();
            }
            drawn?;
            // A thin thumb at the right edge says where in the list the view is, and how much there is.
            let rows = e.ui.recent_count.div_ceil(3);
            if rows > RECENT_ROWS
                && let Some(first) = cards.iter().find(|c| c.button == ToolbarButton::RecentOpen(0))
            {
                let pitch = (view.bottom - view.top + 8.0 * s) / RECENT_ROWS as f32;
                let content = rows as f32 * pitch - 8.0 * s;
                let span = view.bottom - view.top;
                let thumb = (span * span / content).max(24.0 * s);
                let travel = (content - span).max(1.0);
                let at = ((view.top - first.rect.top) / travel).clamp(0.0, 1.0);
                let top = view.top + (span - thumb) * at;
                let x = pose.rect.right - 5.0 * s;
                fill_rounded(gpu, fade(theme.text_muted, alpha * 0.5), rect(x - 1.5 * s, top, x + 1.5 * s, top + thumb), 1.5 * s);
            }
            Ok(())
        })();
        unsafe {
            gpu.d2d.SetTransform(&dock);
        }
        result?;
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
fn union(mut rects: impl Iterator<Item = D2D_RECT_F>) -> Option<D2D_RECT_F> {
    let first = rects.next()?;
    Some(
        rects.fold(first, |p, r| {
            rect(p.left.min(r.left), p.top.min(r.top), p.right.max(r.right), p.bottom.max(r.bottom))
        }),
    )
}
/// The tools of the main track: one liquid blob moves between them.
fn is_main(b: ToolbarButton) -> bool {
    matches!(
        b,
        ToolbarButton::Tool(Tool::Select | Tool::Arrow | Tool::Pen | Tool::Text | Tool::Counter)
            | ToolbarButton::Menu(BarMenu::Shapes | BarMenu::Blur | BarMenu::More)
    )
}
/// The blob's corner follows its thickness while it thins in flight.
fn blob_radius(r: D2D_RECT_F, s: f32) -> f32 {
    (18.0 * s).min((r.bottom - r.top) / 2.0).min((r.right - r.left) / 2.0).max(0.0)
}

struct Painter<'a> {
    gpu: &'a Gpu,
    shared: &'a Shared,
    text: &'a TextStyles,
    theme: &'a Theme,
    extra: Extra,
    e: EditorBar,
    toolbar: Toolbar<'a>,
    s: f32,
}
impl Painter<'_> {
    fn rect(&self, c: &super::transition::Button) -> D2D_RECT_F {
        inflate(c.rect, -self.s * c.press)
    }
    /// A tile of the shapes and blur menus.
    fn is_tile(b: ToolbarButton, menu: Option<BarMenu>) -> bool {
        !is_main(b) && matches!(b, ToolbarButton::Tool(_)) && matches!(menu, Some(BarMenu::Shapes | BarMenu::Blur))
    }
    /// One layer after the reference's `.track` + `.liquid`: backgrounds, then each liquid blob over
    /// them, then the ink, then the blob's own copy of the ink in the on-accent colour, exactly where
    /// the blob covers it (the reference's `mirror()`), and focus rings on top. The blob is above the
    /// controls during its whole flight, never under a hover or a tile.
    fn layer(
        &self,
        buttons: &[&super::transition::Button],
        menu: Option<BarMenu>,
        blobs: &[(Option<usize>, Option<D2D_RECT_F>)],
        alpha: f32,
    ) -> Result<()> {
        let (gpu, s) = (self.gpu, self.s);
        for c in buttons {
            self.back(c, menu, alpha)?;
        }
        let blobs: Vec<_> = blobs.iter().filter_map(|(g, r)| r.map(|r| (*g, inflate(r, -4.0 * s)))).collect();
        for (_, b) in &blobs {
            fill_rounded(gpu, fade(self.theme.accent, alpha), *b, blob_radius(*b, s));
        }
        for c in buttons {
            self.ink(c, menu, alpha)?;
        }
        for (group, b) in &blobs {
            let covered: Vec<_> = buttons
                .iter()
                .filter(|c| {
                    (if let Some(g) = group { radio_group(c.button) == Some(*g) } else { is_main(c.button) })
                        && intersect(c.rect, *b).is_some()
                })
                .collect();
            if covered.is_empty() {
                continue;
            }
            unsafe {
                let geometry = gpu.d2d.GetFactory()?.CreateRoundedRectangleGeometry(&D2D1_ROUNDED_RECT {
                    rect: *b,
                    radiusX: blob_radius(*b, s),
                    radiusY: blob_radius(*b, s),
                })?;
                let mut mask = D2D1_LAYER_PARAMETERS1 {
                    contentBounds: *b,
                    geometricMask: ManuallyDrop::new(Some(geometry.into())),
                    maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
                    maskTransform: Matrix3x2::identity(),
                    opacity: 1.0,
                    opacityBrush: ManuallyDrop::new(None),
                    layerOptions: D2D1_LAYER_OPTIONS1_NONE,
                };
                gpu.d2d.PushLayer(&mask, None::<&ID2D1Layer>);
                let result = covered.iter().try_for_each(|c| {
                    let on = fade(self.theme.on_accent, alpha * c.opacity);
                    self.paint(c, self.rect(c), menu, (on, on), alpha * c.opacity)
                });
                gpu.d2d.PopLayer();
                ManuallyDrop::drop(&mut mask.geometricMask);
                result?;
            }
        }
        for c in buttons {
            if self.e.ui.focus == Some(c.button) && c.opacity * alpha > 0.002 {
                edge(gpu, self.theme, inflate(self.rect(c), -2.0 * s), 20.0 * s, s, alpha * c.opacity);
            }
        }
        Ok(())
    }
    /// Fills under the ink: hover washes, tiles, pressed tints, primary actions, recent cards.
    fn back(&self, c: &super::transition::Button, menu: Option<BarMenu>, layer: f32) -> Result<()> {
        let (gpu, theme, s, b) = (self.gpu, self.theme, self.s, c.button);
        let (r, alpha) = (self.rect(c), layer * c.opacity);
        if alpha < 0.002 {
            return Ok(());
        }
        let main = is_main(b);
        let group = radio_group(b);
        if let ToolbarButton::RecentOpen(i) = b {
            if let Some(item) = self.toolbar.recent.get(i) {
                draw_recent(gpu, self.shared, self.text, theme, r, item, (s, alpha, c.hover))?;
            }
            return Ok(());
        }
        if Self::is_tile(b, menu) {
            fill_rounded(gpu, fade(if c.selected > 0.5 { theme.accent } else { theme.control }, alpha), r, 14.0 * s);
            if c.selected <= 0.5 && c.hover > 0.001 {
                fill_rounded(gpu, fade(self.extra.press, alpha * c.hover), r, 14.0 * s);
            }
        } else if b == ToolbarButton::Copy || b == ToolbarButton::StartRecord {
            fill_rounded(gpu, fade(theme.accent, alpha), r, 22.0 * s);
        } else if !main
            && group.is_none()
            && !matches!(
                b,
                ToolbarButton::Color(_)
                    | ToolbarButton::SystemAudio
                    | ToolbarButton::Microphone
                    | ToolbarButton::RecordCursor
                    | ToolbarButton::RecentPin(_)
                    | ToolbarButton::RecentDelete(_)
            )
            && c.selected > 0.001
        {
            fill_rounded(
                gpu,
                fade(
                    if matches!(b, ToolbarButton::Tool(_)) { theme.accent } else { theme.selected },
                    alpha * c.selected,
                ),
                r,
                if menu == Some(BarMenu::More) { 14.0 * s } else { 22.0 * s },
            );
        } else if !matches!(b, ToolbarButton::RecentPin(_) | ToolbarButton::RecentDelete(_)) {
            // A group's trigger keeps its wash while its menu is open.
            let open = matches!(b, ToolbarButton::Menu(m) if main && self.e.ui.menu == Some(m));
            let hover = if open { 1.0 } else { c.hover };
            if hover > 0.001 {
                fill_rounded(
                    gpu,
                    fade(self.extra.hover, alpha * hover),
                    if main || group.is_some() { inflate(r, -4.0 * s) } else { r },
                    if main || group.is_some() {
                        18.0 * s
                    } else if menu.is_some() {
                        14.0 * s
                    } else {
                        22.0 * s
                    },
                );
            }
        }
        Ok(())
    }
    /// Icons, labels, swatches and switches.
    fn ink(&self, c: &super::transition::Button, menu: Option<BarMenu>, layer: f32) -> Result<()> {
        let (gpu, theme, s, b, e) = (self.gpu, self.theme, self.s, c.button, self.e);
        let (r, alpha) = (self.rect(c), layer * c.opacity);
        if alpha < 0.002 || matches!(b, ToolbarButton::RecentOpen(_)) {
            return Ok(());
        }
        let main = is_main(b);
        let group = radio_group(b);
        if let ToolbarButton::RecentPin(i) | ToolbarButton::RecentDelete(i) = b {
            let pinned =
                matches!(b, ToolbarButton::RecentPin(_)) && self.toolbar.recent.get(i).is_some_and(|item| item.pinned);
            let near = |button: Option<ToolbarButton>| matches!(button,Some(ToolbarButton::RecentOpen(j)|ToolbarButton::RecentPin(j)|ToolbarButton::RecentDelete(j)) if j==i);
            if pinned || near(self.toolbar.hovered) || near(e.ui.focus) {
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
            return Ok(());
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
            return Ok(());
        }
        let ink = fade(if !c.enabled { theme.disabled } else { theme.text }, alpha);
        if matches!(b, ToolbarButton::SystemAudio | ToolbarButton::Microphone | ToolbarButton::RecordCursor) {
            glyph(gpu, b, rect(r.left + 12.0 * s, r.top, r.left + 32.0 * s, r.bottom), ink, s, e);
            draw_text(gpu, &c.label, r.left + 42.0 * s, (r.top + r.bottom - c.label.h) / 2.0, ink);
            let track = rect(r.right - 52.0 * s, r.top + 10.0 * s, r.right - 12.0 * s, r.bottom - 10.0 * s);
            fill_rounded(gpu, fade(if c.selected > 0.5 { theme.accent } else { theme.track }, alpha), track, 12.0 * s);
            let x = if c.selected > 0.5 { track.right - 22.0 * s } else { track.left + 2.0 * s };
            fill_rounded(
                gpu,
                fade(theme.on_accent, alpha),
                rect(x, track.top + 2.0 * s, x + 20.0 * s, track.bottom - 2.0 * s),
                10.0 * s,
            );
            return Ok(());
        }
        if b == ToolbarButton::Menu(BarMenu::Properties) {
            let rgb = COLORS[e.color.min(COLORS.len() - 1)];
            fill_rounded(
                gpu,
                D2D1_COLOR_F { r: rgb[0] as f32 / 255.0, g: rgb[1] as f32 / 255.0, b: rgb[2] as f32 / 255.0, a: alpha },
                rect(r.left + 14.0 * s, r.top + 15.0 * s, r.left + 28.0 * s, r.top + 29.0 * s),
                7.0 * s,
            );
            let t = measure(self.shared, &self.text.dock_label, &format!("{} px", e.width))?;
            draw_text(gpu, &t, r.left + 34.0 * s, (r.top + r.bottom - t.h) / 2.0, ink);
            return Ok(());
        }
        let primary = b == ToolbarButton::Copy
            || b == ToolbarButton::StartRecord
            || (Self::is_tile(b, menu) && c.selected > 0.5)
            || (menu == Some(BarMenu::More) && !main && c.selected > 0.5);
        let ink = if primary {
            fade(theme.on_accent, alpha)
        } else if b == ToolbarButton::RecentClear || (c.selected > 0.5 && !main && group.is_none()) {
            fade(theme.on_selected, alpha)
        } else {
            ink
        };
        self.paint(c, r, menu, (ink, fade(theme.text_muted, alpha)), alpha)
    }
    /// The content of a control in the given ink; the blob repaints it in the on-accent colour.
    fn paint(
        &self,
        c: &super::transition::Button,
        r: D2D_RECT_F,
        menu: Option<BarMenu>,
        (ink, muted): (D2D1_COLOR_F, D2D1_COLOR_F),
        alpha: f32,
    ) -> Result<()> {
        let (gpu, shared, text, s, b, e) = (self.gpu, self.shared, self.text, self.s, c.button, self.e);
        let level = match b {
            ToolbarButton::Censor(level) => Some((
                level.name(),
                match level {
                    Censor::Light => "Контакты и коды",
                    Censor::Hard => "Плюс имена и адреса",
                    Censor::Full => "Весь текст",
                },
            )),
            ToolbarButton::CensorOff => Some(("Без цензуры", "Снимок как есть")),
            _ => None,
        };
        if let Some((name, note)) = level {
            let mut title = measure(shared, &text.dock_strong, name)?;
            if b == ToolbarButton::CensorOff && title.w > r.right - r.left - 8.0 * s {
                title = measure(shared, &text.dock_strong, "Выкл")?;
            }
            let desc = measure(shared, &text.dock_small, note)?;
            draw_text(gpu, &title, (r.left + r.right - title.w) / 2.0, r.top + 7.0 * s, ink);
            if desc.w < r.right - r.left - 8.0 * s {
                draw_text(gpu, &desc, (r.left + r.right - desc.w) / 2.0, r.top + 24.0 * s, muted);
            }
        } else if Self::is_tile(b, menu) {
            glyphs::v4(gpu, tool_icon(b), rect(r.left, r.top + 16.0 * s, r.right, r.top + 38.0 * s), ink, 22.0 * s);
            let t = &c.label;
            draw_text(gpu, t, (r.left + r.right - t.w) / 2.0, r.top + 44.0 * s, ink);
        } else if menu == Some(BarMenu::More) {
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
        } else if radio_group(b).is_some() || matches!(b, ToolbarButton::RecentClear | ToolbarButton::StartRecord) {
            draw_text(gpu, &c.label, (r.left + r.right - c.label.w) / 2.0, (r.top + r.bottom - c.label.h) / 2.0, ink);
        } else if c.label.w > 0.0 {
            // `.btn`: icon 20, gap 6, label; the badge and the return key follow with the same gap.
            let badge = if b == ToolbarButton::Recent {
                Some(measure(shared, &text.dock_badge, &e.ui.recent_count.to_string())?)
            } else {
                None
            };
            let badge_width = badge.as_ref().map_or(0.0, |t| (t.w + 10.0 * s).max(18.0 * s));
            let trailing = if b == ToolbarButton::Copy { 28.0 * s } else { 0.0 };
            let width = 26.0 * s + c.label.w + if badge.is_some() { 6.0 * s + badge_width } else { 0.0 } + trailing;
            let left = r.left + (r.right - r.left - width) / 2.0;
            let icon = if b == ToolbarButton::Record { fade(self.extra.red, alpha) } else { ink };
            glyph(gpu, b, rect(left, r.top, left + 20.0 * s, r.bottom), icon, s, e);
            draw_text(gpu, &c.label, left + 26.0 * s, (r.top + r.bottom - c.label.h) / 2.0, ink);
            if let Some(t) = badge {
                let x = left + 26.0 * s + c.label.w + 6.0 * s;
                let pill = rect(x, r.top + 13.0 * s, x + badge_width, r.top + 31.0 * s);
                fill_rounded(gpu, fade(self.theme.control, alpha), pill, 9.0 * s);
                draw_text(gpu, &t, (pill.left + pill.right - t.w) / 2.0, (r.top + r.bottom - t.h) / 2.0, muted);
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
    }
    /// A popover's surface and its titles.
    fn popover(&self, p: D2D_RECT_F, menu: Option<BarMenu>, a: f32) -> Result<()> {
        let (gpu, shared, text, theme, s, e) = (self.gpu, self.shared, self.text, self.theme, self.s, self.e);
        shadow(gpu, theme.shadow, p, 22.0 * s, s, a, true);
        fill_rounded(gpu, fade(theme.surface, a), p, 22.0 * s);
        edge(gpu, theme, p, 22.0 * s, s, a);
        match menu {
            Some(BarMenu::Recent) => {
                caption(
                    gpu,
                    shared,
                    &text.dock_heading,
                    "Недавние",
                    p.left + 20.0 * s,
                    p.top + 23.0 * s,
                    fade(theme.text_muted, a),
                )?;
                let count = e.ui.recent_count;
                if count > 0 {
                    let label = count_label(count, ["снимок", "снимка", "снимков"]);
                    caption(
                        gpu,
                        shared,
                        &text.dock_option,
                        &label,
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
            }
            Some(menu @ (BarMenu::Shapes | BarMenu::Blur | BarMenu::Record)) => {
                let title = if menu == BarMenu::Record { "Запись области" } else { menu.label() };
                caption(
                    gpu,
                    shared,
                    &text.dock_heading,
                    title,
                    p.left + 20.0 * s,
                    p.top + 14.0 * s,
                    fade(theme.text_muted, a),
                )?;
            }
            Some(BarMenu::Properties) => {
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
                    draw_text(
                        gpu,
                        &t,
                        (r.left + r.right - t.w) / 2.0,
                        (r.top + r.bottom - t.h) / 2.0,
                        fade(theme.text, a),
                    );
                }
            }
            Some(BarMenu::More) => {
                fill(
                    gpu,
                    fade(theme.border, a),
                    rect(p.left + 20.0 * s, p.top + 188.0 * s, p.right - 20.0 * s, p.top + 189.0 * s),
                );
            }
            None => {}
        }
        Ok(())
    }
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
        ToolbarButton::Censor(_) | ToolbarButton::CensorOff => Some(0),
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
        ToolbarButton::RecordCursor => "cursor",
        ToolbarButton::StrengthStep(false) => "minus",
        ToolbarButton::StrengthStep(true) => "plus",
        _ => "info",
    };
    if matches!(b, ToolbarButton::Menu(BarMenu::Shapes | BarMenu::Blur)) {
        let x = (r.left + r.right) / 2.0;
        // `.seg` gap 2 and the chevron's margin −1: icon 20 + 1 + chevron 10, centred.
        glyphs::v4(gpu, name, rect(x - 15.5 * s, r.top, x + 4.5 * s, r.bottom), ink, 20.0 * s);
        glyphs::v4(gpu, "chevron-small", rect(x + 5.5 * s, r.top, x + 15.5 * s, r.bottom), fade(ink, 0.7), 10.0 * s);
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
                (BarMenu::Record, 288.0, 232.8),
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
    /// The drawer grows without overlapping its neighbours, the level row opens on «Без цензуры»,
    /// and a popover grows out of its trigger and leaves faster than it came.
    #[test]
    fn drawer_and_popover_move_as_the_reference_does() {
        let shared = Shared::new().unwrap();
        let text = TextStyles::new(&shared, 1.0).unwrap();
        text.enable_motion(true);
        let (size, sel) = ((1440.0, 900.0), rect(100.0, 100.0, 1100.0, 600.0));
        let mut e = editor(None);
        e.tool = Tool::Select;
        let pose = |e: EditorBar, t: f64| {
            text.set_animation_time(Some(t));
            let target = toolbar_layout(&shared, &text, size, 1.0, sel, Some(e)).unwrap().unwrap();
            (text.displayed(&target, Some(e), Some(None), 1.0), target)
        };
        pose(e, 0.0);
        pose(e, 2.0);
        let open = EditorBar { smart: SmartUi::Open, ..e };
        pose(open, 3.0);
        let (early, target) = pose(open, 3.08);
        let d = early.drawer.unwrap();
        let row = |l: &super::super::transition::Layout, b| l.buttons.iter().find(|c| c.button == b).unwrap().rect;
        let tools = row(&early, ToolbarButton::Tool(Tool::Select));
        let copy = row(&early, ToolbarButton::Copy);
        assert!(d.clip.bottom - d.clip.top > 1.0 && d.clip.bottom - d.clip.top < 40.0, "{:?}", d.clip);
        assert!(d.clip.top >= tools.bottom - 0.01 && copy.top >= d.clip.bottom - 0.01);
        assert!(d.opacity < 0.9 && d.scale < 1.0);
        let off = target.buttons.iter().find(|b| b.0 == ToolbarButton::CensorOff).unwrap().1;
        let (settled, _) = pose(open, 4.0);
        assert_eq!(settled.radios[0].map(|r| r.left), Some(off.left));
        assert!(settled.drawer.unwrap().opacity == 1.0);
        // Closing takes ~260 ms against ~420 ms opening.
        pose(e, 5.0);
        let (closing, _) = pose(e, 5.2);
        assert!(closing.drawer.is_none_or(|d| d.clip.bottom - d.clip.top < 4.0));
        let more = EditorBar { ui: BarState { menu: Some(BarMenu::More), ..e.ui }, ..e };
        let (start, target) = pose(more, 7.0);
        let p = start.popovers[0];
        let trigger = target.buttons.iter().find(|b| b.0 == ToolbarButton::Menu(BarMenu::More)).unwrap().1;
        assert!((p.scale - 0.96).abs() < 0.001 && p.opacity < 0.01);
        assert!((p.origin.0 - (trigger.left + trigger.right) / 2.0).abs() < 0.5);
        let (shown, _) = pose(more, 7.4);
        assert!(shown.popovers[0].scale == 1.0 && shown.popovers[0].opacity == 1.0);
        let (leaving, _) = pose(e, 8.0);
        assert_eq!(leaving.popovers.len(), 1);
        assert!(pose(e, 8.08).0.popovers[0].opacity < 0.6);
        assert!(pose(e, 8.2).0.popovers.is_empty());
    }
    /// With KADR_MOTION_FRAMES=<dir>, renders the liquid pill, the drawer and a popover mid-flight
    /// to PNGs for review; otherwise just draws them.
    #[test]
    fn mid_flight_frames_render() {
        use windows::Win32::Graphics::Direct2D::Common::D2D_SIZE_U;
        use windows::Win32::Graphics::Direct2D::{
            D2D1_BITMAP_OPTIONS_CANNOT_DRAW, D2D1_BITMAP_OPTIONS_CPU_READ, D2D1_BITMAP_OPTIONS_TARGET,
            D2D1_MAP_OPTIONS_READ,
        };
        use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1};
        let shared = Shared::new().unwrap();
        let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1().unwrap() };
        let gpu = Gpu::new(&shared, &unsafe { factory.EnumAdapters1(0).unwrap() }, "motion frames".into()).unwrap();
        let s = 1.5;
        let (w, h) = (1100u32 * 3 / 2, 760u32 * 3 / 2);
        let text = TextStyles::new(&shared, s).unwrap();
        text.enable_motion(true);
        let out = std::env::var_os("KADR_MOTION_FRAMES").map(std::path::PathBuf::from);
        let sel = rect(40.0 * s, 380.0 * s, 1060.0 * s, 500.0 * s);
        let base = EditorBar { tool: Tool::Select, ..editor(None) };
        let counter = EditorBar { tool: Tool::Counter, ..base };
        let open = EditorBar { smart: SmartUi::Open, ..counter };
        let hard = EditorBar { smart: SmartUi::On(Censor::Hard, 6), ..open };
        let more = EditorBar { ui: BarState { menu: Some(BarMenu::More), ..open.ui }, ..open };
        let recent = EditorBar { ui: BarState { menu: Some(BarMenu::Recent), recent_count: 10, ..base.ui }, ..base };
        let frames: &[(&str, EditorBar, f64)] = &[
            ("rest", base, 0.0),
            ("rest", base, 2.0),
            ("pill-00", counter, 3.0),
            ("pill-40ms", counter, 3.04),
            ("pill-90ms", counter, 3.09),
            ("pill-150ms", counter, 3.15),
            ("pill-end", counter, 4.0),
            ("drawer-00", open, 5.0),
            ("drawer-80ms", open, 5.08),
            ("drawer-160ms", open, 5.16),
            ("drawer-300ms", open, 5.3),
            ("drawer-end", open, 6.0),
            ("level-00", hard, 6.5),
            ("level-60ms", hard, 6.56),
            ("level-end", hard, 7.5),
            ("popover-00", more, 8.0),
            ("popover-60ms", more, 8.06),
            ("popover-140ms", more, 8.14),
            ("popover-end", more, 9.0),
            ("popover-leave-00", hard, 10.0),
            ("popover-leave-60ms", hard, 10.06),
            ("drawer-close-00", counter, 11.0),
            ("drawer-close-100ms", counter, 11.1),
            ("recent-paging", recent, 12.0),
            ("recent-paging", recent, 13.0),
        ];
        for (name, e, t) in frames {
            text.set_animation_time(Some(*t));
            let marks = Marks {
                selection: Some(RECT {
                    left: sel.left as i32,
                    top: sel.top as i32,
                    right: sel.right as i32,
                    bottom: sel.bottom as i32,
                }),
                toolbar: Some(Toolbar { recent: &[], hovered: None, editor: Some(*e) }),
                ..Default::default()
            };
            let scene = Scene { frame: None, origin: (0, 0), size: (w, h), scale: s, marks };
            unsafe {
                let props = crate::gpu::bitmap_props(D2D1_BITMAP_OPTIONS_TARGET);
                let target = gpu.d2d.CreateBitmap(D2D_SIZE_U { width: w, height: h }, None, 0, &props).unwrap();
                gpu.d2d.SetTarget(&target);
                gpu.d2d.BeginDraw();
                gpu.d2d.Clear(Some(&D2D1_COLOR_F { r: 0.35, g: 0.45, b: 0.5, a: 1.0 }));
                draw(&gpu, &shared, &text, &Theme::current(), &scene, sel, scene.marks.toolbar.unwrap()).unwrap();
                gpu.d2d.EndDraw(None, None).unwrap();
                let Some(dir) = &out else { continue };
                let read = crate::gpu::bitmap_props(D2D1_BITMAP_OPTIONS_CPU_READ | D2D1_BITMAP_OPTIONS_CANNOT_DRAW);
                let readback = gpu.d2d.CreateBitmap(D2D_SIZE_U { width: w, height: h }, None, 0, &read).unwrap();
                readback.CopyFromBitmap(None, &target, None).unwrap();
                let mapped = readback.Map(D2D1_MAP_OPTIONS_READ).unwrap();
                let mut bgra = Vec::with_capacity((w * h * 4) as usize);
                for y in 0..h {
                    bgra.extend_from_slice(std::slice::from_raw_parts(
                        mapped.bits.add((y * mapped.pitch) as usize),
                        (w * 4) as usize,
                    ));
                }
                readback.Unmap().unwrap();
                let image = kadr_doc::Image::new(kadr_doc::Rect::new(0, 0, w as i32, h as i32), bgra);
                std::fs::create_dir_all(dir).unwrap();
                std::fs::write(dir.join(format!("{name}.png")), kadr_doc::pixels::encode_png(&image).unwrap()).unwrap();
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
                        // Cards scrolled under the grid's edge are hidden: they must not take the click.
                        let view = recent_view(popup, scale);
                        let (cx, cy) = ((r.left + r.right) / 2.0, (r.top + r.bottom) / 2.0);
                        let shown = cx >= view.left && cx < view.right && cy >= view.top && cy < view.bottom;
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
                        if shown {
                            assert_eq!(hit, Some(*b));
                        } else {
                            assert_ne!(hit, Some(*b), "{b:?} is scrolled out of view");
                        }
                    }
                }
            }
        }
    }
}
