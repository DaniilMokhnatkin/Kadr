//! The feature tour's illustrations: small looping scenes drawn live, each showing one thing
//! Kadr does — selecting, drawing, censoring, translating, recording, the corner thumbnail.
//! Everything is vector and timed by the seconds since the page appeared.
use crate::glass::{fade, fill, mix, rect, rgb, shadow, stroke, text};
use kadr_render::gpu::{Gpu, Shared};
use kadr_render::theme::Theme;
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D_SIZE_F, D2D1_COLOR_F, D2D1_FIGURE_BEGIN_FILLED, D2D1_FIGURE_BEGIN_HOLLOW, D2D1_FIGURE_END_CLOSED,
    D2D1_FIGURE_END_OPEN,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_ARC_SEGMENT, D2D1_ARC_SIZE_LARGE, D2D1_CAP_STYLE_ROUND, D2D1_ELLIPSE, D2D1_LINE_JOIN_ROUND,
    D2D1_STROKE_STYLE_PROPERTIES1, D2D1_SWEEP_DIRECTION_CLOCKWISE, ID2D1StrokeStyle1,
};
use windows::Win32::Graphics::DirectWrite::IDWriteTextFormat;
use windows_numerics::Vector2;

pub const RED: D2D1_COLOR_F = rgb(0xff3b30, 1.0);
pub const GREEN: D2D1_COLOR_F = rgb(0x34c759, 1.0);
const ORANGE: D2D1_COLOR_F = rgb(0xff9f0a, 1.0);
const PURPLE: D2D1_COLOR_F = rgb(0xaf52de, 1.0);
const WHITE: D2D1_COLOR_F = rgb(0xffffff, 1.0);

/// Fonts the scenes write with.
pub struct Ink<'a> {
    pub small: &'a IDWriteTextFormat,
    pub bold: &'a IDWriteTextFormat,
    pub center: &'a IDWriteTextFormat,
}

pub struct Scene<'a> {
    pub gpu: &'a Gpu,
    pub shared: &'a Shared,
    pub theme: &'a Theme,
    pub ink: Ink<'a>,
    /// Card the scene lives in, in pixels.
    pub r: D2D_RECT_F,
    pub s: f32,
    /// Seconds since the page appeared.
    pub t: f64,
    pub alpha: f32,
}

/// 0 → 1 over `len` seconds starting at `from`.
pub fn phase(t: f64, from: f64, len: f64) -> f32 {
    ((t - from) / len).clamp(0.0, 1.0) as f32
}
pub fn ease(p: f32) -> f32 {
    1.0 - (1.0 - p.clamp(0.0, 1.0)).powi(3)
}
/// A small overshoot, as a springy pop-in.
pub fn pop(p: f32) -> f32 {
    let p = p.clamp(0.0, 1.0);
    if p >= 1.0 {
        return 1.0;
    }
    1.0 - (-6.5 * p).exp() * (11.0 * p).cos()
}
fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

impl Scene<'_> {
    /// A point at fractions of the card.
    fn at(&self, fx: f32, fy: f32) -> (f32, f32) {
        (lerp(self.r.left, self.r.right, fx), lerp(self.r.top, self.r.bottom, fy))
    }
    fn area(&self, l: f32, t: f32, r: f32, b: f32) -> D2D_RECT_F {
        let (a, c) = (self.at(l, t), self.at(r, b));
        rect(a.0, a.1, c.0, c.1)
    }
    fn c(&self, c: D2D1_COLOR_F) -> D2D1_COLOR_F {
        fade(c, self.alpha)
    }
    fn style(&self) -> Option<ID2D1StrokeStyle1> {
        unsafe {
            self.shared
                .d2d
                .CreateStrokeStyle(
                    &D2D1_STROKE_STYLE_PROPERTIES1 {
                        startCap: D2D1_CAP_STYLE_ROUND,
                        endCap: D2D1_CAP_STYLE_ROUND,
                        lineJoin: D2D1_LINE_JOIN_ROUND,
                        ..Default::default()
                    },
                    None,
                )
                .ok()
        }
    }
    fn polyline(&self, points: &[(f32, f32)], c: D2D1_COLOR_F, width: f32) {
        let Some((first, rest)) = points.split_first() else { return };
        unsafe {
            let Ok(g) = self.shared.d2d.CreatePathGeometry() else { return };
            let Ok(sink) = g.Open() else { return };
            sink.BeginFigure(Vector2::new(first.0, first.1), D2D1_FIGURE_BEGIN_HOLLOW);
            let rest: Vec<Vector2> = rest.iter().map(|p| Vector2::new(p.0, p.1)).collect();
            sink.AddLines(&rest);
            sink.EndFigure(D2D1_FIGURE_END_OPEN);
            let _ = sink.Close();
            self.gpu.brush.SetColor(&self.c(c));
            self.gpu.d2d.DrawGeometry(&g, &self.gpu.brush, width, self.style().as_ref().map(|s| s.into()));
        }
    }
    fn polygon(&self, points: &[(f32, f32)], c: D2D1_COLOR_F) {
        let Some((first, rest)) = points.split_first() else { return };
        unsafe {
            let Ok(g) = self.shared.d2d.CreatePathGeometry() else { return };
            let Ok(sink) = g.Open() else { return };
            sink.BeginFigure(Vector2::new(first.0, first.1), D2D1_FIGURE_BEGIN_FILLED);
            let rest: Vec<Vector2> = rest.iter().map(|p| Vector2::new(p.0, p.1)).collect();
            sink.AddLines(&rest);
            sink.EndFigure(D2D1_FIGURE_END_CLOSED);
            let _ = sink.Close();
            self.gpu.brush.SetColor(&self.c(c));
            self.gpu.d2d.FillGeometry(&g, &self.gpu.brush, None);
        }
    }
    fn dot(&self, (x, y): (f32, f32), radius: f32, c: D2D1_COLOR_F) {
        unsafe {
            self.gpu.brush.SetColor(&self.c(c));
            self.gpu.d2d.FillEllipse(
                &D2D1_ELLIPSE { point: Vector2::new(x, y), radiusX: radius, radiusY: radius },
                &self.gpu.brush,
            );
        }
    }
    fn fill(&self, r: D2D_RECT_F, c: D2D1_COLOR_F, radius: f32) {
        fill(self.gpu, self.c(c), r, radius);
    }
    fn stroke(&self, r: D2D_RECT_F, c: D2D1_COLOR_F, radius: f32, width: f32) {
        stroke(self.gpu, self.c(c), r, radius, width);
    }
    fn write(&self, format: &IDWriteTextFormat, value: &str, r: D2D_RECT_F, c: D2D1_COLOR_F) {
        text(self.gpu, format, value, r, self.c(c));
    }
    fn shadow(&self, r: D2D_RECT_F, radius: f32, alpha: f32) {
        shadow(self.gpu, r, radius, self.s, 8.0, 3.0, alpha * self.alpha);
    }
    /// Width of `value` in `format`, for placing boxes over words.
    fn measure(&self, format: &IDWriteTextFormat, value: &str) -> f32 {
        let utf16: Vec<u16> = value.encode_utf16().collect();
        unsafe {
            self.shared
                .dwrite
                .CreateTextLayout(&utf16, format, 10_000.0, 100.0)
                .and_then(|l| {
                    let mut m = Default::default();
                    l.GetMetrics(&mut m).map(|()| m.widthIncludingTrailingWhitespace)
                })
                .unwrap_or(0.0)
        }
    }
    fn paper(&self) -> D2D1_COLOR_F {
        if self.theme.dark { rgb(0x2c2c2e, 1.0) } else { WHITE }
    }
    fn desk(&self) -> D2D1_COLOR_F {
        if self.theme.dark { rgb(0x1c2230, 1.0) } else { rgb(0xdfe7f5, 1.0) }
    }
    fn line_ink(&self) -> D2D1_COLOR_F {
        fade(self.theme.text_muted, 0.35)
    }
    /// A window with a title strip and a few lines of «text».
    fn window(&self, r: D2D_RECT_F, lines: usize, accent: D2D1_COLOR_F) {
        let s = self.s;
        self.shadow(r, 8.0 * s, 0.18);
        self.fill(r, self.paper(), 8.0 * s);
        let bar = rect(r.left, r.top, r.right, r.top + 16.0 * s);
        self.fill(bar, fade(accent, 0.16), 8.0 * s);
        for (i, c) in [RED, ORANGE, GREEN].iter().enumerate() {
            self.dot((r.left + (10.0 + 9.0 * i as f32) * s, r.top + 8.0 * s), 2.6 * s, *c);
        }
        for i in 0..lines {
            let y = r.top + (26.0 + 12.0 * i as f32) * s;
            if y + 6.0 * s > r.bottom - 6.0 * s {
                break;
            }
            let w = (r.right - r.left - 24.0 * s) * [0.9, 0.7, 0.82, 0.55, 0.76][i % 5];
            self.fill(rect(r.left + 12.0 * s, y, r.left + 12.0 * s + w, y + 5.0 * s), self.line_ink(), 2.5 * s);
        }
    }
    fn arrow_cursor(&self, (x, y): (f32, f32)) {
        let s = self.s * 0.9;
        let pts: Vec<(f32, f32)> =
            [(0.0, 0.0), (0.0, 16.0), (4.0, 12.5), (7.0, 18.5), (9.5, 17.5), (6.6, 11.6), (12.0, 11.6)]
                .iter()
                .map(|(a, b)| (x + a * s, y + b * s))
                .collect();
        self.polygon(&pts, rgb(0x111111, 1.0));
        let mut outline = pts.clone();
        outline.push(pts[0]);
        self.polyline(&outline, WHITE, 1.2 * s);
    }
    fn pill(&self, center: (f32, f32), label: &str, bg: D2D1_COLOR_F, fg: D2D1_COLOR_F, k: f32) {
        if k <= 0.01 {
            return;
        }
        let s = self.s;
        let w = self.measure(self.ink.bold, label) + 22.0 * s;
        let h = 22.0 * s;
        let (w, h) = (w * k, h * k);
        let r = rect(center.0 - w / 2.0, center.1 - h / 2.0, center.0 + w / 2.0, center.1 + h / 2.0);
        self.shadow(r, h / 2.0, 0.2);
        self.fill(r, bg, h / 2.0);
        if k > 0.7 {
            self.write(self.ink.center, label, r, fade(fg, (k - 0.7) / 0.3));
        }
    }
    /// A numbered marker with a tail from its bubble down to the spot.
    fn marker(&self, tip: (f32, f32), center: (f32, f32), n: &str, k: f32) {
        if k <= 0.01 {
            return;
        }
        let radius = 10.0 * self.s * k;
        let (dx, dy) = (tip.0 - center.0, tip.1 - center.1);
        let d = dx.hypot(dy);
        let toward = dy.atan2(dx);
        let half = (radius / d.max(radius + 0.1)).acos();
        unsafe {
            if let Ok(g) = self.shared.d2d.CreatePathGeometry()
                && let Ok(sink) = g.Open()
            {
                let on = |a: f32| Vector2::new(center.0 + radius * a.cos(), center.1 + radius * a.sin());
                sink.BeginFigure(
                    Vector2::new(lerp(center.0, tip.0, k), lerp(center.1, tip.1, k)),
                    D2D1_FIGURE_BEGIN_FILLED,
                );
                sink.AddLine(on(toward + half));
                sink.AddArc(&D2D1_ARC_SEGMENT {
                    point: on(toward - half),
                    size: D2D_SIZE_F { width: radius, height: radius },
                    rotationAngle: 0.0,
                    sweepDirection: D2D1_SWEEP_DIRECTION_CLOCKWISE,
                    arcSize: D2D1_ARC_SIZE_LARGE,
                });
                sink.EndFigure(D2D1_FIGURE_END_CLOSED);
                let _ = sink.Close();
                self.gpu.brush.SetColor(&self.c(RED));
                self.gpu.d2d.FillGeometry(&g, &self.gpu.brush, None);
            }
        }
        let box_ = rect(center.0 - radius, center.1 - radius, center.0 + radius, center.1 + radius);
        if k > 0.6 {
            self.write(self.ink.center, n, box_, WHITE);
        }
    }

    /// 1. PrtSc: the screen freezes and dims, a selection is pulled open, and it is copied.
    pub fn select(&self) {
        let s = self.s;
        let t = self.t % 4.6;
        let screen = self.area(0.06, 0.08, 0.94, 0.92);
        self.fill(screen, self.desk(), 10.0 * s);
        let windows = [(self.area(0.12, 0.16, 0.6, 0.6), 7), (self.area(0.42, 0.38, 0.88, 0.86), 6)];
        for (r, n) in windows {
            self.window(r, n, self.theme.accent);
        }
        let dim = 0.45 * ease(phase(t, 0.2, 0.3)) * (1.0 - phase(t, 4.2, 0.4));
        self.fill(screen, rgb(0x000000, dim), 10.0 * s);
        let p = ease(phase(t, 0.6, 1.1));
        let a = self.at(0.24, 0.3);
        let b = self.at(0.78, 0.76);
        let sel = rect(a.0, a.1, lerp(a.0, b.0, p), lerp(a.1, b.1, p));
        if p > 0.0 && t < 4.2 {
            // Inside the selection the screen shows through undimmed.
            unsafe {
                self.gpu.d2d.PushAxisAlignedClip(&sel, windows::Win32::Graphics::Direct2D::D2D1_ANTIALIAS_MODE_ALIASED);
            }
            self.fill(screen, self.desk(), 10.0 * s);
            for (r, n) in windows {
                self.window(r, n, self.theme.accent);
            }
            unsafe {
                self.gpu.d2d.PopAxisAlignedClip();
            }
            self.stroke(sel, WHITE, 0.0, 1.6 * s);
            if p >= 1.0 {
                for (x, y) in
                    [(sel.left, sel.top), (sel.right, sel.top), (sel.left, sel.bottom), (sel.right, sel.bottom)]
                {
                    self.fill(rect(x - 3.0 * s, y - 3.0 * s, x + 3.0 * s, y + 3.0 * s), WHITE, 1.0 * s);
                }
            }
            let (w, h) = ((640.0 * p) as i32, (360.0 * p) as i32);
            self.pill((sel.left + 38.0 * s, sel.top - 14.0 * s), &format!("{w} × {h}"), rgb(0x1c1c1e, 0.9), WHITE, 1.0);
        }
        if t < 2.0 {
            let c = (lerp(a.0, b.0, p), lerp(a.1, b.1, p));
            self.arrow_cursor(c);
        }
        // Copied: a flash, then a confirmation.
        let flash = (1.0 - (t as f32 - 2.4).abs() / 0.18).clamp(0.0, 1.0) * 0.35;
        self.fill(sel, rgb(0xffffff, flash), 0.0);
        let k = pop(phase(t, 2.5, 0.5)) * (1.0 - phase(t, 4.0, 0.3));
        self.pill(self.at(0.5, 0.86), "Скопировано · Ctrl+C", self.theme.accent, WHITE, k);
    }

    /// 2. Drawing: an arrow draws itself, a frame goes round, numbered markers pop in.
    pub fn draw(&self) {
        let s = self.s;
        let t = self.t % 5.0;
        let page = self.area(0.06, 0.08, 0.94, 0.92);
        self.window(page, 3, self.theme.accent);
        let block = self.area(0.5, 0.42, 0.86, 0.78);
        self.fill(block, fade(self.theme.accent, 0.18), 6.0 * s);
        self.fill(self.area(0.14, 0.5, 0.42, 0.53), self.line_ink(), 2.5 * s);
        self.fill(self.area(0.14, 0.58, 0.36, 0.61), self.line_ink(), 2.5 * s);
        self.fill(self.area(0.14, 0.66, 0.4, 0.69), self.line_ink(), 2.5 * s);
        let out = 1.0 - phase(t, 4.6, 0.4);
        // The arrow.
        let from = self.at(0.2, 0.84);
        let to = self.at(0.46, 0.62);
        let p = ease(phase(t, 0.3, 0.6));
        if p > 0.0 {
            let end = (lerp(from.0, to.0, p), lerp(from.1, to.1, p));
            self.polyline(&[from, end], fade(RED, out), 3.2 * s);
            let head = pop(phase(t, 0.8, 0.35));
            if head > 0.0 {
                let ang = (to.1 - from.1).atan2(to.0 - from.0);
                let len = 12.0 * s * head;
                let tip = |d: f32| (to.0 - len * (ang + d).cos(), to.1 - len * (ang + d).sin());
                self.polygon(&[to, tip(0.5), tip(-0.5)], fade(RED, out));
            }
        }
        // The frame, drawn around the block.
        let q = ease(phase(t, 1.0, 0.8));
        if q > 0.0 {
            let r = crate::glass::inflate(block, 5.0 * s);
            let corners = [(r.left, r.top), (r.right, r.top), (r.right, r.bottom), (r.left, r.bottom), (r.left, r.top)];
            let lens: Vec<f32> =
                corners.windows(2).map(|w| (w[1].0 - w[0].0).abs() + (w[1].1 - w[0].1).abs()).collect();
            let mut left = q * lens.iter().sum::<f32>();
            let mut pts = vec![corners[0]];
            for (i, l) in lens.iter().enumerate() {
                let k = (left / l).min(1.0);
                let (a, b) = (corners[i], corners[i + 1]);
                pts.push((lerp(a.0, b.0, k), lerp(a.1, b.1, k)));
                left -= l;
                if left <= 0.0 {
                    break;
                }
            }
            self.polyline(&pts, fade(RED, out), 2.6 * s);
        }
        self.marker(self.at(0.5, 0.42), self.at(0.62, 0.24), "1", pop(phase(t, 1.9, 0.5)) * out);
        self.marker(self.at(0.2, 0.84), self.at(0.1, 0.7), "2", pop(phase(t, 2.4, 0.5)) * out);
        self.pill(self.at(0.76, 0.86), "Важно!", WHITE, RED, pop(phase(t, 2.9, 0.5)) * out);
    }

    /// 3. Censor: the same chat at three levels — light, hard, full.
    pub fn censor(&self) -> &'static str {
        let s = self.s;
        let t = self.t % 7.2;
        let level = (t / 2.4) as usize;
        let local = t - level as f64 * 2.4;
        let card = self.area(0.06, 0.08, 0.94, 0.78);
        self.shadow(card, 12.0 * s, 0.16);
        self.fill(card, self.paper(), 12.0 * s);
        // (text, hidden from level): 0 light, 1 hard, 2 full only.
        let rows: [&[(&str, usize)]; 5] = [
            &[("Анна Смирнова", 1), ("  ", 9), ("12:40", 1)],
            &[("Пиши на ", 2), ("anna@mail.ru", 0)],
            &[("или звони ", 2), ("+7 912 345-67-89", 0)],
            &[("Пароль от Wi-Fi: ", 2), ("Sun-2026!", 0)],
            &[("Встречаемся в ", 2), ("18:30", 1), (", офис ", 2), ("305", 1)],
        ];
        let mut hidden = 0;
        let left = card.left + 16.0 * s;
        for (i, row) in rows.iter().enumerate() {
            let y = card.top + 14.0 * s + i as f32 * (card.bottom - card.top - 30.0 * s) / 5.0;
            let mut x = left;
            let font = if i == 0 { self.ink.bold } else { self.ink.small };
            for (j, (word, from)) in row.iter().enumerate() {
                let w = self.measure(font, word);
                let r = rect(x, y, x + w, y + 16.0 * s);
                self.write(
                    font,
                    word,
                    rect(x, y, x + w + 40.0 * s, y + 18.0 * s),
                    if i == 0 { self.theme.text } else { mix(self.theme.text, self.theme.text_muted, 0.2) },
                );
                if *from <= level && !word.trim().is_empty() {
                    hidden += 1;
                    // Blurs arrive in reading order, each sliding open from the left.
                    let k =
                        ease(phase(local, 0.25 + 0.07 * (i * 3 + j) as f64, 0.35)) * (1.0 - phase(local, 2.15, 0.25));
                    let b = rect(r.left - 2.0 * s, r.top, r.left - 2.0 * s + (w + 4.0 * s) * k, r.bottom);
                    self.fill(b, mix(self.paper(), self.theme.text_muted, 0.55), 4.0 * s);
                    for d in 0..((w / (6.0 * s)) as usize) {
                        let cx = b.left + (3.0 + 6.0 * d as f32) * s;
                        if cx < b.right - 2.0 * s {
                            self.dot(
                                (cx, (b.top + b.bottom) / 2.0 + ((d * 7) % 5) as f32 * 0.6 * s - 1.2 * s),
                                1.6 * s,
                                fade(self.paper(), 0.35),
                            );
                        }
                    }
                }
                x += w;
            }
        }
        let names = ["Лёгкая", "Жёсткая", "Полная"];
        let k = pop(phase(local, 0.0, 0.45));
        let label = format!("{} · {}", names[level], hidden);
        self.pill(
            self.at(0.5, 0.88),
            &label,
            mix(self.theme.selected, self.theme.accent, 0.15),
            self.theme.on_selected,
            k,
        );
        names[level]
    }

    /// 4. Text and translation: a line of light reads the text, and Alt+click shows it in Russian.
    pub fn translate(&self) {
        let s = self.s;
        let t = self.t % 5.6;
        let card = self.area(0.06, 0.08, 0.94, 0.62);
        self.shadow(card, 12.0 * s, 0.16);
        self.fill(card, self.paper(), 12.0 * s);
        let lines = ["The meeting moved to Friday.", "Bring the slides and coffee.", "See you at 10 AM!"];
        for (i, l) in lines.iter().enumerate() {
            let y = card.top + (16.0 + 22.0 * i as f32) * s;
            let read = phase(t, 0.5 + 0.25 * i as f64, 0.2);
            let w = self.measure(self.ink.small, l);
            self.fill(
                rect(card.left + 14.0 * s, y - 2.0 * s, card.left + 18.0 * s + w, y + 18.0 * s),
                fade(self.theme.accent, 0.12 * read),
                4.0 * s,
            );
            self.write(self.ink.small, l, rect(card.left + 16.0 * s, y, card.right, y + 18.0 * s), self.theme.text);
        }
        // The scanning beam.
        let p = phase(t, 0.4, 1.0);
        if p > 0.0 && p < 1.0 {
            let y = lerp(card.top + 8.0 * s, card.bottom - 8.0 * s, ease(p));
            self.fill(
                rect(card.left + 8.0 * s, y - 1.0 * s, card.right - 8.0 * s, y + 1.0 * s),
                self.theme.accent,
                1.0 * s,
            );
            self.fill(
                rect(card.left + 8.0 * s, y - 10.0 * s, card.right - 8.0 * s, y),
                fade(self.theme.accent, 0.12),
                0.0,
            );
        }
        let chip = pop(phase(t, 1.7, 0.4)) * (1.0 - phase(t, 2.6, 0.3));
        self.pill(self.at(0.7, 0.3), "Alt + клик", rgb(0x1c1c1e, 0.92), WHITE, chip);
        if t > 1.7 && t < 2.8 {
            self.arrow_cursor(self.at(0.52, 0.24));
        }
        // The translation bubble.
        let k = pop(phase(t, 2.6, 0.55)) * (1.0 - phase(t, 5.2, 0.3));
        if k > 0.01 {
            let base = self.area(0.14, 0.5, 0.9, 0.94);
            let h = (base.bottom - base.top) * k;
            let bubble = rect(base.left, base.top, base.right, base.top + h);
            self.shadow(bubble, 12.0 * s, 0.25);
            self.fill(bubble, if self.theme.dark { rgb(0x3a3a3c, 1.0) } else { WHITE }, 12.0 * s);
            if k > 0.8 {
                let a = (k - 0.8) / 0.2;
                self.write(
                    self.ink.bold,
                    "Перевод · EN → RU",
                    rect(bubble.left + 14.0 * s, bubble.top + 8.0 * s, bubble.right, bubble.top + 24.0 * s),
                    fade(self.theme.accent, a),
                );
                for (i, l) in
                    ["Встречу перенесли на пятницу.", "Возьми слайды и кофе.", "Увидимся в 10 утра!"].iter().enumerate()
                {
                    let y = bubble.top + (28.0 + 18.0 * i as f32) * s;
                    self.write(
                        self.ink.small,
                        l,
                        rect(bubble.left + 14.0 * s, y, bubble.right, y + 18.0 * s),
                        fade(self.theme.text, a),
                    );
                }
            }
        }
    }

    /// 5. Video: a red frame breathes around the area, the clock runs, the sound moves.
    pub fn record(&self) {
        let s = self.s;
        let t = self.t % 6.0;
        let screen = self.area(0.06, 0.06, 0.94, 0.74);
        self.fill(screen, self.desk(), 10.0 * s);
        let area = self.area(0.16, 0.14, 0.84, 0.64);
        self.fill(area, if self.theme.dark { rgb(0x0e1726, 1.0) } else { rgb(0x0b1a33, 1.0) }, 6.0 * s);
        // Something lively inside: a ball bouncing in the area.
        let u = (t * 0.9).fract() as f32;
        let bx = lerp(area.left + 16.0 * s, area.right - 16.0 * s, ((t * 0.35).fract() as f32 * 2.0 - 1.0).abs());
        let by = area.bottom - 14.0 * s - (u * (1.0 - u) * 4.0) * (area.bottom - area.top - 40.0 * s);
        self.dot((bx, by), 9.0 * s, mix(PURPLE, self.theme.accent, 0.4));
        let live = t < 4.6;
        let breathe = 0.7 + 0.3 * (t as f32 * 2.2).sin();
        if live {
            self.stroke(crate::glass::inflate(area, 3.0 * s), fade(RED, breathe), 6.0 * s, 2.2 * s);
        }
        // The panel.
        let panel = self.area(0.26, 0.78, 0.74, 0.92);
        let k = pop(phase(t, 0.1, 0.5)) * (1.0 - phase(t, 4.6, 0.3));
        if k > 0.01 {
            let h = panel.bottom - panel.top;
            self.shadow(panel, h / 2.0, 0.2 * k);
            self.fill(panel, fade(if self.theme.dark { rgb(0x3a3a3c, 1.0) } else { WHITE }, k), h / 2.0);
            let cy = (panel.top + panel.bottom) / 2.0;
            self.dot((panel.left + 16.0 * s, cy), 5.0 * s, fade(RED, breathe * k));
            let secs = (t.min(4.6)) as u32;
            self.write(
                self.ink.center,
                &format!("0:{secs:02}"),
                rect(panel.left + 24.0 * s, panel.top, panel.left + 64.0 * s, panel.bottom),
                fade(self.theme.text, k),
            );
            // Sound: five bars.
            for i in 0..5 {
                let level = (0.35 + 0.65 * ((t * 7.0 + i as f64 * 1.7).sin() * 0.5 + 0.5)) as f32;
                let x = panel.left + (74.0 + 6.0 * i as f32) * s;
                let half = 7.0 * s * level;
                self.fill(rect(x, cy - half, x + 3.0 * s, cy + half), fade(GREEN, k), 1.5 * s);
            }
            let stop = rect(panel.right - 58.0 * s, panel.top + 5.0 * s, panel.right - 6.0 * s, panel.bottom - 5.0 * s);
            self.fill(stop, fade(RED, k), (stop.bottom - stop.top) / 2.0);
            self.write(self.ink.center, "Стоп", stop, fade(WHITE, k));
        }
        // Stopped: the video lands in the corner, ready to paste.
        let land = pop(phase(t, 4.8, 0.5)) * (1.0 - phase(t, 5.7, 0.3));
        if land > 0.01 {
            let r = self.area(0.6, 0.5, 0.92, 0.88);
            let (cx, cy) = ((r.left + r.right) / 2.0, (r.top + r.bottom) / 2.0);
            let (w, h) = ((r.right - r.left) * land / 2.0, (r.bottom - r.top) * land / 2.0);
            let thumb = rect(cx - w, cy - h, cx + w, cy + h);
            self.shadow(thumb, 8.0 * s, 0.3);
            self.fill(thumb, rgb(0x0b1a33, 1.0), 8.0 * s);
            let (px, py) = (cx - 5.0 * s * land, cy);
            self.polygon(&[(px, py - 8.0 * s * land), (px, py + 8.0 * s * land), (px + 12.0 * s * land, py)], WHITE);
            self.pill((cx, thumb.bottom + 2.0 * s), "Видео в буфере", GREEN, WHITE, land);
        }
    }

    /// 6. The corner thumbnail: it slides in, and is dragged straight into a chat.
    pub fn thumbnail(&self) {
        let s = self.s;
        let t = self.t % 5.4;
        let screen = self.area(0.06, 0.06, 0.94, 0.94);
        self.fill(screen, self.desk(), 10.0 * s);
        let chat = self.area(0.1, 0.12, 0.58, 0.88);
        self.shadow(chat, 8.0 * s, 0.18);
        self.fill(chat, self.paper(), 8.0 * s);
        self.write(
            self.ink.bold,
            "Чат",
            rect(chat.left + 12.0 * s, chat.top + 8.0 * s, chat.right, chat.top + 24.0 * s),
            self.theme.text,
        );
        for (i, (w, mine)) in [(0.6, false), (0.45, true), (0.7, false)].iter().enumerate() {
            let y = chat.top + (34.0 + 22.0 * i as f32) * s;
            let width = (chat.right - chat.left - 24.0 * s) * *w as f32;
            let x = if *mine { chat.right - 12.0 * s - width } else { chat.left + 12.0 * s };
            self.fill(
                rect(x, y, x + width, y + 16.0 * s),
                if *mine { fade(self.theme.accent, 0.85) } else { mix(self.paper(), self.theme.text_muted, 0.18) },
                8.0 * s,
            );
        }
        // The thumbnail: in from the right, then carried to the chat.
        let home = self.area(0.66, 0.6, 0.9, 0.86);
        let slide = pop(phase(t, 0.3, 0.6));
        let carry = ease(phase(t, 1.9, 0.9));
        let dropped = t > 2.85;
        let (hw, hh) = ((home.right - home.left) / 2.0, (home.bottom - home.top) / 2.0);
        let start = ((home.left + home.right) / 2.0 + (1.0 - slide) * 120.0 * s, (home.top + home.bottom) / 2.0);
        let target = ((chat.left + chat.right) / 2.0 + 30.0 * s, chat.bottom - 34.0 * s);
        let c = (lerp(start.0, target.0, carry), lerp(start.1, target.1, carry));
        let out = 1.0 - phase(t, 5.0, 0.3);
        if !dropped {
            let k = 1.0 - 0.12 * carry;
            let r = rect(c.0 - hw * k, c.1 - hh * k, c.0 + hw * k, c.1 + hh * k);
            self.shadow(r, 8.0 * s, 0.3 * slide);
            self.fill(r, fade(self.paper(), slide), 8.0 * s);
            self.fill(
                crate::glass::inflate(r, -4.0 * s),
                fade(mix(self.theme.accent, PURPLE, 0.3), 0.35 * slide),
                5.0 * s,
            );
            self.stroke(r, fade(WHITE, 0.8 * slide), 8.0 * s, 1.5 * s);
            if t > 1.3 {
                self.arrow_cursor((c.0 + 6.0 * s, c.1 + 4.0 * s));
            }
        } else {
            // In the chat as a sent picture.
            let k = pop(phase(t, 2.85, 0.45)) * out;
            let w = (chat.right - chat.left) * 0.5 * k;
            let r = rect(
                chat.right - 12.0 * s - w,
                chat.bottom - 12.0 * s - 56.0 * s * k,
                chat.right - 12.0 * s,
                chat.bottom - 12.0 * s,
            );
            self.fill(r, fade(mix(self.theme.accent, PURPLE, 0.3), 0.55), 8.0 * s);
            self.pill(((r.left + r.right) / 2.0, r.top - 4.0 * s), "Отправлено", GREEN, WHITE, k);
        }
    }
}
