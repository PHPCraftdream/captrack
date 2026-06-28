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
//! * `peak_capacity` — maximum capacity observed across all instances of that
//!   name (updated in `Drop` via `fetch_max`).
//! * `creation_count` — total number of instances created (updated in ctor
//!   via `fetch_add`).
//!
//! Call [`dump_capacity_stats`] at any point (e.g. end of a benchmark) to
//! write the accumulated stats as pretty-printed JSON.
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

// ---------------------------------------------------------------------------
// Sub-modules
// ---------------------------------------------------------------------------

pub mod hasher;

#[cfg(feature = "telemetry")]
pub mod registry;

pub mod dump;

#[cfg(feature = "telemetry")]
mod tracked;

// ---------------------------------------------------------------------------
// Public re-exports
// ---------------------------------------------------------------------------

pub use captrack_macros::declare_collections;
pub use dump::dump_capacity_stats;
pub use hasher::CapHasher;

#[cfg(feature = "telemetry")]
pub use tracked::{
    TrackedBTreeMap, TrackedBTreeSet, TrackedBytesMut, TrackedDashMap, TrackedHashMap,
    TrackedHashSet, TrackedIndexMap, TrackedIndexSet, TrackedSccHashMap, TrackedSccHashSet,
    TrackedSccTreeIndex, TrackedVec, TrackedVecDeque,
};

// ---------------------------------------------------------------------------
// 13 call-site macros
//
// CRITICAL: every off-feature expansion is wrapped in `{ #[allow(...)] expr }`
// so that when callers add `disallowed-methods` bans on bare constructors,
// the macros themselves don't trigger those lints on call-sites.
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
#[cfg(not(feature = "telemetry"))]
#[macro_export]
macro_rules! tvec {
    ($name:literal, $cap:expr) => {{
        let _: &'static str = $name;
        {
            #[allow(clippy::disallowed_methods)]
            ::std::vec::Vec::with_capacity($cap)
        }
    }};
}

#[cfg(feature = "telemetry")]
#[macro_export]
macro_rules! tvec {
    ($name:literal, $cap:expr) => {
        $crate::TrackedVec::with_capacity_named($cap, $name)
    };
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
#[cfg(not(feature = "telemetry"))]
#[macro_export]
macro_rules! tvecdeque {
    ($name:literal, $cap:expr) => {{
        let _: &'static str = $name;
        {
            #[allow(clippy::disallowed_methods)]
            ::std::collections::VecDeque::with_capacity($cap)
        }
    }};
}

#[cfg(feature = "telemetry")]
#[macro_export]
macro_rules! tvecdeque {
    ($name:literal, $cap:expr) => {
        $crate::TrackedVecDeque::with_capacity_named($cap, $name)
    };
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
#[cfg(not(feature = "telemetry"))]
#[macro_export]
macro_rules! tbtreemap {
    ($name:literal, $_cap:expr) => {{
        let _: &'static str = $name;
        {
            #[allow(clippy::disallowed_methods)]
            ::std::collections::BTreeMap::new()
        }
    }};
}

#[cfg(feature = "telemetry")]
#[macro_export]
macro_rules! tbtreemap {
    ($name:literal, $cap:expr) => {
        $crate::TrackedBTreeMap::new_named($cap, $name)
    };
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
#[cfg(not(feature = "telemetry"))]
#[macro_export]
macro_rules! tbtreeset {
    ($name:literal, $_cap:expr) => {{
        let _: &'static str = $name;
        {
            #[allow(clippy::disallowed_methods)]
            ::std::collections::BTreeSet::new()
        }
    }};
}

#[cfg(feature = "telemetry")]
#[macro_export]
macro_rules! tbtreeset {
    ($name:literal, $cap:expr) => {
        $crate::TrackedBTreeSet::new_named($cap, $name)
    };
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
        $crate::TrackedBytesMut::with_capacity_named($cap, $name)
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
#[cfg(not(feature = "telemetry"))]
#[macro_export]
macro_rules! tfxmap {
    ($name:literal, $cap:expr) => {{
        let _: &'static str = $name;
        {
            #[allow(clippy::disallowed_types, clippy::disallowed_methods)]
            ::std::collections::HashMap::with_capacity_and_hasher(
                $cap,
                <$crate::CapHasher as ::core::default::Default>::default(),
            )
        }
    }};
    ($name:literal, $cap:expr; $hasher:expr) => {{
        let _: &'static str = $name;
        {
            #[allow(clippy::disallowed_types, clippy::disallowed_methods)]
            ::std::collections::HashMap::with_capacity_and_hasher($cap, $hasher)
        }
    }};
}

#[cfg(feature = "telemetry")]
#[macro_export]
macro_rules! tfxmap {
    ($name:literal, $cap:expr) => {
        $crate::TrackedHashMap::<_, _, $crate::CapHasher>::with_capacity_named($cap, $name)
    };
    ($name:literal, $cap:expr; $hasher:expr) => {
        $crate::TrackedHashMap::with_capacity_and_hasher_named($cap, $hasher, $name)
    };
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
#[cfg(not(feature = "telemetry"))]
#[macro_export]
macro_rules! tfxset {
    ($name:literal, $cap:expr) => {{
        let _: &'static str = $name;
        {
            #[allow(clippy::disallowed_types, clippy::disallowed_methods)]
            ::std::collections::HashSet::with_capacity_and_hasher(
                $cap,
                <$crate::CapHasher as ::core::default::Default>::default(),
            )
        }
    }};
    ($name:literal, $cap:expr; $hasher:expr) => {{
        let _: &'static str = $name;
        {
            #[allow(clippy::disallowed_types, clippy::disallowed_methods)]
            ::std::collections::HashSet::with_capacity_and_hasher($cap, $hasher)
        }
    }};
}

#[cfg(feature = "telemetry")]
#[macro_export]
macro_rules! tfxset {
    ($name:literal, $cap:expr) => {
        $crate::TrackedHashSet::<_, $crate::CapHasher>::with_capacity_named($cap, $name)
    };
    ($name:literal, $cap:expr; $hasher:expr) => {
        $crate::TrackedHashSet::with_capacity_and_hasher_named($cap, $hasher, $name)
    };
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
        $crate::TrackedIndexMap::<_, _, $crate::CapHasher>::with_capacity_named($cap, $name)
    };
    ($name:literal, $cap:expr; $hasher:expr) => {
        $crate::TrackedIndexMap::with_capacity_and_hasher_named($cap, $hasher, $name)
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
        $crate::TrackedIndexSet::<_, $crate::CapHasher>::with_capacity_named($cap, $name)
    };
    ($name:literal, $cap:expr; $hasher:expr) => {
        $crate::TrackedIndexSet::with_capacity_and_hasher_named($cap, $hasher, $name)
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
        $crate::TrackedDashMap::<_, _, $crate::CapHasher>::with_capacity_named($cap, $name)
    };
    ($name:literal, $cap:expr; $hasher:expr) => {
        $crate::TrackedDashMap::with_capacity_and_hasher_named($cap, $hasher, $name)
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
        $crate::TrackedSccHashMap::<_, _, $crate::CapHasher>::with_capacity_named($cap, $name)
    };
    ($name:literal, $cap:expr; $hasher:expr) => {
        $crate::TrackedSccHashMap::with_capacity_and_hasher_named($cap, $hasher, $name)
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
        $crate::TrackedSccHashSet::<_, $crate::CapHasher>::with_capacity_named($cap, $name)
    };
    ($name:literal, $cap:expr; $hasher:expr) => {
        $crate::TrackedSccHashSet::with_capacity_and_hasher_named($cap, $hasher, $name)
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
    ($name:literal, $_cap:expr) => {{
        let _: &'static str = $name;
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
        $crate::TrackedSccTreeIndex::new_named($cap, $name)
    };
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests;
