//! Fixed-capacity sample history. Ordered oldest to newest so a renderer can
//! take the tail without reversing.

#[derive(Debug, Clone)]
pub struct History {
    capacity: usize,
    values: Vec<f64>,
}

impl History {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            values: Vec::with_capacity(capacity),
        }
    }

    pub fn push(&mut self, v: f64) {
        if self.capacity == 0 {
            return;
        }
        if self.values.len() == self.capacity {
            self.values.remove(0);
        }
        self.values.push(v);
    }

    pub fn values(&self) -> &[f64] {
        &self.values
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
}

impl Default for History {
    fn default() -> Self {
        History::new(crate::layout::HISTORY_CAPACITY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_most_recent_samples_and_drops_the_oldest() {
        let mut h = History::new(3);
        for v in [1.0, 2.0, 3.0, 4.0] {
            h.push(v);
        }
        assert_eq!(h.values(), &[2.0, 3.0, 4.0]);
    }

    #[test]
    fn starts_empty_and_grows_to_capacity() {
        let mut h = History::new(4);
        assert_eq!(h.len(), 0);
        h.push(1.0);
        assert_eq!(h.len(), 1);
        for v in [2.0, 3.0, 4.0, 5.0] {
            h.push(v);
        }
        assert_eq!(h.len(), 4);
    }

    #[test]
    fn zero_capacity_never_panics() {
        let mut h = History::new(0);
        h.push(1.0);
        assert_eq!(h.len(), 0);
    }
}
