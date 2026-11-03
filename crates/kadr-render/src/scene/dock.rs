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
    let pw = (934.0 * s).min((w - 16.0 * s).max(60.0 * s));
    let content = pw / s - 16.0;
    let mut buttons = Vec::new();
    let mut y = 8.0;
    let empty = measure(shared, &text.button, "")?;
    let mut add = |b, x: f32, y: f32, width: f32, label: &str| -> Result<()> {
        buttons.push((
            b,
            rect(x * s, y * s, (x + width) * s, (y + SIDE) * s),
            if label.is_empty() {
                empty.clone()
            } else {
                measure(shared, if b == ToolbarButton::Copy { &text.title } else { &text.button }, label)?
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
        (ToolbarButton::Smart, 112.0, "Цензура"),
        (ToolbarButton::Undo, 44.0, ""),
        (ToolbarButton::Redo, 44.0, ""),
        (ToolbarButton::Menu(BarMenu::Properties), 76.0, ""),
    ];
    let narrow = content < 700.0;
    let track_width = if narrow { content } else { content - 308.0 };
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
        let info = if content >= 700.0 { 200.0 } else { 0.0 };
        let level_width = (content - SIDE - GAP - info) / 3.0;
        for (i, (level, label)) in levels.into_iter().enumerate() {
            add(ToolbarButton::Censor(level), 8.0 + i as f32 * level_width, y, level_width, label)?;
        }
        add(ToolbarButton::Original, pw / s - 8.0 - SIDE, y, SIDE, "")?;
        y += SIDE + GAP;
    }
    // One dense action row; no icon-only pin/text and no gap stretched across the middle.
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
        .map(|(_, label)| {
            if label.is_empty() {
                Ok(SIDE)
            } else {
                Ok((measure(shared, &text.button, label)?.w / s
                    + if narrow { 40.0 } else { 48.0 }
                    + if *label == "Недавние" && e.ui.recent_count > 0 { 20.0 } else { 0.0 })
                .ceil())
            }
        })
        .collect::<Result<_>>()?;
    let required = widths.iter().sum::<f32>() + GAP * 7.0;
    let extra = ((content - required) / 7.0).max(0.0);
    x = 8.0;
    for ((b, label), width) in actions.into_iter().zip(widths) {
        let width = (width + if b == ToolbarButton::SaveAs { 0.0 } else { extra }).min(content);
        if x + width > pw / s - 8.0 + 0.1 && x > 8.0 {
            x = 8.0;
            y += SIDE + GAP;
        }
        add(b, x, y, width, label)?;
        x += width + GAP;
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
            .map(|t| (ToolbarButton::Tool(t), t.label().into()))
            .collect(),
        BarMenu::Blur => [Tool::Blur, Tool::BlurEllipse, Tool::BlurBrush]
            .into_iter()
            .map(|t| (ToolbarButton::Tool(t), t.label().into()))
            .collect(),
        BarMenu::More => vec![
            (ToolbarButton::Tool(Tool::Eraser), "Ластик".into()),
            (ToolbarButton::Tool(Tool::Lens), "Увеличение".into()),
            (ToolbarButton::Tool(Tool::Spotlight), "Размытие вокруг".into()),
            (ToolbarButton::Tool(Tool::Redact), "Скрыть вручную".into()),
            (ToolbarButton::PinWindow, "Поверх окон".into()),
            (ToolbarButton::About, "О программе…".into()),
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
    } else {
        228.0
    }) * s)
        .min(size.0 - 16.0 * s);
    let colored = !e.tool.is_blur() && e.tool != Tool::Lens;
    let strength = e.tool.is_blur() || e.tool == Tool::Lens;
    let grid = menu == BarMenu::Recent;
    let count = e.ui.recent_count.saturating_sub(e.ui.page * 6).min(6);
    let ph = if grid {
        (64.0 + count.max(1).div_ceil(3) as f32 * 132.0 + if e.ui.recent_count > 6 { 44.0 } else { 0.0 }) * s
    } else if menu == BarMenu::Properties {
        (16.0 + (1 + usize::from(colored) + usize::from(strength) + usize::from(e.tool == Tool::Text)) as f32 * 52.0)
            * s
    } else {
        (16.0 + rows.len() as f32 * 44.0) * s
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
    let x = anchor.left.clamp(8.0 * s, (size.0 - pw - 8.0 * s).max(8.0 * s));
    let y = if panel.top - ph - 8.0 * s >= 8.0 * s {
        panel.top - ph - 8.0 * s
    } else {
        (panel.bottom + 8.0 * s).min(size.1 - ph - 8.0 * s).max(8.0 * s)
    };
    let popup = rect(x, y, x + pw, y + ph);
    let mut push = |b, r, label: &str| -> Result<()> {
        buttons.push((b, r, measure(shared, &text.button, label)?));
        Ok(())
    };
    if grid {
        push(
            ToolbarButton::RecentClear,
            rect(x + pw - 152.0 * s, y + 8.0 * s, x + pw - 8.0 * s, y + 52.0 * s),
            "Очистить всё",
        )?;
        let cell = (pw - 24.0 * s) / 3.0;
        for n in 0..count {
            let i = e.ui.page * 6 + n;
            let left = x + 8.0 * s + (n % 3) as f32 * (cell + 4.0 * s);
            let top = y + 60.0 * s + (n / 3) as f32 * 132.0 * s;
            push(ToolbarButton::RecentOpen(i), rect(left, top, left + cell, top + 124.0 * s), "")?;
            push(ToolbarButton::RecentPin(i), rect(left, top, left + 32.0 * s, top + 32.0 * s), "")?;
            push(ToolbarButton::RecentDelete(i), rect(left + cell - 32.0 * s, top, left + cell, top + 32.0 * s), "")?;
        }
        if e.ui.recent_count > 6 {
            for (next, label, left) in [(false, "Назад", x + 8.0 * s), (true, "Далее", x + pw - 104.0 * s)] {
                push(ToolbarButton::RecentPage(next), rect(left, y + ph - 44.0 * s, left + 96.0 * s, y + ph), label)?;
            }
        }
    } else if menu == BarMenu::Properties {
        let mut top = y + 8.0 * s;
        if colored {
            let cell = (pw - 16.0 * s) / COLORS.len() as f32;
            for i in 0..COLORS.len() {
                let left = x + 8.0 * s + i as f32 * cell;
                push(ToolbarButton::Color(i), rect(left, top, left + cell, top + 44.0 * s), "")?;
            }
            top += 52.0 * s;
        }
        let cell = (pw - 16.0 * s) / 4.0;
        for (i, n) in [2, 3, 5, 8].into_iter().enumerate() {
            let left = x + 8.0 * s + i as f32 * cell;
            push(ToolbarButton::WidthValue(n), rect(left, top, left + cell, top + 44.0 * s), &format!("{n} px"))?;
        }
        top += 52.0 * s;
        if strength {
            push(
                ToolbarButton::Strength,
                rect(x + 8.0 * s, top, x + pw - 8.0 * s, top + 44.0 * s),
                &format!("Сила / увеличение: {}", e.strength + 1),
            )?;
            top += 52.0 * s;
        }
        if e.tool == Tool::Text {
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
                push(b, rect(left, top, left + cell, top + 44.0 * s), b.label())?;
            }
        }
    } else {
        for (i, (b, label)) in rows.drain(..).enumerate() {
            push(
                b,
                rect(x + 8.0 * s, y + (8.0 + i as f32 * 44.0) * s, x + pw - 8.0 * s, y + (52.0 + i as f32 * 44.0) * s),
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
    if let Some(track) = target.track {
        fill_rounded(gpu, fade(theme.control, a), track, 22.0 * s);
    }
    if let Some(r) = l.indicator {
        fill_rounded(gpu, fade(theme.accent, a), inflate(r, -4.0 * s), 18.0 * s);
    }
    if let Some(p) = target.popup {
        shadow(gpu, theme.shadow, p, 22.0 * s, s, a, true);
        fill_rounded(gpu, fade(theme.surface, a), p, 22.0 * s);
        edge(gpu, theme, p, 22.0 * s, s, a);
        if e.ui.menu == Some(BarMenu::Recent) {
            caption(
                gpu,
                shared,
                &text.label,
                "Недавние",
                p.left + 16.0 * s,
                p.top + 20.0 * s,
                fade(theme.text_muted, a),
            )?;
            if e.ui.recent_count == 0 {
                caption(
                    gpu,
                    shared,
                    &text.label,
                    "Снимков пока нет",
                    p.left + 16.0 * s,
                    p.top + 76.0 * s,
                    fade(theme.text_muted, a),
                )?;
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
        let ink = fade(if !c.enabled { theme.disabled } else { theme.text }, alpha);
        let is_main = target.track.is_some_and(|track| inside_rect(track, r));
        if let ToolbarButton::RecentOpen(i) = b {
            if let Some(item) = toolbar.recent.get(i) {
                draw_recent(gpu, shared, text, theme, r, item, (s, alpha, c.hover))?;
            }
            continue;
        }
        let selected = if let ToolbarButton::RecentPin(i) = b {
            f32::from(toolbar.recent.get(i).is_some_and(|item| item.pinned))
        } else {
            c.selected
        };
        let neutral = fade(theme.control, alpha * c.hover);
        if !is_main {
            if b == ToolbarButton::Copy {
                fill_rounded(gpu, fade(theme.accent, alpha), r, 22.0 * s);
            } else if selected > 0.001 {
                fill_rounded(
                    gpu,
                    fade(
                        if matches!(b, ToolbarButton::Censor(_) | ToolbarButton::WidthValue(_)) {
                            theme.accent
                        } else {
                            theme.selected
                        },
                        alpha * selected,
                    ),
                    r,
                    22.0 * s,
                );
            } else if matches!(b, ToolbarButton::RecentPin(_) | ToolbarButton::RecentDelete(_)) {
                fill_rounded(gpu, fade(theme.surface, alpha * 0.8), r, 16.0 * s);
            } else if c.hover > 0.001 {
                fill_rounded(gpu, neutral, r, 22.0 * s);
            }
        } else if c.hover > 0.001 {
            fill_rounded(gpu, neutral, inflate(r, -4.0 * s), 18.0 * s);
        }
        if e.ui.focus == Some(b) {
            edge(gpu, theme, inflate(r, -2.0 * s), 20.0 * s, s, alpha);
        }
        if let ToolbarButton::Color(i) = b {
            let rgb = COLORS[i];
            let color =
                D2D1_COLOR_F { r: rgb[0] as f32 / 255.0, g: rgb[1] as f32 / 255.0, b: rgb[2] as f32 / 255.0, a: alpha };
            fill_rounded(
                gpu,
                color,
                rect(
                    (r.left + r.right) / 2.0 - 10.0 * s,
                    r.top + 12.0 * s,
                    (r.left + r.right) / 2.0 + 10.0 * s,
                    r.bottom - 12.0 * s,
                ),
                10.0 * s,
            );
            if e.color == i {
                edge(gpu, theme, inflate(r, -4.0 * s), 20.0 * s, s, alpha);
            }
            continue;
        }
        if matches!(b, ToolbarButton::SystemAudio | ToolbarButton::Microphone) {
            draw_text(gpu, &c.label, r.left + 12.0 * s, r.top + (44.0 * s - c.label.h) / 2.0, ink);
            let track = rect(r.right - 46.0 * s, r.top + 11.0 * s, r.right - 10.0 * s, r.bottom - 11.0 * s);
            fill_rounded(gpu, fade(if selected > 0.5 { theme.accent } else { theme.track }, alpha), track, 11.0 * s);
            let x = if selected > 0.5 { track.right - 20.0 * s } else { track.left + 2.0 * s };
            fill_rounded(
                gpu,
                fade(theme.on_accent, alpha),
                rect(x, track.top + 2.0 * s, x + 18.0 * s, track.bottom - 2.0 * s),
                9.0 * s,
            );
            continue;
        }
        let under = l
            .indicator
            .is_some_and(|p| is_main && (p.left + p.right) / 2.0 >= r.left && (p.left + p.right) / 2.0 < r.right);
        let mut ink = if b == ToolbarButton::Copy
            || under
            || (matches!(b, ToolbarButton::Censor(_) | ToolbarButton::WidthValue(_)) && selected > 0.5)
        {
            fade(theme.on_accent, alpha)
        } else if selected > 0.5 {
            fade(theme.on_selected, alpha)
        } else {
            ink
        };
        if b == ToolbarButton::RecentClear {
            ink = fade(theme.on_selected, alpha);
        }
        if let ToolbarButton::Censor(level) = b {
            draw_text(gpu, &c.label, r.left + (r.right - r.left - c.label.w) / 2.0, r.top + 4.0 * s, ink);
            let help = match level {
                Censor::Light => "Контакты и секреты",
                Censor::Hard => "Данные, имена, адреса",
                Censor::Full => "Весь текст",
            };
            let t = measure(shared, &text.label, help)?;
            if t.w < r.right - r.left {
                draw_text(gpu, &t, r.left + (r.right - r.left - t.w) / 2.0, r.bottom - t.h - 3.0 * s, ink);
            }
            continue;
        }
        if b == ToolbarButton::Menu(BarMenu::Properties) {
            let rgb = COLORS[e.color.min(COLORS.len() - 1)];
            fill_rounded(
                gpu,
                D2D1_COLOR_F { r: rgb[0] as f32 / 255.0, g: rgb[1] as f32 / 255.0, b: rgb[2] as f32 / 255.0, a: alpha },
                rect(r.left + 8.0 * s, r.top + 15.0 * s, r.left + 22.0 * s, r.top + 29.0 * s),
                7.0 * s,
            );
            caption(
                gpu,
                shared,
                &text.button,
                &format!("{} px", e.width),
                r.left + 29.0 * s,
                r.top + (44.0 * s - c.label.h) / 2.0,
                ink,
            )?;
            continue;
        }
        let is_label_only = matches!(
            b,
            ToolbarButton::RecentClear
                | ToolbarButton::RecentPage(_)
                | ToolbarButton::WidthValue(_)
                | ToolbarButton::Strength
                | ToolbarButton::TextPlain
                | ToolbarButton::TextFilled
                | ToolbarButton::TextOutline
                | ToolbarButton::TextCallout
        );
        if c.label.w > 0.0 && !is_label_only {
            let badge = if b == ToolbarButton::Recent && e.ui.recent_count > 0 { 20.0 * s } else { 0.0 };
            let content = 24.0 * s + 6.0 * s + c.label.w + badge;
            let left = r.left + ((r.right - r.left - content) / 2.0).max(4.0 * s);
            let icon = rect(left, r.top, left + 24.0 * s, r.bottom);
            glyph(
                gpu,
                b,
                icon,
                if b == ToolbarButton::Record { D2D1_COLOR_F { r: 1.0, g: 0.27, b: 0.23, a: alpha } } else { ink },
                s,
                e,
            );
            draw_text(gpu, &c.label, left + 30.0 * s, r.top + (r.bottom - r.top - c.label.h) / 2.0, ink);
            if badge > 0.0 {
                let t = measure(shared, &text.label, &e.ui.recent_count.to_string())?;
                let center = left + content - 8.0 * s;
                fill_rounded(
                    gpu,
                    fade(theme.control, alpha),
                    rect(center - 8.0 * s, r.top + 14.0 * s, center + 8.0 * s, r.bottom - 14.0 * s),
                    8.0 * s,
                );
                draw_text(gpu, &t, center - t.w / 2.0, r.top + (44.0 * s - t.h) / 2.0, fade(theme.text_muted, alpha));
            }
        } else if c.label.w > 0.0 {
            // Four styles use shorter native captions to fit the same 44 DIP targets.
            let short = match b {
                ToolbarButton::TextPlain => "Текст",
                ToolbarButton::TextFilled => "Фон",
                ToolbarButton::TextOutline => "Контур",
                ToolbarButton::TextCallout => "Выноска",
                ToolbarButton::Strength => "",
                _ => "",
            };
            let t = if !short.is_empty() { measure(shared, &text.button, short)? } else { c.label.clone() };
            draw_text(gpu, &t, r.left + (r.right - r.left - t.w) / 2.0, r.top + (r.bottom - r.top - t.h) / 2.0, ink);
        } else {
            glyph(gpu, b, r, ink, s, e);
        }
    }
    if e.smart != SmartUi::Off {
        let value = match e.smart {
            SmartUi::On(_, n) => format!("Скрыто: {n}"),
            SmartUi::Scanning(_) => "Поиск данных…".into(),
            SmartUi::Off => String::new(),
        };
        if let Some((_, r, _)) = target.buttons.iter().find(|b| b.0 == ToolbarButton::Original) {
            let t = measure(shared, &text.label, &value)?;
            if target.panel.right - target.panel.left >= 700.0 * s {
                draw_text(gpu, &t, r.left - 16.0 * s - t.w, r.top + (44.0 * s - t.h) / 2.0, fade(theme.text_muted, a));
            }
        }
    }
    unsafe {
        gpu.d2d.SetTransform(&previous);
    }
    if let Some((button, r, opacity)) = l.tip.filter(|_| e.ui.menu.is_none()) {
        let label = button.label();
        if !label.is_empty() {
            let t = measure(shared, &text.label, label)?;
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
fn caption(gpu: &Gpu, shared: &Shared, style: &Style, value: &str, x: f32, y: f32, ink: D2D1_COLOR_F) -> Result<()> {
    draw_text(gpu, &measure(shared, style, value)?, x, y, ink);
    Ok(())
}
fn glyph(gpu: &Gpu, b: ToolbarButton, r: D2D_RECT_F, ink: D2D1_COLOR_F, s: f32, e: EditorBar) {
    if b == ToolbarButton::SaveAs {
        chevron(gpu, r, ink, s);
        return;
    }
    let icon = match b {
        ToolbarButton::Menu(BarMenu::Shapes) => ToolbarButton::Tool(e.ui.shape),
        ToolbarButton::Menu(BarMenu::Blur) => ToolbarButton::Tool(e.ui.blur),
        ToolbarButton::RecentPin(_) | ToolbarButton::PinWindow => ToolbarButton::Pin,
        ToolbarButton::RecentDelete(_) => ToolbarButton::Close,
        _ => b,
    };
    glyphs::draw(gpu, icon, r, ink, s);
    if matches!(b, ToolbarButton::Menu(BarMenu::Shapes | BarMenu::Blur)) {
        chevron(gpu, rect(r.right - 20.0 * s, r.top, r.right - 8.0 * s, r.bottom), ink, s * 0.55);
    }
}
fn chevron(gpu: &Gpu, r: D2D_RECT_F, ink: D2D1_COLOR_F, s: f32) {
    let (x, y) = ((r.left + r.right) / 2.0, (r.top + r.bottom) / 2.0);
    unsafe {
        gpu.brush.SetColor(&ink);
        gpu.d2d.DrawLine(
            Vector2::new(x - 4.0 * s, y - 2.0 * s),
            Vector2::new(x, y + 2.0 * s),
            &gpu.brush,
            1.5 * s,
            None::<&ID2D1StrokeStyle>,
        );
        gpu.d2d.DrawLine(
            Vector2::new(x, y + 2.0 * s),
            Vector2::new(x + 4.0 * s, y - 2.0 * s),
            &gpu.brush,
            1.5 * s,
            None::<&ID2D1StrokeStyle>,
        );
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
    let image_rect = rect(r.left, r.top, r.right, r.top + 80.0 * s);
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
                gpu.d2d.DrawBitmap(bitmap, Some(&raw const dest), a, D2D1_INTERPOLATION_MODE_LINEAR, None, None);
            }
        }
    }
    // Clip captions rather than painting them over the neighbouring capture.
    unsafe {
        gpu.d2d.PushAxisAlignedClip(&r, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
    }
    caption(gpu, shared, &text.button, &item.title, r.left + 6.0 * s, r.top + 84.0 * s, fade(theme.text, a))?;
    caption(gpu, shared, &text.label, &item.caption, r.left + 6.0 * s, r.top + 104.0 * s, fade(theme.text_muted, a))?;
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
