// captrack-pgo-lint per-type integration test: SmallVec — capacity rewrite.
//
// Third-party type: requires --extern smallvec flag passed by test harness.
// Profile provides p95=60 → next_pow2(60) = 64.

#![allow(unused_variables)]

fn main() {
    let v = smallvec::SmallVec::<[u32; 4]>::with_capacity(0);
    let _ = v.len();
}
