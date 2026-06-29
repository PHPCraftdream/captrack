// captrack-pgo-lint per-type integration test: IndexMap — capacity rewrite.
//
// Third-party type: requires --extern indexmap flag passed by test harness.
// Profile provides p95=60 → next_pow2(60) = 64.

#![allow(unused_variables)]

fn main() {
    let m = indexmap::IndexMap::<u32, u32>::with_capacity(0);
    let _ = m.len();
}
