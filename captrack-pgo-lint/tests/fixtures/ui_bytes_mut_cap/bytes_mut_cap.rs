// captrack-pgo-lint per-type integration test: BytesMut — capacity rewrite.
//
// Third-party type: requires --extern bytes flag passed by test harness.
// Profile provides p95=60 → next_pow2(60) = 64.

#![allow(unused_variables)]

fn main() {
    let b = bytes::BytesMut::with_capacity(0);
    let _ = b.len();
}
