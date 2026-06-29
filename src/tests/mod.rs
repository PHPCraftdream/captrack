// Test manifest — re-exports only.
//
// Note: declare_collections! tests live in tests/declare_collections.rs
// (integration tests) because the generated macros call `::captrack::t*!`
// with absolute paths that only resolve from outside the crate.

pub mod cap_inspect_tests;
pub mod off_feature_tests;
pub mod per_call_override_tests;

#[cfg(feature = "telemetry")]
pub mod on_feature_tests;

#[cfg(feature = "telemetry")]
pub mod smallvec_tests;

#[cfg(feature = "telemetry")]
pub mod wrap_from_tests;

#[cfg(feature = "fxhash")]
pub mod fxhash_tests;
