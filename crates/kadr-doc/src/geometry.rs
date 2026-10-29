#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

impl Point {
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }
}

/// `[left, right) × [top, bottom)`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Rect {
    pub const fn new(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Self { left, top, right, bottom }
    }

    /// Прямоугольник по двум пикселям включительно: клик без движения даёт 1×1.
    pub fn spanning(a: Point, b: Point) -> Self {
        Self::new(a.x.min(b.x), a.y.min(b.y), a.x.max(b.x) + 1, a.y.max(b.y) + 1)
    }

    pub fn width(&self) -> i32 {
        self.right - self.left
    }

    pub fn height(&self) -> i32 {
        self.bottom - self.top
    }

    pub fn is_empty(&self) -> bool {
        self.width() <= 0 || self.height() <= 0
    }

    pub fn contains(&self, p: Point) -> bool {
        p.x >= self.left && p.x < self.right && p.y >= self.top && p.y < self.bottom
    }

    pub fn intersect(&self, other: &Rect) -> Option<Rect> {
        let r = Rect::new(
            self.left.max(other.left),
            self.top.max(other.top),
            self.right.min(other.right),
            self.bottom.min(other.bottom),
        );
        (!r.is_empty()).then_some(r)
    }

    pub fn union(&self, other: &Rect) -> Rect {
        Rect::new(
            self.left.min(other.left),
            self.top.min(other.top),
            self.right.max(other.right),
            self.bottom.max(other.bottom),
        )
    }

    pub fn offset(&self, dx: i32, dy: i32) -> Rect {
        Rect::new(self.left + dx, self.top + dy, self.right + dx, self.bottom + dy)
    }

    /// Сдвигает внутрь `bounds`, сохраняя размер; не влезает — прижимает к левому верхнему краю.
    pub fn moved_inside(&self, bounds: &Rect) -> Rect {
        let shift = |lo: i32, hi: i32, min: i32, max: i32| {
            if hi > max { (max - hi).max(min - lo) } else if lo < min { min - lo } else { 0 }
        };
        self.offset(
            shift(self.left, self.right, bounds.left, bounds.right),
            shift(self.top, self.bottom, bounds.top, bounds.bottom),
        )
    }
}

/// За что тянем выделение: стороны, углы или всё целиком.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handle {
    Move,
    N,
    S,
    W,
    E,
    NW,
    NE,
    SW,
    SE,
}

impl Handle {
    /// Какие стороны двигает: (левая, верхняя, правая, нижняя).
    fn edges(self) -> (bool, bool, bool, bool) {
        match self {
            Handle::Move => (true, true, true, true),
            Handle::N => (false, true, false, false),
            Handle::S => (false, false, false, true),
            Handle::W => (true, false, false, false),
            Handle::E => (false, false, true, false),
            Handle::NW => (true, true, false, false),
            Handle::NE => (false, true, true, false),
            Handle::SW => (true, false, false, true),
            Handle::SE => (false, false, true, true),
        }
    }
}

/// Маркер под точкой: углы и стороны — в пределах `grab` пикселей от границы, изнутри — перенос.
/// Если обе стороны близко (узкое выделение), берётся ближайшая.
pub fn hit_handle(sel: &Rect, p: Point, grab: i32) -> Option<Handle> {
    let outer = Rect::new(sel.left - grab, sel.top - grab, sel.right + grab, sel.bottom + grab);
    if !outer.contains(p) {
        return None;
    }
    let side = |pos: i32, lo: i32, hi: i32| {
        let (to_lo, to_hi) = ((pos - lo).abs(), (pos - hi).abs());
        match (to_lo <= grab, to_hi <= grab) {
            (false, false) => 0,
            (true, false) => -1,
            (false, true) => 1,
            (true, true) => if to_lo <= to_hi { -1 } else { 1 },
        }
    };
    let handle = match (side(p.x, sel.left, sel.right), side(p.y, sel.top, sel.bottom)) {
        (-1, -1) => Handle::NW,
        (1, -1) => Handle::NE,
        (-1, 1) => Handle::SW,
        (1, 1) => Handle::SE,
        (-1, 0) => Handle::W,
        (1, 0) => Handle::E,
        (0, -1) => Handle::N,
        (0, 1) => Handle::S,
        _ => return sel.contains(p).then_some(Handle::Move),
    };
    Some(handle)
}

/// Результат перетаскивания маркера от `from` к `to`, начиная с `start`. Сторона, протащенная за
/// противоположную, меняется с ней местами, а не схлопывает выделение. Всё остаётся внутри `bounds`.
pub fn drag_handle(start: &Rect, handle: Handle, from: Point, to: Point, bounds: &Rect) -> Rect {
    let (dx, dy) = (to.x - from.x, to.y - from.y);
    if handle == Handle::Move {
        return start.offset(dx, dy).moved_inside(bounds);
    }
    let (left, top, right, bottom) = handle.edges();
    let mut r = *start;
    if left {
        r.left += dx;
    }
    if right {
        r.right += dx;
    }
    if top {
        r.top += dy;
    }
    if bottom {
        r.bottom += dy;
    }
    if r.left > r.right {
        (r.left, r.right) = (r.right, r.left);
    }
    if r.top > r.bottom {
        (r.top, r.bottom) = (r.bottom, r.top);
    }
    r.right = r.right.max(r.left + 1);
    r.bottom = r.bottom.max(r.top + 1);
    r.intersect(bounds).unwrap_or(*start)
}

/// Верхнее по z-порядку окно под точкой. `windows` идут сверху вниз.
pub fn window_at(windows: &[Rect], p: Point) -> Option<Rect> {
    windows.iter().find(|w| w.contains(p)).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEL: Rect = Rect::new(100, 100, 300, 200);
    const SCREEN: Rect = Rect::new(0, 0, 1000, 800);

    #[test]
    fn spanning_includes_both_pixels() {
        assert_eq!(Rect::spanning(Point::new(5, 5), Point::new(5, 5)), Rect::new(5, 5, 6, 6));
        assert_eq!(Rect::spanning(Point::new(9, 2), Point::new(3, 7)), Rect::new(3, 2, 10, 8));
    }

    #[test]
    fn moved_inside_keeps_size() {
        let r = Rect::new(-50, 790, 50, 840).moved_inside(&SCREEN);
        assert_eq!(r, Rect::new(0, 750, 100, 800));
    }

    #[test]
    fn handles_by_position() {
        assert_eq!(hit_handle(&SEL, Point::new(100, 100), 6), Some(Handle::NW));
        assert_eq!(hit_handle(&SEL, Point::new(303, 203), 6), Some(Handle::SE));
        assert_eq!(hit_handle(&SEL, Point::new(200, 96), 6), Some(Handle::N));
        assert_eq!(hit_handle(&SEL, Point::new(298, 150), 6), Some(Handle::E));
        assert_eq!(hit_handle(&SEL, Point::new(200, 150), 6), Some(Handle::Move));
        assert_eq!(hit_handle(&SEL, Point::new(50, 50), 6), None);
    }

    #[test]
    fn narrow_selection_picks_nearest_side() {
        let thin = Rect::new(100, 100, 104, 200);
        assert_eq!(hit_handle(&thin, Point::new(101, 150), 6), Some(Handle::W));
        assert_eq!(hit_handle(&thin, Point::new(104, 150), 6), Some(Handle::E));
    }

    #[test]
    fn corner_drag_resizes() {
        let r = drag_handle(&SEL, Handle::SE, Point::new(300, 200), Point::new(350, 260), &SCREEN);
        assert_eq!(r, Rect::new(100, 100, 350, 260));
    }

    #[test]
    fn edge_dragged_past_opposite_flips() {
        let r = drag_handle(&SEL, Handle::W, Point::new(100, 150), Point::new(400, 150), &SCREEN);
        assert_eq!(r, Rect::new(300, 100, 400, 200));
    }

    #[test]
    fn move_stays_on_screen() {
        let r = drag_handle(&SEL, Handle::Move, Point::new(200, 150), Point::new(-500, 150), &SCREEN);
        assert_eq!(r, Rect::new(0, 100, 200, 200));
    }

    #[test]
    fn topmost_window_wins() {
        let windows = [Rect::new(0, 0, 100, 100), Rect::new(0, 0, 500, 500)];
        assert_eq!(window_at(&windows, Point::new(50, 50)), Some(windows[0]));
        assert_eq!(window_at(&windows, Point::new(200, 200)), Some(windows[1]));
        assert_eq!(window_at(&windows, Point::new(600, 600)), None);
    }
}
