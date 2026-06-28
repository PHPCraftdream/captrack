# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build, test, lint

Two-crate Cargo workspace (`.` + `captrack-macros`). The library has a `doctest = false` lib target — doc snippets are illustrative only and are not compiled.

```bash
# Default build — telemetry OFF, no hasher feature → CapHasher = RandomState.
cargo build
cargo test                          # runs off-feature + per-call-override tests only

# Telemetry ON — pulls in scc/serde/serde_json/bytes/indexmap/dashmap.
cargo test --features telemetry     # adds on_feature_tests + tests/declare_collections.rs

# Hasher matrix (axis 2A) — pick exactly one, never two (compile_error! enforces).
cargo test --features fxhash        # adds fxhash_tests
cargo test --features "telemetry ahash"

# Single test (module-qualified path under src/tests/):
cargo test --features telemetry on_feature_tests::peak_capacity_is_recorded

# Integration test for the proc-macro (must be a separate file because the
# generated macros use absolute `::captrack::` paths that don't resolve from inside the crate):
cargo test --features telemetry --test declare_collections

cargo clippy --all-targets --all-features
```

Note: `tests/declare_collections.rs` requires the `telemetry` feature plus collection backends; without features it still compiles but exercises only the off-feature expansions.

## Architecture

Three orthogonal axes (named in the README and reflected in the module layout). Editing one axis usually means touching code in a predictable set of files.

### Module map

| Module | Compiled when | Purpose |
|---|---|---|
| `src/hasher.rs` | always | `CapHasher` type alias — 5 mutually exclusive feature variants |
| `src/registry.rs` | `telemetry` | Global lock-free registry (`scc::HashMap`) |
| `src/dump.rs` | always | `dump_capacity_stats` — no-op stub off-feature, real impl on-feature |
| `src/stats.rs` | always | `SampleStats` — pure arithmetic helper for post-processing |
| `src/tracked/` | `telemetry` | One file per `Tracked*` struct |
| `src/aliases.rs` | `not(telemetry)` | `TrackedX = StdX` type aliases for source-level symmetry |
| `src/ctor.rs` | always | `#[doc(hidden)]` free ctor functions used by unified std macros |

### Axis 1 — telemetry on/off (`telemetry` feature)

The 13 macros in `src/lib.rs` split into two implementation strategies:

**6 std macros** (`tvec!`, `tvecdeque!`, `tbtreemap!`, `tbtreeset!`, `tfxmap!`, `tfxset!`) are **unified** — a single `macro_rules!` arm delegates to `$crate::ctor::<fn>(...)`. The ctor free-functions in `src/ctor.rs` are cfg-branched internally (`#[cfg(feature = "telemetry")]` / `#[cfg(not(...))]`) with `#[inline(always)]`. Off-feature: the function folds into the bare constructor. On-feature: delegates to `Tracked*::with_capacity_named(...)`.

**7 optional third-party macros** (`tbytesmut!`, `tmap!`, `tset!`, `tdashmap!`, `tsccmap!`, `tsccset!`, `tscctree!`) retain **dual `#[cfg]` arms** directly in `lib.rs`. Reason: ctor free-functions for these types would require the dep in the library's own graph, but in off-feature mode the dep may be absent from captrack's graph (the consumer provides it). The macro expansion is resolved at the call-site in the consumer's context where the dep exists.

Off-feature: `TrackedX` names are type aliases to the bare std/third-party types defined in `src/aliases.rs`. Optional types are gated on the corresponding mirror feature flag (`bytes`, `indexmap`, `dashmap`, `scc`). Enabling `telemetry` activates all mirror features automatically.

### Axis 2 — hasher choice (three levels, all wired through `CapHasher`)

- **2A — global default:** `src/hasher.rs` is a wall of `#[cfg]` `compile_error!`s (mutual exclusion guard) plus five `pub type CapHasher = ...` aliases. Adding a hasher means: new optional dep in `Cargo.toml`, new feature flag, new `compile_error!` pairs against every existing hasher, new type alias.
- **2B — per-call override:** every hash macro (`tfxmap!`, `tfxset!`, `tmap!`, `tset!`, `tdashmap!`, `tsccmap!`, `tsccset!` — 7 total) has **two arms**: `($name, $cap)` uses `CapHasher`, `($name, $cap; $hasher)` injects the caller's hasher. Non-hash macros (`tvec!`, `tvecdeque!`, `tbtreemap!`, `tbtreeset!`, `tbytesmut!`, `tscctree!` — 6 total) have only the single arm.
- **2C — `declare_collections!` proc-macro:** lives in `captrack-macros/src/lib.rs`. Generates 13 `macro_rules!` per invocation that delegate to `::captrack::t*!` using the `;`-arm to inject the user's hasher. The proc-macro is needed because stable `macro_rules!` can't emit `$`-metavariables (no `$$` on stable). **Both arms** of each generated hash macro must forward correctly — the `($n, $c; $h)` arm exists so callers can still override per-call even after picking a custom default.

The 13 generated names follow a strict pattern: `{prefix}_{vec, vecdeque, btreemap, btreeset, bytesmut, fxmap, fxset, map, set, dashmap, sccmap, sccset, scctree}`. If you add a new primitive `tX!` macro, you must also add `{prefix}_X` to the proc-macro's generator.

### Axis 3 — clippy enforcement

`clippy.toml.example` is the source of truth for the bare-constructor ban list users opt into. Every captrack macro must include `#[allow(clippy::disallowed_methods, clippy::disallowed_types)]` in its expansion so user-supplied bans don't fire on captrack-generated code. **If you add a new macro, also add the corresponding ban entry to `clippy.toml.example`.**

### Registry (telemetry-only)

`src/registry.rs` holds an `OnceLock<scc::HashMap<Loc, CapStats>>` where:

```
type Loc = (&'static str, u32, u32);  // (file, line, column)

struct CapStats {
    name: &'static str,       // label from the macro literal — never changed after first insert
    creation_count: AtomicU64, // incremented on every construction (fetch_add, Relaxed)
    samples: scc::Bag<usize>, // one push per Drop or into_iter — raw capacity/len values
}
```

**Key** is `(file, line, column)` — the call-site location captured via `file!()`, `line!()`, `column!()` in the macro. Each distinct source location is one independent registry entry; the `name` string is a human label only.

All operations are **lock-free**: `scc::HashMap` and `scc::Bag::push` never block. Counter updates use `Ordering::Relaxed` — `creation_count` and `samples` are independent fields with no cross-field ordering requirement.

The `record_creation` insert race (two threads first-seeing the same location) is intentionally tolerated — `scc::HashMap::insert` returns `Err` for the loser; the subsequent `get` succeeds for both.

### Samples model

Every `Drop` impl (and every `IntoIterator::into_iter` impl, which must pre-empt Drop) pushes one `usize` sample to the `Bag`:

- **Capacity-based** collections (`Vec`, `VecDeque`, `HashMap`, `HashSet`, `IndexMap`, `IndexSet`, `BytesMut`): push `inner.capacity()`.  Capacity is monotonically non-decreasing, so this equals the peak.
- **Length-based** collections (`BTreeMap`, `BTreeSet`, `DashMap`, `scc::HashMap`, `scc::HashSet`, `scc::TreeIndex`): push `inner.len()` at Drop time.  This is **not** peak — if the collection is drained or partially cleared before Drop the sample undercounts.

Aggregate statistics are computed **post-process** after `dump_capacity_stats`:

```rust
use captrack::SampleStats;
if let Some(s) = SampleStats::from_samples(&entry.samples) {
    println!("median={} p95={} p99={}", s.median, s.p95, s.p99);
}
```

`src/dump.rs` drains the `scc::Bag` via `pop_all`, serialises, then pushes all values back (leaving the bag intact between repeated dump calls). Entries are sorted by `max(samples)` descending.

### Type bridge — `IntoInner` trait (`src/lib.rs`)

`IntoInner` converts a `TrackedX` (or bare collection in off-feature mode) to its inner type with deterministic type inference:

```rust
pub trait IntoInner: Sized {
    type Inner;
    fn into_inner(self) -> Self::Inner;
}
```

- **Off-feature**: `TrackedX<T>` IS `StdX<T>` (type alias); the identity impl is `#[inline(always)]` zero-cost.
- **On-feature**: the wrapper impl records a final capacity sample, then delegates to `From<TrackedX> for StdX`.

`From<TrackedX> for StdX` (and `IntoIterator` impls) use `unsafe { std::ptr::read(&tracked.inner) }` + `std::mem::forget(tracked)` to move the inner value without requiring `S: Default` or `S: Clone` on the hasher type parameter.

## Adding a new tracked collection — checklist

1. `src/tracked/<name>.rs` — `Tracked<Name>` struct with fields `inner`, `name: &'static str`, `file: &'static str`, `line: u32`, `column: u32`.  Implement:
   - `with_capacity_named(cap, name, file, line, column)` — calls `registry::record_creation`.
   - `with_capacity_and_hasher_named(...)` — if hash-keyed (per-call override, Axis 2B).
   - `Deref` / `DerefMut` to the inner type.
   - `Drop` — push sample via `registry::record_sample(self.file, self.line, self.column, sample)`.
   - `IntoIterator` — record sample **before** the move, then `ptr::read(&self.inner)` + `mem::forget(self)`.
   - `From<TrackedX> for StdX` — same pattern (ptr::read + forget, avoids `Default`/`Clone` bounds).
   - `IntoInner for TrackedX` — delegates to `From`.
2. `src/tracked/mod.rs` — `pub mod` + `pub use`.
3. `src/aliases.rs` — `pub type TrackedX<...> = BareX<...>` (off-feature alias, gated on mirror feature if optional).
4. `src/ctor.rs` — cfg-branched free ctor function(s) for std-only collections; optional third-party families skip this file.
5. `src/lib.rs` — `pub use` from `tracked::` (on-feature), then macro: unified single-arm for std, dual `#[cfg]` arms for optional third-party.  Add `IntoInner` identity impl for the bare type.  If hash-keyed, add `;`-arm to both off/on variants.
6. `clippy.toml.example` — disallowed-method ban entry for the bare constructor.
7. Tests — `off_feature_tests`, `on_feature_tests` (including `into_iter` regression to verify the IntoIterator trap), `per_call_override_tests` (if hash-keyed), `tests/declare_collections.rs` (if adding to proc-macro).

## Repo conventions

- `&'static str` everywhere for `$name` — macros require `$name:literal`, never `&str` or `String`. This is what makes the registry key safe.
- Off-feature macro bodies **must not** introduce any code the compiler can't fold away. Block `{ ... }` is fine; closures, helper fns, intermediate `let` bindings holding values are not (the `let _: &'static str = $name;` line is just a type assertion — it generates no code).
- Don't bump versions or publish without an explicit user request.
