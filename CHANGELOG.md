# Changelog

All notable changes to `captrack` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

## 0.1.0 — Initial release

### Added

- 13 call-site macros: `tvec!`, `tvecdeque!`, `tbtreemap!`, `tbtreeset!`,
  `tbytesmut!`, `tfxmap!`, `tfxset!`, `tmap!`, `tset!`, `tdashmap!`,
  `tsccmap!`, `tsccset!`, `tscctree!`.
- Axis 1: `telemetry` feature — zero-overhead off, `Tracked*` wrappers on.
- Axis 2A: `CapHasher` feature-matrix — `RandomState` default, opt-in to
  `fxhash` / `ahash` / `foldhash` / `rustc-hash`.
- Axis 2B: `;`-arm per-call hasher override in all 7 hash macros.
- Axis 2C: `declare_collections!` proc-macro (crate `captrack-macros`) that
  generates a custom-hasher macro family.
- `TrackedHashMap<K,V,S>`, `TrackedIndexMap<K,V,S>`, `TrackedHashSet<T,S>`,
  `TrackedIndexSet<T,S>`, `TrackedDashMap<K,V,S>`, `TrackedSccHashMap<K,V,S>`,
  `TrackedSccHashSet<T,S>` — all generic over the build-hasher `S`.
- `dump_capacity_stats(path)` — JSON dump, no-op in off-feature mode.
- `clippy.toml.example` — full list of bare constructors to ban (Axis 3).
