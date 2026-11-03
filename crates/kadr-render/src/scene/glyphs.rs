//! One 24-unit outline family, drawn at display resolution without icon fonts or raster assets.
//! Every outline is a real path geometry stroked with round caps and round joins, so corners and
//! curves stay clean at any scale instead of being butt-ended segments laid side by side.
use super::*;
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_FIGURE_BEGIN_FILLED, D2D1_FIGURE_BEGIN_HOLLOW, D2D1_FIGURE_END_CLOSED, D2D1_FIGURE_END_OPEN,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_CAP_STYLE_ROUND, D2D1_ELLIPSE, D2D1_LINE_JOIN_ROUND, D2D1_STROKE_STYLE_PROPERTIES, ID2D1Factory,
};

/// Stroke width in grid units.
const STROKE: f32 = 1.75;

struct Pen<'a> {
    gpu: &'a Gpu,
    factory: ID2D1Factory,
    style: Option<ID2D1StrokeStyle>,
    x: f32,
    y: f32,
    s: f32,
}

impl Pen<'_> {
    fn at(&self, (a, b): (f32, f32)) -> Vector2 {
        Vector2::new(self.x + a * self.s, self.y + b * self.s)
    }
    fn figure(&self, points: &[(f32, f32)], closed: bool, filled: bool) {
        let Some((first, rest)) = points.split_first() else { return };
        unsafe {
            let Ok(geometry) = self.factory.CreatePathGeometry() else { return };
            let Ok(sink) = geometry.Open() else { return };
            sink.BeginFigure(self.at(*first), if filled { D2D1_FIGURE_BEGIN_FILLED } else { D2D1_FIGURE_BEGIN_HOLLOW });
            let rest: Vec<Vector2> = rest.iter().map(|p| self.at(*p)).collect();
            sink.AddLines(&rest);
            sink.EndFigure(if closed { D2D1_FIGURE_END_CLOSED } else { D2D1_FIGURE_END_OPEN });
            if sink.Close().is_err() {
                return;
            }
            if filled {
                self.gpu.d2d.FillGeometry(&geometry, &self.gpu.brush, None);
            } else {
                self.gpu.d2d.DrawGeometry(&geometry, &self.gpu.brush, STROKE * self.s, self.style.as_ref());
            }
        }
    }
    /// An open polyline with joined corners.
    fn path(&self, points: &[(f32, f32)]) {
        self.figure(points, false, false);
    }
    fn shape(&self, points: &[(f32, f32)]) {
        self.figure(points, true, false);
    }
    fn fill(&self, points: &[(f32, f32)]) {
        self.figure(points, true, true);
    }
    fn line(&self, a: (f32, f32), b: (f32, f32)) {
        self.path(&[a, b]);
    }
    fn ellipse(&self, c: (f32, f32), rx: f32, ry: f32) {
        let e = D2D1_ELLIPSE { point: self.at(c), radiusX: rx * self.s, radiusY: ry * self.s };
        unsafe { self.gpu.d2d.DrawEllipse(&e, &self.gpu.brush, STROKE * self.s, self.style.as_ref()) };
    }
    fn dot(&self, c: (f32, f32), r: f32) {
        let e = D2D1_ELLIPSE { point: self.at(c), radiusX: r * self.s, radiusY: r * self.s };
        unsafe { self.gpu.d2d.FillEllipse(&e, &self.gpu.brush) };
    }
    fn rounded(&self, l: f32, t: f32, r: f32, b: f32, radius: f32) {
        let rr = D2D1_ROUNDED_RECT {
            rect: rect(self.x + l * self.s, self.y + t * self.s, self.x + r * self.s, self.y + b * self.s),
            radiusX: radius * self.s,
            radiusY: radius * self.s,
        };
        unsafe { self.gpu.d2d.DrawRoundedRectangle(&rr, &self.gpu.brush, STROKE * self.s, self.style.as_ref()) };
    }
}

/// Points along a quadratic Bézier, for smooth curves made of short segments.
fn quad(a: (f32, f32), c: (f32, f32), b: (f32, f32)) -> Vec<(f32, f32)> {
    (0..=16)
        .map(|i| {
            let t = i as f32 / 16.0;
            let u = 1.0 - t;
            (u * u * a.0 + 2.0 * u * t * c.0 + t * t * b.0, u * u * a.1 + 2.0 * u * t * c.1 + t * t * b.1)
        })
        .collect()
}

/// Points along a circular arc, angles in degrees, y pointing down.
fn arc(c: (f32, f32), r: f32, from: f32, to: f32) -> Vec<(f32, f32)> {
    let n = ((to - from).abs() / 10.0).ceil().max(2.0) as usize;
    (0..=n)
        .map(|i| {
            let a = (from + (to - from) * i as f32 / n as f32).to_radians();
            (c.0 + r * a.cos(), c.1 + r * a.sin())
        })
        .collect()
}

pub(super) fn draw(gpu: &Gpu, button: ToolbarButton, r: D2D_RECT_F, ink: D2D1_COLOR_F, scale: f32) {
    let Ok(factory) = (unsafe { gpu.d2d.GetFactory() }) else { return };
    let style = unsafe {
        factory.CreateStrokeStyle(
            &D2D1_STROKE_STYLE_PROPERTIES {
                startCap: D2D1_CAP_STYLE_ROUND,
                endCap: D2D1_CAP_STYLE_ROUND,
                dashCap: D2D1_CAP_STYLE_ROUND,
                lineJoin: D2D1_LINE_JOIN_ROUND,
                ..Default::default()
            },
            None,
        )
    }
    .ok();
    let p = Pen {
        gpu,
        factory,
        style,
        x: (r.left + r.right) / 2.0 - 12.0 * scale,
        y: (r.top + r.bottom) / 2.0 - 12.0 * scale,
        s: scale,
    };
    unsafe {
        gpu.brush.SetColor(&ink);
    }
    let dots = || {
        for a in [8.5, 12.0, 15.5] {
            for b in [8.5, 12.0, 15.5] {
                p.dot((a, b), 0.95);
            }
        }
    };
    match button {
        ToolbarButton::Menu(BarMenu::More) => {
            for x in [5.0, 12.0, 19.0] {
                p.dot((x, 12.0), 1.5);
            }
        }
        ToolbarButton::Menu(BarMenu::Shapes | BarMenu::Blur) | ToolbarButton::RecentPage(_) => {
            p.path(&[(8.0, 10.0), (12.0, 14.0), (16.0, 10.0)]);
        }
        ToolbarButton::Original => {
            p.shape(&[(2.5, 12.0), (7.0, 7.0), (17.0, 7.0), (21.5, 12.0), (17.0, 17.0), (7.0, 17.0)]);
            p.ellipse((12.0, 12.0), 3.0, 3.0);
        }
        ToolbarButton::About => {
            p.ellipse((12.0, 12.0), 8.5, 8.5);
            p.line((12.0, 11.0), (12.0, 17.0));
            p.dot((12.0, 7.0), 0.8);
        }
        ToolbarButton::Tool(tool) => match tool {
            Tool::Select => p.shape(&[
                (6.5, 3.5),
                (6.5, 19.0),
                (10.3, 15.3),
                (13.0, 21.0),
                (15.6, 19.8),
                (13.0, 14.2),
                (18.2, 14.0),
            ]),
            // A shaft and an open head two fifths of its length, at a right angle.
            Tool::Arrow => {
                p.line((5.5, 18.5), (17.8, 6.2));
                p.path(&[(10.8, 6.0), (18.0, 6.0), (18.0, 13.2)]);
            }
            Tool::Line => p.line((5.5, 18.5), (18.5, 5.5)),
            Tool::Rectangle => p.rounded(4.5, 6.0, 19.5, 18.0, 2.0),
            Tool::Ellipse => p.ellipse((12.0, 12.0), 8.0, 8.0),
            Tool::Lens => {
                p.ellipse((10.0, 10.0), 6.5, 6.5);
                p.line((15.0, 15.0), (20.0, 20.0));
                p.line((7.0, 10.0), (13.0, 10.0));
                p.line((10.0, 7.0), (10.0, 13.0));
            }
            Tool::Pen => {
                p.shape(&[(4.5, 19.5), (5.6, 14.6), (15.6, 4.6), (19.4, 8.4), (9.4, 18.4)]);
                p.line((13.6, 6.6), (17.4, 10.4));
            }
            Tool::Eraser | Tool::Marker => {
                p.shape(&[(5.0, 16.5), (14.0, 5.0), (19.5, 9.5), (10.0, 20.0)]);
                p.line((7.0, 14.0), (12.5, 18.0));
                p.line((4.0, 21.5), (12.0, 21.5));
            }
            Tool::Text => {
                p.path(&[(5.0, 7.5), (5.0, 4.5), (19.0, 4.5), (19.0, 7.5)]);
                p.line((12.0, 4.5), (12.0, 19.5));
                p.line((9.0, 19.5), (15.0, 19.5));
            }
            // A numbered badge: a ring with an optically centred «1».
            Tool::Counter => {
                p.ellipse((12.0, 12.0), 8.5, 8.5);
                p.path(&[(10.0, 9.4), (12.8, 7.6), (12.8, 16.4)]);
            }
            Tool::Blur => {
                p.rounded(4.0, 4.0, 20.0, 20.0, 2.5);
                dots();
            }
            Tool::BlurEllipse => {
                p.ellipse((12.0, 12.0), 9.0, 8.0);
                dots();
            }
            Tool::BlurBrush => {
                p.path(&quad((6.0, 18.0), (11.0, 9.0), (19.0, 4.5)));
                p.ellipse((7.0, 17.0), 3.2, 3.2);
            }
            Tool::Spotlight => {
                p.rounded(4.0, 4.0, 20.0, 20.0, 2.5);
                p.rounded(9.0, 9.0, 15.0, 15.0, 1.5);
            }
            Tool::Redact => {
                p.rounded(4.0, 5.0, 20.0, 19.0, 2.0);
                p.fill(&[(4.0, 10.0), (20.0, 10.0), (20.0, 14.0), (4.0, 14.0)]);
            }
        },
        ToolbarButton::Copy => {
            p.rounded(8.5, 8.5, 20.0, 20.0, 2.0);
            p.path(&[(15.5, 5.5), (15.5, 4.0), (4.0, 4.0), (4.0, 15.5), (5.5, 15.5)]);
        }
        ToolbarButton::Save => {
            p.line((12.0, 3.5), (12.0, 14.5));
            p.path(&[(7.5, 10.0), (12.0, 14.5), (16.5, 10.0)]);
            p.path(&[(4.5, 14.5), (4.5, 19.5), (19.5, 19.5), (19.5, 14.5)]);
        }
        ToolbarButton::SaveAs => {
            p.shape(&[(4.0, 18.5), (4.0, 6.0), (9.5, 6.0), (11.5, 8.5), (20.0, 8.5), (20.0, 18.5)]);
            p.line((8.5, 13.5), (15.5, 13.5));
            p.path(&[(12.8, 10.8), (15.5, 13.5), (12.8, 16.2)]);
        }
        // An upright push pin: a cap, a flared collar, and a straight needle.
        ToolbarButton::Pin => {
            p.shape(&[(8.5, 3.5), (15.5, 3.5), (14.2, 9.5), (17.5, 13.5), (6.5, 13.5), (9.8, 9.5)]);
            p.line((12.0, 13.5), (12.0, 20.5));
        }
        ToolbarButton::Recent => {
            let mut back = arc((12.5, 12.5), 8.0, 200.0, 520.0);
            back.reverse();
            p.path(&back);
            p.path(&[(3.0, 7.5), (5.0, 10.0), (8.0, 8.5)]);
            p.path(&[(12.5, 8.0), (12.5, 12.5), (15.5, 14.5)]);
        }
        // An eye under a stroke: «hide what should not be seen».
        ToolbarButton::Smart => {
            p.shape(&[
                (12.0, 3.0),
                (20.0, 6.5),
                (19.0, 14.0),
                (16.0, 18.0),
                (12.0, 21.0),
                (8.0, 18.0),
                (5.0, 14.0),
                (4.0, 6.5),
            ]);
            p.line((8.0, 11.0), (16.0, 11.0));
        }
        // Scan corners around a T: «text from the picture».
        ToolbarButton::Text => {
            p.path(&[(3.5, 8.0), (3.5, 3.5), (8.0, 3.5)]);
            p.path(&[(16.0, 3.5), (20.5, 3.5), (20.5, 8.0)]);
            p.path(&[(3.5, 16.0), (3.5, 20.5), (8.0, 20.5)]);
            p.path(&[(16.0, 20.5), (20.5, 20.5), (20.5, 16.0)]);
            p.line((8.5, 8.5), (15.5, 8.5));
            p.line((12.0, 8.5), (12.0, 16.0));
        }
        // A Latin A beside a CJK-like character, both about as wide as they are tall.
        ToolbarButton::Translate => {
            p.path(&[(3.0, 18.0), (7.0, 8.5), (11.0, 18.0)]);
            p.line((4.6, 14.5), (9.4, 14.5));
            p.line((17.25, 5.0), (17.25, 7.5));
            p.line((13.0, 7.5), (21.5, 7.5));
            p.line((14.0, 10.5), (20.5, 18.0));
            p.line((20.5, 10.5), (14.0, 18.0));
        }
        // A record button: a ring with a solid dot.
        ToolbarButton::Record => {
            p.ellipse((12.0, 12.0), 8.5, 8.5);
            p.dot((12.0, 12.0), 4.2);
        }
        ToolbarButton::Close => {
            p.line((6.5, 6.5), (17.5, 17.5));
            p.line((17.5, 6.5), (6.5, 17.5));
        }
        ToolbarButton::Crop => {
            p.path(&[(7.0, 3.0), (7.0, 17.0), (21.0, 17.0)]);
            p.path(&[(3.0, 7.0), (17.0, 7.0), (17.0, 21.0)]);
        }
        ToolbarButton::Undo => {
            p.path(&[(8.0, 4.5), (3.5, 9.0), (8.0, 13.5)]);
            let mut curve = vec![(3.5, 9.0), (14.0, 9.0)];
            curve.extend(arc((14.0, 14.0), 5.0, -90.0, 90.0).into_iter().skip(1));
            curve.push((10.5, 19.0));
            p.path(&curve);
        }
        ToolbarButton::Redo => {
            p.path(&[(16.0, 4.5), (20.5, 9.0), (16.0, 13.5)]);
            let mut curve = vec![(20.5, 9.0), (10.0, 9.0)];
            curve.extend(arc((10.0, 14.0), 5.0, 270.0, 90.0).into_iter().skip(1));
            curve.push((13.5, 19.0));
            p.path(&curve);
        }
        _ => {}
    }
}
