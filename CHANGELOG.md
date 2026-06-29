# Changelog

All notable changes to `captrack` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

## Unreleased

### `captrack-pgo` — M11: capacity policy knobs (`--cap-from`, `--cap-mul`, `--cap-round`)

**Three new flags on `captrack-pgo apply`** expose the formula used to compute
the proposed capacity value.  Defaults are chosen to reproduce M9 behaviour
exactly (zero behavioural diff when flags are omitted).

| Flag | Env var | Values | Default |
|---|---|---|---|
| `--cap-from` | `CAPTRACK_PGO_CAP_FROM` | `max` \| `mean` \| `median` \| `p95` \| `p99` | `p95` |
| `--cap-mul` | `CAPTRACK_PGO_CAP_MUL` | float > 0 | `1.0` |
| `--cap-round` | `CAPTRACK_PGO_CAP_ROUND` | `pow2` \| `to8` \| `exact` | `pow2` |

Formula: `cap = round_mode( source_statistic × cap_mul )`.

**Per-site policy override** — profile JSON entries may carry an optional
`policy` field.  Fields present in the per-site policy override the
corresponding CLI defaults for that one site; absent fields fall back to globals.
This allows hot-path sites to use `max` while the rest of the workspace uses
`p95`, without two separate `apply` invocations.

**Plugin changes (`captrack-pgo-lint`):**

- New `PolicyDefaults` struct: `{ cap_from: CapFrom, cap_mul: f64, cap_round:
  CapRound }` constructed once per compiler process from `OnceLock`-cached env
  var readers (`read_cap_from`, `read_cap_mul`, `read_cap_round`).
- `propose_cap` signature extended: `propose_cap(stats, current, globals:
  PolicyDefaults) -> Decision`.  Per-site `stats.policy` fields override
  individual `globals` fields; invalid `cap_mul` (≤ 0 or NaN) is clamped to 1.0.
- `CapFrom` and `CapRound` (from M10 model) are now consumed in the rule logic.
- New unit tests (rules module): `cap_from_max_uses_peak`,
  `cap_from_median_with_mul_2`, `cap_from_p99_falls_back_to_p95_when_none`,
  `cap_from_mean_falls_back_to_peak_when_none`, `cap_round_exact_no_rounding`,
  `per_site_policy_overrides_global_cap_from`, `per_site_policy_overrides_global_cap_round`,
  `cap_mul_zero_degrades_to_one`, `cap_round_to8_boundary`.

**CLI changes (`captrack-pgo`):**

- `Apply` subcommand gains `--cap-from`, `--cap-mul`, `--cap-round` arguments.
- New `CapFromChoice` and `CapRoundChoice` enums in `lint_apply.rs` (local
  mirrors of the plugin model — `captrack-pgo` remains stable-only).
- `LintApplyArgs` gains `cap_from: CapFromChoice`, `cap_mul: f64`,
  `cap_round: CapRoundChoice` fields.
- `run_lint_apply` forwards choices as env vars; default-variant values are
  omitted (env var removed) to keep the environment minimal.
- `--dry-run` output includes `CAPTRACK_PGO_CAP_FROM=<value>` etc. when set to
  non-default values.
- Pre-flight check: `--cap-mul <= 0.0` or NaN exits with a clear error before
  touching any files.
- New integration tests: `cap_policy_flags_pass_env_vars`,
  `cap_policy_defaults_omit_env_vars`, `cap_mul_zero_is_rejected`,
  `cap_mul_negative_is_rejected`, `invalid_cap_from_is_rejected_by_clap`,
  `invalid_cap_round_is_rejected_by_clap`.

---

### `captrack-pgo` — M9: `--hasher` flag in `apply`

**`captrack-pgo apply` now accepts `--hasher <fx|ahash|foldhash|none>`.**

When set to anything other than `none` (the default), every matched
`HashMap`/`HashSet` constructor in the target workspace is upgraded to
`with_capacity_and_hasher(N, <hasher_default_expr>)` in addition to the
existing capacity rewrite.

| `--hasher` | Replacement expression |
|---|---|
| `fx` | `::fxhash::FxBuildHasher::default()` |
| `ahash` | `::ahash::RandomState::new()` |
| `foldhash` | `::foldhash::fast::RandomState::default()` |
| `none` | no hasher change (default) |

**Behaviour details:**

- `HashMap::new()` → `HashMap::with_capacity_and_hasher(N, <expr>)`
- `HashMap::with_capacity(K)` → `HashMap::with_capacity_and_hasher(N, <expr>)`
- `HashMap::with_capacity_and_hasher(K, h)` where `h` is one of the three
  known defaulted hasher expressions → replace both K and h (idempotent).
- `HashMap::with_capacity_and_hasher(K, h)` where `h` is a custom expression
  → preserve the user's hasher, replace K only.
- `Vec`, `VecDeque`, `BTreeMap`, `BTreeSet` — `--hasher` is silently ignored.

**Type-ascription guard:** sites with an explicit `let` binding type annotation
(`let m: HashMap<K, V> = ...`) have the hasher injection skipped automatically.
A note is emitted in the suggestion text.  Sites in struct fields or function
return types are not detected; users may see compile errors if they accept the
suggestion in those contexts.

**Dependency reminder:** after applying with `--hasher fx`, captrack-pgo prints
a reminder to add `fxhash` (or the chosen crate) to your `Cargo.toml`.

**Plugin changes (`captrack-pgo-lint`):**

- New `HasherChoice` enum and `read_hasher_choice()` function (reads
  `CAPTRACK_PGO_HASHER` env var once, cached in `OnceLock`).
- New `has_local_type_ascription()` HIR helper — walks the parent chain via
  `tcx.hir_parent_id_iter()` / `tcx.hir_node()` to detect `LetStmt` with
  `ty: Some(...)`.
- `emit_with_suggestion` and `build_suggestion` extended to emit the
  `with_capacity_and_hasher` form when appropriate.
- New UI test fixture `ui_hasher/suggest_hasher.rs` + `.stderr` covering all
  four cases (HashMap/new, HashMap/with_capacity+ascription, HashSet/new,
  Vec/new).

**CLI changes (`captrack-pgo`):**

- `Apply` subcommand gains `--hasher <value>` argument (validated by clap,
  default `none`).
- `LintApplyArgs` gains `pub hasher: HasherChoice` field.
- `run_lint_apply` forwards the choice as `CAPTRACK_PGO_HASHER` env var.
- `dry_run` output includes `CAPTRACK_PGO_HASHER=<value>` when hasher is set.

---

### `captrack-pgo` — BREAKING CHANGES (M5: Path-B migration completed)

The syn-based `propose` / `apply` (old) / `auto` subcommands and their
underlying pipeline (`scan.rs`, `plan.rs`, `rules.rs`, `report.rs`,
`apply.rs`) have been **removed**.  They are replaced by a single Dylint-based
`apply` subcommand that delegates to `cargo dylint --fix` via the
`captrack-pgo-lint` plugin.

**What changed:**

- `captrack-pgo apply` now means the Dylint-driven rewrite (formerly
  `lint-apply`).  The old syn-based `apply` (byte-splice patcher) is gone.
- `captrack-pgo propose` — removed.  Use `captrack-pgo apply --dry-run`.
- `captrack-pgo auto` — removed.  Use `captrack-pgo apply`.
- `captrack-pgo undo` — simplified.  Only handles the new
  `last-lint-apply.json` manifest format; the old `last-apply.json` (v1,
  syn-based) is no longer producible and can no longer be reverted with
  `undo`.  If you have an old manifest from before M5, restore via `git`.
- Dependencies `syn`, `quote`, `proc-macro2`, `walkdir` removed from
  `captrack-pgo/Cargo.toml`.

**Why the switch:**

The syn-based matcher had coverage gaps: it could not resolve type aliases,
`Default::default()` calls, or constructors inside macro expansions.  The
Dylint plugin operates on rustc's HIR after type-checking, giving true
semantic resolution with no false negatives for standard collection types.
The trade-off is a nightly pin in the plugin workspace — accepted at the
user's direction (M4 decision, 2026-06-28).

## 0.1.0 — initial release

### Added

**Public API — macros (13 total)**

- `tvec!("name", cap)` — `Vec<T>` (unified, zero-overhead off-feature)
- `tvecdeque!("name", cap)` — `VecDeque<T>`
- `tbtreemap!("name", cap)` — `BTreeMap<K,V>` (cap hint accepted, ignored)
- `tbtreeset!("name", cap)` — `BTreeSet<T>` (cap hint accepted, ignored)
- `tbytesmut!("name", cap)` — `bytes::BytesMut` (requires `bytes` crate or `telemetry`)
- `tfxmap!("name", cap[; hasher])` — `std::HashMap<K,V,S>` with `;`-arm per-call hasher override
- `tfxset!("name", cap[; hasher])` — `std::HashSet<T,S>` with `;`-arm override
- `tmap!("name", cap[; hasher])` — `indexmap::IndexMap<K,V,S>` with `;`-arm override
- `tset!("name", cap[; hasher])` — `indexmap::IndexSet<T,S>` with `;`-arm override
- `tdashmap!("name", cap[; hasher])` — `dashmap::DashMap<K,V,S>` with `;`-arm override
- `tsccmap!("name", cap[; hasher])` — `scc::HashMap<K,V,S>` with `;`-arm override
- `tsccset!("name", cap[; hasher])` — `scc::HashSet<T,S>` with `;`-arm override
- `tscctree!("name", cap)` — `scc::TreeIndex<K,V>` (cap hint accepted, ignored)

**Public API — types and traits**

- `trait IntoInner` — converts `TrackedX` (or its off-feature alias) to the inner bare type
  without requiring `S: Default` or `S: Clone`; uses `ptr::read` + `mem::forget` internally.
- `struct SampleStats` with fields `count`, `min`, `max`, `mean`, `median`, `p95`, `p99`,
  `stddev`; constructed via `SampleStats::from_samples(&[usize]) -> Option<SampleStats>`.
- `fn dump_capacity_stats(path: impl AsRef<Path>) -> std::io::Result<()>` — writes a sorted
  JSON report (`version`, `stats[]`) in telemetry mode; no-op stub in off-feature mode.

**Axis 1 — `telemetry` feature (on/off)**

- Off (default): every macro expands to the bare constructor; compiler sees no extra code.
- On: macros return `Tracked*` wrapper structs; a global lock-free registry keyed by
  `(file, line, column)` call-site location collects samples via `scc::Bag<usize>`.
- `TrackedVec`, `TrackedVecDeque`, `TrackedBTreeMap`, `TrackedBTreeSet`, `TrackedBytesMut`,
  `TrackedHashMap`, `TrackedHashSet`, `TrackedIndexMap`, `TrackedIndexSet`,
  `TrackedDashMap`, `TrackedSccHashMap`, `TrackedSccHashSet`, `TrackedSccTreeIndex` —
  all implement `Deref`/`DerefMut`, `Drop`, `IntoIterator`, `From<TrackedX> for BareX`,
  and `IntoInner`.

**Axis 1B — off-feature mirror features (alias-only, no telemetry overhead)**

- `bytes`, `indexmap`, `dashmap`, `scc` feature flags expose `TrackedX` as a type alias
  to the underlying bare type so consumer code compiles without `#[cfg]` guards.

**Axis 2A — `CapHasher` global default via feature flags**

- Default: `std::collections::hash_map::RandomState`
- `fxhash` → `fxhash::FxBuildHasher`
- `ahash` → `ahash::RandomState`
- `foldhash` → `foldhash::fast::RandomState`
- `rustc-hash` → `rustc_hash::FxBuildHasher`
- Selecting two hasher features simultaneously triggers a `compile_error!`.

**Axis 2B — `;`-arm per-call hasher override**

All 7 hash macros (`tfxmap!`, `tfxset!`, `tmap!`, `tset!`, `tdashmap!`, `tsccmap!`,
`tsccset!`) accept an optional `; hasher_expr` arm to inject a different hasher at a
single call-site without changing the global default.

**Axis 2C — `declare_collections!` proc-macro**

- Crate `captrack-macros` (companion proc-macro crate, published separately).
- `captrack::declare_collections! { hasher = MyHasher, prefix = my }` generates 13
  `macro_rules!` (`my_vec!`, `my_map!`, …) that delegate to `::captrack::t*!` with the
  named hasher injected via the `;`-arm.  Per-call `; hasher` override in generated
  macros is preserved.

**Axis 3 — clippy enforcement**

- `clippy.toml.example` — full disallowed-methods ban list covering all bare constructors
  for Vec, VecDeque, HashMap, HashSet, BTreeMap, BTreeSet, BytesMut, IndexMap, IndexSet,
  DashMap, scc::HashMap, scc::HashSet, scc::TreeIndex.
- All captrack macro expansions include `#[allow(clippy::disallowed_methods,
  clippy::disallowed_types)]` so consumer-level bans never fire on generated code.

### Internal architecture

- Registry key is `(&'static str, u32, u32)` — `(file, line, column)` captured via
  `file!()`, `line!()`, `column!()` in each macro.  Each distinct source location is one
  independent entry; the `name` string is a human label only.
- Samples are stored in a `scc::Bag<usize>` (lock-free); `dump_capacity_stats` drains via
  `pop_all`, serialises, then pushes values back (registry survives repeated dumps).
- `src/ctor.rs` — `#[cfg]`-branched `#[inline(always)]` free ctor functions for the 6 std
  macros; off-feature variant folds to a bare constructor with zero overhead.
- `src/aliases.rs` — off-feature `TrackedX = BareX` type aliases for source-level symmetry.
- `From<TrackedX> for BareX` impls use `unsafe { ptr::read(&self.inner) }` +
  `mem::forget(self)` to move the inner value without `S: Default` or `S: Clone` bounds.
