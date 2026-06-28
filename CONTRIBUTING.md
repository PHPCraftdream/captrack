# Contributing to captrack

Thank you for your interest in contributing!

## Build matrix — all must pass before a PR

```bash
# Off-feature (default) — bare constructors, zero overhead path
cargo build
cargo test

# Telemetry on — Tracked* wrappers, registry, JSON dump
cargo build --features telemetry
cargo test --features telemetry

# Hasher variants
cargo test --features "telemetry fxhash"
cargo test --features "telemetry ahash"

# Lint
cargo clippy --all-targets -- -D warnings
cargo clippy --all-targets --features telemetry -- -D warnings

# Format check
cargo fmt -- --check
```

That is 8 commands.  All must exit 0.

## Adding a new tracked collection

1. `src/tracked/<name>.rs` — `Tracked<Name>` struct with fields `inner`, `name`,
   `file`, `line`, `column`.  Implement:
   - `with_capacity_named(...)` — calls `registry::record_creation`.
   - `with_capacity_and_hasher_named(...)` — if hash-keyed (Axis 2B).
   - `Deref` / `DerefMut` to the inner type.
   - `Drop` — push sample via `registry::record_sample(...)`.
   - `IntoIterator` — record sample **before** the move, then
     `ptr::read(&self.inner)` + `mem::forget(self)`.
   - `From<TrackedX> for BareX` — same pattern (avoids `Default`/`Clone` bounds).
   - `IntoInner for TrackedX` — delegates to `From`.
2. `src/tracked/mod.rs` — add `pub mod` + `pub use`.
3. `src/aliases.rs` — `pub type TrackedX<...> = BareX<...>` (off-feature alias,
   gated on mirror feature flag if optional dep).
4. `src/ctor.rs` — cfg-branched free ctor function(s) for std-only collections;
   optional third-party collections skip this step.
5. `src/lib.rs` — `pub use` from `tracked::` (on-feature), then macro: unified
   single-arm for std, dual `#[cfg]` arms for optional third-party.  Add
   `IntoInner` identity impl for the bare type.  If hash-keyed, add `;`-arm.
6. `clippy.toml.example` — add disallowed-method ban entry for the bare constructor.
7. `captrack-macros/src/lib.rs` — add the new collection to `declare_collections!`
   generator (13 → 14, follow the `{prefix}_{name}` naming pattern).
8. Tests — `off_feature_tests`, `on_feature_tests` (including `into_iter`
   regression), `per_call_override_tests` (if hash-keyed),
   `tests/declare_collections.rs`.

See `CLAUDE.md` for the full architecture reference.

## Style

- `cargo fmt` — always.
- `cargo clippy --all-targets -- -D warnings` — must be clean.
- No `unsafe` outside of the existing `ptr::read` + `mem::forget` pattern in
  `From<TrackedX>` impls; any new `unsafe` block requires a `// SAFETY:` comment.

## Commit style

Short subject line (≤72 chars).  Conventional commit prefixes are welcome but not
required: `feat:`, `fix:`, `docs:`, `perf:`, `test:`, `refactor:`, `chore:`.

## Licensing

`captrack` is dual-licensed under **MIT OR Apache-2.0** (your choice).

By submitting a pull request you agree that your contribution will be licensed
under the same terms — MIT OR Apache-2.0 — as the rest of the project.  This is
the standard Rust ecosystem contributor agreement; no CLA is required.
