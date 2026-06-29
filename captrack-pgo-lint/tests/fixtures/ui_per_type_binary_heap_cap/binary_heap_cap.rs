// captrack-pgo-lint per-type integration test: BinaryHeap — capacity rewrite.
//
// BinaryHeap::with_capacity exists and is capacity-backed.
// Profile provides p95=60 → next_pow2(60) = 64.
// This file is scanned by per_type.rs::per_type_binary_heap_cap to locate the call-site.

#![allow(unused_variables)]

use std::collections::BinaryHeap;

fn main() {
    let h = BinaryHeap::<u32>::with_capacity(0);
    let _ = h.len();
}
