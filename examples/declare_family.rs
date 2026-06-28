//! Example: declare a custom macro family with a project-specific default hasher.
//! Run with:
//!
//!     cargo run --example declare_family --features telemetry
//!
//! `declare_collections!` generates 13 macros (`my_vec!`, `my_map!`, ...) that
//! delegate to `::captrack::t*!` with the named hasher injected as the default.
//! Per-call override is still available via the `;`-arm.

use captrack::IntoInner;

// Declare the family once, at crate root.
captrack::declare_collections! { hasher = fxhash::FxBuildHasher, prefix = my }

fn main() {
    let mut rows = my_vec!("example/declare_family/rows", 32);
    for i in 0..10u32 {
        rows.push(i);
    }
    println!("rows.len() = {}", rows.len());

    let mut index = my_map!("example/declare_family/index", 16);
    for i in 0..5u32 {
        index.insert(i, format!("v{i}"));
    }
    println!("index.len() = {}", index.len());

    // Per-call override still works — pick a different hasher just for this site:
    let mut hot = my_map!(
        "example/declare_family/hot",
        8;
        std::collections::hash_map::RandomState::new()
    );
    hot.insert(42u32, "answer");
    println!("hot.len() = {}", hot.len());

    // Boundary conversion — identical in both feature modes:
    let raw_rows: Vec<u32> = rows.into_inner();
    println!("converted to plain Vec, len = {}", raw_rows.len());
}
