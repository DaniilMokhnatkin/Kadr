use crate::geometry::{Point, Rect};

/// Пиксели BGRA (8 бит на канал, строки подряд без выравнивания), привязанные к месту на рабочем столе.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    pub rect: Rect,
    pub bgra: Vec<u8>,
}

impl Image {
    pub fn new(rect: Rect, bgra: Vec<u8>) -> Self {
        assert_eq!(bgra.len(), (rect.width() * rect.height() * 4) as usize, "размер буфера не совпадает с прямоугольником");
        Self { rect, bgra }
    }

    pub fn width(&self) -> u32 {
        self.rect.width() as u32
    }

    pub fn height(&self) -> u32 {
        self.rect.height() as u32
    }

    /// BGRA пикселя в координатах рабочего стола.
    pub fn pixel(&self, p: Point) -> Option<[u8; 4]> {
        if !self.rect.contains(p) {
            return None;
        }
        let i = (((p.y - self.rect.top) * self.rect.width() + (p.x - self.rect.left)) * 4) as usize;
        Some([self.bgra[i], self.bgra[i + 1], self.bgra[i + 2], self.bgra[i + 3]])
    }

    /// Вырезает `area` из кадров мониторов. Пиксели мониторов становятся непрозрачными (у кадров
    /// дубликатора альфа не определена), места между мониторами остаются прозрачными.
    pub fn compose<'a>(sources: impl IntoIterator<Item = &'a Image>, area: Rect) -> Image {
        let mut out = vec![0u8; (area.width().max(0) * area.height().max(0) * 4) as usize];
        for src in sources {
            let Some(part) = src.rect.intersect(&area) else { continue };
            let row_bytes = (part.width() * 4) as usize;
            for y in part.top..part.bottom {
                let from = (((y - src.rect.top) * src.rect.width() + (part.left - src.rect.left)) * 4) as usize;
                let to = (((y - area.top) * area.width() + (part.left - area.left)) * 4) as usize;
                let row = &mut out[to..to + row_bytes];
                row.copy_from_slice(&src.bgra[from..from + row_bytes]);
                for px in row.as_chunks_mut::<4>().0 {
                    px[3] = 255;
                }
            }
        }
        Image::new(area, out)
    }
}

/// `#RRGGBB` для пикселя BGRA.
pub fn hex(bgra: [u8; 4]) -> String {
    format!("#{:02X}{:02X}{:02X}", bgra[2], bgra[1], bgra[0])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(rect: Rect, bgra: [u8; 4]) -> Image {
        Image::new(rect, bgra.repeat((rect.width() * rect.height()) as usize))
    }

    #[test]
    fn compose_across_monitors_with_gap() {
        // Два монитора 4×2 и 4×2, второй ниже на 1 px — справа сверху остаётся дыра.
        let left = solid(Rect::new(0, 0, 4, 2), [10, 20, 30, 0]);
        let right = solid(Rect::new(4, 1, 8, 3), [40, 50, 60, 0]);
        let out = Image::compose([&left, &right], Rect::new(2, 0, 6, 2));
        assert_eq!(out.width(), 4);
        assert_eq!(out.pixel(Point::new(2, 0)), Some([10, 20, 30, 255]));
        assert_eq!(out.pixel(Point::new(5, 0)), Some([0, 0, 0, 0]), "между мониторами — прозрачно");
        assert_eq!(out.pixel(Point::new(5, 1)), Some([40, 50, 60, 255]));
    }

    #[test]
    fn pixel_outside_is_none() {
        let img = solid(Rect::new(10, 10, 12, 12), [1, 2, 3, 4]);
        assert_eq!(img.pixel(Point::new(9, 10)), None);
        assert_eq!(hex(img.pixel(Point::new(11, 11)).unwrap()), "#030201");
    }
}
