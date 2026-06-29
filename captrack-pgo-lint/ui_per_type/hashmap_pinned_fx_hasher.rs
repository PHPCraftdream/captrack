// captrack-pgo-lint Phase O UI test: HashMap with FxBuildHasher already pinned.
//
// CAPTRACK_PGO_HASHER=fx set.
// The let binding has an explicit type ascription `HashMap<u32, u32, fxhash::FxBuildHasher>`
// (3 generics, hasher pinned = HasherPinned, kind = FastKnown).
//
// Phase O: detect that the pinned hasher is already a fast hasher → emit
// capacity-only suggestion with "already pins a fast hasher" label.
// The ascription is NOT rewritten — only the ctor capacity is updated.
//
// Note: `HashMap::with_capacity_and_hasher(0, fxhash::FxBuildHasher::default())` is used so
// the code compiles (ascription and ctor agree on the hasher type).

#![allow(unused_variables)]

use std::collections::HashMap;

fn main() {
    let m: HashMap<u32, u32, fxhash::FxBuildHasher> =
        HashMap::with_capacity_and_hasher(0, fxhash::FxBuildHasher::default());
    let _ = m.len();
}
