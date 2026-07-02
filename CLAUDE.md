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

The **17 macros** in `src/lib.rs` split into two implementation strategies (was 13 before T2 added `tstring!` / `tbinaryheap!` / `thashbrownmap!` and Phase A-B added `tsmallvec!`).

**std macros** — `tvec!`, `tvecdeque!`, `tbtreemap!`, `tbtreeset!`, `tfxmap!`, `tfxset!`, `tstring!`, `tbinaryheap!` — are **unified** — a single `macro_rules!` arm delegates to `$crate::ctor::<fn>(...)`. The ctor free-functions in `src/ctor.rs` are cfg-branched internally (`#[cfg(feature = "telemetry")]` / `#[cfg(not(...))]`) with `#[inline(always)]`. Off-feature: the function folds into the bare constructor. On-feature: delegates to `Tracked*::with_capacity_named(...)`.

**Optional third-party macros** — `tbytesmut!`, `tmap!`, `tset!`, `tdashmap!`, `tsccmap!`, `tsccset!`, `tscctree!`, `tsmallvec!`, `thashbrownmap!` — retain **dual `#[cfg]` arms** directly in `lib.rs`. Reason: ctor free-functions for these types would require the dep in the library's own graph, but in off-feature mode the dep may be absent from captrack's graph (the consumer provides it). The macro expansion is resolved at the call-site in the consumer's context where the dep exists.

Off-feature: `TrackedX` names are type aliases to the bare std/third-party types defined in `src/aliases.rs`. Optional types are gated on the corresponding mirror feature flag (`bytes`, `indexmap`, `dashmap`, `scc`). Enabling `telemetry` activates all mirror features automatically.

**`t*_owned!` siblings** (`tvec_owned!`, `tvecdeque_owned!`, `tbytesmut_owned!`, `tfxmap_owned!`, `tfxset_owned!`, `tmap_owned!`, `tset_owned!`, `tdashmap_owned!`, `tsccmap_owned!`, `tsccset_owned!` — 10 total, `src/lib.rs`) are a separate initial-cap-only family: they always return the bare collection type (never `Tracked*`) and record just the requested initial capacity as a single sample, skipping Drop-time peak tracking. No `_owned` variant exists for `tbtreemap!`/`tbtreeset!`/`tscctree!` since those types lack a `with_capacity` constructor.

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
    name: &'static str,        // label from the macro literal — fixed at first insert
    creation_count: AtomicU64, // one increment per CONSTRUCTION (wrap_from / with_capacity_named)
    samples: Reservoir,        // bounded reservoir of capacity/len observations
}
```

**Key** is `(file, line, column)` — the call-site location captured via `file!()`, `line!()`, `column!()` in the macro. Each distinct source location is one independent registry entry; the `name` string is a human label only.

All operations are lock-free EXCEPT the reservoir's inner `Mutex<Vec<usize>>` (T1) — `record_sample` only fires on Drop / `cap_inspect_at` / construction-time sample, never on a hot inner loop, so the mutex contention is negligible.  The reservoir uses Vitter Algorithm R to bound memory to `CAPTRACK_SAMPLE_CAP` (default 4096) statistically-representative samples plus an `AtomicU64 seen_count` tracking the true population.

The `record_creation` insert race (two threads first-seeing the same location) is intentionally tolerated — `scc::HashMap::insert` returns `Err` for the loser; the subsequent `get` succeeds for both.

### Samples model — three numbers, three meanings (T3-reconciled)

After T3 the registry exposes three distinct telemetry numbers:

- **`creation_count`** — `+1` per CONSTRUCTION of an instance.  Fires from `with_capacity_named` and `wrap_from`.  A binding with one construction site has `creation_count == 1` regardless of how many times its consumption points are inspected.
- **`samples`** — the reservoir-bounded snapshot of capacity / length observations.  `samples.len() ≤ CAPTRACK_SAMPLE_CAP`.
- **`total_observed`** — the true count of every sample ever recorded for this site, including those evicted by the reservoir.  For a binding constructed once with N `cap_inspect_at` consumption points, `total_observed == 1 (construction sample) + N (consumption samples) + 1 (Drop)`.

`record_sample` is a no-op (debug_assert in debug, silent in release) when the call-site isn't registered yet — an orphan `cap_inspect_at` for a binding that was constructed in non-instrumented code doesn't crash, it just drops the sample.

### Capacity vs length-based sample sources

Every `Drop` impl (and every `IntoIterator::into_iter` impl, which must pre-empt Drop) pushes one `usize` sample to the reservoir:

- **Capacity-based** collections (`Vec`, `VecDeque`, `String`, `HashMap`, `HashSet`, `IndexMap`, `IndexSet`, `BytesMut`, `SmallVec`, `hashbrown::HashMap`, `BinaryHeap`, `DashMap`, `scc::HashMap`, `scc::HashSet`): push `inner.capacity()`.  Capacity is monotonically non-decreasing, so the final value equals the peak.
- **Length-based** collections (`BTreeMap`, `BTreeSet`, `scc::TreeIndex`): push `inner.len()` at Drop time.  This is **not** peak — if the collection is drained or partially cleared before Drop the sample undercounts.

### `CapInspect` trait (Phase L) — consumption-point sampling

`src/cap_inspect.rs` defines `CapInspect::cap_inspect_at(&self, name, file, line, column)` with one impl per tracked type.  The lint injects calls at by-value escape positions (return, struct field init, function argument, type-ascribed `let` init) where `wrap_from` cannot be applied without tripping `E0308`.  Each `cap_inspect_at` records a sample (only — not a creation) against the binding's CONSTRUCTION-site location, so percentiles aggregate naturally across construction-time, mid-lifetime, and Drop-time observations.

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
captrack library).  Eight subcommands form the full pipeline:

```
                 ┌──────────────────────────────────────────────────┐
                 │  captrack-pgo measure --workspace … --bench … …  │
                 │  (RAII-guarded one-command orchestration — T5)   │
                 └──────────────────────────────────────────────────┘
                                       │
            ┌──────────┬───────────────┼──────────────────┬─────────────────────┐
            ▼          ▼               ▼                  ▼                     ▼
        wire ─►  instrument ─►  cargo bench × N ─►  merge ─►  uninstrument ─►  unwire
        (T-)     (Dylint plugin                    (T4 — was      (Dylint)     (T-)
                  injects wrap_from                  jq one-liner)
                  + cap_inspect_at —
                  Phase K + L)        ─► profile-<bin>.json (autodump,
                                          atomic .tmp+rename, every 500ms by default)

profiles                                                       merged.json
                                                                    │
                                                                    ▼ (optional)
                                                          captrack-pgo analyze
                                                          (Phase O distribution
                                                            classifier + per-site
                                                            policy injection — T6)
                                                                    │
                                                                    ▼
                                                          captrack-pgo apply
                                                          --cap-from p95 --hasher fx
                                                            • Phase N multi-span
                                                              for type-ascribed lets
                                                            • Phase O THasher-aware
                                                              skip of already-fast
                                                                    │
                                                                    ▼
                                                          rewritten source files
                                                          + last-apply.json
                                                                    │
                                                                    ▼
                                                          captrack-pgo undo
                                                          (restore from manifest)
```

Each pipeline step is its own subcommand; `measure` is the convenience
orchestration that runs `wire → instrument → bench×N → merge →
uninstrument → unwire` with `CleanupGuard` (RAII, `disarm()` on success,
Drop on panic).

### Capacity policy knobs (M11)

Three env vars control the capacity formula:

| Env var | CLI flag | Default | Notes |
|---|---|---|---|
| `CAPTRACK_PGO_CAP_FROM` | `--cap-from` | `p95` | Statistic: max/mean/median/p95/p99 |
| `CAPTRACK_PGO_CAP_MUL` | `--cap-mul` | `1.0` | Multiplier (float > 0) |
| `CAPTRACK_PGO_CAP_ROUND` | `--cap-round` | `pow2` | Rounding: pow2/to8/exact |

The default values (`P95 × 1.0, Pow2`) reproduce the pre-M11 `next_pow2(p95)` formula exactly.
Default-variant values cause `run_lint_apply` to **omit** the env var (removal) so the plugin
defaults match; only non-default values are forwarded.

Per-site `policy` fields in the profile JSON override individual globals.  The `PolicyDefaults`
struct (in `captrack-pgo-lint/src/rules.rs`) is constructed from `OnceLock`-cached readers
(`read_cap_from`, `read_cap_mul`, `read_cap_round`) and passed to `propose_cap` as an argument
(pure function — no I/O inside `propose_cap`).

### Hasher swap (M9 → Phase N → Phase O)

When `CAPTRACK_PGO_HASHER=fx|ahash|foldhash` is set, the
`CAPTRACK_PGO_CAPACITY` lint upgrades `HashMap` / `HashSet` / `IndexMap`
/ `IndexSet` / `DashMap` / `scc::HashMap` / `scc::HashSet` /
`hashbrown::HashMap` constructors to the hasher-bearing form.  The env
var is forwarded by `run_lint_apply` from `LintApplyArgs.hasher`
(`HasherChoice::{None, Fx, AHash, FoldHash}`).

**Phase N — multi-span suggestion for type-ascribed lets:**
`let m: HashMap<K, V> = HashMap::new();` would E0308 if only the
constructor is rewritten (ascription pins `S = RandomState`).  The lint
emits a `multipart_suggestion` that atomically rewrites both spans:
the ascription's generic arg list (`HashMap<K, V>` → `HashMap<K, V,
::fxhash::FxBuildHasher>`) AND the constructor (`HashMap::new()` →
`HashMap::with_capacity_and_hasher(N, ::fxhash::FxBuildHasher::default())`).
`cargo fix` applies both edits atomically.

`AscriptionForm` (in `lib.rs`) classifies the ascription:
  - `HasherOmitted` — Phase N multi-span suggestion
  - `HasherPinned` — Phase O sub-classification (see below)
  - `Wildcard` (no generics) → capacity-only rewrite
  - `NotApplicable` (non-hashing type) → capacity-only rewrite

**Phase O — `HasherKind` classifier (already-fast detection):**
When `HasherPinned`, `classify_hasher_snippet` reads the snippet of the
last type-list argument and classifies it as:
  - `FastKnown` — `fxhash::FxBuildHasher`, `ahash::RandomState`,
    `foldhash::fast::RandomState`, `rustc_hash::FxBuildHasher`,
    `BuildHasherDefault<FxHasher>` (covers shamir-db's THasher alias).
    → Skip the swap, capacity-only rewrite with the message "ascription
    already pins a fast hasher".  Prevents churn rewrites in workspaces
    where everything is on FxHash already.
  - `SlowDefault` — explicit `RandomState`.  Skip the swap and emit a
    nudge ("consider removing the explicit hasher so the lint can swap
    in fx").
  - `Unknown` — user-defined hasher.  Skip the swap.

**Known default expressions recognized as idempotent (for swap-in):**
- `::fxhash::FxBuildHasher::default()`
- `::ahash::RandomState::new()`
- `::foldhash::fast::RandomState::default()`

### Module map (captrack-pgo)

| Module | Purpose |
|---|---|
| `src/cli.rs` | Subcommand definitions: `measure` (T5), `apply` (Dylint + `--hasher` + `--cap-from/mul/round`), `instrument`, `undo`, `uninstrument`, `wire`, `unwire`, `merge` (T4), `analyze` (T6) |
| `src/measure.rs` | T5 — one-command orchestration: wire → instrument → bench×N → merge → uninstrument → unwire, with `CleanupGuard` (RAII, `disarm()` on success) and `cargo metadata`-driven bench-crate auto-detection |
| `src/merge.rs` | T4 — group-by `(file, line, col)`, sum `creation_count`, concat `samples`, reservoir-sample down to `--reservoir-cap` via per-site seeded LCG (deterministic) |
| `src/analyze.rs` | T6 — `SiteShape` classifier (UnimodalTight / UnimodalSpread / Bimodal / HeavyTail / MostlyZero / InsufficientData), per-shape `PolicyOverride` recommendation, optional `--write-policy` to inject back into the profile JSON |
| `src/wire.rs` | Patch every workspace member's `Cargo.toml` to add the captrack dep + the `telemetry` feature flag; `unwire` reverts |
| `src/lint_apply.rs` | Core of `apply`: `LintApplyArgs` (incl. `hasher`, `cap_from`, `cap_mul`, `cap_round`), `HasherChoice`/`CapFromChoice`/`CapRoundChoice` enums, pre-flight, snapshot before, run `cargo dylint --fix` with env vars, diff and write manifest; also `undo_lint_apply` |
| `src/lint_instrument.rs` | Counterpart of `lint_apply.rs` for `instrument` mode (Dylint with `CAPTRACK_PGO_INSTRUMENT=1`) |
| `src/model.rs` | `SiteKey`, `SiteStats`, `Unit` — used by profile loaders |
| `src/profile/mod.rs` | `trait Profile { fn sites() -> Vec<SiteStats> }` |
| `src/profile/captrack.rs` | Parses `captrack::dump_capacity_stats` JSON output; `parse_name()` extracts the original `(file, line, col)` from the `"auto:…"` name (vs the post-instrument file position) |
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

### captrack lib (the wrapper)

1. `src/tracked/<name>.rs` — `Tracked<Name>` struct with fields `inner`, `name: &'static str`, `file: &'static str`, `line: u32`, `column: u32`.  Implement:
   - `with_capacity_named(cap, name, file, line, column)` — calls `registry::record_creation`.
   - `with_capacity_and_hasher_named(...)` — if hash-keyed (per-call override, Axis 2B).
   - `wrap_from(inner: BareX, name, file, line, column)` (Phase K) — wraps a pre-constructed value without allocating; calls `registry::record_creation` then moves `inner` into the wrapper.  This is the universal instrument path.
   - `Deref` / `DerefMut` to the inner type.
   - `Drop` — push sample via `registry::record_sample(self.file, self.line, self.column, sample)`.
   - `IntoIterator` — record sample **before** the move, then `ptr::read(&self.inner)` + `mem::forget(self)`.
   - `From<TrackedX> for StdX` — same pattern (ptr::read + forget, avoids `Default`/`Clone` bounds).
   - `IntoInner for TrackedX` — delegates to `From`.
2. `src/cap_inspect.rs` (Phase L) — add `impl CapInspect for BareX { fn cap_inspect_at(&self, name, file, line, column) { registry::record_sample(file, line, column, self.capacity()) /* or self.len() for len-based */ } }`.  Skipped easily — without it, by-value-escape sites for this type don't get inspected.
3. `src/tracked/mod.rs` — `pub mod` + `pub use`.
4. `src/aliases.rs` — `pub type TrackedX<...> = BareX<...>` (off-feature alias, gated on mirror feature if optional).
5. `src/ctor.rs` — cfg-branched free ctor function(s) for std-only collections; optional third-party families skip this file.
6. `src/lib.rs` — `pub use` from `tracked::` (on-feature), then macro: unified single-arm for std, dual `#[cfg]` arms for optional third-party.  Add `IntoInner` identity impl for the bare type.  If hash-keyed, add `;`-arm to both off/on variants.
7. `clippy.toml.example` — disallowed-method ban entry for the bare constructor.
8. Tests — `src/tests/<name>_tests.rs`: `wrap_from_records_creation`, `drop_records_capacity_sample`, `cap_inspect_works`, `from_impl_preserves_inner` (mirror existing `string_tests.rs` / `smallvec_tests.rs` patterns).  Register the module in `src/tests/mod.rs` under the `#[cfg(feature = "telemetry")]` gate.

### captrack-pgo-lint (recognition)

9. `captrack-pgo-lint/src/lib.rs` — add the new variant to `enum TrackedType`.  Wire it into `recognise_tracked_type`:
   - **std types** — match against `cx.tcx.get_diagnostic_name(adt_def.did())` against the relevant `sym::*` constant.  **WARNING (lesson from #354):** in some nightlies the `#[rustc_diagnostic_item = "X"]` is NOT on the struct itself (it's on individual methods).  Always add a path-string fallback before `match_third_party_path` — see the `"alloc::string::String"` / `"std::string::String"` handling for the canonical pattern.
   - **third-party types** — extend `match_third_party_path(&str)` with the canonical `def_path_str` shape.
10. Update `tracked_type_to_ctor`, `tracked_type_to_static_path`, `tracked_type_supports_hasher`, `hasher_arg_counts` exhaustively for the new variant.  `Ctor` enum in `model.rs` gets a new arm.
11. `captrack-pgo-lint/src/instrument.rs` — extend `tracked_type_to_path` (returns the `::captrack::TrackedX::<…>` path string used in `wrap_from` rewrites) and `tracked_type_display`.
12. **Integration test** — `captrack-pgo-lint/tests/per_type.rs::per_type_<name>_cap` + `tests/fixtures/ui_per_type_<name>_cap/<name>_cap.rs` + `tests/fixtures/ui_per_type_<name>_cap/<name>_cap.stderr`.  If the type's crate isn't already in `captrack-pgo-lint/Cargo.toml` `[dev-dependencies]`, ADD it — the fixture won't compile otherwise (lesson from T2 / hashbrown).

### Final sanity

13. Run `cargo test --features "telemetry smallvec hashbrown <new-feature>" --lib` (captrack), `cargo test --lib` + `cargo test --test ui_test` + `cargo test --test per_type` (captrack-pgo-lint), and `cargo build --release -p captrack-pgo`.  All should be green.

## Repo conventions

- `&'static str` everywhere for `$name` — macros require `$name:literal`, never `&str` or `String`. This is what makes the registry key safe.
- Off-feature macro bodies **must not** introduce any code the compiler can't fold away. Block `{ ... }` is fine; closures, helper fns, intermediate `let` bindings holding values are not (the `let _: &'static str = $name;` line is just a type assertion — it generates no code).
- Don't bump versions or publish without an explicit user request.
