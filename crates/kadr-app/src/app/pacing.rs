use super::*;
use windows::Win32::Foundation::WAIT_OBJECT_0;
impl App {
    pub(super) fn request_redraw(&self) {
        if self.active.is_some() {
            self.frames.borrow_mut().request(self.overlays.len());
        }
    }
    fn draw_ready(&self, woken: Option<HANDLE>) {
        if self.active.is_none() {
            self.frames.borrow_mut().clear();
            return;
        }
        let pending = self.frames.borrow().pending();
        for index in pending {
            let Some(overlay) = self.overlays.get(index) else {
                continue;
            };
            if (overlay.latency_handle().is_some() && overlay.latency_handle() == woken || overlay.ready())
                && self.render_scene(Some(index), true)
            {
                self.frames.borrow_mut().complete(index);
            }
        }
    }
    fn frame_waits(&self) -> (Vec<HANDLE>, bool) {
        let pending = self.frames.borrow().pending();
        let handles = pending
            .iter()
            .filter_map(|i| self.overlays.get(*i).and_then(Overlay::latency_handle))
            .collect::<Vec<_>>();
        let fallback = pending.iter().any(|i| {
            self.overlays
                .get(*i)
                .is_some_and(|o| o.latency_handle().is_none() || o.needs_retry())
        });
        (handles, fallback)
    }
}
/// Ввод обрабатывается, пока DXGI ждёт следующий кадр. Медленный монитор не задерживает быстрый.
pub(super) unsafe fn message_loop() {
    unsafe {
        let mut woken = None;
        'messages: loop {
            let mut msg = MSG::default();
            for _ in 0..64 {
                if !PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    break;
                }
                if msg.message == WM_QUIT {
                    break 'messages;
                }
                if crate::recent_window::dialog_message(&msg) || crate::pin_window::dialog_message(&msg) {
                    continue;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            let mut waits = (Vec::new(), false);
            with_app(|a| {
                a.draw_ready(woken.take());
                waits = a.frame_waits();
            });
            let result = MsgWaitForMultipleObjectsEx(
                Some(&waits.0),
                if waits.1 { 1 } else { u32::MAX },
                QS_ALLINPUT,
                MWMO_INPUTAVAILABLE,
            );
            let index = result.0.wrapping_sub(WAIT_OBJECT_0.0) as usize;
            if index < waits.0.len() {
                woken = Some(waits.0[index]);
            }
        }
    }
}
