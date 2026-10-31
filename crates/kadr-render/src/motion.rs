//! Time-based damped springs. No frame counter or fixed 60 Hz step: the analytic solution gives the
//! same pose at 60, 170 or 240 Hz, and retargeting keeps the current position and velocity.
//!
//! Curves follow the SwiftUI model (`response`, `dampingFraction`): critically damped springs only
//! settle, slightly underdamped ones carry the weight and small overshoot that make macOS motion feel
//! physical rather than tweened.

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Curve {
    /// Period of the undamped oscillation, seconds. Roughly how long the motion takes.
    pub response: f32,
    /// 1.0 — no overshoot; 0.8 — a soft ~1.5 % overshoot; lower bounces.
    pub damping: f32,
}

impl Curve {
    /// Panel and control geometry: quick, with a barely visible settle.
    pub const LAYOUT: Curve = Curve { response: 0.40, damping: 0.86 };
    /// Panel appearance: scale and lift with a soft pop.
    pub const POP: Curve = Curve { response: 0.36, damping: 0.76 };
    /// The selection pill and swatch ring gliding to a new item.
    pub const GLIDE: Curve = Curve { response: 0.34, damping: 0.80 };
    /// Opacity in.
    pub const FADE: Curve = Curve { response: 0.24, damping: 1.0 };
    /// Opacity out: leaving content gets out of the way before new content arrives.
    pub const FADE_OUT: Curve = Curve { response: 0.08, damping: 1.0 };
    /// Opacity of entering controls: arrives quickly, so a hand-over reads as one gesture.
    pub const FADE_IN: Curve = Curve { response: 0.18, damping: 1.0 };
    /// Hover responds at once and lets go slowly, like AppKit highlight.
    pub const HOVER_IN: Curve = Curve { response: 0.08, damping: 1.0 };
    pub const HOVER_OUT: Curve = Curve { response: 0.30, damping: 1.0 };
    /// Window highlight following the pointer: smooth but never lagging the hand.
    pub const TRACK: Curve = Curve { response: 0.16, damping: 1.0 };
    /// Screen dimming when the overlay opens.
    pub const DIM: Curve = Curve { response: 0.22, damping: 1.0 };
    /// A tapped control gives under the finger and springs back with a small rebound.
    pub const PRESS: Curve = Curve { response: 0.30, damping: 0.5 };

    fn omega(self) -> f32 {
        std::f32::consts::TAU / self.response.max(0.001)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Spring {
    origin: f32,
    velocity: f32,
    goal: f32,
    started: f64,
    curve: Curve,
    /// Settle threshold in the value's own units: pixels need 0.01, a scale factor far less.
    epsilon: f32,
}

impl Spring {
    /// Critically damped spring with angular frequency `omega`, kept for native chrome.
    pub fn new(value: f32, omega: f32) -> Self {
        Self::with(value, Curve { response: std::f32::consts::TAU / omega, damping: 1.0 })
    }

    pub fn with(value: f32, curve: Curve) -> Self {
        Self { origin: value, velocity: 0.0, goal: value, started: 0.0, curve, epsilon: 0.01 }
    }

    pub fn precise(mut self, epsilon: f32) -> Self {
        self.epsilon = epsilon;
        self
    }

    pub fn sample(&self, now: f64) -> (f32, f32) {
        let t = (now - self.started).max(0.0) as f32;
        let delta = self.origin - self.goal;
        if t > 4.0 * self.curve.response.max(0.05) + 1.0 {
            return (self.goal, 0.0);
        }
        let w = self.curve.omega();
        let z = self.curve.damping;
        let (value, velocity) = if z >= 1.0 {
            let b = self.velocity + w * delta;
            let decay = (-w * t).exp();
            (self.goal + (delta + b * t) * decay, (self.velocity - w * b * t) * decay)
        } else {
            let wd = w * (1.0 - z * z).sqrt();
            let b = (self.velocity + z * w * delta) / wd;
            let decay = (-z * w * t).exp();
            let (sin, cos) = (wd * t).sin_cos();
            (
                self.goal + decay * (delta * cos + b * sin),
                decay * ((b * wd - z * w * delta) * cos - (delta * wd + z * w * b) * sin),
            )
        };
        if (value - self.goal).abs() < self.epsilon && velocity.abs() < self.epsilon * 5.0 {
            (self.goal, 0.0)
        } else {
            (value, velocity)
        }
    }

    pub fn value(&self, now: f64) -> f32 {
        self.sample(now).0
    }

    pub fn target(&mut self, value: f32, now: f64) {
        self.target_at(value, now, now);
    }

    /// Retarget with another curve, e.g. hover in versus hover out.
    pub fn target_with(&mut self, value: f32, curve: Curve, now: f64) {
        if self.goal == value {
            return;
        }
        self.target_at(value, now, now);
        self.curve = curve;
    }

    /// Start moving at `start` (≥ `now`): staggered cascades hold their pose until their turn.
    pub fn target_at(&mut self, value: f32, now: f64, start: f64) {
        if self.goal == value {
            return;
        }
        (self.origin, self.velocity) = self.sample(now);
        self.goal = value;
        self.started = start.max(now);
    }

    /// A push of `velocity` from wherever the spring is now; it settles back to its goal. With
    /// `Curve::PRESS` a velocity of about 38 peaks near 1 after ~60 ms and rebounds ~16 %.
    pub fn kick(&mut self, velocity: f32, now: f64) {
        (self.origin, self.velocity) = self.sample(now);
        self.velocity += velocity;
        self.started = now;
    }

    pub fn snap(&mut self, value: f32) {
        self.origin = value;
        self.goal = value;
        self.velocity = 0.0;
    }

    pub fn goal(&self) -> f32 {
        self.goal
    }

    pub fn active(&self, now: f64) -> bool {
        let (v, s) = self.sample(now);
        v != self.goal || s != 0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    /// The tap on a toolbar button: gives about 1 within a frame or four, rebounds a little past
    /// rest (the swell that makes it feel physical), and is still within a second.
    #[test]
    fn a_kicked_press_gives_rebounds_and_settles() {
        let mut s = Spring::with(0.0, Curve::PRESS).precise(0.001);
        s.kick(38.0, 0.0);
        let samples: Vec<(f64, f32)> = (0..=60).map(|i| (i as f64 * 0.01, s.value(i as f64 * 0.01))).collect();
        let (at, peak) = samples.iter().copied().max_by(|a, b| a.1.total_cmp(&b.1)).unwrap();
        assert!((0.85..1.15).contains(&peak), "peak {peak}");
        assert!((0.03..0.1).contains(&at), "peak at {at}");
        let rebound = samples.iter().map(|(_, v)| *v).fold(0.0f32, f32::min);
        assert!((-0.3..-0.05).contains(&rebound), "rebound {rebound}");
        assert_eq!(s.value(1.0), 0.0);
        assert!(!s.active(1.0));
    }
    #[test]
    fn motion_has_the_same_position_at_sixty_one_seventy_and_two_forty_hz() {
        for curve in [Curve::LAYOUT, Curve::POP, Curve::FADE] {
            let mut results = Vec::new();
            for hz in [60, 170, 240] {
                let mut s = Spring::with(0.0, curve);
                s.target(48.0, 0.0);
                for frame in 0..hz / 5 {
                    let _ = s.value(frame as f64 / hz as f64);
                }
                results.push(s.value(0.2));
                assert!(s.value(0.05) > 0.0 && s.value(0.05) < 48.0);
                assert!(!s.active(3.0));
            }
            assert!(results.windows(2).all(|p| (p[0] - p[1]).abs() < 0.0001));
        }
    }
    #[test]
    fn interrupted_motion_keeps_position_and_velocity_and_does_not_queue_old_targets() {
        let mut s = Spring::new(0.0, 30.0);
        s.target(48.0, 0.0);
        let (value, velocity) = s.sample(0.06);
        s.target(8.0, 0.06);
        let (after, speed) = s.sample(0.06);
        assert!((value - after).abs() < 0.0001 && (velocity - speed).abs() < 0.0001);
        for i in 1..=1000 {
            s.target((i % 2) as f32, 0.06 + i as f64 * 0.001);
        }
        assert_eq!(s.value(2.0), 0.0);
        s.snap(9.0);
        assert_eq!(s.value(2.1), 9.0);
        assert!(!s.active(2.1));
    }
    #[test]
    fn underdamped_curves_overshoot_softly_and_critical_ones_never_do() {
        let peak = |curve| {
            let mut s = Spring::with(0.0, curve);
            s.target(100.0, 0.0);
            (0..600).map(|i| s.value(i as f64 / 600.0)).fold(0.0, f32::max)
        };
        let pop = peak(Curve::POP);
        assert!(pop > 100.5 && pop < 104.0, "pop peak {pop}");
        assert!(peak(Curve::GLIDE) < 103.0);
        assert!(peak(Curve::FADE) <= 100.0);
    }
    #[test]
    fn velocity_is_the_derivative_of_position_for_both_damping_regimes() {
        for curve in [Curve::GLIDE, Curve::FADE] {
            let mut s = Spring::with(0.0, curve);
            s.target(10.0, 0.0);
            let h = 1e-4;
            let (_, v) = s.sample(0.05);
            let slope = (s.value(0.05 + h) - s.value(0.05 - h)) / (2.0 * h as f32);
            assert!((v - slope).abs() < 0.5, "{v} vs {slope}");
        }
    }
    #[test]
    fn delayed_start_holds_the_pose_and_retargeting_switches_the_curve() {
        let mut s = Spring::with(0.0, Curve::FADE);
        s.target_at(1.0, 0.0, 0.1);
        assert_eq!(s.value(0.05), 0.0);
        assert!(s.active(0.05));
        assert!(s.value(0.2) > 0.3);
        let mut h = Spring::with(0.0, Curve::HOVER_IN);
        h.target_with(1.0, Curve::HOVER_IN, 0.0);
        let quick = h.value(0.03);
        h.target_with(0.0, Curve::HOVER_OUT, 1.0);
        assert!(quick > 0.4 && h.value(1.03) > 0.8);
    }
}
