# captrack

Capacity telemetry for Rust collections — call-site macros that record peak
capacity, with **zero overhead when disabled**.

## What it does

`captrack` wraps every major Rust collection constructor with a named macro.
When the `telemetry` feature is **off** (the default) each macro expands to
the bare constructor — the compiler sees exactly `Vec::with_capacity(n)` etc.
and optimises accordingly.  When `telemetry` is **on**, each macro returns a
thin `Tracked*` wrapper that records two counters in a global lock-free
registry (using `scc::HashMap`):

- `peak_capacity` — maximum capacity observed across all instances of that name.
- `creation_count` — total number of times that call-site was reached.

At the end of a benchmark call `captrack::dump_capacity_stats("path.json")` to
write a sorted JSON report.

```
[
  { "name": "engine/write_batch", "peak_capacity": 512, "creation_count": 1234 },
  ...
]
```

Use the data to replace guesses like `Vec::with_capacity(16)` with
data-driven values.

## Quick start

```toml
[dependencies]
captrack = "0.1"
indexmap = "2"   # for tmap!/tset!
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

Off (default) = zero overhead, bare constructors.
On = `Tracked*` wrappers, global registry, JSON dump.

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

## All 13 macros

| Macro         | Collection                        | Notes                         |
|---------------|-----------------------------------|-------------------------------|
| `tvec!`       | `Vec<T>`                          |                               |
| `tvecdeque!`  | `VecDeque<T>`                     |                               |
| `tbtreemap!`  | `BTreeMap<K,V>`                   | cap hint accepted, ignored    |
| `tbtreeset!`  | `BTreeSet<T>`                     | cap hint accepted, ignored    |
| `tbytesmut!`  | `bytes::BytesMut`                 | requires `bytes` crate        |
| `tfxmap!`     | `std::HashMap<K,V,S>`             | `;`-arm supported             |
| `tfxset!`     | `std::HashSet<T,S>`               | `;`-arm supported             |
| `tmap!`       | `indexmap::IndexMap<K,V,S>`       | `;`-arm, requires `indexmap`  |
| `tset!`       | `indexmap::IndexSet<T,S>`         | `;`-arm, requires `indexmap`  |
| `tdashmap!`   | `dashmap::DashMap<K,V,S>`         | `;`-arm, requires `dashmap`   |
| `tsccmap!`    | `scc::HashMap<K,V,S>`             | `;`-arm, requires `scc`       |
| `tsccset!`    | `scc::HashSet<T,S>`               | `;`-arm, requires `scc`       |
| `tscctree!`   | `scc::TreeIndex<K,V>`             | cap hint accepted, ignored    |

## Tracked types (telemetry mode)

When `telemetry` is enabled the macros return `Tracked*` wrappers:
`TrackedVec<T>`, `TrackedHashMap<K,V,S>`, `TrackedIndexMap<K,V,S>`, etc.
All wrappers implement `Deref`/`DerefMut` to the underlying collection so
existing code continues to work transparently.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT License ([LICENSE-MIT](LICENSE-MIT))

at your option.
