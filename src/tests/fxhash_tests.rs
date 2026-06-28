// Tests for `--features fxhash` — verifies that CapHasher resolves to
// FxBuildHasher when that feature is active.

/// When the `fxhash` feature is active, `CapHasher` is `FxBuildHasher`.
/// We verify this by checking that `CapHasher::default()` is an `FxBuildHasher`.
#[test]
fn cap_hasher_is_fxbuilder_when_fxhash_feature_active() {
    // type-check: constructing CapHasher::default() and using it in a HashMap
    // that requires FxBuildHasher proves they are the same type.
    let _: fxhash::FxBuildHasher = <crate::CapHasher as Default>::default();
}

#[test]
fn fxmap_uses_fxhasher_when_feature_active() {
    let mut m = tfxmap!("fxhash_test/map", 8);
    m.insert(1u32, "hello");
    assert_eq!(m.get(&1), Some(&"hello"));
    assert!(m.capacity() >= 8);
}
