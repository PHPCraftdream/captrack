// captrack-pgo-lint per-type integration test: Vec — capacity rewrite.
//
// Profile provides p95=60 → next_pow2(60) = 64.
// This file is scanned by ui_test.rs::per_type_vec to locate the call-site.

#![allow(unused_variables)]

fn main() {
    let v = Vec::<u32>::with_capacity(0);
    let _ = v.len();
}
