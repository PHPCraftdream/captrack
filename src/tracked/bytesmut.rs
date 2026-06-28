use bytes::BytesMut;

use crate::registry;

/// A `BytesMut` wrapper that records creation count and peak capacity.
pub struct TrackedBytesMut {
    inner: BytesMut,
    name: &'static str,
}

impl TrackedBytesMut {
    pub fn with_capacity_named(cap: usize, name: &'static str) -> Self {
        registry::record_creation(name);
        Self {
            inner: BytesMut::with_capacity(cap),
            name,
        }
    }
}

impl std::ops::Deref for TrackedBytesMut {
    type Target = BytesMut;
    fn deref(&self) -> &BytesMut {
        &self.inner
    }
}

impl std::ops::DerefMut for TrackedBytesMut {
    fn deref_mut(&mut self) -> &mut BytesMut {
        &mut self.inner
    }
}

impl Drop for TrackedBytesMut {
    fn drop(&mut self) {
        registry::record_peak(self.name, self.inner.capacity());
    }
}
