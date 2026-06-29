// captrack-pgo-lint per-type integration test: HashSet — capacity-only rewrite.
//
// Profile provides p95=60 → next_pow2(60) = 64.

#![allow(unused_variables)]

use std::collections::HashSet;

fn main() {
    let s = HashSet::<u32>::with_capacity(0);
    let _ = s.len();
}
