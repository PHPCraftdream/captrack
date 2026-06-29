// captrack-pgo-lint per-type integration test: hashbrown::HashMap — capacity rewrite.
//
// Third-party type: requires --extern hashbrown flag passed by test harness.
// Profile provides p95=60 → next_pow2(60) = 64.

#![allow(unused_variables)]

fn main() {
    let m = hashbrown::HashMap::<u32, u32>::with_capacity(0);
    let _ = m.len();
}
