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
        self.dirty
            .iter()
            .enumerate()
            .filter_map(|(i, dirty)| dirty.then_some(i))
            .collect()
    }
    pub fn complete(&mut self, index: usize) {
        if let Some(dirty) = self.dirty.get_mut(index) {
            *dirty = false;
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
            f.complete(0);
            assert_eq!(f.pending(), vec![1]);
        }
        f.complete(1);
        assert!(f.pending().is_empty());
        f.request(2);
        f.request(1);
        assert_eq!(f.pending(), vec![0]);
        f.clear();
        assert!(f.pending().is_empty());
    }
}
