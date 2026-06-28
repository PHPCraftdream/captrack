use bytes::BytesMut;

use crate::registry;

/// A `BytesMut` wrapper that records creation count and peak capacity.
pub struct TrackedBytesMut {
    inner: BytesMut,
    #[allow(dead_code)]
    name: &'static str,
    file: &'static str,
    line: u32,
    column: u32,
}

impl TrackedBytesMut {
    pub fn with_capacity_named(
        cap: usize,
        name: &'static str,
        file: &'static str,
        line: u32,
        column: u32,
    ) -> Self {
        registry::record_creation(name, file, line, column);
        Self {
            inner: BytesMut::with_capacity(cap),
            name,
            file,
            line,
            column,
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
        registry::record_sample(self.file, self.line, self.column, self.inner.capacity());
    }
}
