//! A fixed-size rolling window of recent audio. It never grows: older samples are overwritten,
//! so audio older than the window is gone. Nothing here is written to disk.

pub struct RollingBuffer {
    samples: Box<[f32]>,
    /// Index where the next sample is written.
    next: usize,
    len: usize,
}

impl RollingBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            samples: vec![0.0; capacity.max(1)].into_boxed_slice(),
            next: 0,
            len: 0,
        }
    }

    pub fn capacity(&self) -> usize {
        self.samples.len()
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn push(&mut self, mut audio: &[f32]) {
        let capacity = self.capacity();
        if audio.len() > capacity {
            audio = &audio[audio.len() - capacity..];
        }
        let first = audio.len().min(capacity - self.next);
        self.samples[self.next..self.next + first].copy_from_slice(&audio[..first]);
        self.samples[..audio.len() - first].copy_from_slice(&audio[first..]);
        self.next = (self.next + audio.len()) % capacity;
        self.len = (self.len + audio.len()).min(capacity);
    }

    /// Removes and returns the buffered audio, oldest first.
    pub fn take(&mut self) -> Vec<f32> {
        let start = (self.next + self.capacity() - self.len) % self.capacity();
        let mut audio = Vec::with_capacity(self.len);
        for offset in 0..self.len {
            audio.push(self.samples[(start + offset) % self.capacity()]);
        }
        self.clear();
        audio
    }

    /// Discards everything, overwriting the stored samples.
    pub fn clear(&mut self) {
        self.samples.fill(0.0);
        self.next = 0;
        self.len = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp(from: usize, to: usize) -> Vec<f32> {
        (from..to).map(|value| value as f32).collect()
    }

    #[test]
    fn keeps_only_the_most_recent_audio() {
        let mut buffer = RollingBuffer::new(5);
        buffer.push(&ramp(0, 3));
        buffer.push(&ramp(3, 8));
        assert_eq!(buffer.len(), 5);
        assert_eq!(buffer.take(), ramp(3, 8));
        assert_eq!(buffer.len(), 0);
    }

    #[test]
    fn a_large_push_keeps_its_tail() {
        let mut buffer = RollingBuffer::new(4);
        buffer.push(&ramp(0, 10));
        assert_eq!(buffer.take(), ramp(6, 10));
    }

    #[test]
    fn expired_audio_is_overwritten_and_memory_never_grows() {
        let mut buffer = RollingBuffer::new(16_000 * 5);
        for second in 0..60 {
            buffer.push(&vec![second as f32; 16_000]);
            assert!(buffer.len() <= buffer.capacity());
        }
        assert_eq!(buffer.capacity(), 16_000 * 5);
        let audio = buffer.take();
        assert_eq!(audio.len(), 16_000 * 5);
        assert!(audio.iter().all(|sample| *sample >= 55.0));
    }

    #[test]
    fn clearing_discards_everything() {
        let mut buffer = RollingBuffer::new(8);
        buffer.push(&ramp(0, 6));
        buffer.clear();
        assert!(buffer.take().is_empty());
        assert!(buffer.samples.iter().all(|sample| *sample == 0.0));
    }
}
