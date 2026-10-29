use super::*;
use kadr_doc::annotation::Tool;
use kadr_editor::pointer::Sample;
use windows::Win32::Security::Cryptography::{BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom};
use windows::Win32::UI::Input::KeyboardAndMouse::{GMMP_USE_DISPLAY_POINTS, GetMouseMovePointsEx, MOUSEMOVEPOINT};
use windows::Win32::UI::Input::KeyboardAndMouse::{VK_DELETE, VK_F2};
impl App {
    pub(super) fn drawing_pointer(&mut self, hwnd: HWND, msg: u32, p: DocPoint) -> bool {
        let Some(active) = &mut self.active else {
            return false;
        };
        let Some(crop) = active.session.selection() else {
            return false;
        };
        let current = Sample {
            point: p,
            time: unsafe { GetMessageTime() } as u32,
        };
        if msg == WM_LBUTTONDOWN {
            self.pointer_history.begin(current);
        }
        let shift = mods().shift;
        if matches!(msg, WM_MOUSEMOVE | WM_LBUTTONUP) && active.drawing.busy() && active.drawing.tool.is_path() {
            let query = MOUSEMOVEPOINT {
                x: p.x & 65535,
                y: p.y & 65535,
                time: if msg == WM_MOUSEMOVE { current.time } else { 0 },
                dwExtraInfo: 0,
            };
            let mut history = [MOUSEMOVEPOINT::default(); 64];
            let count = unsafe {
                GetMouseMovePointsEx(
                    size_of::<MOUSEMOVEPOINT>() as u32,
                    &query,
                    &mut history,
                    GMMP_USE_DISPLAY_POINTS,
                )
            };
            let samples: Vec<_> = history
                .iter()
                .take(count.max(0) as usize)
                .map(|s| Sample {
                    point: DocPoint::new(s.x as u16 as i16 as i32, s.y as u16 as i16 as i32),
                    time: s.time,
                })
                .collect();
            for point in self.pointer_history.recover(&samples, current) {
                active.drawing.motion(point, crop, shift);
            }
        }
        let handled = match msg {
            WM_LBUTTONDOWN => {
                let mut seed = [0; 8];
                let result = unsafe { BCryptGenRandom(None, &mut seed, BCRYPT_USE_SYSTEM_PREFERRED_RNG) };
                if result.is_err() {
                    log_warn!("не удалось получить случайный seed");
                    return true;
                }
                let handled = active.drawing.down(p, crop, u64::from_le_bytes(seed));
                if handled {
                    unsafe {
                        SetCapture(hwnd);
                    }
                }
                handled
            }
            WM_MOUSEMOVE => active.drawing.motion(p, crop, mods().shift),
            WM_LBUTTONUP => {
                let handled = active.drawing.up(p, crop, mods().shift);
                if handled {
                    unsafe {
                        let _ = ReleaseCapture();
                    }
                }
                handled
            }
            _ => false,
        };
        if handled {
            active.session.observe_cursor(p);
            if msg != WM_MOUSEMOVE {
                self.schedule_draft();
            }
            self.request_redraw();
        }
        handled
    }
    pub(super) fn editor_key(&mut self, vk: u16) -> bool {
        let Some(active) = &mut self.active else {
            return false;
        };
        let Some(crop) = active.session.selection() else {
            return false;
        };
        let modifiers = mods();
        let d = &mut active.drawing;
        if vk == VK_ESCAPE.0 {
            if !d.cancel() {
                return false;
            }
            unsafe {
                let _ = ReleaseCapture();
            }
        } else if d.typing() {
            if modifiers.ctrl && vk == 0x56 {
                if let Ok(value) = clipboard::text(self.hwnd) {
                    d.text_insert(&value);
                }
            } else if modifiers.ctrl && matches!(vk, 0x43 | 0x58) {
                if let Some(value) = d.selected_text().filter(|s| !s.is_empty())
                    && clipboard::put_text(self.hwnd, value).is_ok()
                {
                    self.clipboard = None;
                    if vk == 0x58 {
                        d.text_insert("");
                    }
                }
            } else if vk == VK_RETURN.0 {
                if modifiers.shift {
                    d.text_char('\n');
                } else {
                    d.finish_text();
                }
            } else if modifiers.ctrl && vk == 0x53 {
                d.finish_text();
                return false;
            } else if matches!(vk, 0x23..=0x28 | 0x2E) || (modifiers.ctrl && vk == 0x41) {
                d.text_key(vk, modifiers.shift, modifiers.ctrl);
            } else {
                return true;
            }
        } else if modifiers.ctrl && vk == b'Z' as u16 {
            if modifiers.shift {
                d.redo();
            } else {
                d.undo();
            }
        } else if modifiers.ctrl && vk == b'Y' as u16 {
            d.redo();
        } else if vk == VK_DELETE.0 {
            d.delete();
        } else if vk == VK_F2.0 {
            d.edit_text();
        } else if vk == 0xDB {
            d.set_width(d.width.saturating_sub(1));
        } else if vk == 0xDD {
            d.set_width(d.width + 1);
        } else if d.selected.is_some() && matches!(vk, 0x25..=0x28) {
            let step = if modifiers.shift { 10 } else { 1 };
            let (dx, dy) = match vk {
                0x25 => (-step, 0),
                0x26 => (0, -step),
                0x27 => (step, 0),
                _ => (0, step),
            };
            d.nudge(dx, dy, crop);
        } else if !modifiers.ctrl {
            if vk == 0x09 {
                let index = Tool::ALL.iter().position(|t| *t == d.tool).unwrap_or(0);
                d.set_tool(Tool::ALL[(index + if modifiers.shift { 13 } else { 1 }) % 14]);
                self.schedule_draft();
                self.request_redraw();
                return true;
            }
            if (0x31..=0x36).contains(&vk) {
                d.set_color((vk - 0x31) as usize);
                self.schedule_draft();
                self.request_redraw();
                return true;
            }
            let tool = match vk {
                0x56 => Some(Tool::Select),
                0x41 => Some(Tool::Arrow),
                0x4C => Some(Tool::Line),
                0x52 => Some(Tool::Rectangle),
                0x4F => Some(Tool::Ellipse),
                0x50 => Some(Tool::Pen),
                0x4D => Some(Tool::Marker),
                0x54 => Some(Tool::Text),
                0x4E => Some(Tool::Counter),
                0x42 => Some(Tool::Blur),
                0x45 => Some(Tool::BlurEllipse),
                0x55 => Some(Tool::BlurBrush),
                0x49 => Some(Tool::Spotlight),
                0x58 => Some(Tool::Redact),
                _ => None,
            };
            if let Some(tool) = tool {
                d.set_tool(tool);
            } else {
                return false;
            }
        } else {
            return false;
        }
        self.schedule_draft();
        self.request_redraw();
        true
    }
    pub(super) fn on_text(&mut self, unit: u16) {
        if mods().ctrl {
            return;
        }
        let Some(active) = &mut self.active else {
            return;
        };
        if !active.drawing.typing() {
            active.text_surrogate = None;
            return;
        }
        if (0xD800..=0xDBFF).contains(&unit) {
            active.text_surrogate = Some(unit);
            return;
        }
        let ch = if let Some(high) = active.text_surrogate.take() {
            if (0xDC00..=0xDFFF).contains(&unit) {
                char::from_u32(0x10000 + (((high as u32 - 0xD800) << 10) | (unit as u32 - 0xDC00)))
            } else {
                char::from_u32(unit as u32)
            }
        } else {
            char::from_u32(unit as u32)
        };
        if let Some(ch) = ch.filter(|c| *c != '\r' && *c != '\n') {
            active.drawing.text_char(ch);
            self.schedule_draft();
            self.request_redraw();
        }
    }
    pub(super) fn editor_wheel(&mut self, delta: i32) {
        let Some(active) = &mut self.active else {
            return;
        };
        // Scrolling down inside the frame, before any drawing, starts a long screenshot.
        let inside = active.session.selection().is_some_and(|s| s.contains(doc_point(cursor_pos())));
        if delta < 0
            && inside
            && active.drawing.tool == kadr_doc::annotation::Tool::Select
            && active.drawing.shapes().is_empty()
        {
            self.start_scroll_from_selection();
            return;
        }
        let d = &mut active.drawing;
        if d.tool.is_blur() {
            let strength = (d.strength as i32 + delta.signum()).clamp(0, 2);
            d.set_strength(strength as u8);
        } else {
            d.set_width((d.width as i32 + delta.signum()).clamp(1, 12) as u32);
        }
        self.schedule_draft();
        self.request_redraw();
    }
    pub(super) fn editor_button(&mut self, button: ToolbarButton) {
        let Some(active) = &mut self.active else {
            return;
        };
        let d = &mut active.drawing;
        match button {
            ToolbarButton::Tool(tool) => d.set_tool(tool),
            ToolbarButton::Color(index) => d.set_color(index),
            ToolbarButton::Width => d.set_width(if d.width >= 8 { 1 } else { d.width + 1 }),
            ToolbarButton::Strength => d.set_strength((d.strength + 1) % 3),
            ToolbarButton::Undo => d.undo(),
            ToolbarButton::Redo => d.redo(),
            _ => {}
        }
        self.schedule_draft();
        self.request_redraw();
    }
}
