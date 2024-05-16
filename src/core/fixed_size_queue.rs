use std::collections::VecDeque;

#[derive(Clone)]
pub(crate) struct FixedSizeQueue<T> {
    queue: VecDeque<T>,
    capacity: usize,
}

impl<T> FixedSizeQueue<T>
where
    T: Copy + Into<f64> + std::iter::Sum<T>,
{
    pub(crate) fn new(capacity: usize) -> Self {
        FixedSizeQueue {
            queue: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    pub(crate) async fn push(&mut self, item: T) {
        if self.queue.len() == self.capacity {
            self.queue.pop_front();
        }
        self.queue.push_back(item);
    }

    async fn len(&self) -> usize {
        self.queue.len()
    }

    async fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    async fn pop(&mut self) -> Option<T> {
        self.queue.pop_front()
    }

    pub(crate) async fn average(&self) -> Option<f64> {
        if self.queue.is_empty() {
            None
        } else {
            let sum: f64 = self.queue.iter().copied().map(Into::into).sum();
            Some(sum / self.queue.len() as f64)
        }
    }
}
