use super::*;
use kadr_render::motion::{Curve, Spring};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    time::{Duration, Instant},
};

struct State {
    spring: Spring,
    target: f32,
    epoch: Instant,
    next: Instant,
    period: Duration,
    dirty: bool,
}
thread_local! {
    static STATES:RefCell<HashMap<isize,State>>=RefCell::new(HashMap::new());
    static TIMER_AVAILABLE:Cell<bool>=const {Cell::new(true)};
}
fn period(hwnd: HWND) -> Duration {
    unsafe {
        let mut info = MONITORINFOEXW {
            monitorInfo: MONITORINFO { cbSize: size_of::<MONITORINFOEXW>() as u32, ..Default::default() },
            ..Default::default()
        };
        let mut mode = DEVMODEW { dmSize: size_of::<DEVMODEW>() as u16, ..Default::default() };
        if GetMonitorInfoW(MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST), &mut info as *mut _ as *mut MONITORINFO)
            .as_bool()
            && EnumDisplaySettingsW(windows::core::PCWSTR(info.szDevice.as_ptr()), ENUM_CURRENT_SETTINGS, &mut mode)
                .as_bool()
            && mode.dmDisplayFrequency > 1
        {
            return Duration::from_secs_f64(1.0 / mode.dmDisplayFrequency as f64);
        }
    }
    Duration::from_secs_f64(1.0 / 170.0)
}
pub(super) fn begin(hwnd: HWND, value: bool) {
    STATES.with(|states| {
        let mut states = states.borrow_mut();
        let now = Instant::now();
        let s = states.entry(hwnd.0 as isize).or_insert_with(|| State {
            spring: Spring::with(0.0, Curve::HOVER_IN).precise(0.002),
            target: 0.0,
            epoch: now,
            next: now,
            period: period(hwnd),
            dirty: false,
        });
        let value = f32::from(value);
        if s.target != value {
            // A pin/recent window may have moved from the 60 Hz screen to the 170 Hz screen.
            s.period = period(hwnd);
            s.target = value;
            // Same feel as the toolbar: lights up at once, lets go slowly.
            let curve = if value > 0.5 { Curve::HOVER_IN } else { Curve::HOVER_OUT };
            s.spring.target_with(value, curve, s.epoch.elapsed().as_secs_f64());
            s.dirty = true;
            s.next = now;
            unsafe {
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
        }
    });
}
pub fn progress(hwnd: HWND, value: bool) -> f32 {
    if hwnd.is_invalid() {
        return f32::from(value);
    }
    begin(hwnd, value);
    STATES.with(|states| {
        let mut states = states.borrow_mut();
        let s = states.get_mut(&(hwnd.0 as isize)).unwrap();
        if !kadr_render::theme::motion_enabled() || !TIMER_AVAILABLE.with(Cell::get) {
            s.spring.snap(s.target);
            s.dirty = false;
        }
        s.next = Instant::now() + s.period;
        s.spring.value(s.epoch.elapsed().as_secs_f64()).clamp(0.0, 1.0)
    })
}
pub(super) fn remove(hwnd: HWND) {
    STATES.with(|s| {
        s.borrow_mut().remove(&(hwnd.0 as isize));
    });
}
pub fn timer_available(value: bool) {
    TIMER_AVAILABLE.with(|v| v.set(value));
}
/// Returns the next native-control frame deadline; static/hidden controls cause no wakeups.
pub fn tick() -> Option<Duration> {
    STATES.with(|states| {
        let mut next = None;
        let now = Instant::now();
        for (handle, s) in states.borrow_mut().iter_mut() {
            if !s.dirty {
                continue;
            }
            let hwnd = HWND(*handle as *mut _);
            if !unsafe { IsWindowVisible(hwnd) }.as_bool() {
                s.spring.snap(s.target);
                s.dirty = false;
                continue;
            }
            if now >= s.next {
                unsafe {
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
                s.next = now + s.period;
                if !s.spring.active(s.epoch.elapsed().as_secs_f64()) {
                    s.dirty = false;
                }
            }
            if s.dirty {
                let delay = s.next.saturating_duration_since(now);
                next = Some(next.map_or(delay, |v: Duration| v.min(delay)));
            }
        }
        next
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hidden_native_controls_do_not_start_an_idle_loop_and_destroy_releases_motion_state() {
        unsafe {
            let foreground = GetForegroundWindow();
            let window = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                windows::core::w!("BUTTON"),
                windows::core::w!("Motion fixture"),
                WS_POPUP,
                0,
                0,
                120,
                40,
                None,
                None,
                None,
                None,
            )
            .unwrap();
            super::super::track(window);
            let _ = progress(window, true);
            assert!(tick().is_none());
            assert!(!IsWindowVisible(window).as_bool());
            assert_eq!(GetForegroundWindow(), foreground);
            DestroyWindow(window).unwrap();
            STATES.with(|s| assert!(!s.borrow().contains_key(&(window.0 as isize))));
            assert!(tick().is_none());
            assert_eq!(GetForegroundWindow(), foreground);
        }
    }
}
