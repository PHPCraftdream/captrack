# captrack

[![CI](https://github.com/PHPCraftdream/captrack/actions/workflows/ci.yml/badge.svg)](https://github.com/PHPCraftdream/captrack/actions/workflows/ci.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)
[![MSRV: 1.74](https://img.shields.io/badge/MSRV-1.74-orange.svg)](https://github.com/PHPCraftdream/captrack#minimum-supported-rust-version)
[![docs.rs](https://img.shields.io/docsrs/captrack)](https://docs.rs/captrack)
[![crates.io](https://img.shields.io/crates/v/captrack.svg)](https://crates.io/crates/captrack)

Capacity telemetry for Rust collections — call-site macros that record actual
observed capacities, with **zero overhead when disabled**.

## What it does

`captrack` wraps every major Rust collection constructor with a named macro.
When the `telemetry` feature is **off** (the default) each macro expands to
the bare constructor — the compiler sees exactly `Vec::with_capacity(n)` etc.
and optimises accordingly.  When `telemetry` is **on**, each macro returns a
thin `Tracked*` wrapper that records per-call-site statistics in a global
lock-free registry (using `scc::HashMap`):

- **Registry key** — `(file, line, column)` of the macro call-site, not the
  name string.  Each distinct source location is one independent entry.
- **`creation_count`** — total number of instances created at that call-site
  (atomic `u64`, updated on construction).
- **`samples`** — a reservoir-sampled list of observed capacities/lengths
  (Vitter Algorithm R, bounded to `CAPTRACK_SAMPLE_CAP`, default 4096).
  Pushed on every `Drop`, `into_iter`, or `cap_inspect_at` call:
  - For capacity-based collections (`Vec`, `VecDeque`, `String`, `HashMap`,
    `HashSet`, `IndexMap`, `IndexSet`, `BytesMut`, `SmallVec`,
    `hashbrown::HashMap`, `BinaryHeap`): records `inner.capacity()` — this is
    the allocated backing-store size, which grows monotonically, so the final
    value equals the peak.
  - For length-based collections (`BTreeMap`, `BTreeSet`, `DashMap`,
    `scc::HashMap`, `scc::HashSet`, `scc::TreeIndex`): records `inner.len()`
    at the moment of Drop.  This is **NOT** peak occupancy — if the collection
    shrinks before Drop the recorded value undercounts the true peak.
- **`total_observed`** — the true population count of all capacity
  observations (including those beyond the reservoir cap).  When
  `total_observed > samples.len()` the reservoir is saturated and
  percentile statistics should be read as approximate (±5% for uniform
  distributions, per regression tests).

At the end of a benchmark call `captrack::dump_capacity_stats("path.json")` to
write a sorted JSON report.

```json
{
  "version": 1,
  "stats": [
    {
      "name": "engine/write_batch",
      "file": "src/engine.rs",
      "line": 142,
      "column": 17,
      "creation_count": 1234,
      "total_observed": 19000,
      "samples": [16, 32, 32, 64, 64, 64, 128, 128, 256, 1024]
    }
  ]
}
```

`total_observed` is omitted when the reservoir was never saturated
(`total_observed == samples.len()`), keeping the format backward-compatible
with pre-reservoir readers.

Entries are sorted by `max(samples)` descending so the largest allocations
surface first.  The `samples` list is a reservoir snapshot; aggregate
statistics (median, p95, p99, stddev) are computed in post-processing:

```rust
use captrack::SampleStats;

// After deserialising the JSON — for each entry:
if let Some(stats) = SampleStats::from_samples(&entry.samples) {
    println!("p95 = {}, median = {}", stats.p95, stats.median);
}
```

Set `CAPTRACK_SAMPLE_CAP` at process start to override the reservoir size
(e.g. `CAPTRACK_SAMPLE_CAP=128` for minimal memory, `CAPTRACK_SAMPLE_CAP=65536`
for high-accuracy profiling of skewed distributions).

Use the data to replace guesses like `Vec::with_capacity(16)` with
data-driven values.

## Quick start

```toml
[dependencies]
captrack = "0.1"
indexmap = "2"   # for tmap!/tset!

# To use TrackedIndexMap as a type alias even without telemetry:
# captrack = { version = "0.1", features = ["indexmap"] }
```

```rust
use captrack::{tvec, tmap, tbtreemap};

// Named, zero-overhead in production:
let mut v = tvec!("my_module/rows", 64);
let mut m = tmap!("my_module/index", 32);
let mut b = tbtreemap!("my_module/sorted", 0);
```

## Three orthogonal axes

### Axis 1 — telemetry on/off

```toml
# Enable telemetry (e.g. in a benchmark profile):
[dependencies]
captrack = { version = "0.1", features = ["telemetry"] }
```

Off (default) = zero overhead, bare constructors.  `TrackedX` names are still
available as type aliases to the underlying std/third-party types via
`src/aliases.rs`.

On = `Tracked*` wrapper structs, global lock-free registry (`scc::HashMap`
keyed by call-site location), JSON dump.  Enabling `telemetry` automatically
activates all optional mirror features (`bytes`, `indexmap`, `dashmap`, `scc`).

The `TrackedX` alias mirror features let you use e.g. `TrackedIndexMap` in
off-feature mode without pulling in telemetry overhead:

```toml
[dependencies]
captrack = { version = "0.1", features = ["indexmap"] }  # TrackedIndexMap alias, no telemetry
```

```rust
// Works in both modes — no #[cfg] needed:
captrack::dump_capacity_stats("target/cap-stats/my_bench.json")?;
```

### Axis 2 — hasher choice

Three levels, from coarsest to finest:

#### Level A — global default via feature flag

| Feature      | `CapHasher`                                          |
|--------------|------------------------------------------------------|
| *(none)*     | `RandomState` (DoS-safe, std default)                |
| `fxhash`     | `fxhash::FxBuildHasher` (fast, non-cryptographic)    |
| `ahash`      | `ahash::RandomState`                                 |
| `foldhash`   | `foldhash::fast::RandomState`                        |
| `rustc-hash` | `rustc_hash::FxBuildHasher`                          |

Select **at most one** — `compile_error!` fires otherwise.

```toml
# Your Cargo.toml:
captrack = { version = "0.1", features = ["ahash"] }
```

```rust
// All hash macros now use ahash as the default:
let m = captrack::tmap!("my/map", 16);
```

#### Level B — per-call override via `;`-arm

```rust
use captrack::{tmap, tfxmap};

// uses CapHasher (global default):
let m1 = tmap!("cache/entries", 64);

// this one call uses ahash regardless of CapHasher:
let m2 = tmap!("cache/hotpath", 8; ahash::RandomState::new());
```

All 7 hash macros (`tfxmap!`, `tfxset!`, `tmap!`, `tset!`, `tdashmap!`,
`tsccmap!`, `tsccset!`) support the `;`-arm.

#### Level C — custom family via `declare_collections!`

```rust
// In your crate root (once) — requires captrack in [dependencies]:
captrack::declare_collections! { hasher = MyExoticHasher, prefix = my }

// Generated macros:
//   my_vec!   my_vecdeque!  my_btreemap!  my_btreeset!  my_bytesmut!
//   my_fxmap! my_fxset!     my_map!       my_set!
//   my_dashmap! my_sccmap!  my_sccset!    my_scctree!

let rows = my_vec!("table/rows", 128);
let index = my_map!("table/index", 64);
// index uses MyExoticHasher by default
```

The generated macros delegate to `::captrack::t*!` with the custom hasher
injected via the `;`-arm.  The telemetry on/off decision is made by captrack's
own feature flag, not yours.

### Axis 3 — enforcing the discipline (clippy)

Copy `clippy.toml.example` (fully or partially) into your project's
`clippy.toml` to ban bare collection constructors.  The captrack macros carry
`#[allow(clippy::disallowed_methods, clippy::disallowed_types)]` internally so
they are always safe — the ban applies only to hand-written bare constructors.

```toml
# clippy.toml (your project) — partial example:
disallowed-methods = [
    { path = "std::vec::Vec::with_capacity",
      reason = "use captrack::tvec!(\"name\", cap)" },
    { path = "std::collections::HashMap::with_capacity_and_hasher",
      reason = "use captrack::tfxmap!(\"name\", cap)" },
    # ... see clippy.toml.example for the full list
]
```

## All 17 macros

| Macro            | Collection                           | Notes                                      |
|------------------|--------------------------------------|--------------------------------------------|
| `tvec!`          | `Vec<T>`                             |                                            |
| `tvecdeque!`     | `VecDeque<T>`                        |                                            |
| `tbtreemap!`     | `BTreeMap<K,V>`                      | cap hint accepted, ignored                 |
| `tbtreeset!`     | `BTreeSet<T>`                        | cap hint accepted, ignored                 |
| `tbytesmut!`     | `bytes::BytesMut`                    | requires `bytes` crate                     |
| `tfxmap!`        | `std::HashMap<K,V,S>`                | `;`-arm supported                          |
| `tfxset!`        | `std::HashSet<T,S>`                  | `;`-arm supported                          |
| `tmap!`          | `indexmap::IndexMap<K,V,S>`          | `;`-arm, requires `indexmap`               |
| `tset!`          | `indexmap::IndexSet<T,S>`            | `;`-arm, requires `indexmap`               |
| `tdashmap!`      | `dashmap::DashMap<K,V,S>`            | `;`-arm, requires `dashmap`                |
| `tsccmap!`       | `scc::HashMap<K,V,S>`               | `;`-arm, requires `scc`                    |
| `tsccset!`       | `scc::HashSet<T,S>`                  | `;`-arm, requires `scc`                    |
| `tscctree!`      | `scc::TreeIndex<K,V>`                | cap hint accepted, ignored                 |
| `tstring!`       | `String`                             | capacity-based                             |
| `tbinaryheap!`   | `BinaryHeap<T>`                      | capacity-based                             |
| `thashbrownmap!` | `hashbrown::HashMap<K,V,S>`          | `;`-arm supported; capacity-based          |
| `tsmallvec!`     | `smallvec::SmallVec<A>`              | requires `smallvec` crate; capacity-based  |

## `t*_owned!` — initial-capacity-only siblings

Ten of the macros above have an `_owned` sibling: `tvec_owned!`,
`tvecdeque_owned!`, `tbytesmut_owned!`, `tfxmap_owned!`, `tfxset_owned!`,
`tmap_owned!`, `tset_owned!`, `tdashmap_owned!`, `tsccmap_owned!`,
`tsccset_owned!`. Unlike their `t*!` counterparts, these:

- always return the **bare** collection type (`Vec<T>`, `HashMap<K,V,S>`,
  …) in both feature modes — no `Tracked*` wrapper, no `.into_inner()` call;
- record only the **initial** requested capacity as a single sample,
  instead of tracking the Drop-time peak.

Use them at call-sites where the capacity you pass in already is the final
size (e.g. `tvec_owned!("name", input.len())`) and you want a plain
collection at the function boundary. `tbtreemap!`, `tbtreeset!`, and
`tscctree!` have no `_owned` variant — those types have no `with_capacity`
constructor, so an initial-capacity sample would always be `0`.

## Tracked types (telemetry mode)

When `telemetry` is enabled the macros return `Tracked*` wrappers:
`TrackedVec<T>`, `TrackedHashMap<K,V,S>`, `TrackedIndexMap<K,V,S>`, etc.
All wrappers implement `Deref`/`DerefMut` to the underlying collection so
existing code continues to work transparently.

## Profile-guided capacity optimization (`captrack-pgo`)

`captrack-pgo` is a companion CLI that closes the measure-apply loop.
The recommended one-command UX runs the full collection cycle and writes
a merged profile to disk:

```text
captrack-pgo measure --workspace <path> --captrack-path <captrack-checkout> \
    --bench tx_pipeline --bench wal_append --bench filter_eval
# → target/captrack-pgo/merged.json
```

`measure` is an RAII-guarded orchestration: `wire` (patch every member's
`Cargo.toml`) → `instrument` (Dylint plugin injects `TrackedX::wrap_from`
on construction sites and `CapInspect::cap_inspect_at` on by-value-escape
consumption points) → for each `--bench`, `cargo bench --no-run` then run
the binary with `CAPTRACK_DUMP_DIR` set + a per-bench timeout → `merge`
the per-bench dumps into one profile → `uninstrument` → `unwire`.  A
panic at any inner step still runs the cleanup tail.

Once you have a profile, you can optionally classify per-site
distribution and inject tailored cap policies before apply:

```text
captrack-pgo analyze --profile merged.json --write-policy
# UnimodalTight  → cap_from=max  (zero realloc, minimal waste)
# UnimodalSpread → cap_from=p95  (global default)
# Bimodal        → cap_from=median × 2.0  (size typical case)
# HeavyTail      → cap_from=p95 × 0.5     (don't follow the tail)
# MostlyZero     → cap_from=p99 over non-zero subset
```

Then apply the profile to source:

```text
captrack-pgo apply --profile merged.json [--workspace <path>] \
    [--hasher fx|ahash|foldhash|none] \
    [--cap-from max|mean|median|p95|p99] \
    [--cap-mul <float>] \
    [--cap-round pow2|to8|exact] \
    [--dry-run]
```

- Without `--hasher` (or `--hasher none`): only capacity hints are
  updated (`Vec::new()` → `Vec::with_capacity(N)`, etc.).
- With `--hasher fx`: matched `HashMap` / `HashSet` / `IndexMap` /
  `IndexSet` / `DashMap` / `scc::HashMap` / `scc::HashSet`
  constructors are upgraded to `with_capacity_and_hasher(N,
  ::fxhash::FxBuildHasher::default())`.  Other hasher options:
  `ahash` (`::ahash::RandomState::new()`) and `foldhash`
  (`::foldhash::fast::RandomState::default()`).
- The chosen hasher crate must already be a dependency of your
  workspace (captrack-pgo emits a reminder).
- **Type-ascribed lets are rewritten via multi-span suggestions**
  (Phase N): `let m: HashMap<K, V> = HashMap::new();` becomes
  `let m: HashMap<K, V, FxBuildHasher> = HashMap::with_capacity_and_hasher(N, FxBuildHasher::default());`
  — the ascription's generic argument list and the constructor are
  rewritten atomically through `cargo fix`, so the diff never goes
  through an intermediate `E0308` state.
- **Already-fast hashers are detected and skipped** (Phase O): a
  binding ascribed `HashMap<K, V, FxBuildHasher>` or any
  `BuildHasherDefault<FxHasher>` alias (e.g. shamir-db's `THasher`)
  is recognised as already-fast and the hasher-swap suggestion is
  suppressed.  Capacity-only rewrite still fires.

### Architecture — wrap_from + CapInspect

`captrack-pgo instrument` does NOT replace the constructor.  It WRAPS
the original expression:

```rust
// before
let v = vec![item, other_item];

// after
let v = ::captrack::TrackedVec::wrap_from(
    vec![item, other_item],   // original expression evaluated first
    "auto:src/foo.rs:42:13",
    file!(), line!(), column!(),
);
```

This is universal — it works for any constructor shape (`X::new()`,
`X::with_capacity(N)`, `X::default()`, `vec![…]`, `smallvec![…]`,
`X::from_iter(…)`, custom builders) without losing element
initialisation.  `TrackedX` derefs to `X`, so receiver / `&v` /
indexing positions continue to compile against the wrapped value.

The data-flow guard skips by-value escape positions (return, struct
field init, function argument, type-ascribed `let` init) because
`TrackedX != X` there would trip `E0308`.  For those sites, the lint
instead injects a borrow-only inspection at the consumption point
(Phase L), leaving the binding's type untouched:

```rust
// before
fn build() -> Vec<u8> {
    let mut v = Vec::new();
    fill(&mut v);
    v
}

// after
fn build() -> Vec<u8> {
    let mut v = Vec::new();
    fill(&mut v);
    { ::captrack::CapInspect::cap_inspect_at(&v, "auto:…",
                                              file!(), line!(),
                                              column!()); v }
}
```

The block expression evaluates `cap_inspect_at(&v, …)` for its
side-effect (record `v.capacity()` against the binding's call-site)
then yields `v` unchanged.  Type stays `Vec<u8>`; no E0308.

   **Capacity policy knobs** (defaults reproduce the pre-M11 behaviour exactly):

   | Flag | Env var | Values | Default |
   |---|---|---|---|
   | `--cap-from` | `CAPTRACK_PGO_CAP_FROM` | `max` \| `mean` \| `median` \| `p95` \| `p99` | `p95` |
   | `--cap-mul` | `CAPTRACK_PGO_CAP_MUL` | any float > 0 | `1.0` |
   | `--cap-round` | `CAPTRACK_PGO_CAP_ROUND` | `pow2` \| `to8` \| `exact` | `pow2` |

   Formula: `cap = round_mode( source_statistic × cap_mul )`.

   Examples:

   ```bash
   # Never reallocate (capacity = peak observed value):
   captrack-pgo apply --profile profile.json --cap-from max

   # Conservative: median × 2, rounded to next power of two:
   captrack-pgo apply --profile profile.json --cap-from median --cap-mul 2.0

   # Exact 99th-percentile value (no rounding):
   captrack-pgo apply --profile profile.json --cap-from p99 --cap-round exact
   ```

   **Per-site policy override in the profile JSON:**

   Individual hot-path sites can override the global policy by adding a `policy`
   field to their entry in the profile JSON.  Each sub-field is independent;
   missing ones fall back to the global CLI defaults.

   ```json
   {
     "key": { "file": "src/engine.rs", "line": 42, "col": 13 },
     "peak": 1024, "mean": 96.4, "p50": 64, "p95": 256, "p99": 512, "count": 100,
     "policy": { "cap_from": "max", "cap_mul": 1.0, "cap_round": "pow2" }
   }
   ```

   A site with `"policy": { "cap_from": "max" }` will always use its peak value,
   regardless of the `--cap-from` flag passed on the command line.  Other fields
   (`cap_mul`, `cap_round`) not listed in the per-site policy still come from
   the CLI defaults.

   Internally this runs `cargo dylint --fix` with the `captrack-pgo-lint`
   plugin, which resolves collection constructors at the semantic (HIR) level
   and emits `rustfix` suggestions.  A before/after manifest is written to
   `target/captrack-pgo/last-apply.json`.

4. **Undo** — revert the last apply or instrument:

   ```text
   captrack-pgo undo
   ```

**Requirements:** `cargo install cargo-dylint dylint-link` and the
`nightly-2026-04-16` toolchain (pinned in `captrack-pgo-lint/rust-toolchain.toml`).

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT License ([LICENSE-MIT](LICENSE-MIT))

at your option.
