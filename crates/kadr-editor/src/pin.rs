//! Геометрия закреплённого снимка, независимо от Win32 и текущего рабочего стола.
#[derive(Clone, Debug)]
pub struct Pin {
    pub width: u32,
    pub height: u32,
    pub zoom: f64,
    pub opacity: u8,
}
impl Pin {
    pub fn new(width: u32, height: u32, available: (i32, i32)) -> Self {
        let mut pin = Self {
            width: width.max(1),
            height: height.max(1),
            zoom: 1.0,
            opacity: 255,
        };
        pin.fit(available);
        pin
    }
    pub fn size(&self) -> (i32, i32) {
        (
            ((self.width as f64 * self.zoom).round() as i32).max(1),
            ((self.height as f64 * self.zoom).round() as i32).max(1),
        )
    }
    pub fn fit(&mut self, available: (i32, i32)) {
        let max = (available.0.max(1) as f64 / self.width as f64).min(available.1.max(1) as f64 / self.height as f64);
        self.zoom = self.zoom.min(max).max(1.0 / self.width.max(self.height) as f64);
    }
    pub fn zoom_by(&mut self, steps: i32, available: (i32, i32)) {
        self.zoom = (self.zoom * 1.1f64.powi(steps.clamp(-20, 20))).clamp(0.05, 8.0);
        self.fit(available);
    }
    pub fn original(&mut self, available: (i32, i32)) {
        self.zoom = 1.0;
        self.fit(available);
    }
    pub fn fade(&mut self, steps: i32) {
        self.opacity = (self.opacity as i32 + steps.clamp(-20, 20) * 15).clamp(77, 255) as u8;
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fit_large_images_keeps_ratio_and_zoom_never_overflows_workspace() {
        let mut pin = Pin::new(3840, 2160, (900, 600));
        assert_eq!(pin.size(), (900, 506));
        pin.zoom_by(20, (900, 600));
        assert!(pin.size().0 <= 900 && pin.size().1 <= 600);
        for _ in 0..100 {
            pin.zoom_by(-20, (900, 600));
        }
        assert!(pin.size().0 > 0 && pin.size().1 > 0);
        pin.original((4000, 3000));
        assert_eq!(pin.size(), (3840, 2160));
    }
    #[test]
    fn opacity_has_a_readable_lower_bound() {
        let mut pin = Pin::new(100, 100, (500, 500));
        pin.fade(-100);
        assert_eq!(pin.opacity, 77);
        pin.fade(100);
        assert_eq!(pin.opacity, 255);
    }
}
