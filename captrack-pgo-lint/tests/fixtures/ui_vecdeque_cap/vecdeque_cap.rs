// captrack-pgo-lint per-type integration test: VecDeque — capacity rewrite.
//
// Profile provides p95=60 → next_pow2(60) = 64.

#![allow(unused_variables)]

use std::collections::VecDeque;

fn main() {
    let v = VecDeque::<u32>::with_capacity(0);
    let _ = v.len();
}
