// captrack-pgo-lint Phase N UI test: HashMap with type ascription + hasher injection.
//
// CAPTRACK_PGO_HASHER=fx set.
// The let binding has an explicit type ascription `HashMap<u32, u32>` (2 generics,
// hasher omitted = HasherOmitted) → multi-span suggestion:
//   Span A: ascription → HashMap<u32, u32, ::fxhash::FxBuildHasher>
//   Span B: ctor       → HashMap::with_capacity_and_hasher(64, ::fxhash::FxBuildHasher::default())

#![allow(unused_variables)]

use std::collections::HashMap;

fn main() {
    let m: HashMap<u32, u32> = HashMap::<u32, u32>::with_capacity(0);
    let _ = m.len();
}
