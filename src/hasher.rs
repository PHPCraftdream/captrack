// CapHasher — feature-selected default hasher (Axis 2A).
//
// Exactly one hasher feature may be active at a time.  Selecting more than one
// is a compile error (see the guard below).  When no hasher feature is enabled
// the default is `std::collections::hash_map::RandomState` — the safe OSS
// default that is not vulnerable to hash-flooding attacks from untrusted input.
//
// Users who want a faster hasher opt in explicitly:
//   [features]
//   captrack = { version = "...", features = ["fxhash"] }

// ── Guard: at most one hasher feature ────────────────────────────────────────

#[cfg(all(feature = "fxhash", feature = "ahash"))]
compile_error!(
    "captrack: select at most one default-hasher feature (fxhash / ahash / foldhash / rustc-hash)"
);

#[cfg(all(feature = "fxhash", feature = "foldhash"))]
compile_error!(
    "captrack: select at most one default-hasher feature (fxhash / ahash / foldhash / rustc-hash)"
);

#[cfg(all(feature = "fxhash", feature = "rustc-hash"))]
compile_error!(
    "captrack: select at most one default-hasher feature (fxhash / ahash / foldhash / rustc-hash)"
);

#[cfg(all(feature = "ahash", feature = "foldhash"))]
compile_error!(
    "captrack: select at most one default-hasher feature (fxhash / ahash / foldhash / rustc-hash)"
);

#[cfg(all(feature = "ahash", feature = "rustc-hash"))]
compile_error!(
    "captrack: select at most one default-hasher feature (fxhash / ahash / foldhash / rustc-hash)"
);

#[cfg(all(feature = "foldhash", feature = "rustc-hash"))]
compile_error!(
    "captrack: select at most one default-hasher feature (fxhash / ahash / foldhash / rustc-hash)"
);

// ── CapHasher type alias ──────────────────────────────────────────────────────

/// The default build-hasher used by all hash-keyed `captrack` macros.
///
/// Resolved at compile time based on which feature flag (if any) is enabled:
///
/// | Feature      | `CapHasher`                                              |
/// |--------------|----------------------------------------------------------|
/// | *(none)*     | `std::collections::hash_map::RandomState` (DoS-safe)     |
/// | `fxhash`     | `fxhash::FxBuildHasher`                                  |
/// | `ahash`      | `ahash::RandomState`                                     |
/// | `foldhash`   | `foldhash::fast::RandomState`                            |
/// | `rustc-hash` | `rustc_hash::FxBuildHasher`                              |
///
/// # Selecting a faster hasher
///
/// ```toml
/// # Cargo.toml
/// [dependencies]
/// captrack = { version = "0.1", features = ["ahash"] }
/// ```
///
/// # Per-call override (Axis 2B)
///
/// Use the `;`-arm of any hash macro to override the hasher for a single
/// call-site without changing `CapHasher` globally:
///
/// ```ignore
/// use captrack::tmap;
/// // uses CapHasher (the workspace default):
/// let m1 = tmap!("my/map", 16);
/// // uses ahash for this one call:
/// let m2 = tmap!("my/special", 8; ahash::RandomState::new());
/// ```
#[cfg(not(any(
    feature = "fxhash",
    feature = "ahash",
    feature = "foldhash",
    feature = "rustc-hash"
)))]
pub type CapHasher = std::collections::hash_map::RandomState;

#[cfg(feature = "fxhash")]
pub type CapHasher = fxhash::FxBuildHasher;

#[cfg(feature = "ahash")]
pub type CapHasher = ahash::RandomState;

#[cfg(feature = "foldhash")]
pub type CapHasher = foldhash::fast::RandomState;

#[cfg(feature = "rustc-hash")]
pub type CapHasher = rustc_hash::FxBuildHasher;
