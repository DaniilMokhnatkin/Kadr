//! Тайминги горячего пути на QueryPerformanceCounter.

use std::sync::OnceLock;

use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};

pub fn now() -> i64 {
    let mut t = 0;
    unsafe {
        let _ = QueryPerformanceCounter(&mut t);
    }
    t
}

fn ticks_per_ms() -> f64 {
    static FREQ: OnceLock<f64> = OnceLock::new();
    *FREQ.get_or_init(|| {
        let mut f = 0;
        unsafe {
            let _ = QueryPerformanceFrequency(&mut f);
        }
        f as f64 / 1000.0
    })
}

pub fn ms(from: i64, to: i64) -> f64 {
    (to - from) as f64 / ticks_per_ms()
}

/// Один проход «хоткей → экран».
#[derive(Clone, Copy, Default)]
pub struct Timing {
    /// Клавиша → WM_HOTKEY в нашей очереди. По системным тикам, точность ~10–16 мс.
    pub queue_ms: u32,
    /// Часть `capture_ms`, ушедшая на создание дубликаторов (0 в прогретом режиме).
    pub dup_create_ms: f64,
    pub capture_ms: f64,
    pub render_ms: f64,
    pub show_ms: f64,
    /// Снятие маскировки → DwmFlush: кадр ушёл в композицию, то есть на экран.
    pub composed_ms: f64,
    /// SetForegroundWindow уже после показа — в итог не входит.
    pub focus_ms: f64,
}

impl Timing {
    /// Хоткей → кадр на экране.
    pub fn total(&self) -> f64 {
        self.capture_ms + self.render_ms + self.show_ms + self.composed_ms
    }
}

#[derive(Default)]
pub struct Stats {
    totals: Vec<f64>,
}

impl Stats {
    pub fn push(&mut self, total: f64) {
        self.totals.push(total);
    }

    pub fn len(&self) -> usize {
        self.totals.len()
    }

    pub fn is_empty(&self) -> bool {
        self.totals.is_empty()
    }

    pub fn percentile(&self, p: f64) -> f64 {
        if self.totals.is_empty() {
            return 0.0;
        }
        let mut sorted = self.totals.clone();
        sorted.sort_by(f64::total_cmp);
        let index = ((p / 100.0) * (sorted.len() - 1) as f64).round() as usize;
        sorted[index]
    }
}
