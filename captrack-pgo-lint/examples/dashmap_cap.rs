// captrack-pgo-lint per-type integration test example: DashMap — capacity rewrite.
//
// Compiled as a Cargo example so `dashmap` dep is available.
// Profile provides p95=60 → next_pow2(60) = 64.

#![allow(unused_variables)]

fn main() {
    let m = dashmap::DashMap::<u32, u32>::with_capacity(0);
    let _ = m.len();
}
