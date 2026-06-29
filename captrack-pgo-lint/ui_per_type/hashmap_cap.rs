// captrack-pgo-lint per-type integration test: HashMap — capacity-only rewrite.
//
// Profile provides p95=60 → next_pow2(60) = 64.
// No hasher env var set → capacity-only path.

#![allow(unused_variables)]

use std::collections::HashMap;

fn main() {
    let m = HashMap::<u32, u32>::with_capacity(0);
    let _ = m.len();
}
