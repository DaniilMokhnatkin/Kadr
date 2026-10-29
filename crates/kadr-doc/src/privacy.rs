//! Уменьшение с потерей деталей и детерминированный шум. Это визуальное скрытие, не доказательство секретности.
use crate::annotation::{Annotation, Tool, distance};
use crate::{Image, Point};
pub fn blur_patch(source: &Image, shape: &Annotation) -> Image {
    let k = [16usize, 24, 40][shape.strength.min(2) as usize];
    let w = source.width() as usize;
    let h = source.height() as usize;
    let gw = w.div_ceil(k);
    let gh = h.div_ceil(k);
    let mut grid = vec![[0.0f64; 3]; gw * gh];
    let mut random = shape.seed.wrapping_add(0x9e3779b97f4a7c15).max(1);
    for gy in 0..gh {
        for gx in 0..gw {
            let mut sums = [0u64; 3];
            let mut count = 0;
            for y in gy * k..((gy + 1) * k).min(h) {
                for x in gx * k..((gx + 1) * k).min(w) {
                    let offset = (y * w + x) * 4;
                    for (c, sum) in sums.iter_mut().enumerate() {
                        *sum += source.bgra[offset + c] as u64;
                    }
                    count += 1;
                }
            }
            for (c, sum) in sums.iter().enumerate() {
                random ^= random << 13;
                random ^= random >> 7;
                random ^= random << 17;
                let noise = (random % 17) as f64 - 8.0;
                grid[gy * gw + gx][c] = (*sum as f64 / count.max(1) as f64 + noise).clamp(0.0, 255.0);
            }
        }
    }
    let mut out = source.clone();
    let r = shape.rect();
    // Растеризуем только окрестности сегментов: стоимость не area × число точек.
    let brush_mask = if shape.tool == Tool::BlurBrush {
        let mut mask = vec![false; w * h];
        let radius = (shape.width * 4) as i32;
        for segment in shape.points.windows(2) {
            let [a, b] = [segment[0], segment[1]];
            let left = (a[0].min(b[0]) - radius - source.rect.left).clamp(0, w as i32);
            let right = (a[0].max(b[0]) + radius + 1 - source.rect.left).clamp(0, w as i32);
            let top = (a[1].min(b[1]) - radius - source.rect.top).clamp(0, h as i32);
            let bottom = (a[1].max(b[1]) + radius + 1 - source.rect.top).clamp(0, h as i32);
            for y in top..bottom {
                for x in left..right {
                    let i = y as usize * w + x as usize;
                    if !mask[i]
                        && distance(Point::new(source.rect.left + x, source.rect.top + y), a, b) <= radius as f64
                    {
                        mask[i] = true;
                    }
                }
            }
        }
        Some(mask)
    } else {
        None
    };
    for y in 0..h {
        for x in 0..w {
            let p = Point::new(source.rect.left + x as i32, source.rect.top + y as i32);
            let masked = match shape.tool {
                Tool::Spotlight => !r.contains(p),
                Tool::BlurEllipse => {
                    let cx = (r.left + r.right) as f64 / 2.0;
                    let cy = (r.top + r.bottom) as f64 / 2.0;
                    ((p.x as f64 - cx) / (r.width().max(1) as f64 / 2.0)).powi(2)
                        + ((p.y as f64 - cy) / (r.height().max(1) as f64 / 2.0)).powi(2)
                        <= 1.0
                }
                Tool::BlurBrush => brush_mask.as_ref().unwrap()[y * w + x],
                _ => r.contains(p),
            };
            if !masked {
                continue;
            }
            let fx = ((x as f64 + 0.5) / k as f64 - 0.5).clamp(0.0, (gw - 1) as f64);
            let fy = ((y as f64 + 0.5) / k as f64 - 0.5).clamp(0.0, (gh - 1) as f64);
            let ix = fx.floor() as usize;
            let iy = fy.floor() as usize;
            let tx = fx - ix as f64;
            let ty = fy - iy as f64;
            for (c, channel) in out.bgra[(y * w + x) * 4..(y * w + x) * 4 + 3].iter_mut().enumerate() {
                let top = grid[iy * gw + ix][c] * (1.0 - tx) + grid[iy * gw + (ix + 1).min(gw - 1)][c] * tx;
                let bottom = grid[(iy + 1).min(gh - 1) * gw + ix][c] * (1.0 - tx)
                    + grid[(iy + 1).min(gh - 1) * gw + (ix + 1).min(gw - 1)][c] * tx;
                *channel = (top * (1.0 - ty) + bottom * ty).round() as u8;
            }
            out.bgra[(y * w + x) * 4 + 3] = 255;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::annotation::{Annotation, Tool};
    use crate::{Image, Point, Rect};
    #[test]
    fn same_seed_same_result_and_spatial_details_are_lost() {
        let mut a = Vec::new();
        let mut b = Vec::new();
        for y in 0..32 {
            for x in 0..32 {
                let c = if (x + y) % 2 == 0 { 0 } else { 254 };
                a.extend_from_slice(&[c, c, c, 255]);
                b.extend_from_slice(&[254 - c, 254 - c, 254 - c, 255]);
            }
        }
        let a = Image::new(Rect::new(0, 0, 32, 32), a);
        let b = Image::new(a.rect, b);
        let mut s = Annotation::new(1, Tool::Blur, Point::new(0, 0));
        s.points[1] = [31, 31];
        s.seed = 42;
        s.strength = 2;
        let pa = blur_patch(&a, &s);
        assert_eq!(pa, blur_patch(&a, &s));
        assert_eq!(pa, blur_patch(&b, &s));
        assert_ne!(pa, a);
        s.seed = 43;
        assert_ne!(pa, blur_patch(&a, &s));
    }
}
