use super::*;
impl App {
    pub(super) fn pin(&mut self) {
        if let Some(a) = &mut self.active {
            a.drawing.finish_gesture();
            a.drawing.finish_text();
        }
        self.schedule_draft();
        let Some(image) = self.selected_image() else { return };
        self.pins.retain(crate::pin_window::Window::alive);
        if self.pins.len() >= 20 {
            self.notify(
                "Открыто 20 закреплённых снимков",
                "Закройте один из снимков, чтобы закрепить новый.",
            );
            return;
        }
        match crate::pin_window::Window::new(
            self.instance,
            self.hwnd,
            WM_APP_PIN,
            Arc::new(image),
            self.icons.small(),
        ) {
            Ok(pin) => {
                self.close_session();
                pin.show();
                self.pins.push(pin);
            }
            Err(e) => self.notify("Не удалось закрепить снимок", &e.message()),
        }
    }
    pub(super) fn on_pin(&mut self) {
        for request in crate::pin_window::requests() {
            match request {
                crate::pin_window::Request::Copy(image) => self.copy_image(image),
                crate::pin_window::Request::Recent => self.show_recent(),
            }
        }
        self.pins.retain(crate::pin_window::Window::alive);
    }
}
