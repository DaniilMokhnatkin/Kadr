//! Blur that destroys detail first and looks good second. Visual hiding, not proof of secrecy.
//!
//! 1. Box-average into k×k cells on a seeded random grid offset: detail finer than a cell is gone.
//! 2. Monochrome noise per cell from the seed: matching candidates against the result stops working,
//!    and unlike per-channel noise it does not tint the result with coloured blotches.
//! 3. Cubic B-spline reconstruction: C²-smooth, so no squares, diamonds or Mach bands remain.
//! 4. A slight saturation lift and a fine grain, as in system materials: no banding on gradients.
//! 5. The edge feathers *outward*: inside the shape every pixel is fully replaced; only the
//!    surroundings blend, so the border is soft without revealing anything under the shape.
// Three colour channels are indexed in step across parallel arrays; ranges read clearest here.
#![allow(clippy::needless_range_loop)]
use crate::annotation::{Annotation, Tool, distance};
use crate::{Image, Point};

fn block(shape: &Annotation) -> usize {
    [16usize, 24, 40][shape.strength.min(2) as usize]
}

/// Soft edge width outside the shape, pixels.
pub fn feather(shape: &Annotation) -> i32 {
    block(shape) as i32 / 2
}

/// How far around `bounds()` the result reaches: callers pass a source this much larger.
pub fn reach(shape: &Annotation) -> i32 {
    feather(shape) + if shape.tool == Tool::BlurBrush { brush_radius(shape) as i32 } else { 0 }
}

fn brush_radius(shape: &Annotation) -> f64 {
    (shape.width * 4) as f64
}

struct Rng(u64);
impl Rng {
    fn new(seed: u64, stream: u64) -> Self {
        Self((seed ^ stream).wrapping_mul(0x9e3779b97f4a7c15).max(1))
    }
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    /// Uniform in [-1, 1).
    fn signed(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 23) as f32 - 1.0
    }
}

fn window(cell: usize, k: usize, offset: usize, len: usize) -> (usize, usize) {
    let start = (cell * k).saturating_sub(offset).min(len.saturating_sub(k));
    (start, (start + k).min(len))
}

/// Uniform cubic B-spline weights for the four samples around `t` ∈ [0, 1).
fn bspline(t: f32) -> [f32; 4] {
    let (t2, t3) = (t * t, t * t * t);
    let u = 1.0 - t;
    [u * u * u / 6.0, (3.0 * t3 - 6.0 * t2 + 4.0) / 6.0, (-3.0 * t3 + 3.0 * t2 + 3.0 * t + 1.0) / 6.0, t3 / 6.0]
}

/// Taps and weights along one axis: pixel → cell coordinates with the grid offset.
fn taps(len: usize, cells: usize, k: usize, offset: usize) -> Vec<([usize; 4], [f32; 4])> {
    (0..len)
        .map(|x| {
            let f = (x + offset) as f32 / k as f32 + 0.5 / k as f32 - 0.5;
            let i = f.floor();
            let w = bspline(f - i);
            let at = |d: i32| (i as i32 + d).clamp(0, cells as i32 - 1) as usize;
            ([at(-1), at(0), at(1), at(2)], w)
        })
        .collect()
}

pub fn blur_patch(source: &Image, shape: &Annotation) -> Image {
    blur_patch_masked(source, shape).0
}

/// Result plus its outward feather mask, so overlapping patches never restore source pixels.
pub fn blur_patch_masked(source: &Image, shape: &Annotation) -> (Image, Vec<f32>) {
    let k = block(shape);
    let w = source.width() as usize;
    let h = source.height() as usize;
    let mut cell_rng = Rng::new(shape.seed, 0x5eed_ce11);
    let (ox, oy) = ((cell_rng.next() % k as u64) as usize, (cell_rng.next() % k as u64) as usize);
    let gw = (w + ox).div_ceil(k);
    let gh = (h + oy).div_ceil(k);

    // 1–2. Cell averages plus one shared luminance offset per cell.
    let amplitude = [3.0f32, 4.0, 6.0][shape.strength.min(2) as usize];
    let mut grid = vec![[0.0f32; 3]; gw * gh];
    for gy in 0..gh {
        for gx in 0..gw {
            // Every cell averages a full k-wide window, shifted inside at the borders: a sliver cell
            // of one or two pixels would otherwise keep those pixels almost intact.
            let (y0, y1) = window(gy, k, oy, h);
            let (x0, x1) = window(gx, k, ox, w);
            let mut sums = [0u64; 3];
            for y in y0..y1 {
                for px in source.bgra[(y * w + x0) * 4..(y * w + x1) * 4].as_chunks::<4>().0 {
                    for c in 0..3 {
                        sums[c] += px[c] as u64;
                    }
                }
            }
            let count = ((y1.saturating_sub(y0)) * (x1.saturating_sub(x0))).max(1) as f32;
            let noise = cell_rng.signed() * amplitude;
            grid[gy * gw + gx] = sums.map(|s| s as f32 / count + noise);
        }
    }

    // 3. Separable B-spline: grid rows to full width, then columns to full height.
    let xs = taps(w, gw, k, ox);
    let ys = taps(h, gh, k, oy);
    let mut rows = vec![[0.0f32; 3]; gh * w];
    for gy in 0..gh {
        for (x, (i, wt)) in xs.iter().enumerate() {
            let mut v = [0.0f32; 3];
            for j in 0..4 {
                let g = grid[gy * gw + i[j]];
                for c in 0..3 {
                    v[c] += g[c] * wt[j];
                }
            }
            rows[gy * w + x] = v;
        }
    }

    // 5. Distance outside the shape, for the outward feather.
    let f = feather(shape).max(1) as f32;
    let r = shape.rect();
    let brush = (shape.tool == Tool::BlurBrush).then(|| brush_distances(source, shape, f));
    let outside = |x: usize, y: usize| -> f32 {
        let (px, py) = (source.rect.left as f32 + x as f32 + 0.5, source.rect.top as f32 + y as f32 + 0.5);
        let rect_gap = || {
            let dx = (r.left as f32 - px).max(px - r.right as f32).max(0.0);
            let dy = (r.top as f32 - py).max(py - r.bottom as f32).max(0.0);
            dx.hypot(dy)
        };
        match shape.tool {
            Tool::BlurEllipse => {
                let (rx, ry) = (r.width().max(1) as f32 / 2.0, r.height().max(1) as f32 / 2.0);
                let (nx, ny) = ((px - r.left as f32 - rx) / rx, (py - r.top as f32 - ry) / ry);
                ((nx.hypot(ny) - 1.0) * rx.min(ry)).max(0.0)
            }
            Tool::BlurBrush => brush.as_ref().unwrap()[y * w + x],
            _ => rect_gap(),
        }
    };

    let spotlight = shape.tool == Tool::Spotlight;
    let mask: Vec<f32> = (0..h)
        .flat_map(|y| {
            let outside = &outside;
            (0..w).map(move |x| {
                let t = (outside(x, y) / f).clamp(0.0, 1.0);
                let alpha = 1.0 - t * t * (3.0 - 2.0 * t);
                if spotlight { 1.0 - alpha } else { alpha }
            })
        })
        .collect();

    // 4. Blend with saturation lift and grain. Rows are independent, so they spread over all cores;
    //    the grain is a hash of the pixel position, identical however the rows are split.
    let mut out = source.clone();
    let row_bytes = w * 4;
    // Small patches stay on one thread: starting workers costs more than they save.
    let threads =
        if w * h < 250_000 { 1 } else { std::thread::available_parallelism().map_or(1, |n| n.get()).clamp(1, 2) };
    let band = h.div_ceil(threads).max(16);
    std::thread::scope(|scope| {
        for (n, chunk) in out.bgra.chunks_mut(row_bytes * band).enumerate() {
            let (rows, ys, mask) = (&rows, &ys, &mask);
            scope.spawn(move || {
                for (dy, line) in chunk.chunks_mut(row_bytes).enumerate() {
                    let y = n * band + dy;
                    let (i, wt) = &ys[y];
                    let taps = i.map(|k| &rows[k * w..k * w + w]);
                    for x in 0..w {
                        let alpha = mask[y * w + x];
                        if alpha <= 0.0 {
                            continue;
                        }
                        let (a, b, c, d) = (taps[0][x], taps[1][x], taps[2][x], taps[3][x]);
                        let v: [f32; 3] =
                            std::array::from_fn(|ch| a[ch] * wt[0] + b[ch] * wt[1] + c[ch] * wt[2] + d[ch] * wt[3]);
                        let luma = 0.114 * v[0] + 0.587 * v[1] + 0.299 * v[2];
                        let dither = grain(shape.seed, (y * w + x) as u64) * 1.5;
                        let px = &mut line[x * 4..x * 4 + 4];
                        for c in 0..3 {
                            let blurred = luma + (v[c] - luma) * 1.08 + dither;
                            // `out` starts as a copy of the source, so the pixel still holds the original.
                            let o = px[c] as f32;
                            // +0.5 and truncation instead of round(): no libm call per channel.
                            px[c] = ((o + (blurred - o) * alpha).clamp(0.0, 255.0) + 0.5) as u8;
                        }
                        px[3] = 255;
                    }
                }
            });
        }
    });
    (out, mask)
}

/// Triangular grain in [-1, 1] from the pixel index: SplitMix64 of the seed and position.
fn grain(seed: u64, index: u64) -> f32 {
    let mut z = seed ^ 0x6a1a_0000 ^ index.wrapping_mul(0x9e3779b97f4a7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
    z ^= z >> 31;
    let a = (z & 0xffff) as f32 / 65535.0;
    let b = ((z >> 16) & 0xffff) as f32 / 65535.0;
    a + b - 1.0
}

/// Distance from each pixel to the brush stroke's edge (0 inside it), only near the stroke.
fn brush_distances(source: &Image, shape: &Annotation, feather: f32) -> Vec<f32> {
    let (w, h) = (source.width() as usize, source.height() as usize);
    let radius = brush_radius(shape);
    let reach = radius + feather as f64;
    let mut out = vec![f32::INFINITY; w * h];
    // Mouse samples arrive every pixel or two; overlapping segments would recompute the same
    // neighbourhood dozens of times. Points closer than a quarter of the radius add nothing visible.
    let spacing = (radius / 4.0).max(2.0);
    let mut points = vec![shape.points[0]];
    for p in &shape.points[1..] {
        let last = *points.last().unwrap();
        if (((p[0] - last[0]).pow(2) + (p[1] - last[1]).pow(2)) as f64).sqrt() >= spacing {
            points.push(*p);
        }
    }
    let end = *shape.points.last().unwrap();
    if *points.last().unwrap() != end || points.len() == 1 {
        points.push(end);
    }
    for segment in points.windows(2) {
        let [a, b] = [segment[0], segment[1]];
        let span = |lo: i32, hi: i32, origin: i32, len: usize| {
            ((lo as f64 - reach) as i32 - origin).clamp(0, len as i32) as usize
                ..((hi as f64 + reach) as i32 + 1 - origin).clamp(0, len as i32) as usize
        };
        for y in span(a[1].min(b[1]), a[1].max(b[1]), source.rect.top, h) {
            for x in span(a[0].min(b[0]), a[0].max(b[0]), source.rect.left, w) {
                let slot = &mut out[y * w + x];
                // Already inside the stroke: neighbouring segments cannot bring it closer.
                if *slot == 0.0 {
                    continue;
                }
                let p = Point::new(source.rect.left + x as i32, source.rect.top + y as i32);
                *slot = slot.min((distance(p, a, b) - radius).max(0.0) as f32);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::annotation::{Annotation, Tool};
    use crate::{Image, Point, Rect};
    fn checker(invert: bool) -> Image {
        let mut px = Vec::new();
        for y in 0..32 {
            for x in 0..32 {
                let c = if ((x + y) % 2 == 0) != invert { 0 } else { 254 };
                px.extend_from_slice(&[c, c, c, 255]);
            }
        }
        Image::new(Rect::new(0, 0, 32, 32), px)
    }
    fn blur(strength: u8, seed: u64) -> Annotation {
        let mut s = Annotation::new(1, Tool::Blur, Point::new(0, 0));
        s.points[1] = [31, 31];
        s.seed = seed;
        s.strength = strength;
        s
    }
    #[test]
    fn same_seed_same_result_and_spatial_details_are_lost() {
        let (a, b) = (checker(false), checker(true));
        let s = blur(2, 42);
        let pa = blur_patch(&a, &s);
        assert_eq!(pa, blur_patch(&a, &s));
        // Opposite fine patterns become the same smooth field.
        assert_eq!(pa, blur_patch(&b, &s));
        assert_ne!(pa, a);
        assert_ne!(pa, blur_patch(&a, &blur(2, 43)));
    }
    #[test]
    fn result_is_smooth_without_blocks_or_colour_blotches() {
        let mut px = Vec::new();
        let mut rng = Rng::new(7, 7);
        for _ in 0..96 * 96 {
            let v = (rng.next() % 256) as u8;
            px.extend_from_slice(&[v, v, v, 255]);
        }
        let image = Image::new(Rect::new(0, 0, 96, 96), px);
        let mut s = Annotation::new(1, Tool::Blur, Point::new(0, 0));
        s.points[1] = [95, 95];
        for strength in 0..3 {
            s.strength = strength;
            let out = blur_patch(&image, &s);
            let at = |x: usize, y: usize, c: usize| out.bgra[(y * 96 + x) * 4 + c] as i32;
            for y in 0..95 {
                for x in 0..95 {
                    // Neighbours differ only by gradient + grain; a block edge would jump.
                    assert!((at(x, y, 1) - at(x + 1, y, 1)).abs() <= 6, "step at {x},{y}");
                    assert!((at(x, y, 1) - at(x, y + 1, 1)).abs() <= 6, "step at {x},{y}");
                    // Grey in, grey out: the noise is luminance-only.
                    assert!((at(x, y, 0) - at(x, y, 2)).abs() <= 3);
                }
            }
        }
    }
    #[test]
    fn edge_feathers_outward_and_never_inward() {
        let source = checker(false);
        let mut s = Annotation::new(1, Tool::Blur, Point::new(8, 8));
        s.points[1] = [23, 23];
        s.seed = 5;
        s.strength = 0;
        let out = blur_patch(&source, &s);
        let at = |x: usize, y: usize| out.bgra[(y * 32 + x) * 4];
        let r = s.rect();
        // Inside, nothing of the 0/254 checkerboard survives.
        for y in r.top as usize..r.bottom as usize {
            for x in r.left as usize..r.right as usize {
                assert!((60..=200).contains(&at(x, y)), "{} at {x},{y}", at(x, y));
            }
        }
        // Just outside the edge the original is mixed in; far away it is untouched.
        let edge = r.left as usize - 1;
        assert!(at(edge, 16) != source.bgra[(16 * 32 + edge) * 4]);
        assert_eq!(at(0, 0), source.bgra[0]);
    }
}
