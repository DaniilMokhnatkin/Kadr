//! Накапливаем изменение, а не очередь устаревших кадров. У каждого монитора своя готовность.
#[derive(Default)]
pub struct Frames {
    dirty: Vec<bool>,
}
impl Frames {
    pub fn request(&mut self, count: usize) {
        self.dirty.resize(count, false);
        self.dirty.fill(true);
    }
    pub fn pending(&self) -> Vec<usize> {
        self.dirty.iter().enumerate().filter_map(|(i, dirty)| dirty.then_some(i)).collect()
    }
    pub fn presented(&mut self, index: usize, animating: bool) {
        if let Some(dirty) = self.dirty.get_mut(index) {
            *dirty = animating;
        }
    }
    pub fn clear(&mut self) {
        self.dirty.clear();
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rapid_input_does_not_queue_frames_or_make_fast_screen_wait_for_slow_one() {
        let mut f = Frames::default();
        for _ in 0..1000 {
            f.request(2);
            f.presented(0, false);
            assert_eq!(f.pending(), vec![1]);
        }
        f.presented(1, false);
        assert!(f.pending().is_empty());
        f.request(2);
        f.request(1);
        assert_eq!(f.pending(), vec![0]);
        f.clear();
        assert!(f.pending().is_empty());
    }

    #[test]
    fn animation_keeps_waiting_without_mouse_messages_then_sleeps_when_done() {
        let mut frames = Frames::default();
        frames.request(2);
        frames.presented(1, false);
        for _ in 0..240 {
            assert_eq!(frames.pending(), vec![0]);
            frames.presented(0, true);
        }
        frames.presented(0, false);
        assert!(frames.pending().is_empty());
        frames.request(2);
        frames.presented(0, true);
        frames.clear();
        assert!(frames.pending().is_empty());
    }
}
