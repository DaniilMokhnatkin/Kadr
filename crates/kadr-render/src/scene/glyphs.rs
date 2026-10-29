//! One 24-unit outline family, drawn at display resolution without icon fonts or raster assets.
use super::*;
use windows::Win32::Graphics::Direct2D::D2D1_ELLIPSE;

pub(super) fn draw(gpu: &Gpu, button: ToolbarButton, r: D2D_RECT_F, ink: D2D1_COLOR_F, scale: f32) {
    let x = (r.left + r.right) / 2.0 - 12.0 * scale;
    let y = (r.top + r.bottom) / 2.0 - 12.0 * scale;
    let point = |a: f32, b: f32| Vector2::new(x + a * scale, y + b * scale);
    unsafe {
        gpu.brush.SetColor(&ink);
    }
    let line = |a: f32, b: f32, c: f32, d: f32| unsafe {
        gpu.d2d.DrawLine(
            point(a, b),
            point(c, d),
            &gpu.brush,
            1.7 * scale,
            None::<&ID2D1StrokeStyle>,
        );
    };
    let path = |points: &[(f32, f32)]| {
        for p in points.windows(2) {
            line(p[0].0, p[0].1, p[1].0, p[1].1);
        }
    };
    let ellipse = |a: f32, b: f32, rx: f32, ry: f32| unsafe {
        gpu.d2d.DrawEllipse(
            &D2D1_ELLIPSE {
                point: point(a, b),
                radiusX: rx * scale,
                radiusY: ry * scale,
            },
            &gpu.brush,
            1.7 * scale,
            None::<&ID2D1StrokeStyle>,
        );
    };
    let box_ = |a: f32, b: f32, c: f32, d: f32| path(&[(a, b), (c, b), (c, d), (a, d), (a, b)]);
    let dots = || {
        for a in [8.0, 12.0, 16.0] {
            for b in [8.0, 12.0, 16.0] {
                unsafe {
                    gpu.d2d.FillEllipse(
                        &D2D1_ELLIPSE {
                            point: point(a, b),
                            radiusX: 0.9 * scale,
                            radiusY: 0.9 * scale,
                        },
                        &gpu.brush,
                    );
                }
            }
        }
    };
    match button {
        ToolbarButton::Tool(tool) => match tool {
            Tool::Select => path(&[
                (5.0, 3.0),
                (5.0, 19.0),
                (9.0, 15.0),
                (12.0, 21.0),
                (15.0, 19.5),
                (12.0, 13.5),
                (18.0, 13.0),
                (5.0, 3.0),
            ]),
            Tool::Arrow => {
                line(5.0, 19.0, 19.0, 5.0);
                path(&[(10.0, 5.0), (19.0, 5.0), (19.0, 14.0)]);
            }
            Tool::Line => line(5.0, 19.0, 19.0, 5.0),
            Tool::Rectangle => box_(4.5, 5.5, 19.5, 18.5),
            Tool::Ellipse => ellipse(12.0, 12.0, 8.0, 6.5),
            Tool::Pen => {
                path(&[
                    (4.0, 20.0),
                    (5.5, 14.0),
                    (16.0, 3.5),
                    (20.5, 8.0),
                    (10.0, 18.5),
                    (4.0, 20.0),
                ]);
                line(14.0, 6.0, 18.0, 10.0);
            }
            Tool::Marker => {
                path(&[(4.0, 17.0), (14.0, 4.0), (20.0, 9.0), (10.0, 20.0), (4.0, 17.0)]);
                line(3.0, 22.0, 13.0, 22.0);
                line(6.5, 14.0, 12.0, 18.0);
            }
            Tool::Text => {
                path(&[(4.0, 7.0), (4.0, 4.0), (20.0, 4.0), (20.0, 7.0)]);
                line(12.0, 4.0, 12.0, 20.0);
                line(8.0, 20.0, 16.0, 20.0);
            }
            Tool::Counter => {
                ellipse(12.0, 12.0, 8.5, 8.5);
                path(&[(9.0, 10.0), (12.0, 7.0), (12.0, 17.0)]);
                line(9.0, 17.0, 15.0, 17.0);
            }
            Tool::Blur => {
                box_(3.5, 3.5, 20.5, 20.5);
                dots();
            }
            Tool::BlurEllipse => {
                ellipse(12.0, 12.0, 9.0, 8.0);
                dots();
            }
            Tool::BlurBrush => {
                path(&[(5.0, 18.0), (10.0, 14.0), (13.0, 7.0), (18.0, 4.0)]);
                ellipse(7.0, 17.0, 3.5, 3.5);
                line(14.0, 7.0, 18.0, 11.0);
            }
            Tool::Spotlight => {
                box_(4.0, 4.0, 20.0, 20.0);
                box_(9.0, 9.0, 15.0, 15.0);
                line(4.0, 8.0, 7.0, 8.0);
                line(17.0, 16.0, 20.0, 16.0);
            }
            Tool::Redact => {
                box_(4.0, 5.0, 20.0, 19.0);
                unsafe {
                    gpu.d2d.FillRectangle(
                        &rect(x + 4.0 * scale, y + 10.0 * scale, x + 20.0 * scale, y + 14.0 * scale),
                        &gpu.brush,
                    );
                }
            }
        },
        ToolbarButton::Copy => {
            box_(8.0, 8.0, 20.0, 20.0);
            path(&[(16.0, 5.0), (16.0, 3.0), (3.0, 3.0), (3.0, 16.0), (5.0, 16.0)]);
        }
        ToolbarButton::Save => {
            line(12.0, 3.0, 12.0, 15.0);
            path(&[(7.0, 10.0), (12.0, 15.0), (17.0, 10.0)]);
            path(&[(4.0, 14.0), (4.0, 20.0), (20.0, 20.0), (20.0, 14.0)]);
        }
        ToolbarButton::SaveAs => {
            path(&[
                (4.0, 19.0),
                (4.0, 6.0),
                (10.0, 6.0),
                (12.0, 9.0),
                (20.0, 9.0),
                (20.0, 19.0),
                (4.0, 19.0),
            ]);
            line(8.0, 14.0, 16.0, 14.0);
            path(&[(13.0, 11.0), (16.0, 14.0), (13.0, 17.0)]);
        }
        ToolbarButton::Pin => {
            path(&[
                (9.0, 3.0),
                (19.0, 3.0),
                (17.0, 10.0),
                (20.0, 14.0),
                (10.0, 14.0),
                (7.0, 11.0),
                (10.0, 9.0),
                (9.0, 3.0),
            ]);
            line(11.0, 14.0, 5.0, 21.0);
        }
        ToolbarButton::Recent => {
            path(&[(4.0, 10.0), (4.0, 4.0), (4.0, 10.0), (10.0, 10.0)]);
            ellipse(13.0, 13.0, 8.0, 8.0);
            path(&[(13.0, 8.0), (13.0, 13.0), (17.0, 15.0)]);
        }
        // Scan corners around a T: «text from the picture».
        ToolbarButton::Text => {
            path(&[(3.0, 8.0), (3.0, 3.0), (8.0, 3.0)]);
            path(&[(16.0, 3.0), (21.0, 3.0), (21.0, 8.0)]);
            path(&[(3.0, 16.0), (3.0, 21.0), (8.0, 21.0)]);
            path(&[(16.0, 21.0), (21.0, 21.0), (21.0, 16.0)]);
            line(8.0, 8.0, 16.0, 8.0);
            line(12.0, 8.0, 12.0, 17.0);
        }
        // A Latin A beside a CJK-like character: the usual translate mark.
        ToolbarButton::Translate => {
            path(&[(2.5, 20.0), (7.0, 7.0), (11.5, 20.0)]);
            line(4.4, 15.0, 9.6, 15.0);
            line(13.0, 6.0, 21.5, 6.0);
            line(17.25, 3.0, 17.25, 6.0);
            line(14.0, 9.0, 21.0, 20.0);
            line(21.0, 9.0, 14.0, 20.0);
        }
        ToolbarButton::Close => {
            line(6.0, 6.0, 18.0, 18.0);
            line(18.0, 6.0, 6.0, 18.0);
        }
        ToolbarButton::Undo => {
            path(&[(8.0, 4.0), (3.0, 9.0), (8.0, 14.0)]);
            path(&[
                (3.0, 9.0),
                (15.0, 9.0),
                (19.0, 11.0),
                (20.0, 14.0),
                (19.0, 17.0),
                (16.0, 19.0),
                (11.0, 19.0),
            ]);
        }
        ToolbarButton::Redo => {
            path(&[(16.0, 4.0), (21.0, 9.0), (16.0, 14.0)]);
            path(&[
                (21.0, 9.0),
                (9.0, 9.0),
                (5.0, 11.0),
                (4.0, 14.0),
                (5.0, 17.0),
                (8.0, 19.0),
                (13.0, 19.0),
            ]);
        }
        _ => {}
    }
}
