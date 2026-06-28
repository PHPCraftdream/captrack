use std::collections::VecDeque;

use crate::registry;

/// A `VecDeque<T>` wrapper that records creation count and peak capacity.
pub struct TrackedVecDeque<T> {
    inner: VecDeque<T>,
    name: &'static str,
}

impl<T> TrackedVecDeque<T> {
    pub fn with_capacity_named(cap: usize, name: &'static str) -> Self {
        registry::record_creation(name);
        Self {
            inner: VecDeque::with_capacity(cap),
            name,
        }
    }
}

impl<T> std::ops::Deref for TrackedVecDeque<T> {
    type Target = VecDeque<T>;
    fn deref(&self) -> &VecDeque<T> {
        &self.inner
    }
}

impl<T> std::ops::DerefMut for TrackedVecDeque<T> {
    fn deref_mut(&mut self) -> &mut VecDeque<T> {
        &mut self.inner
    }
}

impl<T> Drop for TrackedVecDeque<T> {
    fn drop(&mut self) {
        registry::record_peak(self.name, self.inner.capacity());
    }
}

impl<T> IntoIterator for TrackedVecDeque<T> {
    type Item = T;
    type IntoIter = std::collections::vec_deque::IntoIter<T>;

    fn into_iter(mut self) -> Self::IntoIter {
        registry::record_peak(self.name, self.inner.capacity());
        let inner = std::mem::take(&mut self.inner);
        std::mem::forget(self);
        inner.into_iter()
    }
}
