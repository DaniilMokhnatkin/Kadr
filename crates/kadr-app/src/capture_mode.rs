use kadr_doc::{Point, Rect};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Region,
    Fullscreen,
    Window,
    LastRegion,
}

impl Mode {
    /// None — интерактивное выделение, Some — немедленный экспорт без показа оверлеев.
    pub fn region(
        self,
        monitors: &[Rect],
        cursor: Point,
        foreground: Option<Rect>,
        last: Option<Rect>,
    ) -> Result<Option<Rect>, &'static str> {
        let rect = match self {
            Self::Region => return Ok(None),
            Self::Fullscreen => monitors
                .iter()
                .find(|r| r.contains(cursor))
                .copied()
                .ok_or("Под курсором нет подключённого монитора")?,
            Self::Window => foreground.ok_or("Не удалось определить активное окно")?,
            Self::LastRegion => last.ok_or("Сначала выделите область для снимка")?,
        };
        if !monitors.iter().any(|m| m.intersect(&rect).is_some()) {
            return Err("Эта область больше не находится на подключённых мониторах");
        }
        let bounds = monitors.iter().copied().reduce(|a, b| a.union(&b)).unwrap();
        Ok(rect.intersect(&bounds))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kadr_doc::{Point, Rect};

    const MAIN: Rect = Rect::new(0, 0, 2560, 1440);
    const LEFT: Rect = Rect::new(-1920, 156, 0, 1236);

    #[test]
    fn fullscreen_uses_monitor_under_cursor_including_negative_coordinates() {
        assert_eq!(Mode::Fullscreen.region(&[MAIN, LEFT], Point::new(-100, 500), None, None), Ok(Some(LEFT)));
        assert_eq!(Mode::Region.region(&[MAIN], Point::new(0, 0), None, None), Ok(None));
    }

    #[test]
    fn window_uses_foreground_bounds_and_clips_offscreen_pixels() {
        let window = Rect::new(-3000, 400, 200, 800);
        assert_eq!(
            Mode::Window.region(&[MAIN, LEFT], Point::new(500, 500), Some(window), None),
            Ok(Some(Rect::new(-1920, 400, 200, 800)))
        );
        assert!(Mode::Window.region(&[MAIN], Point::new(0, 0), None, None).is_err());
    }

    #[test]
    fn last_region_needs_previous_selection_and_a_connected_monitor() {
        assert!(Mode::LastRegion.region(&[MAIN], Point::new(0, 0), None, None).is_err());
        assert!(Mode::LastRegion.region(&[MAIN], Point::new(0, 0), None, Some(LEFT)).is_err());
        let region = Rect::new(100, 100, 400, 300);
        assert_eq!(Mode::LastRegion.region(&[MAIN], Point::new(0, 0), None, Some(region)), Ok(Some(region)));
    }

    #[test]
    fn selection_in_gap_between_monitors_is_rejected() {
        let gap = Rect::new(-400, 0, -100, 100);
        assert!(Mode::LastRegion.region(&[MAIN, LEFT], Point::new(0, 0), None, Some(gap)).is_err());
    }
}
