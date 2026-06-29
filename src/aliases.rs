//! Off-feature type aliases — symmetry with the `tracked` module so that
//! consumer code can refer to `TrackedVec<T>` etc. in BOTH feature modes
//! and the source is identical.
//!
//! # Design decision
//!
//! In on-feature mode `TrackedVec<T>` is a `pub struct` defined in
//! `tracked::vec`.  Here, off-feature, it is `pub type TrackedVec<T> = Vec<T>`
//! — same identifier, same surface.
//!
//! ## Optional dependencies
//!
//! The standard-library types (`Vec`, `VecDeque`, `BTreeMap`, `BTreeSet`,
//! `HashMap`, `HashSet`) are always available.
//!
//! The optional third-party types (`bytes::BytesMut`, `indexmap::IndexMap`,
//! `indexmap::IndexSet`, `dashmap::DashMap`, `scc::HashMap`, `scc::HashSet`,
//! `scc::TreeIndex`) require the consumer to add those crates as *direct*
//! dependencies.  To advertise which aliases are available, captrack exposes
//! mirror feature flags that only pull in the dep (no telemetry overhead):
//!
//! * `bytes`    → `TrackedBytesMut`
//! * `indexmap` → `TrackedIndexMap`, `TrackedIndexSet`
//! * `dashmap`  → `TrackedDashMap`
//! * `scc`      → `TrackedSccHashMap`, `TrackedSccHashSet`, `TrackedSccTreeIndex`
//!
//! A consumer wishing to use `TrackedBytesMut` off-feature adds
//! `captrack = { features = ["bytes"] }` (no `telemetry`).
//!
//! When `telemetry` IS active these aliases are never compiled — the real
//! `TrackedX` structs from `tracked::` win.
//!
//! ## Hasher default consistency
//!
//! Aliases for hash types (`HashMap`, `HashSet`, `IndexMap`, `IndexSet`,
//! `DashMap`, `scc::HashMap`, `scc::HashSet`) default `S = CapHasher`.  This
//! matches the on-feature `Tracked*` struct defaults, keeping the type
//! signatures identical across both modes.
//!
//! Without any hasher feature, `CapHasher == RandomState` and the alias is
//! identical to `std::HashMap<K, V>` etc.
//!
//! **Caveat**: when the consumer enables `fxhash` / `ahash` / `foldhash` /
//! `rustc-hash`, `CapHasher` changes to the fast hasher and the alias
//! silently changes its default `S` relative to a hand-written bare
//! `std::HashMap<K, V>` (which always uses `RandomState`).  Mixing the alias
//! with bare collections in the same scope then needs explicit type
//! annotations such as `let m: HashMap<_, _, _> = ...` to disambiguate the
//! hasher parameter.

#![cfg(not(feature = "telemetry"))]
#![allow(clippy::disallowed_types)] // mirroring std/third-party types is the whole point

// ── Always-available std types ───────────────────────────────────────────────

pub type TrackedVec<T> = std::vec::Vec<T>;
pub type TrackedVecDeque<T> = std::collections::VecDeque<T>;
pub type TrackedBTreeMap<K, V> = std::collections::BTreeMap<K, V>;
pub type TrackedBTreeSet<T> = std::collections::BTreeSet<T>;
pub type TrackedHashMap<K, V, S = crate::CapHasher> = std::collections::HashMap<K, V, S>;
pub type TrackedHashSet<T, S = crate::CapHasher> = std::collections::HashSet<T, S>;

// ── Optional third-party types ───────────────────────────────────────────────
//
// Each alias is gated on the corresponding standalone feature flag.
// The `telemetry` feature also activates these deps, but this file is
// only compiled in `not(feature = "telemetry")` mode — so there is no
// overlap risk.

#[cfg(feature = "bytes")]
pub type TrackedBytesMut = ::bytes::BytesMut;

#[cfg(feature = "indexmap")]
pub type TrackedIndexMap<K, V, S = crate::CapHasher> = ::indexmap::IndexMap<K, V, S>;

#[cfg(feature = "indexmap")]
pub type TrackedIndexSet<T, S = crate::CapHasher> = ::indexmap::IndexSet<T, S>;

#[cfg(feature = "dashmap")]
pub type TrackedDashMap<K, V, S = crate::CapHasher> = ::dashmap::DashMap<K, V, S>;

#[cfg(feature = "scc")]
pub type TrackedSccHashMap<K, V, S = crate::CapHasher> = ::scc::HashMap<K, V, S>;

#[cfg(feature = "scc")]
pub type TrackedSccHashSet<T, S = crate::CapHasher> = ::scc::HashSet<T, S>;

#[cfg(feature = "scc")]
pub type TrackedSccTreeIndex<K, V> = ::scc::TreeIndex<K, V>;

#[cfg(feature = "smallvec")]
pub type TrackedSmallVec<A> = ::smallvec::SmallVec<A>;
