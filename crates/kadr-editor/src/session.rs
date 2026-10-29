//! Логика сессии захвата без Win32: выделение мышью и клавиатурой, привязка к окнам, маркеры.
//! Оконная часть только переводит события в вызовы и рисует то, что отсюда видно.

use kadr_doc::{Handle, Point, Rect, drag_handle, hit_handle, window_at};

/// Что сделать после события.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    None,
    Redraw,
    Close,
    Copy,
    Save,
    SaveAs,
    /// Скопировать цвет пикселя под курсором.
    CopyColor,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Escape,
    Enter,
    Space,
    Left,
    Right,
    Up,
    Down,
    /// Буква по VK-коду, в нижнем регистре латиницей — так шорткаты работают и в русской раскладке.
    Char(char),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mods {
    pub ctrl: bool,
    pub shift: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorKind {
    Cross,
    Move,
    SizeNS,
    SizeWE,
    SizeNWSE,
    SizeNESW,
}

/// Сколько пикселей мышь должна пройти, чтобы клик стал перетаскиванием.
const DRAG_THRESHOLD: i32 = 3;

enum Drag {
    /// Новое выделение от `anchor`. `space` — где был курсор, пока зажат пробел: рамка едет целиком.
    Create {
        anchor: Point,
        moved: bool,
        space: Option<Point>,
    },
    Handle {
        handle: Handle,
        from: Point,
        start: Rect,
    },
}

pub struct Session {
    bounds: Rect,
    monitors: Vec<Rect>,
    /// Окна сверху вниз по z-порядку.
    windows: Vec<Rect>,
    /// Радиус захвата маркеров в физических пикселях.
    grab: i32,
    selection: Option<Rect>,
    /// После выбора окна/монитора можно сразу нарисовать область внутри него.
    auto_selected: bool,
    hover: Option<Rect>,
    cursor: Point,
    drag: Option<Drag>,
    hud_visible: bool,
}

impl Session {
    /// `hud` — показывать ли панель замеров (флаг `--perf-hud`; по умолчанию её нет).
    pub fn new(monitors: Vec<Rect>, cursor: Point, grab: i32, hud: bool) -> Self {
        let bounds = monitors
            .iter()
            .fold(monitors.first().copied().unwrap_or_default(), |a, m| a.union(m));
        let mut session = Self {
            bounds,
            monitors,
            windows: Vec::new(),
            grab,
            selection: None,
            auto_selected: false,
            hover: None,
            cursor,
            drag: None,
            hud_visible: hud,
        };
        session.hover = session.pick(cursor);
        session
    }

    /// Окна приходят после показа оверлея — их перечисление не держит горячий путь.
    pub fn set_windows(&mut self, windows: Vec<Rect>) {
        self.windows = windows;
        if self.selection.is_none() && self.drag.is_none() {
            self.hover = self.pick(self.cursor);
        }
    }

    pub fn selection(&self) -> Option<Rect> {
        self.selection
    }

    pub fn is_auto_selected(&self) -> bool {
        self.auto_selected
    }

    pub fn restore_selection(&mut self, selection: Option<Rect>, automatic: bool) {
        self.selection = selection.and_then(|r| r.intersect(&self.bounds));
        self.auto_selected = automatic;
        self.drag = None;
    }

    /// Что вырезать из затемнения и обвести: выделение, а пока его нет — окно под курсором.
    pub fn highlight(&self) -> Option<Rect> {
        self.selection.or(self.hover)
    }

    /// Маркеры — только у готового выделения.
    pub fn show_handles(&self) -> bool {
        self.selection.is_some() && !matches!(self.drag, Some(Drag::Create { .. }))
    }

    /// Лупа нужна, пока целишься: до выделения и пока тянешь.
    pub fn show_magnifier(&self) -> bool {
        self.selection.is_none() || self.drag.is_some()
    }

    /// Панель кнопок — у готового выделения, пока его не тянут.
    pub fn show_toolbar(&self) -> bool {
        self.selection.is_some() && self.drag.is_none()
    }

    pub fn cursor(&self) -> Point {
        self.cursor
    }
    pub fn observe_cursor(&mut self, p: Point) {
        self.cursor = p;
    }

    pub fn hud_visible(&self) -> bool {
        self.hud_visible
    }

    pub fn pointer_down(&mut self, p: Point) -> Action {
        self.cursor = p;
        self.drag = Some(match self.selection.and_then(|s| self.handle_at(p).map(|h| (s, h))) {
            Some((start, handle)) => Drag::Handle { handle, from: p, start },
            None => Drag::Create {
                anchor: p,
                moved: false,
                space: None,
            },
        });
        Action::Redraw
    }

    pub fn pointer_move(&mut self, p: Point) -> Action {
        self.cursor = p;
        if self.selection.is_some() && self.drag.is_none() {
            return Action::None;
        }
        match &mut self.drag {
            Some(Drag::Create { anchor, moved, space }) => {
                if let Some(last) = space {
                    // Пробел зажат: рамка едет целиком, вместе с якорем.
                    *anchor = Point::new(anchor.x + p.x - last.x, anchor.y + p.y - last.y);
                    *last = p;
                }
                if !*moved && (p.x - anchor.x).abs().max((p.y - anchor.y).abs()) >= DRAG_THRESHOLD {
                    *moved = true;
                }
                if *moved {
                    self.selection = Rect::spanning(*anchor, p).intersect(&self.bounds);
                    self.auto_selected = false;
                }
            }
            Some(Drag::Handle { handle, from, start }) => {
                self.selection = Some(drag_handle(start, *handle, *from, p, &self.bounds));
                self.auto_selected = false;
            }
            None if self.selection.is_none() => self.hover = self.pick(p),
            None => {}
        }
        Action::Redraw
    }

    pub fn pointer_up(&mut self, p: Point) -> Action {
        self.cursor = p;
        if let Some(Drag::Create { moved: false, .. }) = self.drag {
            // Клик без движения выбирает окно под курсором, а на рабочем столе — весь монитор.
            self.selection = self.pick(p);
            self.auto_selected = true;
        }
        self.drag = None;
        Action::Redraw
    }

    pub fn key_down(&mut self, key: Key, mods: Mods) -> Action {
        match key {
            Key::Escape => Action::Close,
            Key::Enter => self.commit(Action::Copy),
            Key::Char('c') if mods.ctrl => self.commit(Action::Copy),
            Key::Char('c') => Action::CopyColor,
            Key::Char('s') if mods.ctrl => self.commit(if mods.shift { Action::SaveAs } else { Action::Save }),
            Key::Char('a') if mods.ctrl => {
                self.selection = self.monitor_at(self.cursor).or(Some(self.bounds));
                self.auto_selected = true;
                Action::Redraw
            }
            Key::Char('h') => {
                self.hud_visible = !self.hud_visible;
                Action::Redraw
            }
            Key::Space => {
                if let Some(Drag::Create { space, .. }) = &mut self.drag {
                    space.get_or_insert(self.cursor);
                }
                Action::None
            }
            Key::Left | Key::Right | Key::Up | Key::Down => self.nudge(key, mods),
            Key::Char(_) => Action::None,
        }
    }

    pub fn key_up(&mut self, key: Key) -> Action {
        if key == Key::Space
            && let Some(Drag::Create { space, .. }) = &mut self.drag
        {
            *space = None;
        }
        Action::None
    }

    pub fn cursor_kind(&self, p: Point) -> CursorKind {
        let handle = match &self.drag {
            Some(Drag::Handle { handle, .. }) => Some(*handle),
            Some(Drag::Create { .. }) => None,
            None => self.handle_at(p),
        };
        match handle {
            None => CursorKind::Cross,
            Some(Handle::Move) => CursorKind::Move,
            Some(Handle::N | Handle::S) => CursorKind::SizeNS,
            Some(Handle::W | Handle::E) => CursorKind::SizeWE,
            Some(Handle::NW | Handle::SE) => CursorKind::SizeNWSE,
            Some(Handle::NE | Handle::SW) => CursorKind::SizeNESW,
        }
    }

    fn handle_at(&self, p: Point) -> Option<Handle> {
        self.selection
            .and_then(|s| hit_handle(&s, p, self.grab))
            .filter(|handle| !self.auto_selected || *handle != Handle::Move)
    }

    /// Экспорт без выделения берёт подсвеченное окно — Enter на окне сразу копирует его.
    fn commit(&mut self, action: Action) -> Action {
        self.selection = self.selection.or(self.hover);
        if self.selection.is_some() { action } else { Action::None }
    }

    /// Стрелки двигают выделение, с Ctrl — меняют его размер; с Shift шаг 10 px.
    fn nudge(&mut self, key: Key, mods: Mods) -> Action {
        let Some(sel) = self.selection else { return Action::None };
        self.auto_selected = false;
        let step = if mods.shift { 10 } else { 1 };
        let (dx, dy) = match key {
            Key::Left => (-step, 0),
            Key::Right => (step, 0),
            Key::Up => (0, -step),
            _ => (0, step),
        };
        self.selection = Some(if mods.ctrl {
            let resized = Rect::new(
                sel.left,
                sel.top,
                (sel.right + dx).max(sel.left + 1),
                (sel.bottom + dy).max(sel.top + 1),
            );
            resized.intersect(&self.bounds).unwrap_or(sel)
        } else {
            sel.offset(dx, dy).moved_inside(&self.bounds)
        });
        Action::Redraw
    }

    fn pick(&self, p: Point) -> Option<Rect> {
        window_at(&self.windows, p)
            .and_then(|w| w.intersect(&self.bounds))
            .or_else(|| self.monitor_at(p))
    }

    fn monitor_at(&self, p: Point) -> Option<Rect> {
        self.monitors.iter().find(|m| m.contains(p)).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEFT: Rect = Rect::new(-1920, 156, 0, 1236);
    const MAIN: Rect = Rect::new(0, 0, 2560, 1440);
    const WINDOW: Rect = Rect::new(100, 100, 900, 700);

    fn session() -> Session {
        let mut s = Session::new(vec![MAIN, LEFT], Point::new(500, 500), 6, false);
        s.set_windows(vec![WINDOW]);
        s
    }
    #[test]
    fn moving_over_a_finished_selection_updates_cursor_without_repainting() {
        let mut s = session();
        s.restore_selection(Some(WINDOW), false);
        assert_eq!(s.pointer_move(Point::new(200, 200)), Action::None);
        assert_eq!(s.cursor(), Point::new(200, 200));
        assert_eq!(s.selection(), Some(WINDOW));
    }

    fn drag(s: &mut Session, from: (i32, i32), to: (i32, i32)) {
        s.pointer_down(Point::new(from.0, from.1));
        s.pointer_move(Point::new(to.0, to.1));
        s.pointer_up(Point::new(to.0, to.1));
    }

    #[test]
    fn hover_follows_windows_and_monitors() {
        let mut s = session();
        assert_eq!(s.highlight(), Some(WINDOW));
        s.pointer_move(Point::new(2000, 1000));
        assert_eq!(s.highlight(), Some(MAIN), "вне окон — весь монитор");
        s.pointer_move(Point::new(-100, 500));
        assert_eq!(s.highlight(), Some(LEFT));
    }

    #[test]
    fn click_selects_window_under_cursor() {
        let mut s = session();
        drag(&mut s, (500, 500), (501, 501));
        assert_eq!(s.selection(), Some(WINDOW), "сдвиг меньше порога — это клик");
    }

    #[test]
    fn dragging_inside_clicked_window_starts_a_smaller_selection() {
        let mut s = session();
        drag(&mut s, (500, 500), (500, 500));
        assert_eq!(s.selection(), Some(WINDOW));
        assert_eq!(s.cursor_kind(Point::new(300, 300)), CursorKind::Cross);
        drag(&mut s, (300, 300), (420, 360));
        assert_eq!(s.selection(), Some(Rect::new(300, 300, 421, 361)));
    }

    #[test]
    fn dragging_inside_clicked_monitor_or_select_all_can_draw_again() {
        let mut s = session();
        drag(&mut s, (2000, 1000), (2000, 1000));
        assert_eq!(s.selection(), Some(MAIN));
        drag(&mut s, (300, 300), (420, 360));
        assert_eq!(s.selection(), Some(Rect::new(300, 300, 421, 361)));
        s.key_down(
            Key::Char('a'),
            Mods {
                ctrl: true,
                shift: false,
            },
        );
        drag(&mut s, (300, 300), (400, 400));
        assert_eq!(s.selection(), Some(Rect::new(300, 300, 401, 401)));
    }

    #[test]
    fn manually_drawn_selection_still_moves_from_inside() {
        let mut s = session();
        drag(&mut s, (300, 300), (420, 360));
        assert_eq!(s.cursor_kind(Point::new(350, 330)), CursorKind::Move);
        drag(&mut s, (350, 330), (370, 350));
        assert_eq!(s.selection(), Some(Rect::new(320, 320, 441, 381)));
    }

    #[test]
    fn clicked_window_can_still_be_resized_by_its_handles() {
        let mut s = session();
        drag(&mut s, (500, 500), (500, 500));
        assert_eq!(s.cursor_kind(Point::new(900, 700)), CursorKind::SizeNWSE);
        drag(&mut s, (900, 700), (850, 650));
        assert_eq!(s.selection(), Some(Rect::new(100, 100, 850, 650)));
    }

    #[test]
    fn drag_creates_selection_across_monitors() {
        let mut s = session();
        drag(&mut s, (-100, 300), (200, 400));
        assert_eq!(s.selection(), Some(Rect::new(-100, 300, 201, 401)));
        assert!(s.show_handles());
    }

    #[test]
    fn corner_handle_resizes_existing_selection() {
        let mut s = session();
        drag(&mut s, (1000, 1000), (1100, 1100));
        drag(&mut s, (1101, 1101), (1201, 1151));
        assert_eq!(s.selection(), Some(Rect::new(1000, 1000, 1201, 1151)));
    }

    #[test]
    fn space_moves_rubber_band() {
        let mut s = session();
        s.pointer_down(Point::new(1000, 1000));
        s.pointer_move(Point::new(1100, 1050));
        s.key_down(Key::Space, Mods::default());
        s.pointer_move(Point::new(1150, 1050));
        s.key_up(Key::Space);
        s.pointer_up(Point::new(1150, 1050));
        assert_eq!(s.selection(), Some(Rect::new(1050, 1000, 1151, 1051)));
    }

    #[test]
    fn keyboard_moves_and_resizes() {
        let mut s = session();
        drag(&mut s, (1000, 1000), (1099, 1099));
        s.key_down(
            Key::Right,
            Mods {
                ctrl: false,
                shift: true,
            },
        );
        assert_eq!(s.selection(), Some(Rect::new(1010, 1000, 1110, 1100)));
        s.key_down(
            Key::Down,
            Mods {
                ctrl: true,
                shift: false,
            },
        );
        assert_eq!(s.selection(), Some(Rect::new(1010, 1000, 1110, 1101)));
    }

    #[test]
    fn enter_copies_hovered_window_without_selection() {
        let mut s = session();
        assert_eq!(s.key_down(Key::Enter, Mods::default()), Action::Copy);
        assert_eq!(s.selection(), Some(WINDOW));
    }

    #[test]
    fn shortcuts() {
        let mut s = session();
        let ctrl = Mods {
            ctrl: true,
            shift: false,
        };
        assert_eq!(s.key_down(Key::Escape, Mods::default()), Action::Close);
        assert_eq!(s.key_down(Key::Char('s'), ctrl), Action::Save);
        assert_eq!(
            s.key_down(
                Key::Char('s'),
                Mods {
                    ctrl: true,
                    shift: true
                }
            ),
            Action::SaveAs
        );
        assert_eq!(s.key_down(Key::Char('c'), Mods::default()), Action::CopyColor);
        assert_eq!(s.cursor_kind(Point::new(100, 100)), CursorKind::SizeNWSE);
    }
}
