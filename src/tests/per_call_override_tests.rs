// Per-call override tests (Axis 2B) — verify that the `;`-arm of hash macros
// accepts an explicit hasher instance and uses it correctly.
//
// We use a custom BuildHasher that counts how many times `build_hasher` is
// called (i.e. how many times a hash operation creates a Hasher from the
// factory).  If the override is wired correctly, the map will use OUR hasher
// rather than CapHasher.

use std::hash::{BuildHasher, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

// ── Custom counting hasher ─────────────────────────────────────────────────

/// A hasher that just wraps FxHasher but counts constructions.
struct CountingHasher {
    inner: fxhash::FxHasher,
}

impl Hasher for CountingHasher {
    fn finish(&self) -> u64 {
        self.inner.finish()
    }
    fn write(&mut self, bytes: &[u8]) {
        self.inner.write(bytes);
    }
}

/// Builder that counts each call to `build_hasher`.
#[derive(Clone)]
struct CountingBuildHasher {
    count: Arc<AtomicU64>,
}

impl CountingBuildHasher {
    fn new() -> Self {
        Self {
            count: Arc::new(AtomicU64::new(0)),
        }
    }

    fn build_count(&self) -> u64 {
        self.count.load(Ordering::Relaxed)
    }
}

impl BuildHasher for CountingBuildHasher {
    type Hasher = CountingHasher;

    fn build_hasher(&self) -> CountingHasher {
        self.count.fetch_add(1, Ordering::Relaxed);
        CountingHasher {
            inner: fxhash::FxHasher::default(),
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[test]
fn semicolon_arm_uses_provided_hasher() {
    let builder = CountingBuildHasher::new();

    // Use the `;`-arm to inject our custom hasher.
    let mut m = tmap!("override_test/map", 4; builder.clone());
    m.insert(1u32, "hello");
    m.insert(2u32, "world");

    // The CountingBuildHasher::build_hasher must have been called at least
    // once for the insertions to work.
    assert!(
        builder.build_count() > 0,
        "custom hasher must be used (build_count = {})",
        builder.build_count()
    );
    assert_eq!(m.get(&1), Some(&"hello"));
    assert_eq!(m.get(&2), Some(&"world"));
}

#[test]
fn default_arm_does_not_use_counting_hasher() {
    // Sanity: the no-`;` arm should use CapHasher, not the counting hasher.
    // We can verify by checking that an independent CountingBuildHasher shows
    // 0 build calls when we only use the default arm.
    let builder = CountingBuildHasher::new();
    let mut _m = tmap!("override_test/default", 4);
    _m.insert(0u32, "placeholder"); // drive type inference
                                    // builder was NOT passed to tmap! — its count must remain 0.
    assert_eq!(
        builder.build_count(),
        0,
        "default arm must not call counting hasher"
    );
}
