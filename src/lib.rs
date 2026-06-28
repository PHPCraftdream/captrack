//! `captrack` — capacity telemetry for Rust collections.
//!
//! # Overview
//!
//! This crate provides 13 `macro_rules!` macros (`tvec!`, `tmap!`, …) that
//! wrap every major collection constructor.  In off-feature mode (default)
//! each macro expands to the bare constructor with **zero overhead** — the
//! compiler sees exactly `Vec::with_capacity(n)` etc.  When the `telemetry`
//! feature is enabled, each macro instead returns a `Tracked*` wrapper that
//! records two counters in a global lock-free registry:
//!
//! * `samples` — raw capacity recorded on each Drop / into_iter (one entry per
//!   instance; max/mean/p99 are computed by the caller in post-processing).
//! * `creation_count` — total number of instances created (updated in ctor
//!   via `fetch_add`).
//!
//! Call [`dump_capacity_stats`] at any point (e.g. end of a benchmark) to
//! write the accumulated samples as pretty-printed JSON.
//!
//! # Three orthogonal axes
//!
//! ## Axis 1 — telemetry on/off
//!
//! ```toml
//! [dependencies]
//! captrack = { version = "0.1", features = ["telemetry"] }
//! ```
//!
//! Without `telemetry`, every macro is zero-overhead.  Enable it only in
//! benchmarks or profiling builds.
//!
//! ## Axis 2 — hasher choice (three levels)
//!
//! ### Level A — global default via feature flag
//!
//! ```toml
//! # No feature → RandomState (DoS-safe default)
//! captrack = { version = "0.1" }
//!
//! # Opt-in to a fast hasher for the whole crate:
//! captrack = { version = "0.1", features = ["ahash"] }
//! # or "fxhash", "foldhash", "rustc-hash"
//! ```
//!
//! ### Level B — per-call override via `;`-arm
//!
//! ```ignore
//! use captrack::tmap;
//! // uses CapHasher (global default):
//! let m1 = tmap!("my/map", 16);
//! // uses ahash for this one call-site:
//! let m2 = tmap!("my/special", 8; ahash::RandomState::new());
//! ```
//!
//! ### Level C — custom family via `declare_collections!`
//!
//! ```ignore
//! // In your crate root (once):
//! captrack::declare_collections! { hasher = MyHasher, prefix = my }
//!
//! // Then use the generated macros everywhere:
//! let m = my_map!("table/rows", 64);
//! ```
//!
//! ## Axis 3 — enforcing the discipline (clippy)
//!
//! Copy `clippy.toml.example` into your project's `clippy.toml` (fully or
//! partially) to ban bare collection constructors and require macros.  The
//! captrack macros themselves carry `#[allow(clippy::disallowed_methods)]`
//! in their expansions so they are always safe to use regardless of your
//! clippy policy.
//!
//! # Feature flags
//!
//! * `telemetry` — enables `Tracked*` wrappers and the global registry.
//! * `fxhash` / `ahash` / `foldhash` / `rustc-hash` — select the global
//!   [`CapHasher`].  Select at most one; default is `RandomState`.
//! * `bytes` / `indexmap` / `dashmap` / `scc` — off-feature type-alias
//!   mirror features (no telemetry overhead; let consumers use `TrackedX`
//!   names in both modes when they depend on these crates).

// ---------------------------------------------------------------------------
// Sub-modules
// ---------------------------------------------------------------------------

pub mod hasher;

#[cfg(feature = "telemetry")]
pub mod registry;

pub mod dump;

pub mod stats;

#[cfg(feature = "telemetry")]
mod tracked;

/// Off-feature type aliases — `TrackedX<T>` = `StdX<T>` when telemetry is off.
#[cfg(not(feature = "telemetry"))]
pub mod aliases;

/// Free constructor functions used by unified macros — cfg-branched on
/// `feature = "telemetry"`.  Available for the always-present std types.
#[doc(hidden)]
pub mod ctor;

// ---------------------------------------------------------------------------
// Public re-exports
// ---------------------------------------------------------------------------

pub use captrack_macros::declare_collections;
pub use dump::dump_capacity_stats;
pub use hasher::CapHasher;
pub use stats::SampleStats;

#[cfg(feature = "telemetry")]
pub use tracked::{
    TrackedBTreeMap, TrackedBTreeSet, TrackedBytesMut, TrackedDashMap, TrackedHashMap,
    TrackedHashSet, TrackedIndexMap, TrackedIndexSet, TrackedSccHashMap, TrackedSccHashSet,
    TrackedSccTreeIndex, TrackedVec, TrackedVecDeque,
};

#[cfg(not(feature = "telemetry"))]
pub use aliases::*;

// ---------------------------------------------------------------------------
// `IntoInner` — boundary conversion with deterministic type inference
//
// Unlike a free `.into()`, the associated `Inner` type is pinned by the
// source type, so call-chains like `tvec!("foo", 16).into_inner().len()`
// infer correctly in BOTH feature modes.
//
// Off-feature: `TrackedX<T>` IS `StdX<T>` via type alias; the identity impl
//              is `#[inline(always)]` and zero-cost.
// On-feature:  the wrapper impl records the final capacity sample then
//              delegates to `From::from(self)`.
// ---------------------------------------------------------------------------

/// Convert a captrack wrapper (or already-bare collection) into the inner
/// std/third-party type.  Works symmetrically in both feature modes:
///
/// * **Off-feature** — `TrackedX<T>` IS `StdX<T>` via type alias, the impl
///   is the identity move (`#[inline(always)]`, zero-cost).
/// * **On-feature** — `TrackedX<T>` is the wrapper struct, the impl records
///   the final capacity sample and unwraps the inner collection.
///
/// Used at API boundaries where a function's return type is `StdX<T>` but
/// construction goes through a captrack macro.  Unlike a free `.into()`,
/// the associated `Inner` type is pinned by the source type, so call-chains
/// like `tvec!("foo", 16).into_inner().len()` infer correctly in BOTH modes.
///
/// # Examples
///
/// ```ignore
/// use captrack::{tvec, IntoInner};
///
/// fn build() -> Vec<u32> {
///     let mut v = tvec!("my/rows", 64);
///     v.push(1u32);
///     v.into_inner() // identical source in both feature modes
/// }
/// ```
pub trait IntoInner: Sized {
    type Inner;
    fn into_inner(self) -> Self::Inner;
}

// ── Identity impls for always-available std types ──────────────────────────

impl<T> IntoInner for std::vec::Vec<T> {
    type Inner = std::vec::Vec<T>;
    #[inline(always)]
    fn into_inner(self) -> Self::Inner {
        self
    }
}

impl<T> IntoInner for std::collections::VecDeque<T> {
    type Inner = std::collections::VecDeque<T>;
    #[inline(always)]
    fn into_inner(self) -> Self::Inner {
        self
    }
}

impl<K: std::cmp::Ord, V> IntoInner for std::collections::BTreeMap<K, V> {
    type Inner = std::collections::BTreeMap<K, V>;
    #[inline(always)]
    fn into_inner(self) -> Self::Inner {
        self
    }
}

impl<T: std::cmp::Ord> IntoInner for std::collections::BTreeSet<T> {
    type Inner = std::collections::BTreeSet<T>;
    #[inline(always)]
    fn into_inner(self) -> Self::Inner {
        self
    }
}

impl<K, V, S> IntoInner for std::collections::HashMap<K, V, S> {
    type Inner = std::collections::HashMap<K, V, S>;
    #[inline(always)]
    fn into_inner(self) -> Self::Inner {
        self
    }
}

impl<T, S> IntoInner for std::collections::HashSet<T, S> {
    type Inner = std::collections::HashSet<T, S>;
    #[inline(always)]
    fn into_inner(self) -> Self::Inner {
        self
    }
}

// ── Identity impls for optional bare types (mirror the alias feature gates) ─

#[cfg(any(feature = "bytes", feature = "telemetry"))]
impl IntoInner for ::bytes::BytesMut {
    type Inner = ::bytes::BytesMut;
    #[inline(always)]
    fn into_inner(self) -> Self::Inner {
        self
    }
}

#[cfg(any(feature = "indexmap", feature = "telemetry"))]
impl<K, V, S> IntoInner for ::indexmap::IndexMap<K, V, S> {
    type Inner = ::indexmap::IndexMap<K, V, S>;
    #[inline(always)]
    fn into_inner(self) -> Self::Inner {
        self
    }
}

#[cfg(any(feature = "indexmap", feature = "telemetry"))]
impl<T, S> IntoInner for ::indexmap::IndexSet<T, S> {
    type Inner = ::indexmap::IndexSet<T, S>;
    #[inline(always)]
    fn into_inner(self) -> Self::Inner {
        self
    }
}

#[cfg(any(feature = "dashmap", feature = "telemetry"))]
impl<K, V, S> IntoInner for ::dashmap::DashMap<K, V, S>
where
    K: Eq + std::hash::Hash,
    S: std::hash::BuildHasher + Clone,
{
    type Inner = ::dashmap::DashMap<K, V, S>;
    #[inline(always)]
    fn into_inner(self) -> Self::Inner {
        self
    }
}

#[cfg(any(feature = "scc", feature = "telemetry"))]
impl<K, V, S> IntoInner for ::scc::HashMap<K, V, S>
where
    K: Eq + std::hash::Hash + 'static,
    V: 'static,
    S: std::hash::BuildHasher,
{
    type Inner = ::scc::HashMap<K, V, S>;
    #[inline(always)]
    fn into_inner(self) -> Self::Inner {
        self
    }
}

#[cfg(any(feature = "scc", feature = "telemetry"))]
impl<T, S> IntoInner for ::scc::HashSet<T, S>
where
    T: Eq + std::hash::Hash + 'static,
    S: std::hash::BuildHasher,
{
    type Inner = ::scc::HashSet<T, S>;
    #[inline(always)]
    fn into_inner(self) -> Self::Inner {
        self
    }
}

#[cfg(any(feature = "scc", feature = "telemetry"))]
impl<K, V> IntoInner for ::scc::TreeIndex<K, V>
where
    K: Clone + Ord + 'static,
    V: Clone + 'static,
{
    type Inner = ::scc::TreeIndex<K, V>;
    #[inline(always)]
    fn into_inner(self) -> Self::Inner {
        self
    }
}

// ---------------------------------------------------------------------------
// 13 call-site macros
//
// ARCHITECTURE NOTE — unified vs dual-branch macros:
//
//   • The 6 standard-library macros (tvec!, tvecdeque!, tbtreemap!, tbtreeset!,
//     tfxmap!, tfxset!) are **unified**: a single `macro_rules!` declaration
//     delegates to `$crate::ctor::<fn>(...)` which is cfg-branched internally.
//     The ctor functions are always part of the library because std is always
//     available.
//
//   • The 7 optional-third-party macros (tbytesmut!, tmap!, tset!, tdashmap!,
//     tsccmap!, tsccset!, tscctree!) keep **dual cfg arms**.  The reason:
//     ctor free-functions that reference `::bytes::BytesMut`, `::indexmap::...`
//     etc. must compile as part of the library, which requires those crates in
//     the library dependency graph.  In off-feature mode without the matching
//     captrack feature flag the crate is absent; the macro expansion, by
//     contrast, is resolved at the call site (consumer's context) where the
//     consumer already has the dep.
//
//     On-feature arms of these 7 macros delegate to `Tracked*::..._named(...)`.
//
//   This is an implementation detail; from the consumer's perspective all 13
//   macros have the same call syntax and all `TrackedX` type names are
//   available in both modes.
//
// CRITICAL: every off-feature expansion uses `#[allow(clippy::disallowed_methods,
//   clippy::disallowed_types)]` so user-supplied ban-lists don't fire on
//   captrack-generated code.
//
// Each hash macro has TWO arms:
//   1. `($name, $cap)` — uses CapHasher (global default, Axis 2A).
//   2. `($name, $cap; $hasher)` — per-call override (Axis 2B).
// ---------------------------------------------------------------------------

// ── tvec! ────────────────────────────────────────────────────────────────────

/// Create a `Vec<T>` (off-feature) or `TrackedVec<T>` (on-feature) with the
/// given capacity.
///
/// # Examples
///
/// ```
/// # use captrack::tvec;
/// let mut v = tvec!("my/vec", 16);
/// v.push(1u32);
/// assert_eq!(v.len(), 1);
/// ```
#[macro_export]
macro_rules! tvec {
    ($name:literal, $cap:expr) => {{
        let _: &'static str = $name;
        $crate::ctor::vec_with_capacity_named::<_>($cap, $name, file!(), line!(), column!())
    }};
}

// ── tvecdeque! ───────────────────────────────────────────────────────────────

/// Create a `VecDeque<T>` (off) or `TrackedVecDeque<T>` (on) with the given
/// capacity.
///
/// # Examples
///
/// ```
/// # use captrack::tvecdeque;
/// let mut d = tvecdeque!("my/deque", 8);
/// d.push_back(42u32);
/// ```
#[macro_export]
macro_rules! tvecdeque {
    ($name:literal, $cap:expr) => {{
        let _: &'static str = $name;
        $crate::ctor::vecdeque_with_capacity_named::<_>($cap, $name, file!(), line!(), column!())
    }};
}

// ── tbtreemap! ───────────────────────────────────────────────────────────────

/// Cap hint is accepted for API uniformity; `BTreeMap` has no `with_capacity`.
///
/// # Examples
///
/// ```
/// # use captrack::tbtreemap;
/// let mut m = tbtreemap!("my/btreemap", 0);
/// m.insert(1u32, "hello");
/// ```
#[macro_export]
macro_rules! tbtreemap {
    ($name:literal, $cap:expr) => {{
        let _: &'static str = $name;
        $crate::ctor::btreemap_new_named::<_, _>($cap, $name, file!(), line!(), column!())
    }};
}

// ── tbtreeset! ───────────────────────────────────────────────────────────────

/// Cap hint is accepted for API uniformity; `BTreeSet` has no `with_capacity`.
///
/// # Examples
///
/// ```
/// # use captrack::tbtreeset;
/// let mut s = tbtreeset!("my/btreeset", 0);
/// s.insert(42u32);
/// ```
#[macro_export]
macro_rules! tbtreeset {
    ($name:literal, $cap:expr) => {{
        let _: &'static str = $name;
        $crate::ctor::btreeset_new_named::<_>($cap, $name, file!(), line!(), column!())
    }};
}

// ── tbytesmut! ───────────────────────────────────────────────────────────────

/// Create a `bytes::BytesMut` (off) or `TrackedBytesMut` (on) with the given
/// capacity.
///
/// Requires the `bytes` crate as a direct dependency of your crate.
///
/// # Examples
///
/// ```ignore
/// # use captrack::tbytesmut;
/// let mut b = tbytesmut!("my/buf", 64);
/// b.extend_from_slice(b"hello");
/// ```
#[cfg(not(feature = "telemetry"))]
#[macro_export]
macro_rules! tbytesmut {
    ($name:literal, $cap:expr) => {{
        let _: &'static str = $name;
        {
            #[allow(clippy::disallowed_methods)]
            ::bytes::BytesMut::with_capacity($cap)
        }
    }};
}

#[cfg(feature = "telemetry")]
#[macro_export]
macro_rules! tbytesmut {
    ($name:literal, $cap:expr) => {
        $crate::TrackedBytesMut::with_capacity_named($cap, $name, file!(), line!(), column!())
    };
}

// ── tfxmap! ──────────────────────────────────────────────────────────────────

/// `std::HashMap` with `CapHasher` — supports per-call override via `;`-arm.
///
/// # Examples
///
/// ```ignore
/// # use captrack::tfxmap;
/// // uses CapHasher (global default):
/// let mut m = tfxmap!("my/map", 16);
/// m.insert(1u32, 2u32);
///
/// // per-call override:
/// let mut m2 = tfxmap!("my/special", 8; ahash::RandomState::new());
/// ```
#[macro_export]
macro_rules! tfxmap {
    ($name:literal, $cap:expr) => {{
        let _: &'static str = $name;
        $crate::ctor::hashmap_with_capacity_named::<_, _, $crate::CapHasher>(
            $cap,
            $name,
            file!(),
            line!(),
            column!(),
        )
    }};
    ($name:literal, $cap:expr; $hasher:expr) => {{
        let _: &'static str = $name;
        $crate::ctor::hashmap_with_capacity_and_hasher_named(
            $cap,
            $hasher,
            $name,
            file!(),
            line!(),
            column!(),
        )
    }};
}

// ── tfxset! ──────────────────────────────────────────────────────────────────

/// `std::HashSet` with `CapHasher` — supports per-call override via `;`-arm.
///
/// # Examples
///
/// ```ignore
/// # use captrack::tfxset;
/// let mut s = tfxset!("my/set", 8);
/// s.insert(42u32);
/// ```
#[macro_export]
macro_rules! tfxset {
    ($name:literal, $cap:expr) => {{
        let _: &'static str = $name;
        $crate::ctor::hashset_with_capacity_named::<_, $crate::CapHasher>(
            $cap,
            $name,
            file!(),
            line!(),
            column!(),
        )
    }};
    ($name:literal, $cap:expr; $hasher:expr) => {{
        let _: &'static str = $name;
        $crate::ctor::hashset_with_capacity_and_hasher_named(
            $cap,
            $hasher,
            $name,
            file!(),
            line!(),
            column!(),
        )
    }};
}

// ── tmap! ────────────────────────────────────────────────────────────────────

/// `IndexMap` with `CapHasher` — insertion-ordered.  Supports `;`-arm override.
///
/// Requires the `indexmap` crate as a direct dependency of your crate.
///
/// # Examples
///
/// ```ignore
/// # use captrack::tmap;
/// let mut m = tmap!("my/imap", 16);
/// m.insert(1u32, 2u32);
///
/// // per-call override with ahash:
/// let mut m2 = tmap!("my/imap-ah", 8; ahash::RandomState::new());
/// ```
#[cfg(not(feature = "telemetry"))]
#[macro_export]
macro_rules! tmap {
    ($name:literal, $cap:expr) => {{
        let _: &'static str = $name;
        {
            #[allow(clippy::disallowed_methods)]
            ::indexmap::IndexMap::with_capacity_and_hasher(
                $cap,
                <$crate::CapHasher as ::core::default::Default>::default(),
            )
        }
    }};
    ($name:literal, $cap:expr; $hasher:expr) => {{
        let _: &'static str = $name;
        {
            #[allow(clippy::disallowed_methods)]
            ::indexmap::IndexMap::with_capacity_and_hasher($cap, $hasher)
        }
    }};
}

#[cfg(feature = "telemetry")]
#[macro_export]
macro_rules! tmap {
    ($name:literal, $cap:expr) => {
        $crate::TrackedIndexMap::<_, _, $crate::CapHasher>::with_capacity_named(
            $cap,
            $name,
            file!(),
            line!(),
            column!(),
        )
    };
    ($name:literal, $cap:expr; $hasher:expr) => {
        $crate::TrackedIndexMap::with_capacity_and_hasher_named(
            $cap,
            $hasher,
            $name,
            file!(),
            line!(),
            column!(),
        )
    };
}

// ── tset! ────────────────────────────────────────────────────────────────────

/// `IndexSet` with `CapHasher` — insertion-ordered.  Supports `;`-arm override.
///
/// Requires the `indexmap` crate as a direct dependency of your crate.
///
/// # Examples
///
/// ```ignore
/// # use captrack::tset;
/// let mut s = tset!("my/iset", 8);
/// s.insert(42u32);
/// ```
#[cfg(not(feature = "telemetry"))]
#[macro_export]
macro_rules! tset {
    ($name:literal, $cap:expr) => {{
        let _: &'static str = $name;
        {
            #[allow(clippy::disallowed_methods)]
            ::indexmap::IndexSet::with_capacity_and_hasher(
                $cap,
                <$crate::CapHasher as ::core::default::Default>::default(),
            )
        }
    }};
    ($name:literal, $cap:expr; $hasher:expr) => {{
        let _: &'static str = $name;
        {
            #[allow(clippy::disallowed_methods)]
            ::indexmap::IndexSet::with_capacity_and_hasher($cap, $hasher)
        }
    }};
}

#[cfg(feature = "telemetry")]
#[macro_export]
macro_rules! tset {
    ($name:literal, $cap:expr) => {
        $crate::TrackedIndexSet::<_, $crate::CapHasher>::with_capacity_named(
            $cap,
            $name,
            file!(),
            line!(),
            column!(),
        )
    };
    ($name:literal, $cap:expr; $hasher:expr) => {
        $crate::TrackedIndexSet::with_capacity_and_hasher_named(
            $cap,
            $hasher,
            $name,
            file!(),
            line!(),
            column!(),
        )
    };
}

// ── tdashmap! ────────────────────────────────────────────────────────────────

/// `DashMap` with `CapHasher` — sharded concurrent map.  Supports `;`-arm.
///
/// Requires the `dashmap` crate as a direct dependency of your crate.
///
/// # Examples
///
/// ```ignore
/// # use captrack::tdashmap;
/// let d = tdashmap!("my/dmap", 16);
/// d.insert(1u32, 2u32);
/// ```
#[cfg(not(feature = "telemetry"))]
#[macro_export]
macro_rules! tdashmap {
    ($name:literal, $cap:expr) => {{
        let _: &'static str = $name;
        {
            #[allow(clippy::disallowed_methods)]
            ::dashmap::DashMap::with_capacity_and_hasher(
                $cap,
                <$crate::CapHasher as ::core::default::Default>::default(),
            )
        }
    }};
    ($name:literal, $cap:expr; $hasher:expr) => {{
        let _: &'static str = $name;
        {
            #[allow(clippy::disallowed_methods)]
            ::dashmap::DashMap::with_capacity_and_hasher($cap, $hasher)
        }
    }};
}

#[cfg(feature = "telemetry")]
#[macro_export]
macro_rules! tdashmap {
    ($name:literal, $cap:expr) => {
        $crate::TrackedDashMap::<_, _, $crate::CapHasher>::with_capacity_named(
            $cap,
            $name,
            file!(),
            line!(),
            column!(),
        )
    };
    ($name:literal, $cap:expr; $hasher:expr) => {
        $crate::TrackedDashMap::with_capacity_and_hasher_named(
            $cap,
            $hasher,
            $name,
            file!(),
            line!(),
            column!(),
        )
    };
}

// ── tsccmap! ─────────────────────────────────────────────────────────────────

/// `scc::HashMap` with `CapHasher` — lock-free concurrent map.  Supports `;`-arm.
///
/// Requires the `scc` crate as a direct dependency of your crate.
///
/// # Examples
///
/// ```ignore
/// # use captrack::tsccmap;
/// let m = tsccmap!("my/sccmap", 16);
/// let _ = m.insert(1u32, 2u32);
/// ```
#[cfg(not(feature = "telemetry"))]
#[macro_export]
macro_rules! tsccmap {
    ($name:literal, $cap:expr) => {{
        let _: &'static str = $name;
        {
            #[allow(clippy::disallowed_methods)]
            ::scc::HashMap::with_capacity_and_hasher(
                $cap,
                <$crate::CapHasher as ::core::default::Default>::default(),
            )
        }
    }};
    ($name:literal, $cap:expr; $hasher:expr) => {{
        let _: &'static str = $name;
        {
            #[allow(clippy::disallowed_methods)]
            ::scc::HashMap::with_capacity_and_hasher($cap, $hasher)
        }
    }};
}

#[cfg(feature = "telemetry")]
#[macro_export]
macro_rules! tsccmap {
    ($name:literal, $cap:expr) => {
        $crate::TrackedSccHashMap::<_, _, $crate::CapHasher>::with_capacity_named(
            $cap,
            $name,
            file!(),
            line!(),
            column!(),
        )
    };
    ($name:literal, $cap:expr; $hasher:expr) => {
        $crate::TrackedSccHashMap::with_capacity_and_hasher_named(
            $cap,
            $hasher,
            $name,
            file!(),
            line!(),
            column!(),
        )
    };
}

// ── tsccset! ─────────────────────────────────────────────────────────────────

/// `scc::HashSet` with `CapHasher`.  Supports `;`-arm.
///
/// Requires the `scc` crate as a direct dependency of your crate.
#[cfg(not(feature = "telemetry"))]
#[macro_export]
macro_rules! tsccset {
    ($name:literal, $cap:expr) => {{
        let _: &'static str = $name;
        {
            #[allow(clippy::disallowed_methods)]
            ::scc::HashSet::with_capacity_and_hasher(
                $cap,
                <$crate::CapHasher as ::core::default::Default>::default(),
            )
        }
    }};
    ($name:literal, $cap:expr; $hasher:expr) => {{
        let _: &'static str = $name;
        {
            #[allow(clippy::disallowed_methods)]
            ::scc::HashSet::with_capacity_and_hasher($cap, $hasher)
        }
    }};
}

#[cfg(feature = "telemetry")]
#[macro_export]
macro_rules! tsccset {
    ($name:literal, $cap:expr) => {
        $crate::TrackedSccHashSet::<_, $crate::CapHasher>::with_capacity_named(
            $cap,
            $name,
            file!(),
            line!(),
            column!(),
        )
    };
    ($name:literal, $cap:expr; $hasher:expr) => {
        $crate::TrackedSccHashSet::with_capacity_and_hasher_named(
            $cap,
            $hasher,
            $name,
            file!(),
            line!(),
            column!(),
        )
    };
}

// ── tscctree! ────────────────────────────────────────────────────────────────

/// `scc::TreeIndex` — sorted lock-free B+ tree.  Cap hint accepted for API
/// uniformity; `TreeIndex::new()` takes no capacity argument.
///
/// Requires the `scc` crate as a direct dependency of your crate.
#[cfg(not(feature = "telemetry"))]
#[macro_export]
macro_rules! tscctree {
    ($name:literal, $cap:expr) => {{
        let _: &'static str = $name;
        let _ = $cap;
        {
            #[allow(clippy::disallowed_methods)]
            ::scc::TreeIndex::new()
        }
    }};
}

#[cfg(feature = "telemetry")]
#[macro_export]
macro_rules! tscctree {
    ($name:literal, $cap:expr) => {
        $crate::TrackedSccTreeIndex::new_named($cap, $name, file!(), line!(), column!())
    };
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests;
