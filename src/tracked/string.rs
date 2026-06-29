/// A `String` wrapper that records creation count and capacity samples in the
/// global capacity-telemetry registry.
///
/// Compiled only when the `telemetry` feature is enabled; in off-feature mode
/// the `tstring!` macro expands directly to `::std::string::String::with_capacity(cap)`
/// with zero overhead.
///
/// # IntoIterator note
///
/// `String` does not implement `IntoIterator` directly — iteration yields `char`
/// values via `.chars()` which borrows the string.  Consuming iteration is
/// available as `.into_bytes().into_iter()` (yields `u8`).  Neither shape maps
/// naturally to an `IntoIterator` impl on `TrackedString` without committing to
/// one semantics over the other, so `IntoIterator` is intentionally omitted.
/// Use `into_inner()` first if you need to iterate by value.
pub struct TrackedString {
    inner: String,
    #[allow(dead_code)]
    name: &'static str,
    file: &'static str,
    line: u32,
    column: u32,
}

impl TrackedString {
    /// Create a new `TrackedString` with the given capacity and register the
    /// creation in the global registry.
    pub fn with_capacity_named(
        cap: usize,
        name: &'static str,
        file: &'static str,
        line: u32,
        column: u32,
    ) -> Self {
        crate::registry::record_creation(name, file, line, column);
        Self {
            inner: String::with_capacity(cap),
            name,
            file,
            line,
            column,
        }
    }

    /// Wrap an already-constructed `String` for capacity telemetry.
    ///
    /// Records creation in the registry; `inner` is moved into the wrapper
    /// as-is — no reallocation occurs.  The capacity sample is recorded at
    /// `Drop` (or `From`/`into_inner` conversion) as usual.
    ///
    /// This is the universal instrument entry-point used by Phase K of the
    /// `captrack-pgo instrument` pass.
    #[inline]
    pub fn wrap_from(
        inner: String,
        name: &'static str,
        file: &'static str,
        line: u32,
        column: u32,
    ) -> Self {
        crate::registry::record_creation(name, file, line, column);
        Self { inner, name, file, line, column }
    }
}

impl std::ops::Deref for TrackedString {
    type Target = String;
    fn deref(&self) -> &String {
        &self.inner
    }
}

impl std::ops::DerefMut for TrackedString {
    fn deref_mut(&mut self) -> &mut String {
        &mut self.inner
    }
}

impl Drop for TrackedString {
    fn drop(&mut self) {
        crate::registry::record_sample(self.file, self.line, self.column, self.inner.capacity());
    }
}

impl From<TrackedString> for String {
    fn from(tracked: TrackedString) -> String {
        crate::registry::record_sample(
            tracked.file,
            tracked.line,
            tracked.column,
            tracked.inner.capacity(),
        );
        // SAFETY: `tracked` is owned by us and will be forgotten on the next
        // line, so its Drop never runs.  `ptr::read` bit-copies `inner` out;
        // ownership moves to the returned value.
        let inner = unsafe { std::ptr::read(&tracked.inner) };
        std::mem::forget(tracked);
        inner
    }
}

impl crate::IntoInner for TrackedString {
    type Inner = String;
    #[inline]
    fn into_inner(self) -> String {
        String::from(self)
    }
}

impl crate::cap_inspect::CapInspect for TrackedString {
    #[inline]
    fn cap_inspect_at(&self, name: &'static str, file: &'static str, line: u32, column: u32) {
        crate::registry::record_initial(name, file, line, column, self.inner.capacity());
    }
}
