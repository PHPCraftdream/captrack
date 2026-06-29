// captrack-pgo-lint per-type integration test example: BytesMut — capacity rewrite.
//
// Compiled as a Cargo example so `bytes` dep is available.
// Profile provides p95=60 → next_pow2(60) = 64.

#![allow(unused_variables)]

fn main() {
    let b = bytes::BytesMut::with_capacity(0);
    let _ = b.len();
}
