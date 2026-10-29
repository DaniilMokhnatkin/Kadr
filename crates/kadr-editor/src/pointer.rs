//! Восстановление хронологии WM_MOUSEMOVE без интерполяции выдуманных точек.
use kadr_doc::Point;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sample {
    pub point: Point,
    pub time: u32,
}
#[derive(Default)]
pub struct History {
    last: Option<Sample>,
}
impl History {
    pub fn begin(&mut self, sample: Sample) {
        self.last = Some(sample);
    }
    /// Win32 возвращает историю от новой точки к старой, включая запрошенную.
    pub fn recover(&mut self, newest_first: &[Sample], current: Sample) -> Vec<Point> {
        let mut out = Vec::new();
        if let Some(last) = self.last {
            let stop = newest_first.iter().position(|s| *s == last);
            for s in newest_first[..stop.unwrap_or(newest_first.len())].iter().rev() {
                let since = s.time.wrapping_sub(last.time) as i32;
                let until = current.time.wrapping_sub(s.time) as i32;
                if (stop.is_some() || since > 0) && until >= 0 && out.last().copied().unwrap_or(last.point) != s.point {
                    out.push(s.point);
                }
            }
            if out.last().copied().unwrap_or(last.point) != current.point {
                out.push(current.point);
            }
        } else {
            out.push(current.point);
        }
        self.last = Some(current);
        out
    }
}
pub fn message_point(packed: isize, origin: Point) -> Point {
    Point::new(
        origin.x + packed as u16 as i16 as i32,
        origin.y + (packed >> 16) as u16 as i16 as i32,
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    fn s(x: i32, y: i32, time: u32) -> Sample {
        Sample {
            point: Point::new(x, y),
            time,
        }
    }
    #[test]
    fn coalesced_move_keeps_curve_and_never_replays_before_mouse_down() {
        let mut h = History::default();
        h.begin(s(10, 10, 100));
        let points = h.recover(
            &[
                s(40, 10, 103),
                s(30, 30, 102),
                s(20, 40, 101),
                s(10, 10, 100),
                s(0, 0, 99),
            ],
            s(40, 10, 103),
        );
        assert_eq!(points, vec![Point::new(20, 40), Point::new(30, 30), Point::new(40, 10)]);
        let crop = kadr_doc::Rect::new(0, 0, 100, 100);
        let mut d = crate::drawing::Drawing::default();
        d.set_tool(kadr_doc::annotation::Tool::Pen);
        d.down(Point::new(10, 10), crop, 1);
        for p in points {
            d.motion(p, crop, false);
        }
        d.up(Point::new(40, 10), crop, false);
        assert!(
            d.shapes()[0].points.contains(&[20, 40]),
            "coalescing must preserve the bend, not just a straight chord"
        );
        assert!(h.recover(&[s(40, 10, 103), s(30, 30, 102)], s(40, 10, 103)).is_empty());
    }
    #[test]
    fn same_millisecond_points_and_tick_wrap_keep_order() {
        let mut h = History::default();
        h.begin(s(-10, 0, u32::MAX));
        assert_eq!(
            h.recover(&[s(-3, 3, 0), s(-5, 4, 0), s(-10, 0, u32::MAX)], s(-3, 3, 0)),
            vec![Point::new(-5, 4), Point::new(-3, 3)]
        );
    }
    #[test]
    fn message_coordinates_are_signed_and_not_replaced_by_live_cursor() {
        let packed = ((-20i16 as u16 as isize) << 16) | (-5i16 as u16 as isize);
        assert_eq!(message_point(packed, Point::new(-1920, 100)), Point::new(-1925, 80));
    }
}
