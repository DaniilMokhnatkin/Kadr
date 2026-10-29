//! Чистый контроллер M2: каждый штрих, правка текста и перетаскивание — одна операция истории.
use crate::text_input::TextInput;
use kadr_doc::annotation::{Annotation, COLORS, Tool};
use kadr_doc::{Handle, Point, Rect, drag_handle, hit_handle};
use std::sync::Arc;
pub struct Drawing {
    pub tool: Tool,
    pub color: usize,
    pub width: u32,
    pub strength: u8,
    shapes: Vec<Annotation>,
    undo: Vec<Arc<Vec<Annotation>>>,
    redo: Vec<Arc<Vec<Annotation>>>,
    pending: Option<Annotation>,
    edit: Option<Edit>,
    pub selected: Option<usize>,
    /// The shape just drawn: changing colour or width right after drawing restyles it, as in Markup.
    last: Option<u64>,
    /// Blurs added by «Скрыть данные»; the button is «on» while any of them is in the document.
    smart: Vec<u64>,
    text: Option<usize>,
    text_before: Option<Arc<Vec<Annotation>>>,
    text_input: TextInput,
    pub revision: u64,
    next_id: u64,
}
struct Edit {
    start: Point,
    shape: Annotation,
    handle: Handle,
}
impl Default for Drawing {
    fn default() -> Self {
        Self {
            tool: Tool::Select,
            color: 0,
            width: 3,
            // Soft by default: text keeps its outline but cannot be read; stronger levels are one wheel step away.
            strength: 0,
            shapes: Vec::new(),
            undo: Vec::new(),
            redo: Vec::new(),
            pending: None,
            edit: None,
            selected: None,
            last: None,
            smart: Vec::new(),
            text: None,
            text_before: None,
            text_input: TextInput::default(),
            revision: 0,
            next_id: 1,
        }
    }
}
impl Drawing {
    pub fn shapes(&self) -> &[Annotation] {
        &self.shapes
    }
    pub fn preview(&self) -> Option<&Annotation> {
        self.pending.as_ref()
    }
    pub fn typing(&self) -> bool {
        self.text.is_some()
    }
    pub fn text_selection(&self) -> Option<(u64, usize, usize)> {
        self.text
            .map(|i| (self.shapes[i].id, self.text_input.cursor, self.text_input.anchor))
    }
    pub fn selected_text(&self) -> Option<&str> {
        self.text.map(|i| &self.shapes[i].text[self.text_input.range()])
    }
    pub fn text_insert(&mut self, value: &str) {
        if let Some(i) = self.text {
            let clean: String = value
                .replace("\r\n", "\n")
                .chars()
                .filter(|c| *c == '\n' || !c.is_control())
                .take(4097)
                .collect();
            self.text_input.insert(&mut self.shapes[i].text, &clean);
            self.revision += 1;
        }
    }
    pub fn text_key(&mut self, key: u16, shift: bool, ctrl: bool) {
        if let Some(i) = self.text {
            let value = &mut self.shapes[i].text;
            if ctrl && key == 0x41 {
                self.text_input.select_all(value);
            } else if key == 0x2E {
                self.text_input.erase(value, false);
            } else {
                self.text_input.navigate(value, key, shift);
            }
            self.revision += 1;
        }
    }
    pub fn busy(&self) -> bool {
        self.pending.is_some() || self.edit.is_some()
    }
    pub fn selected_bounds(&self) -> Option<Rect> {
        self.selected.and_then(|i| self.shapes.get(i)).map(Annotation::bounds)
    }
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
    pub fn restore(&mut self, shapes: Vec<Annotation>) {
        self.next_id = shapes.iter().map(|s| s.id).max().unwrap_or(0).saturating_add(1);
        self.shapes = shapes;
        self.undo.clear();
        self.redo.clear();
        self.pending = None;
        self.edit = None;
        self.selected = None;
        self.text = None;
        self.text_before = None;
        self.revision += 1;
    }
    fn snapshot(&self) -> Arc<Vec<Annotation>> {
        Arc::new(self.shapes.clone())
    }
    fn record(&mut self, before: Arc<Vec<Annotation>>) {
        if *before == self.shapes {
            return;
        }
        self.undo.push(before);
        if self.undo.len() > 80 {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.revision += 1;
    }
    /// Smart blurs present in the document (undo may have taken them away).
    pub fn smart_shapes(&self) -> Vec<Rect> {
        self.shapes.iter().filter(|s| self.smart.contains(&s.id)).map(Annotation::rect).collect()
    }
    /// Blurs over found personal data: one history step, at the current strength.
    pub fn add_smart(&mut self, rects: &[Rect], mut seed: impl FnMut() -> u64) {
        self.finish_text();
        self.cancel_drag();
        let before = self.snapshot();
        self.smart.clear();
        for r in rects {
            if self.shapes.len() >= 256 {
                break;
            }
            let mut shape = Annotation::new(self.next_id, Tool::Blur, Point::new(r.left, r.top));
            self.next_id += 1;
            shape.points[1] = [r.right - 1, r.bottom - 1];
            shape.strength = self.strength;
            shape.seed = seed();
            self.smart.push(shape.id);
            self.shapes.push(shape);
        }
        self.record(before);
    }
    /// Takes the smart blurs away again, also one step; manual blurs stay.
    pub fn remove_smart(&mut self) {
        self.finish_text();
        self.cancel_drag();
        let before = self.snapshot();
        self.shapes.retain(|s| !self.smart.contains(&s.id));
        self.selected = None;
        self.record(before);
    }
    pub fn set_tool(&mut self, tool: Tool) {
        self.finish_text();
        self.cancel_drag();
        self.tool = tool;
        self.selected = None;
        self.last = None;
    }
    /// What a style change applies to: the text being typed, else the selected shape, else the one
    /// just drawn. The new value also becomes the default for the next shape.
    fn target(&self) -> Option<usize> {
        self.text
            .or(self.selected)
            .or_else(|| self.last.and_then(|id| self.shapes.iter().position(|s| s.id == id)))
            .filter(|i| *i < self.shapes.len())
    }
    fn restyle(&mut self, apply: impl Fn(&mut Annotation) -> bool) {
        let Some(i) = self.target() else { return };
        if self.text.is_some() {
            // Typing is one history step: the whole edit, style included, is recorded when it ends.
            if apply(&mut self.shapes[i]) {
                self.revision += 1;
            }
            return;
        }
        let before = self.snapshot();
        apply(&mut self.shapes[i]);
        self.record(before);
    }
    pub fn set_color(&mut self, index: usize) {
        self.color = index.min(COLORS.len() - 1);
        let color = COLORS[self.color];
        self.restyle(|s| !s.tool.is_blur() && std::mem::replace(&mut s.color, color) != color);
    }
    pub fn set_width(&mut self, width: u32) {
        self.width = width.clamp(1, 12);
        let width = self.width;
        self.restyle(|s| std::mem::replace(&mut s.width, width) != width);
    }
    pub fn set_strength(&mut self, strength: u8) {
        self.strength = strength.min(2);
        let strength = self.strength;
        self.restyle(|s| s.tool.is_blur() && std::mem::replace(&mut s.strength, strength) != strength);
    }
    /// Возвращает true, когда событие принадлежит слою рисования, иначе рамке захвата.
    pub fn down(&mut self, p: Point, crop: Rect, seed: u64) -> bool {
        self.finish_text();
        if !crop.contains(p) {
            return self.tool != Tool::Select;
        }
        if self.tool == Tool::Select {
            let previous = self.selected.filter(|i| *i < self.shapes.len());
            let handle = previous.and_then(|i| hit_handle(&self.shapes[i].bounds(), p, 8));
            let index = if handle.is_some() {
                previous
            } else {
                self.shapes.iter().rposition(|s| s.hit(p))
            };
            self.selected = index;
            self.last = None;
            if let Some(i) = index {
                self.color = COLORS
                    .iter()
                    .position(|c| *c == self.shapes[i].color)
                    .unwrap_or(self.color);
                self.width = self.shapes[i].width;
                self.strength = self.shapes[i].strength;
                self.edit = Some(Edit {
                    start: p,
                    shape: self.shapes[i].clone(),
                    handle: handle.unwrap_or(Handle::Move),
                });
                return true;
            }
            return false;
        }
        if self.shapes.len() >= 256
            || self.shapes.iter().map(|s| s.points.len()).sum::<usize>() >= 65534
            || self.next_id == u64::MAX
        {
            return true;
        }
        let mut shape = Annotation::new(self.next_id, self.tool, p);
        self.next_id += 1;
        shape.color = COLORS[self.color];
        shape.width = self.width;
        shape.strength = self.strength;
        shape.seed = seed;
        if self.tool == Tool::Counter {
            shape.number = self
                .shapes
                .iter()
                .filter(|s| s.tool == Tool::Counter)
                .map(|s| s.number)
                .max()
                .unwrap_or(0)
                .saturating_add(1)
                .min(9999);
            self.last = Some(shape.id);
            let before = self.snapshot();
            self.shapes.push(shape);
            self.record(before);
        } else if self.tool == Tool::Text {
            let before = self.snapshot();
            self.last = Some(shape.id);
            let i = self.shapes.len();
            self.shapes.push(shape);
            self.text = Some(i);
            self.text_input = TextInput::default();
            self.text_before = Some(before);
            self.revision += 1;
        } else {
            self.pending = Some(shape);
        }
        true
    }
    pub fn motion(&mut self, p: Point, crop: Rect, shift: bool) -> bool {
        let remaining = 65536 - self.shapes.iter().map(|s| s.points.len()).sum::<usize>().min(65536);
        let p = Point::new(
            p.x.clamp(crop.left, crop.right - 1),
            p.y.clamp(crop.top, crop.bottom - 1),
        );
        if let Some(shape) = &mut self.pending {
            if shape.tool.is_path() {
                let last = shape.points.last().unwrap();
                if (last[0] - p.x).abs() + (last[1] - p.y).abs() >= 2 && shape.points.len() < 8192.min(remaining) {
                    shape.points.push([p.x, p.y]);
                }
            } else {
                let mut end = [p.x, p.y];
                let start = shape.points[0];
                if shift {
                    let dx = end[0] - start[0];
                    let dy = end[1] - start[1];
                    let side = dx.abs().max(dy.abs());
                    if matches!(shape.tool, Tool::Line | Tool::Arrow) {
                        if dx.abs() > 2 * dy.abs() {
                            end[1] = start[1];
                        } else if dy.abs() > 2 * dx.abs() {
                            end[0] = start[0];
                        } else {
                            end = [start[0] + side * dx.signum(), start[1] + side * dy.signum()];
                        }
                    } else {
                        end = [
                            start[0] + side * if dx < 0 { -1 } else { 1 },
                            start[1] + side * if dy < 0 { -1 } else { 1 },
                        ];
                    }
                }
                shape.points[1] = [
                    end[0].clamp(crop.left, crop.right - 1),
                    end[1].clamp(crop.top, crop.bottom - 1),
                ];
            }
            return true;
        }
        if let Some(edit) = &self.edit {
            let i = self.selected.unwrap();
            let bounds = edit.shape.bounds();
            if edit.handle == Handle::Move {
                let target = bounds
                    .offset(p.x - edit.start.x, p.y - edit.start.y)
                    .moved_inside(&crop);
                let mut shape = edit.shape.clone();
                for xy in &mut shape.points {
                    xy[0] += target.left - bounds.left;
                    xy[1] += target.top - bounds.top;
                }
                self.shapes[i] = shape;
            } else {
                let target = drag_handle(&bounds, edit.handle, edit.start, p, &crop);
                self.shapes[i] = edit.shape.transformed(bounds, target);
            }
            return true;
        }
        false
    }
    pub fn up(&mut self, p: Point, crop: Rect, shift: bool) -> bool {
        self.motion(p, crop, shift);
        self.finish_gesture()
    }
    /// Фиксирует уже видимый preview перед переходом в другое окно, без новой позиции мыши.
    pub fn finish_gesture(&mut self) -> bool {
        if let Some(shape) = self.pending.take() {
            let before = self.snapshot();
            self.last = Some(shape.id);
            self.shapes.push(shape);
            self.record(before);
            return true;
        }
        if let Some(edit) = self.edit.take() {
            let mut before = self.shapes.clone();
            before[self.selected.unwrap()] = edit.shape;
            self.record(Arc::new(before));
            return true;
        }
        self.tool != Tool::Select
    }
    pub fn text_char(&mut self, c: char) -> bool {
        let Some(i) = self.text else { return false };
        let shape = &mut self.shapes[i];
        if c == '\u{8}' {
            self.text_input.erase(&mut shape.text, true);
        } else if c == '\n' || !c.is_control() {
            self.text_input.insert(&mut shape.text, &c.to_string());
        }
        self.revision += 1;
        true
    }
    pub fn finish_text(&mut self) {
        if let Some(i) = self.text.take() {
            if self.shapes[i].text.trim().is_empty() {
                self.shapes.remove(i);
                self.selected = None;
            }
            if let Some(before) = self.text_before.take() {
                self.record(before);
            }
        }
    }
    pub fn edit_text(&mut self) -> bool {
        if let Some(i) = self.selected.filter(|i| self.shapes[*i].tool == Tool::Text) {
            self.text_before = Some(self.snapshot());
            self.text = Some(i);
            self.text_input = TextInput::at_end(&self.shapes[i].text);
            return true;
        }
        false
    }
    pub fn cancel(&mut self) -> bool {
        if self.typing() {
            self.text = None;
            self.selected = None;
            if let Some(before) = self.text_before.take() {
                self.shapes = (*before).clone();
                self.revision += 1;
            }
            return true;
        }
        if self.busy() {
            self.cancel_drag();
            return true;
        }
        if self.selected.take().is_some() {
            return true;
        }
        if self.tool != Tool::Select {
            self.tool = Tool::Select;
            return true;
        }
        false
    }
    fn cancel_drag(&mut self) {
        self.pending = None;
        if let Some(edit) = self.edit.take()
            && let Some(i) = self.selected
        {
            self.shapes[i] = edit.shape;
        }
    }
    pub fn delete(&mut self) {
        self.finish_text();
        if let Some(i) = self.selected.take() {
            let before = self.snapshot();
            self.shapes.remove(i);
            self.record(before);
        }
    }
    pub fn nudge(&mut self, dx: i32, dy: i32, crop: Rect) {
        let Some(i) = self.selected else { return };
        let before = self.snapshot();
        let r = self.shapes[i].bounds();
        let target = r.offset(dx, dy).moved_inside(&crop);
        for p in &mut self.shapes[i].points {
            p[0] += target.left - r.left;
            p[1] += target.top - r.top;
        }
        self.record(before);
    }
    pub fn undo(&mut self) {
        self.finish_text();
        self.cancel_drag();
        if let Some(previous) = self.undo.pop() {
            let now = self.snapshot();
            self.redo.push(now);
            self.shapes = (*previous).clone();
            self.selected = None;
            self.revision += 1;
        }
    }
    pub fn redo(&mut self) {
        self.finish_text();
        self.cancel_drag();
        if let Some(next) = self.redo.pop() {
            let before = self.snapshot();
            self.undo.push(before);
            self.shapes = (*next).clone();
            self.selected = None;
            self.revision += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const C: Rect = Rect::new(-100, 0, 500, 500);
    #[test]
    fn switching_windows_commits_visible_stroke_and_does_not_continue_it_on_return() {
        let mut d = Drawing::default();
        d.set_tool(Tool::Pen);
        d.down(Point::new(10, 20), C, 1);
        d.motion(Point::new(50, 60), C, false);
        let visible = d.preview().unwrap().clone();
        d.finish_gesture();
        assert!(!d.busy());
        assert_eq!(d.shapes(), &[visible]);
        assert!(!d.motion(Point::new(80, 90), C, false));
        d.undo();
        assert!(d.shapes().is_empty());
    }
    #[test]
    fn active_tool_does_not_start_a_crop_drag_outside_the_selection() {
        let mut d = Drawing::default();
        d.set_tool(Tool::Arrow);
        assert!(d.down(Point::new(600, 600), C, 1));
        assert!(d.up(Point::new(450, 450), C, false));
        assert!(d.shapes().is_empty());
        d.set_tool(Tool::Select);
        assert!(!d.down(Point::new(600, 600), C, 1));
    }
    #[test]
    fn empty_selected_text_can_be_deleted_without_a_stale_selection() {
        let mut d = Drawing::default();
        d.set_tool(Tool::Text);
        d.down(Point::new(0, 10), C, 1);
        d.text_char('Я');
        d.finish_text();
        d.set_tool(Tool::Select);
        d.down(Point::new(10, 20), C, 1);
        d.up(Point::new(10, 20), C, false);
        d.edit_text();
        d.text_key(0x41, false, true);
        d.text_insert("");
        d.delete();
        assert!(d.shapes().is_empty());
        assert!(d.selected.is_none());
    }
    #[test]
    fn stroke_is_one_history_action_and_new_edit_clears_redo() {
        let mut d = Drawing::default();
        d.set_tool(Tool::Pen);
        d.down(Point::new(0, 10), C, 1);
        for x in 1..20 {
            d.motion(Point::new(x, 10), C, false);
        }
        d.up(Point::new(20, 10), C, false);
        assert_eq!(d.shapes().len(), 1);
        d.undo();
        assert!(d.shapes().is_empty());
        d.redo();
        assert_eq!(d.shapes().len(), 1);
        d.undo();
        d.down(Point::new(0, 20), C, 2);
        d.up(Point::new(20, 20), C, false);
        assert!(!d.can_redo());
    }
    #[test]
    fn smart_blurs_come_and_go_as_one_step_each_and_leave_manual_blurs_alone() {
        let mut d = Drawing::default();
        d.set_tool(Tool::Blur);
        d.down(Point::new(10, 10), C, 1);
        d.up(Point::new(60, 40), C, false);
        let mut n = 0;
        d.add_smart(&[Rect::new(100, 100, 180, 120), Rect::new(100, 200, 260, 220)], || {
            n += 1;
            n
        });
        assert_eq!(d.shapes().len(), 3);
        assert_eq!(d.smart_shapes(), [Rect::new(100, 100, 180, 120), Rect::new(100, 200, 260, 220)]);
        d.remove_smart();
        assert_eq!(d.shapes().len(), 1);
        assert!(d.smart_shapes().is_empty());
        d.undo();
        assert_eq!(d.smart_shapes().len(), 2, "undo brings them back");
        d.undo();
        assert!(d.smart_shapes().is_empty() && d.shapes().len() == 1);
    }

    #[test]
    fn colour_change_restyles_text_being_typed_and_the_shape_just_drawn() {
        let mut d = Drawing::default();
        d.set_tool(Tool::Text);
        d.down(Point::new(0, 10), C, 1);
        for c in "Hi".chars() {
            d.text_char(c);
        }
        d.set_color(3);
        d.finish_text();
        assert_eq!(d.shapes()[0].color, COLORS[3]);
        // Typing plus its colour is one step.
        d.undo();
        assert!(d.shapes().is_empty());
        d.redo();

        d.set_tool(Tool::Rectangle);
        d.down(Point::new(10, 50), C, 2);
        d.up(Point::new(80, 90), C, false);
        d.set_color(1);
        d.set_width(7);
        assert_eq!((d.shapes()[1].color, d.shapes()[1].width), (COLORS[1], 7));
        assert_eq!(d.shapes()[0].color, COLORS[3], "earlier shapes keep their colour");
        d.undo();
        assert_eq!(d.shapes()[1].width, 3);
        assert_eq!(d.shapes()[1].color, COLORS[1]);

        // A new tool starts fresh: the next colour is for the next shape only.
        d.set_tool(Tool::Arrow);
        d.set_color(2);
        assert_eq!(d.shapes()[1].color, COLORS[1]);
        // Blur takes strength, never a colour.
        d.set_tool(Tool::Blur);
        d.down(Point::new(100, 100), C, 3);
        d.up(Point::new(200, 150), C, false);
        let blur = d.shapes()[2].clone();
        d.set_color(4);
        d.set_strength(2);
        assert_eq!((d.shapes()[2].color, d.shapes()[2].strength), (blur.color, 2));
    }
    #[test]
    fn unicode_text_is_one_operation_and_escape_reverts_edit() {
        let mut d = Drawing::default();
        d.set_tool(Tool::Text);
        d.down(Point::new(0, 10), C, 1);
        for c in "Привет 🌍".chars() {
            d.text_char(c);
        }
        d.text_char('\u{8}');
        d.finish_text();
        assert_eq!(d.shapes()[0].text, "Привет ");
        d.undo();
        assert!(d.shapes().is_empty());
        d.redo();
        d.set_tool(Tool::Select);
        d.down(Point::new(10, 20), C, 1);
        d.up(Point::new(10, 20), C, false);
        assert!(d.edit_text());
        d.text_char('!');
        assert!(d.cancel());
        assert_eq!(d.shapes()[0].text, "Привет ");
    }
    #[test]
    fn cancel_partial_stroke_preserves_committed_drawing() {
        let mut d = Drawing::default();
        d.set_tool(Tool::Arrow);
        d.down(Point::new(0, 10), C, 3);
        d.motion(Point::new(30, 30), C, false);
        assert!(d.cancel());
        assert!(d.shapes().is_empty());
        assert!(d.preview().is_none());
    }
    #[test]
    fn shape_move_undo_and_restore_reset_history() {
        let mut d = Drawing::default();
        d.set_tool(Tool::Redact);
        d.down(Point::new(0, 10), C, 1);
        d.up(Point::new(80, 80), C, false);
        d.set_tool(Tool::Select);
        assert!(d.down(Point::new(30, 30), C, 1));
        d.up(Point::new(50, 40), C, false);
        assert_eq!(d.shapes()[0].points[0], [20, 20]);
        d.undo();
        assert_eq!(d.shapes()[0].points[0], [0, 10]);
        let shapes = d.shapes.clone();
        d.restore(shapes);
        assert!(!d.can_undo());
    }
}
