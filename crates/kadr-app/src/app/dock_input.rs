//! Concept 04 controls operate on the editor and the real recent-capture store.
use super::*;
use crate::recent_worker::Command;
use kadr_render::scene::{BarMenu, BarState, RecentItem};

impl App {
    pub(super) fn bar_state(&self, active: &Active) -> BarState {
        let mut ui = active.bar;
        ui.original = self.original_visible();
        ui.recent_count = self.recent_items.len();
        if let Some(entry) = active.draft.as_ref().and_then(|d| self.recent_entries.iter().find(|e| e.id == d.token.id))
        {
            ui.pinned = entry.pinned;
        }
        ui
    }
    pub(super) fn cache_recent(&mut self, entries: Vec<kadr_store::recent::Entry>) {
        let now = crate::recent_worker::now_ms();
        self.recent_items = entries
            .iter()
            .map(|e| RecentItem {
                preview: e.preview.clone().map(Arc::new),
                title: if self.active.as_ref().and_then(|a| a.draft.as_ref()).is_some_and(|d| d.token.id == e.id) {
                    "Этот снимок".into()
                } else {
                    "Снимок экрана".into()
                },
                caption: crate::recent_window::stamp(e.created_ms, now),
                pinned: e.pinned,
            })
            .collect();
        if let Some(window) = &self.recent_window {
            window.set_entries(entries.clone());
        }
        self.recent_entries = entries;
        if let Some(a) = &mut self.active {
            a.bar.scroll = a.bar.scroll.min(kadr_render::scene::recent_scroll_limit(self.recent_items.len()));
            if let Some(entry) = a.draft.as_ref().and_then(|d| self.recent_entries.iter().find(|e| e.id == d.token.id))
            {
                a.bar.pinned = entry.pinned;
            }
        }
        self.request_redraw();
    }
    pub(super) fn pin_recent(&mut self) {
        let pinned = self.active.as_ref().is_some_and(|a| self.bar_state(a).pinned);
        let Some(a) = &mut self.active else { return };
        a.drawing.finish_gesture();
        a.drawing.finish_text();
        a.bar.pinned = !pinned;
        if a.draft.is_none() {
            self.start_draft();
        }
        self.flush_draft(true);
        if let Some(a) = &self.active {
            if let Some(draft) = &a.draft {
                self.recent.send(Command::Pin { id: draft.token.id.clone(), pinned: a.bar.pinned });
            } else {
                self.notify("Не удалось закрепить снимок", "Дождитесь завершения захвата и повторите.");
            }
        }
        self.request_redraw();
    }
    pub(super) fn dock_button(&mut self, b: ToolbarButton) -> bool {
        match b {
            ToolbarButton::Menu(menu) => {
                if let Some(a) = &mut self.active {
                    a.drawing.finish_text();
                    // «Фигуры» and «Размытие»: the first click takes the group's tool (a rectangle,
                    // an area blur, or whichever was picked last) without covering the picture with
                    // a menu; once it is on, a click (or coming back with the mouse) opens the choice.
                    match group_tool(&a.bar, menu) {
                        Some(tool) if !in_group(menu, a.drawing.tool) => {
                            a.drawing.set_tool(tool);
                            a.bar.select_tool(tool);
                        }
                        _ => a.bar.toggle(menu),
                    }
                }
            }
            ToolbarButton::Recent => {
                if let Some(a) = &mut self.active {
                    a.drawing.finish_text();
                    a.bar.toggle(BarMenu::Recent);
                    self.flush_draft(true);
                    self.recent.send(Command::List);
                } else {
                    self.show_recent();
                }
            }
            ToolbarButton::Pin => self.pin_recent(),
            ToolbarButton::Record => {
                if let Some(a) = &mut self.active {
                    a.bar.toggle(BarMenu::Record);
                }
            }
            ToolbarButton::SystemAudio => {
                if let Some(a) = &mut self.active {
                    a.bar.system_audio = !a.bar.system_audio;
                }
            }
            ToolbarButton::Microphone => {
                if let Some(a) = &mut self.active {
                    a.bar.microphone = !a.bar.microphone;
                }
            }
            ToolbarButton::RecordCursor => {
                if let Some(a) = &mut self.active {
                    a.bar.cursor = !a.bar.cursor;
                }
            }
            ToolbarButton::StartRecord => self.start_record_from_selection(),
            // Opening the drawer only shows the levels, on «Без цензуры»: nothing is recognised
            // until one is chosen. Closing it takes away whatever level was on.
            ToolbarButton::Smart => self.toggle_censor_drawer(),
            ToolbarButton::CensorOff => self.set_censor(None),
            ToolbarButton::Censor(level) => {
                self.last_censor = level;
                self.set_censor(Some(level));
            }
            ToolbarButton::Original => self.toggle_original(),
            ToolbarButton::PinWindow => self.pin(),
            ToolbarButton::About => {
                if let Some(a) = &mut self.active {
                    a.bar.menu = None;
                }
                crate::about_window::show(self.instance);
            }
            ToolbarButton::RecentOpen(index) => {
                if let Some(entry) = self.recent_entries.get(index) {
                    let id = entry.id.clone();
                    if self.active.as_ref().and_then(|a| a.draft.as_ref()).is_some_and(|d| d.token.id == id) {
                        if let Some(a) = &mut self.active {
                            a.bar.menu = None;
                        }
                    } else {
                        if let Some(a) = &mut self.active {
                            a.bar.menu = None;
                        }
                        self.pause_for_recent();
                        self.loading_recent = self.recent.send(Command::Load(id));
                    }
                }
            }
            ToolbarButton::RecentPin(index) => {
                if let Some(entry) = self.recent_entries.get(index) {
                    self.recent.send(Command::Pin { id: entry.id.clone(), pinned: !entry.pinned });
                }
            }
            ToolbarButton::RecentDelete(index) => {
                if let Some(entry) = self.recent_entries.get(index) {
                    let id = entry.id.clone();
                    if let Some(a) = &mut self.active
                        && a.draft.as_ref().is_some_and(|d| d.token.id == id)
                    {
                        a.draft = None;
                        a.bar.pinned = false;
                    }
                    self.recent.send(Command::Delete(id));
                }
            }
            ToolbarButton::RecentClear => {
                let pinned = self.active.as_ref().is_some_and(|a| self.bar_state(a).pinned);
                if let Some(a) = &mut self.active
                    && !pinned
                {
                    a.draft = None;
                }
                self.recent.send(Command::Clear);
            }
            _ => return false,
        }
        self.request_redraw();
        true
    }
    pub(super) fn dock_key(&mut self, vk: u16) -> bool {
        let Some(a) = &self.active else { return false };
        // Without focus only Esc reaches here, through the global hotkey: over a fullscreen game
        // Windows refuses to hand focus to the overlay, and Esc must still close the capture.
        // A Kadr window in front of the overlay (a translation bubble) gets that Esc itself.
        let foreground = unsafe { GetForegroundWindow() };
        if !self.overlays.iter().any(|o| o.hwnd == foreground) {
            if vk != VK_ESCAPE.0 {
                return true;
            }
            if own_window(foreground) && foreground != self.hwnd {
                unsafe {
                    let _ = PostMessageW(Some(foreground), WM_KEYDOWN, WPARAM(VK_ESCAPE.0 as usize), LPARAM(0));
                }
                return true;
            }
        }
        if a.drawing.typing() {
            return false;
        }
        if vk == VK_ESCAPE.0 && (a.bar.menu.is_some() || a.bar.focus.is_some()) {
            let a = self.active.as_mut().unwrap();
            a.bar.menu = None;
            a.bar.focus = None;
            self.request_redraw();
            return true;
        }
        if matches!(vk, 0x0D | 0x20)
            && let Some(b) = a.bar.focus
        {
            self.press(b);
            self.request_redraw();
            return true;
        }
        if vk != 0x09 && !(a.bar.focus.is_some() && matches!(vk, 0x25..=0x28)) {
            return false;
        }
        let Some(selection) = a.session.selection() else { return false };
        let e = editor_bar(&a.drawing, self.smart_ui(&a.drawing), self.bar_state(a));
        let targets = self
            .overlays
            .iter()
            .find_map(|o| {
                let list = o.toolbar_buttons(&self.shared, win_rect(selection), e);
                (!list.is_empty()).then_some(list)
            })
            .unwrap_or_default();
        if targets.is_empty() {
            return true;
        }
        let backwards = if vk == 0x09 { mods().shift } else { matches!(vk, 0x25 | 0x26) };
        let index = a.bar.focus.and_then(|b| targets.iter().position(|t| *t == b));
        let index = match index {
            Some(i) if backwards => (i + targets.len() - 1) % targets.len(),
            Some(i) => (i + 1) % targets.len(),
            None if backwards => targets.len() - 1,
            None => 0,
        };
        self.active.as_mut().unwrap().bar.focus = Some(targets[index]);
        self.request_redraw();
        true
    }
}

/// A window of this very process (a bubble, «Настройки»), not the game or app the capture sits over.
fn own_window(hwnd: HWND) -> bool {
    if hwnd.is_invalid() {
        return false;
    }
    let mut pid = 0;
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
    }
    pid == std::process::id()
}

/// The tool a group button stands for: its last pick, a rectangle and an area blur at first.
pub(super) fn group_tool(bar: &BarState, menu: BarMenu) -> Option<kadr_doc::annotation::Tool> {
    match menu {
        BarMenu::Shapes => Some(bar.shape),
        BarMenu::Blur => Some(bar.blur),
        _ => None,
    }
}

/// `tool` belongs to the group behind `menu`.
pub(super) fn in_group(menu: BarMenu, tool: kadr_doc::annotation::Tool) -> bool {
    use kadr_doc::annotation::Tool;
    match menu {
        BarMenu::Shapes => matches!(tool, Tool::Line | Tool::Rectangle | Tool::Ellipse),
        BarMenu::Blur => matches!(tool, Tool::Blur | Tool::BlurEllipse | Tool::BlurBrush),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kadr_doc::annotation::Tool;

    #[test]
    fn group_buttons_stand_for_their_last_pick_and_know_their_tools() {
        let mut bar = BarState::default();
        assert_eq!(group_tool(&bar, BarMenu::Shapes), Some(Tool::Rectangle));
        assert_eq!(group_tool(&bar, BarMenu::Blur), Some(Tool::Blur));
        bar.select_tool(Tool::BlurBrush);
        assert_eq!(group_tool(&bar, BarMenu::Blur), Some(Tool::BlurBrush));
        assert_eq!(group_tool(&bar, BarMenu::More), None);
        assert!(in_group(BarMenu::Shapes, Tool::Ellipse) && !in_group(BarMenu::Shapes, Tool::Pen));
        assert!(in_group(BarMenu::Blur, Tool::BlurBrush) && !in_group(BarMenu::Blur, Tool::Rectangle));
    }
}
