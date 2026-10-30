use crate::{Point, Rect};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Tool {
    #[default]
    Select,
    Arrow,
    Line,
    Rectangle,
    Ellipse,
    Pen,
    Marker,
    Eraser,
    Lens,
    Text,
    Counter,
    Blur,
    BlurEllipse,
    BlurBrush,
    Spotlight,
    Redact,
}
impl Tool {
    pub fn is_blur(self) -> bool {
        matches!(self, Self::Blur | Self::BlurEllipse | Self::BlurBrush | Self::Spotlight)
    }
    pub fn is_path(self) -> bool {
        matches!(self, Self::Pen | Self::Marker | Self::BlurBrush | Self::Eraser)
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Select => "Выбор · V",
            Self::Arrow => "Стрелка · A",
            Self::Line => "Линия · L",
            Self::Rectangle => "Рамка · R",
            Self::Ellipse => "Круг / овал · O",
            Self::Pen => "Перо · P",
            Self::Marker => "Маркер (архив)",
            Self::Eraser => "Ластик · M",
            Self::Lens => "Увеличение · G",
            Self::Text => "Текст · T",
            Self::Counter => "Нумерация · N",
            Self::Blur => "Размытие · B",
            Self::BlurEllipse => "Размытие овалом · E",
            Self::BlurBrush => "Кисть размытия · U",
            Self::Spotlight => "Размытие вокруг · I",
            Self::Redact => "Скрыть · X",
        }
    }
    pub const ALL: [Tool; 15] = [
        Self::Select,
        Self::Arrow,
        Self::Line,
        Self::Rectangle,
        Self::Ellipse,
        Self::Pen,
        Self::Eraser,
        Self::Lens,
        Self::Text,
        Self::Counter,
        Self::Blur,
        Self::BlurEllipse,
        Self::BlurBrush,
        Self::Spotlight,
        Self::Redact,
    ];
}
pub const COLORS: [[u8; 3]; 6] = [
    [239, 68, 68],
    [250, 204, 21],
    [34, 197, 94],
    [56, 189, 248],
    [255, 255, 255],
    [17, 17, 17],
];
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TextStyle { #[default] Plain, Filled, Outline }

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Annotation {
    pub id: u64,
    pub tool: Tool,
    pub points: Vec<[i32; 2]>,
    pub color: [u8; 3],
    pub width: u32,
    #[serde(default)]
    pub text: String,
    #[serde(default = "legacy_text_style")]
    pub text_style: TextStyle,
    #[serde(default)]
    pub callout: bool,
    #[serde(default)]
    pub number: u32,
    #[serde(default = "normal")]
    pub strength: u8,
    #[serde(default)]
    pub seed: u64,
}
fn legacy_text_style() -> TextStyle { TextStyle::Filled }
fn normal() -> u8 {
    1
}
impl Annotation {
    pub fn new(id: u64, tool: Tool, p: Point) -> Self {
        Self {
            id,
            tool,
            points: vec![[p.x, p.y], [p.x, p.y]],
            color: COLORS[0],
            width: 3,
            text: String::new(),
            text_style: TextStyle::Plain,
            callout: false,
            number: 1,
            strength: 1,
            seed: 0,
        }
    }
    pub fn validate(&self) -> bool {
        self.id > 0
            && !matches!(self.tool, Tool::Select | Tool::Eraser)
            && (2..=8192).contains(&self.points.len())
            && (1..=64).contains(&self.width)
            && self.strength <= 2
            && self.number <= 9999
            && self.text.len() <= 4096
            && self.text.lines().count() <= 64
            && self.points.iter().flatten().all(|v| (-65536..=65536).contains(v))
    }
    pub fn bounds(&self) -> Rect {
        let mut l = self.points[0][0];
        let mut t = self.points[0][1];
        let mut r = l;
        let mut b = t;
        for [x, y] in &self.points {
            l = l.min(*x);
            r = r.max(*x);
            t = t.min(*y);
            b = b.max(*y);
        }
        match self.tool {
            Tool::Text => {
                l=self.points[0][0]; t=self.points[0][1];
                let size = self.font_size();
                let lines = self.text.lines().count().max(1) as i32;
                r = l
                    + (self.text.lines().map(|s| s.chars().count()).max().unwrap_or(0).max(3) as i32 * size * 3 / 5)
                        .max(size);
                b = t + lines * size * 3 / 2;
                if self.callout && let Some([x,y])=self.points.get(2) {l=l.min(*x);t=t.min(*y);r=r.max(*x);b=b.max(*y);}
            }
            Tool::Counter => {
                let radius = self.font_size() * 3 / 4;
                l -= radius;
                t -= radius;
                r += radius;
                b += radius;
            }
            _ => {}
        }
        let pad = match self.tool {
            Tool::BlurBrush | Tool::Marker => (self.width * 4) as i32,
            _ => self.width as i32 + 2,
        };
        Rect::new(l - pad, t - pad, r + pad + 1, b + pad + 1)
    }
    pub fn rect(&self) -> Rect {
        Rect::spanning(
            Point::new(self.points[0][0], self.points[0][1]),
            Point::new(self.points[1][0], self.points[1][1]),
        )
    }
    pub fn font_size(&self) -> i32 {
        (self.width * 4 + 12) as i32
    }
    pub fn transformed(&self, from: Rect, to: Rect) -> Self {
        let mut out = self.clone();
        let sx = to.width() as f64 / from.width().max(1) as f64;
        let sy = to.height() as f64 / from.height().max(1) as f64;
        for p in &mut out.points {
            p[0] = to.left + ((p[0] - from.left) as f64 * sx).round() as i32;
            p[1] = to.top + ((p[1] - from.top) as f64 * sy).round() as i32;
        }
        out.width = ((self.width as f64 * (sx + sy) / 2.0).round() as u32).clamp(1, 64);
        out
    }
    pub fn hit(&self, p: Point) -> bool {
        if !self.bounds().contains(p) {
            return false;
        }
        let tolerance = (self.width as f64 / 2.0 + 6.0).max(8.0);
        match self.tool {
            Tool::Line | Tool::Arrow | Tool::Pen | Tool::Marker | Tool::BlurBrush => self.points.windows(2).any(|s| {
                distance(p, s[0], s[1])
                    <= if self.tool == Tool::BlurBrush {
                        (self.width * 4) as f64
                    } else {
                        tolerance
                    }
            }),
            Tool::Rectangle => {
                let r = self.rect();
                let inner = Rect::new(r.left + 8, r.top + 8, r.right - 8, r.bottom - 8);
                !inner.contains(p)
            }
            // The bubble (second point) and the tail down to its tip (first point).
            Tool::Counter => {
                let [cx, cy] = self.points[1];
                let r = (self.font_size() * 3 / 4) as f64;
                (((p.x - cx) as f64).hypot((p.y - cy) as f64) <= r + 4.0)
                    || distance(p, self.points[1], self.points[0]) <= tolerance
            }
            _ => true,
        }
    }
}
pub fn distance(p: Point, a: [i32; 2], b: [i32; 2]) -> f64 {
    let dx = (b[0] - a[0]) as f64;
    let dy = (b[1] - a[1]) as f64;
    let u = if dx * dx + dy * dy == 0.0 {
        0.0
    } else {
        (((p.x - a[0]) as f64 * dx + (p.y - a[1]) as f64 * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0)
    };
    ((p.x as f64 - a[0] as f64 - u * dx).powi(2) + (p.y as f64 - a[1] as f64 - u * dy).powi(2)).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invalid_annotations_are_bounded() {
        let mut shape = Annotation::new(1, Tool::Arrow, Point::new(-10, 20));
        assert!(shape.validate());
        shape.width = 0;
        assert!(!shape.validate());
        shape.width = 3;
        shape.points = vec![[0, 0]; 8193];
        assert!(!shape.validate());
    }
    #[test]
    fn line_hit_does_not_select_empty_bounding_box() {
        let mut s = Annotation::new(1, Tool::Line, Point::new(0, 0));
        s.points[1] = [100, 100];
        assert!(s.hit(Point::new(50, 50)));
        assert!(!s.hit(Point::new(10, 90)));
    }
    #[test]
    fn transform_preserves_points_across_negative_monitors() {
        let mut s = Annotation::new(1, Tool::Line, Point::new(-100, 20));
        s.points[1] = [100, 100];
        let mapped = s.transformed(Rect::new(-100, 0, 100, 200), Rect::new(0, 0, 100, 100));
        assert_eq!(mapped.points, vec![[0, 10], [100, 50]]);
    }
}
