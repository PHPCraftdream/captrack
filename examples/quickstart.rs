//! Quickstart: record capacity samples for a few call-sites, dump to JSON,
//! then compute summary stats.  Run with:
//!
//!     cargo run --example quickstart --features telemetry
//!
//! Without `--features telemetry` this example still runs but produces an
//! empty dump (off-feature mode is zero-overhead — nothing is recorded).

#[cfg(feature = "telemetry")]
use captrack::SampleStats;
use captrack::{dump_capacity_stats, tvec, IntoInner};

fn main() -> std::io::Result<()> {
    // Simulate a workload: build several Vecs of varying sizes.
    for round in 0..50 {
        let cap = 16 << (round % 4); // 16, 32, 64, 128, repeat
        let mut v = tvec!("example/quickstart/batch", cap);
        for i in 0..cap as u32 {
            v.push(i);
        }
        // Convert at the boundary — identical in both feature modes:
        let raw: Vec<u32> = v.into_inner();
        std::hint::black_box(raw);
    }

    // Write the dump.
    let path = std::env::temp_dir().join("captrack-quickstart.json");
    dump_capacity_stats(&path)?;
    println!("Dump written to {}", path.display());

    // Off-feature this is empty.  On-feature, read it back and summarise.
    #[cfg(feature = "telemetry")]
    {
        let json = std::fs::read_to_string(&path)?;
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        for entry in parsed["stats"].as_array().unwrap() {
            let name = entry["name"].as_str().unwrap();
            let samples: Vec<usize> = entry["samples"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_u64().unwrap() as usize)
                .collect();
            if let Some(s) = SampleStats::from_samples(&samples) {
                println!(
                    "{name}: n={}, min={}, median={}, p95={}, max={}",
                    s.count, s.min, s.median, s.p95, s.max
                );
            }
        }
    }
    Ok(())
}
