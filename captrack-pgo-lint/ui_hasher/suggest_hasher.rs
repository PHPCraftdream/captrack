// captrack-pgo-lint M9 UI test fixture — hasher injection via CAPTRACK_PGO_HASHER=fx.
//
// Note: no `// run-rustfix` directive because the suggested replacements reference
// `::fxhash::FxBuildHasher` which is not a dependency of the test compilation
// unit — the `.fixed` file would fail to compile.  The `.stderr` check is sufficient
// to verify the correct suggestion text is emitted.
//
// Each "MATCH" site has a profile entry generated at test time.
// "NO-MATCH" sites deliberately have no profile entry.

use std::collections::{HashMap, HashSet};

fn main() {
    // MATCH: HashMap::new() → HashMap::with_capacity_and_hasher(64, ::fxhash::FxBuildHasher::default())
    // No type ascription → hasher injection is safe.
    let _m = HashMap::<u8, u8>::new();

    // MATCH: HashMap::with_capacity(4) with explicit type ascription →
    // capacity-only rewrite (hasher skipped due to ascription).
    let _m2: HashMap<u8, u8> = HashMap::with_capacity(4);

    // MATCH: HashSet::new() → HashSet::with_capacity_and_hasher(64, ::fxhash::FxBuildHasher::default())
    // No type ascription → hasher injection is safe.
    let _s = HashSet::<u8>::new();

    // MATCH: Vec::new() → Vec::with_capacity(64) (hasher ignored for Vec)
    let _v = Vec::<u8>::new();

    // NO-MATCH: no profile entry → silent
    let _nm = HashMap::<u8, u8>::new(); // no-match
}
