// captrack-pgo-lint per-type integration test: String — capacity rewrite.
//
// String::with_capacity exists and is capacity-backed (like Vec<u8> internally).
// Profile provides p95=60 → next_pow2(60) = 64.
// This file is scanned by per_type.rs::per_type_string_cap to locate the call-site.

#![allow(unused_variables)]

fn main() {
    let s = String::with_capacity(0);
    let _ = s.len();
}
