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

## captrack-pgo — profile-guided capacity optimization CLI

`captrack-pgo/` is a separate bin-crate in the workspace (not part of the
captrack library).  Its pipeline as of M5 (Path-B migration, 2026-06-28):

```
captrack dump (profile.json)
        │
        ▼
captrack-pgo apply --profile profile.json [--hasher fx|ahash|foldhash|none]
        │  sets CAPTRACK_PGO_PROFILE (and optionally CAPTRACK_PGO_HASHER) env var
        ▼
cargo dylint --path captrack-pgo-lint --fix
        │  HIR-level semantic detection + rustfix suggestions
        │  • capacity rewrite: Vec::new() → Vec::with_capacity(N)
        │  • hasher swap (when CAPTRACK_PGO_HASHER set):
        │      HashMap::new() → HashMap::with_capacity_and_hasher(N, ::fxhash::FxBuildHasher::default())
        │      (skipped for sites with explicit let-binding type ascription)
        ▼
source files rewritten in place
        │
        ▼
last-apply.json  (before/after snapshot manifest)
        │
        ▼
captrack-pgo undo  (restores files from manifest)
```

### Hasher swap (M9)

When `CAPTRACK_PGO_HASHER=fx|ahash|foldhash` is set, the `CAPTRACK_PGO_CAPACITY`
lint also upgrades `HashMap`/`HashSet` constructors to the hasher-bearing form.
The env var is forwarded by `run_lint_apply` from `LintApplyArgs.hasher`
(`HasherChoice::{None, Fx, AHash, FoldHash}`).

**Type-ascription guard:** before emitting the hasher-bearing suggestion, the
lint calls `has_local_type_ascription(cx, call_expr)`.  This walks the HIR
parent chain via `cx.tcx.hir_parent_id_iter(call_expr.hir_id)` and matches on
`Node::LetStmt(local)` — if `local.ty.is_some()`, the hasher injection is
suppressed for that site (capacity rewrite still emitted).  Struct fields and
return types are **not** detected; those may compile-fail if the user accepts
the suggestion (documented limitation).

**Known default expressions recognized as idempotent:**
- `::fxhash::FxBuildHasher::default()`
- `::ahash::RandomState::new()`
- `::foldhash::fast::RandomState::default()`

### Module map (captrack-pgo)

| Module | Purpose |
|---|---|
| `src/cli.rs` | Five subcommands: `measure` (stub), `apply` (Dylint orchestration + `--hasher`), `instrument`, `undo`, `uninstrument` |
| `src/lint_apply.rs` | Core of `apply`: `LintApplyArgs` (incl. `hasher: HasherChoice`), `HasherChoice` enum, pre-flight, snapshot before, run `cargo dylint --fix` with env vars, diff and write manifest; also `undo_lint_apply` |
| `src/model.rs` | `SiteKey`, `SiteStats`, `Unit` — used by profile loaders |
| `src/profile/mod.rs` | `trait Profile { fn sites() -> Vec<SiteStats> }` |
| `src/profile/captrack.rs` | Parses `captrack::dump_capacity_stats` JSON output |
| `src/profile/dhat.rs` | Parses `dhat-heap.json` (byte-level, v2 format) |
| `src/workspace.rs` | `find_workspace_root` + `walk_rust_files` (gitignore-aware) |

### Manifest format (v1)

Written to `target/captrack-pgo/last-lint-apply.json`:

```json
{
  "version": 1,
  "profile_path": "/abs/path/to/profile.json",
  "files": [
    {
      "file": "/abs/path/to/src/lib.rs",
      "sha256_before": "<64 hex chars>",
      "content_before": "fn original() {}",
      "sha256_after": "<64 hex chars>"
    }
  ]
}
```

`undo` restores `content_before` after verifying the current file matches
`sha256_after` (refuses if the file was edited after `apply`).

### Requirements for `apply`

- `cargo install cargo-dylint dylint-link`
- Nightly toolchain pinned in `captrack-pgo-lint/rust-toolchain.toml`
- The `captrack-pgo-lint/` directory (sibling of `captrack-pgo/` in this workspace)

### What was removed (M5)

The syn-based pipeline (`scan.rs`, `plan.rs`, `rules.rs`, `report.rs`,
`apply.rs`, `undo.rs`) and the `propose`/`auto` subcommands were deleted.
Old `last-apply.json` manifests (byte-splice format) can no longer be
reverted via `undo` — use `git` instead.

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
