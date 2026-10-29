//! Time-based critically damped transitions. No frame counter or fixed 60 Hz step.

#[derive(Clone, Copy, Debug)]
pub struct Spring {
    origin: f32,
    velocity: f32,
    goal: f32,
    started: f64,
    omega: f32,
}
impl Spring {
    pub fn new(value: f32, omega: f32) -> Self {
        Self {
            origin: value,
            velocity: 0.0,
            goal: value,
            started: 0.0,
            omega,
        }
    }
    pub fn sample(&self, now: f64) -> (f32, f32) {
        let t = (now - self.started).max(0.0) as f32;
        if t >= 0.6 {
            return (self.goal, 0.0);
        }
        let delta = self.origin - self.goal;
        let b = self.velocity + self.omega * delta;
        let decay = (-self.omega * t).exp();
        let value = self.goal + (delta + b * t) * decay;
        let velocity = (self.velocity - self.omega * b * t) * decay;
        if (value - self.goal).abs() < 0.01 && velocity.abs() < 0.05 {
            (self.goal, 0.0)
        } else {
            (value, velocity)
        }
    }
    pub fn value(&self, now: f64) -> f32 {
        self.sample(now).0
    }
    pub fn target(&mut self, value: f32, now: f64) {
        if self.goal == value {
            return;
        }
        (self.origin, self.velocity) = self.sample(now);
        self.goal = value;
        self.started = now;
    }
    pub fn snap(&mut self, value: f32) {
        self.origin = value;
        self.goal = value;
        self.velocity = 0.0;
    }
    pub fn active(&self, now: f64) -> bool {
        let (v, s) = self.sample(now);
        v != self.goal || s != 0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn motion_has_the_same_position_at_sixty_one_seventy_and_two_forty_hz() {
        let mut results = Vec::new();
        for hz in [60, 170, 240] {
            let mut s = Spring::new(0.0, 30.0);
            s.target(48.0, 0.0);
            for frame in 0..hz / 5 {
                let _ = s.value(frame as f64 / hz as f64);
            }
            results.push(s.value(0.2));
            assert!(s.value(0.1) > 0.0 && s.value(0.1) < 48.0);
            assert!(!s.active(1.0));
        }
        assert!(results.windows(2).all(|p| (p[0] - p[1]).abs() < 0.0001));
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
}
