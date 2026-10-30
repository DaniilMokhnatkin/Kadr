use super::*;
use windows::Win32::Foundation::WAIT_OBJECT_0;
struct NativeTimer(Option<HANDLE>);

fn effect_needs_frames(panel: bool, leaving: bool, selected_here: bool, smart: bool, ants: bool) -> bool {
    panel || leaving || (selected_here && (smart || ants))
}
impl NativeTimer {
    fn new() -> Self {
        use windows::Win32::System::Threading::*;
        let h = unsafe {
            CreateWaitableTimerExW(
                None,
                windows::core::PCWSTR::null(),
                CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
                TIMER_ALL_ACCESS.0,
            )
        }
        .ok();
        crate::chrome::timer_available(h.is_some());
        Self(h)
    }
    fn arm(&self, delay: std::time::Duration) -> Option<HANDLE> {
        let h = self.0?;
        let due = -((delay.as_nanos() / 100).max(1).min(i64::MAX as u128) as i64);
        unsafe { windows::Win32::System::Threading::SetWaitableTimer(h, &due, 0, None, None, false) }.ok().map(|()| h)
    }
}
impl Drop for NativeTimer {
    fn drop(&mut self) {
        if let Some(h) = self.0 {
            unsafe {
                let _ = windows::Win32::Foundation::CloseHandle(h);
            }
        }
    }
}
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
                // An animated surface asks for its own next GPU slot; no 16 ms / 60 Hz timer.
                self.frames.borrow_mut().presented(index, self.overlay_needs_frames(index));
            }
        }
    }
    pub(super) fn overlay_needs_frames(&self, index: usize) -> bool {
        let Some(overlay) = self.overlays.get(index) else { return false };
        // The hovered window already has an animated border before the selection is committed.
        let selected_here = self.active.as_ref().and_then(|a| a.session.highlight()).is_some_and(|r| {
            r.intersect(&DocRect::new(overlay.rect.left, overlay.rect.top, overlay.rect.right, overlay.rect.bottom))
                .is_some()
        });
        effect_needs_frames(
            overlay.animations_pending(),
            self.exit.is_some(),
            selected_here,
            self.smart_needs_frames(),
            self.config.appearance.marching_ants && kadr_render::theme::motion_enabled(),
        )
    }
    fn frame_waits(&self) -> (Vec<HANDLE>, bool) {
        let pending = self.frames.borrow().pending();
        let handles =
            pending.iter().filter_map(|i| self.overlays.get(*i).and_then(Overlay::latency_handle)).collect::<Vec<_>>();
        let fallback = pending
            .iter()
            .any(|i| self.overlays.get(*i).is_some_and(|o| o.latency_handle().is_none() || o.needs_retry()));
        (handles, fallback)
    }
}
/// Ввод обрабатывается, пока DXGI ждёт следующий кадр. Медленный монитор не задерживает быстрый.
pub(super) unsafe fn message_loop() {
    unsafe {
        let mut woken = None;
        let native_timer = NativeTimer::new();
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
                a.tick_exit();
                a.tick_smart();
                a.draw_ready(woken.take());
                waits = a.frame_waits();
            });
            if let Some(delay) = crate::chrome::tick()
                && let Some(h) = native_timer.arm(delay)
            {
                waits.0.push(h);
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scene_effects_keep_their_monitor_awake_after_the_panel_settles() {
        assert!(effect_needs_frames(false, false, true, false, true));
        assert!(effect_needs_frames(false, false, true, true, false));
        assert!(effect_needs_frames(false, true, false, false, false));
        assert!(!effect_needs_frames(false, false, false, true, true));
        assert!(!effect_needs_frames(false, false, true, false, false));
        assert!(effect_needs_frames(true, false, false, false, false));
    }
    #[test]
    fn native_animation_deadline_is_a_waitable_signal_without_a_global_timer_resolution_change() {
        let before = unsafe { GetForegroundWindow() };
        let timer = NativeTimer::new();
        if let Some(h) = timer.arm(std::time::Duration::from_millis(2)) {
            assert_eq!(unsafe { windows::Win32::System::Threading::WaitForSingleObject(h, 100) }, WAIT_OBJECT_0);
        }
        assert_eq!(unsafe { GetForegroundWindow() }, before);
    }
}
