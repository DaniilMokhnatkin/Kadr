//! Long screenshot: frames of the same area taken while the page scrolls are glued into one tall
//! image. Each new frame is matched against the previous one by row fingerprints; rows that do
//! not move (a sticky header, a footer) are recognised and kept only once. The scrollbar strip at
//! the right edge is ignored, since it moves while the page scrolls.
use crate::{Image, Rect};

/// Columns ignored on the right (scrollbars) and on the left (focus rings, gutters).
const RIGHT_STRIP: usize = 24;
const LEFT_STRIP: usize = 4;
/// Tallest result, in pixels.
pub const MAX_HEIGHT: usize = 30_000;

pub enum Step {
    /// Nothing moved.
    Same,
    /// The page moved down by this many rows; the result grew by as much.
    Grew(usize),
    /// Moved too far or sideways since the last frame: no overlap to glue on.
    Lost,
    /// Moved up: already captured.
    Back,
    /// The result reached `MAX_HEIGHT`.
    Full,
}

pub struct Stitcher {
    width: usize,
    /// Body rows (header + everything scrolled so far), BGRA.
    body: Vec<u8>,
    last: Vec<u8>,
    last_rows: Vec<u64>,
    /// Rows at the bottom that stayed put between frames: appended once at the end.
    footer: usize,
    height: usize,
    /// Nothing glued yet: the first frame may still be stale.
    fresh: bool,
    /// The last frame that matched nothing, while nothing is glued yet.
    unmatched: Option<Image>,
}

fn fingerprints(bgra: &[u8], width: usize) -> Vec<u64> {
    let stride = width * 4;
    let (from, to) = (LEFT_STRIP.min(width / 4), width.saturating_sub(RIGHT_STRIP).max(width / 2));
    bgra.chunks_exact(stride)
        .map(|row| {
            // FNV-1a over the row, minus the edge strips.
            let mut h = 0xcbf29ce484222325u64;
            for &b in &row[from * 4..to * 4] {
                h = (h ^ b as u64).wrapping_mul(0x100000001b3);
            }
            h
        })
        .collect()
}

impl Stitcher {
    pub fn new(first: &Image) -> Self {
        let width = first.width() as usize;
        Self {
            width,
            body: first.bgra.clone(),
            last: first.bgra.clone(),
            last_rows: fingerprints(&first.bgra, width),
            footer: 0,
            height: first.height() as usize,
            fresh: true,
            unmatched: None,
        }
    }

    /// Height of the result so far.
    pub fn height(&self) -> usize {
        self.height
    }

    pub fn push(&mut self, frame: &Image) -> Step {
        let stride = self.width * 4;
        if frame.width() as usize != self.width || frame.bgra.len() != self.last.len() {
            return Step::Lost;
        }
        let rows = fingerprints(&frame.bgra, self.width);
        let h = rows.len();
        if rows == self.last_rows {
            return Step::Same;
        }
        // Rows that did not move at the top and bottom: header and footer.
        let top = rows.iter().zip(&self.last_rows).take_while(|(a, b)| a == b).count();
        let bottom = rows.iter().rev().zip(self.last_rows.iter().rev()).take_while(|(a, b)| a == b).count();
        let (top, bottom) = if top + bottom >= h { (0, 0) } else { (top, bottom) };
        let band = h - top - bottom;
        let best = |old: &[u64], new: &[u64]| -> Option<usize> {
            // Shift d: new[i] == old[i + d]. Needs a real overlap, not a coincidence.
            let mut best: Option<(usize, f32)> = None;
            for d in 1..band.saturating_sub(band / 6).max(1) {
                let overlap = band - d;
                let same = (0..overlap).filter(|&i| new[i] == old[i + d]).count();
                let ratio = same as f32 / overlap as f32;
                if ratio >= 0.9 && best.is_none_or(|(_, r)| ratio > r + 0.001) {
                    best = Some((d, ratio));
                }
            }
            best.map(|(d, _)| d)
        };
        let old = &self.last_rows[top..h - bottom];
        let new = &rows[top..h - bottom];
        let step = match best(old, new) {
            Some(d) => {
                if self.height + d > MAX_HEIGHT {
                    return Step::Full;
                }
                // Revealed rows sit just above the footer; the body never contains the footer.
                if self.footer == 0 && bottom > 0 {
                    self.body.truncate((self.height - bottom) * stride);
                    self.height -= bottom;
                } else if bottom < self.footer {
                    // The footer shrank: put back what is no longer fixed.
                    let start = (h - self.footer) * stride;
                    self.body.extend_from_slice(&self.last[start..(h - bottom) * stride]);
                    self.height += self.footer - bottom;
                }
                self.footer = bottom;
                let from = (h - bottom - d) * stride;
                self.body.extend_from_slice(&frame.bgra[from..(h - bottom) * stride]);
                self.height += d;
                self.fresh = false;
                Step::Grew(d)
            }
            // The reference stays the lowest frame reached, or rows would be glued twice.
            None if best(new, old).is_some() => return Step::Back,
            None => {
                // Nothing glued yet and the first frame matches nothing: it may have been caught
                // mid-change. If this frame follows the previous unmatched one (still or scrolled),
                // that one is the real start.
                if self.fresh
                    && let Some(prev) = self.unmatched.take()
                {
                    let mut restart = Self::new(&prev);
                    let step = restart.push(frame);
                    if !matches!(step, Step::Lost) {
                        *self = restart;
                        return step;
                    }
                }
                if self.fresh {
                    self.unmatched = Some(frame.clone());
                }
                return Step::Lost;
            }
        };
        self.last.copy_from_slice(&frame.bgra);
        self.last_rows = rows;
        self.unmatched = None;
        step
    }

    /// The tall image: body, then the footer as it was last seen.
    pub fn finish(mut self) -> Image {
        let stride = self.width * 4;
        let h = self.last.len() / stride;
        if self.footer > 0 {
            self.body.extend_from_slice(&self.last[(h - self.footer) * stride..]);
        }
        let height = self.body.len() / stride;
        Image::new(Rect::new(0, 0, self.width as i32, height as i32), self.body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    /// A page of distinct rows; the window shows `h` rows from `top`, with fixed header/footer.
    fn view(page: &[u8], top: usize, h: usize, header: usize, footer: usize, w: usize) -> Image {
        let mut px = Vec::with_capacity(w * h * 4);
        for y in 0..h {
            let value = if y < header {
                200
            } else if y >= h - footer {
                100
            } else {
                page[top + y - header]
            };
            for x in 0..w {
                // A scrollbar thumb in the right strip that moves with the scroll position.
                let thumb = x >= w - 10 && (top / 4..top / 4 + 8).contains(&y);
                let v = if thumb { 255 } else { value.wrapping_add((x % 3) as u8) };
                px.extend_from_slice(&[v, v, v, 255]);
            }
        }
        Image::new(Rect::new(0, 0, w as i32, h as i32), px)
    }
    fn page(n: usize) -> Vec<u8> {
        // Pseudo-random but deterministic, so no two rows look alike by accident.
        (0..n).map(|i| ((i * 7919 + 13) % 251) as u8).collect()
    }
    fn column(img: &Image) -> Vec<u8> {
        img.bgra.chunks_exact(img.width() as usize * 4).map(|r| r[0]).collect()
    }
    #[test]
    fn scrolling_glues_the_page_once_with_header_and_footer_kept_once() {
        let (w, h, header, footer) = (64, 60, 6, 5);
        let content = page(400);
        let visible = h - header - footer;
        let mut s = Stitcher::new(&view(&content, 0, h, header, footer, w));
        let mut top = 0;
        for step in [7, 13, 0, 21, 9, 30, 11] {
            top += step;
            let r = s.push(&view(&content, top, h, header, footer, w));
            match step {
                0 => assert!(matches!(r, Step::Same)),
                _ => assert!(matches!(r, Step::Grew(d) if d == step), "step {step}"),
            }
        }
        let out = s.finish();
        let rows = column(&out);
        assert_eq!(rows.len(), header + top + visible + footer);
        assert!(rows[..header].iter().all(|&v| v == 200));
        assert!(rows[rows.len() - footer..].iter().all(|&v| v == 100));
        assert_eq!(&rows[header..rows.len() - footer], &content[..top + visible]);
    }
    #[test]
    fn a_jump_without_overlap_and_scrolling_back_do_not_corrupt_the_result() {
        let (w, h) = (48, 50);
        let content = page(500);
        let mut s = Stitcher::new(&view(&content, 0, h, 0, 0, w));
        assert!(matches!(s.push(&view(&content, 200, h, 0, 0, w)), Step::Lost));
        assert!(matches!(s.push(&view(&content, 10, h, 0, 0, w)), Step::Grew(10)));
        assert!(matches!(s.push(&view(&content, 4, h, 0, 0, w)), Step::Back));
        assert!(matches!(s.push(&view(&content, 20, h, 0, 0, w)), Step::Grew(10)));
        let rows = column(&s.finish());
        assert_eq!(rows, content[..20 + h]);
    }
    #[test]
    fn a_first_frame_caught_mid_fade_is_replaced_once_the_screen_holds_still() {
        let (w, h) = (48, 50);
        let content = page(500);
        // The editor's last faded frame: every pixel a shade off.
        let mut stale = view(&content, 0, h, 0, 0, w);
        stale.bgra.iter_mut().for_each(|b| *b = b.saturating_sub(3));
        let mut s = Stitcher::new(&stale);
        assert!(matches!(s.push(&view(&content, 0, h, 0, 0, w)), Step::Lost));
        assert!(matches!(s.push(&view(&content, 0, h, 0, 0, w)), Step::Same));
        assert!(matches!(s.push(&view(&content, 15, h, 0, 0, w)), Step::Grew(15)));
        assert!(matches!(s.push(&view(&content, 32, h, 0, 0, w)), Step::Grew(17)));
        assert_eq!(column(&s.finish()), content[..32 + h]);
    }
    #[test]
    fn a_stale_first_frame_is_replaced_even_when_scrolling_starts_at_once() {
        let (w, h) = (48, 50);
        let content = page(500);
        let mut stale = view(&content, 0, h, 0, 0, w);
        stale.bgra.iter_mut().for_each(|b| *b = b.saturating_add(2));
        let mut s = Stitcher::new(&stale);
        assert!(matches!(s.push(&view(&content, 10, h, 0, 0, w)), Step::Lost));
        assert!(matches!(s.push(&view(&content, 22, h, 0, 0, w)), Step::Grew(12)));
        assert!(matches!(s.push(&view(&content, 40, h, 0, 0, w)), Step::Grew(18)));
        assert_eq!(column(&s.finish()), content[10..40 + h]);
    }
}
