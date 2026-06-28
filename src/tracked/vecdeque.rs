use std::collections::VecDeque;

use crate::registry;

/// A `VecDeque<T>` wrapper that records creation count and peak capacity.
pub struct TrackedVecDeque<T> {
    inner: VecDeque<T>,
    #[allow(dead_code)]
    name: &'static str,
    file: &'static str,
    line: u32,
    column: u32,
}

impl<T> TrackedVecDeque<T> {
    pub fn with_capacity_named(
        cap: usize,
        name: &'static str,
        file: &'static str,
        line: u32,
        column: u32,
    ) -> Self {
        registry::record_creation(name, file, line, column);
        Self {
            inner: VecDeque::with_capacity(cap),
            name,
            file,
            line,
            column,
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
        registry::record_sample(self.file, self.line, self.column, self.inner.capacity());
    }
}

impl<T> From<TrackedVecDeque<T>> for VecDeque<T> {
    fn from(mut tracked: TrackedVecDeque<T>) -> VecDeque<T> {
        registry::record_sample(
            tracked.file,
            tracked.line,
            tracked.column,
            tracked.inner.capacity(),
        );
        let inner = std::mem::take(&mut tracked.inner);
        std::mem::forget(tracked);
        inner
    }
}

impl<T> IntoIterator for TrackedVecDeque<T> {
    type Item = T;
    type IntoIter = std::collections::vec_deque::IntoIter<T>;

    fn into_iter(mut self) -> Self::IntoIter {
        registry::record_sample(self.file, self.line, self.column, self.inner.capacity());
        let inner = std::mem::take(&mut self.inner);
        std::mem::forget(self);
        inner.into_iter()
    }
}
