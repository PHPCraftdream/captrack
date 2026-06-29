use bytes::{Bytes, BytesMut};

use crate::registry;
use crate::IntoInner;

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

    /// Wrap an already-constructed `BytesMut` for capacity telemetry.
    ///
    /// Records creation in the registry; `inner` is moved as-is.
    /// Capacity sample recorded at `Drop` as usual.
    #[inline]
    pub fn wrap_from(
        inner: BytesMut,
        name: &'static str,
        file: &'static str,
        line: u32,
        column: u32,
    ) -> Self {
        registry::record_creation(name, file, line, column);
        Self { inner, name, file, line, column }
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

impl From<TrackedBytesMut> for BytesMut {
    fn from(tracked: TrackedBytesMut) -> BytesMut {
        registry::record_sample(
            tracked.file,
            tracked.line,
            tracked.column,
            tracked.inner.capacity(),
        );
        // SAFETY: `tracked` is owned and forgotten below; ptr::read bit-copies `inner`.
        let inner = unsafe { std::ptr::read(&tracked.inner) };
        std::mem::forget(tracked);
        inner
    }
}

impl IntoInner for TrackedBytesMut {
    type Inner = BytesMut;
    #[inline]
    fn into_inner(self) -> BytesMut {
        BytesMut::from(self)
    }
}

impl TrackedBytesMut {
    /// Consumes the buffer, returning an immutable `Bytes` handle.
    ///
    /// Equivalent to `BytesMut::freeze(self)` but takes ownership of the
    /// tracker so the final-capacity sample is recorded before the inner
    /// buffer is sealed.
    #[inline]
    pub fn freeze(self) -> Bytes {
        registry::record_sample(self.file, self.line, self.column, self.inner.capacity());
        // SAFETY: `self` is owned and forgotten; ptr::read bit-copies `inner`.
        let inner = unsafe { std::ptr::read(&self.inner) };
        std::mem::forget(self);
        inner.freeze()
    }
}
