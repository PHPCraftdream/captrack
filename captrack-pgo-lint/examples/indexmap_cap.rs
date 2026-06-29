// captrack-pgo-lint per-type integration test example: IndexMap — capacity rewrite.
//
// Compiled as a Cargo example so `indexmap` dep is available.
// Profile provides p95=60 → next_pow2(60) = 64.

#![allow(unused_variables)]

fn main() {
    let m = indexmap::IndexMap::<u32, u32>::with_capacity(0);
    let _ = m.len();
}
