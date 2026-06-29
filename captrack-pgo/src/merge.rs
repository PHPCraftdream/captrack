//! `merge` subcommand — combine per-bench captrack profile dumps into one.
//!
//! When `wire → instrument → bench` produces multiple
//! `profile-<binary_stem>.json` files (one per bench binary), this module
//! merges them into a single file that `apply` can consume:
//!
//! - Sites are deduplicated by `(file, line, column)`.
//! - `creation_count` is summed across inputs.
//! - `samples` are concatenated, then optionally sub-sampled with Vitter R
//!   reservoir sampling when the total exceeds `reservoir_cap`.
//! - The output entries are sorted by `max(samples)` descending (hottest
//!   sites first), mirroring the order that captrack's own dump produces.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

// ──────────────────────────────────────────────────────────────────────────────
// Public API
// ──────────────────────────────────────────────────────────────────────────────

/// Arguments for `run_merge`.
pub struct MergeArgs {
    /// Resolved (glob-expanded) list of input profile JSON paths.
    pub inputs: Vec<PathBuf>,
    /// Destination path for the merged JSON.
    pub output: PathBuf,
    /// Maximum samples to keep per site after merging (0 = unlimited).
    pub reservoir_cap: usize,
}

/// Summary returned by `run_merge` (printed to stdout by the CLI dispatcher).
#[derive(Debug)]
pub struct MergeReport {
    pub inputs_count: usize,
    pub unique_sites: usize,
    pub total_samples_pre_reservoir: usize,
    pub total_samples_post: usize,
}

/// Entry point — merge all inputs and write `args.output`.
pub fn run_merge(args: MergeArgs) -> Result<MergeReport> {
    if args.inputs.is_empty() {
        bail!("--inputs must specify at least one file");
    }

    // ── 1. Parse all input files ────────────────────────────────────────────
    // Key: (file_string, line, column)  →  accumulated entry.
    let mut acc: BTreeMap<SiteKey, AccEntry> = BTreeMap::new();
    let inputs_count = args.inputs.len();

    for path in &args.inputs {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("read profile {}", path.display()))?;
        let dump: Dump = serde_json::from_str(&raw)
            .with_context(|| format!("parse profile {}", path.display()))?;

        for entry in dump.stats {
            let key = SiteKey {
                file: entry.file.to_string_lossy().into_owned(),
                line: entry.line,
                column: entry.column,
            };
            let acc_entry = acc.entry(key).or_insert_with(|| AccEntry {
                name: entry.name.clone(),
                file: entry.file.clone(),
                line: entry.line,
                column: entry.column,
                creation_count: 0,
                samples: Vec::new(),
            });

            // Warn when the same site has diverging names across inputs.
            if acc_entry.name != entry.name {
                eprintln!(
                    "warn: site ({}:{}:{}) has diverging names: {:?} vs {:?} — keeping first",
                    entry.file.display(),
                    entry.line,
                    entry.column,
                    acc_entry.name,
                    entry.name,
                );
            }

            acc_entry.creation_count = acc_entry.creation_count.saturating_add(entry.creation_count);
            acc_entry.samples.extend(entry.samples);
        }
    }

    // ── 2. Reservoir sampling ───────────────────────────────────────────────
    let unique_sites = acc.len();
    let mut total_pre = 0usize;
    let mut total_post = 0usize;

    let cap = args.reservoir_cap;

    let mut merged_entries: Vec<DumpEntry> = acc
        .into_values()
        .map(|mut e| {
            total_pre += e.samples.len();
            if cap > 0 && e.samples.len() > cap {
                let seed = site_seed(&e);
                e.samples = reservoir_sample(&e.samples, cap, &seed);
            }
            total_post += e.samples.len();
            DumpEntry {
                name: e.name,
                file: e.file,
                line: e.line,
                column: e.column,
                creation_count: e.creation_count,
                samples: e.samples,
            }
        })
        .collect();

    // ── 3. Sort by max(samples) descending ─────────────────────────────────
    merged_entries.sort_unstable_by(|a, b| {
        let ma = a.samples.iter().copied().max().unwrap_or(0);
        let mb = b.samples.iter().copied().max().unwrap_or(0);
        mb.cmp(&ma)
    });

    // ── 4. Write output ─────────────────────────────────────────────────────
    let out_dump = Dump {
        version: 1,
        stats: merged_entries,
    };
    let json = serde_json::to_string_pretty(&out_dump)
        .context("serialise merged profile")?;

    // Ensure parent directory exists.
    if let Some(parent) = args.output.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create output directory {}", parent.display()))?;
        }
    }
    std::fs::write(&args.output, json.as_bytes())
        .with_context(|| format!("write merged profile {}", args.output.display()))?;

    Ok(MergeReport {
        inputs_count,
        unique_sites,
        total_samples_pre_reservoir: total_pre,
        total_samples_post: total_post,
    })
}

// ──────────────────────────────────────────────────────────────────────────────
// JSON schema types (local — match the captrack dump format directly)
// ──────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, Serialize)]
struct Dump {
    version: u32,
    stats: Vec<DumpEntry>,
}

#[derive(Debug, Deserialize, Serialize)]
struct DumpEntry {
    name: String,
    file: PathBuf,
    line: u32,
    column: u32,
    creation_count: u64,
    samples: Vec<usize>,
}

// ──────────────────────────────────────────────────────────────────────────────
// Internal accumulator
// ──────────────────────────────────────────────────────────────────────────────

/// BTreeMap key — (file_as_string, line, column).
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct SiteKey {
    file: String,
    line: u32,
    column: u32,
}

/// Mutable accumulator for one site across all inputs.
struct AccEntry {
    name: String,
    file: PathBuf,
    line: u32,
    column: u32,
    creation_count: u64,
    samples: Vec<usize>,
}

// ──────────────────────────────────────────────────────────────────────────────
// Reservoir sampling — Vitter R algorithm
// ──────────────────────────────────────────────────────────────────────────────

/// Deterministic seed derived from the site's location so merges of the same
/// inputs always produce the same output regardless of processing order.
fn site_seed(e: &AccEntry) -> u64 {
    let file_str = e.file.to_string_lossy();
    // Simple mix: djb2-style hash of file bytes, then xor with position.
    let mut h: u64 = 5381;
    for b in file_str.bytes() {
        h = h.wrapping_mul(33).wrapping_add(b as u64);
    }
    h ^= (e.line as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    h ^= (e.column as u64).wrapping_mul(0x6c62_272e_07bb_0142);
    h
}

/// LCG parameters (from Knuth, TAOCP Vol.2).
const LCG_A: u64 = 6_364_136_223_846_793_005;
const LCG_C: u64 = 1_442_695_040_888_963_407;

/// Advance the LCG state and return a value in `[0, modulus)`.
#[inline]
fn lcg_next(state: &mut u64, modulus: u64) -> u64 {
    *state = state.wrapping_mul(LCG_A).wrapping_add(LCG_C);
    // Use the upper 32 bits for better quality.
    ((*state >> 32) as u64) % modulus
}

/// Vitter R reservoir sampling — select exactly `cap` items from `population`
/// using the deterministic LCG seeded by `seed`.
///
/// If `population.len() <= cap`, returns a clone of `population`.
fn reservoir_sample(population: &[usize], cap: usize, seed: &u64) -> Vec<usize> {
    if population.len() <= cap {
        return population.to_vec();
    }
    // Fill reservoir with first `cap` elements.
    let mut reservoir: Vec<usize> = population[..cap].to_vec();
    let mut state = *seed;

    for (i, &item) in population[cap..].iter().enumerate() {
        let j = i + cap; // index in the full population
        // Random index in [0, j].
        let r = lcg_next(&mut state, (j + 1) as u64) as usize;
        if r < cap {
            reservoir[r] = item;
        }
    }
    reservoir
}

// ──────────────────────────────────────────────────────────────────────────────
// Glob expansion helper (used by CLI dispatcher)
// ──────────────────────────────────────────────────────────────────────────────

/// Expand a list of path strings, resolving any glob patterns.
///
/// Paths that do not contain `*` or `?` are passed through as-is (as
/// `PathBuf`).  Paths with wildcards are expanded through the `glob` crate.
/// Glob results are sorted for deterministic ordering.
///
/// Returns an error if a glob pattern is syntactically invalid, or if a
/// literal (non-glob) path does not exist.
pub fn expand_inputs(raw: &[String]) -> Result<Vec<PathBuf>> {
    let mut out: Vec<PathBuf> = Vec::new();
    for pattern in raw {
        let is_glob = pattern.contains('*') || pattern.contains('?') || pattern.contains('[');
        if is_glob {
            let mut matched: Vec<PathBuf> = glob::glob(pattern)
                .with_context(|| format!("invalid glob pattern {:?}", pattern))?
                .map(|r| r.with_context(|| format!("glob entry in {:?}", pattern)))
                .collect::<Result<Vec<_>>>()?;
            if matched.is_empty() {
                eprintln!("warn: glob {:?} matched no files", pattern);
            }
            matched.sort();
            out.extend(matched);
        } else {
            let p = PathBuf::from(pattern);
            if !p.exists() {
                bail!("input file not found: {}", p.display());
            }
            out.push(p);
        }
    }
    Ok(out)
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    // ── helpers ───────────────────────────────────────────────────────────

    fn write_profile(dir: &Path, name: &str, stats: &[(&str, &str, u32, u32, u64, &[usize])]) {
        let entries: Vec<_> = stats
            .iter()
            .map(|(entry_name, file, line, col, cc, samples)| {
                serde_json::json!({
                    "name": entry_name,
                    "file": file,
                    "line": line,
                    "column": col,
                    "creation_count": cc,
                    "samples": samples
                })
            })
            .collect();
        let dump = serde_json::json!({
            "version": 1,
            "stats": entries
        });
        std::fs::write(
            dir.join(name),
            serde_json::to_string_pretty(&dump).unwrap(),
        )
        .unwrap();
    }

    // ── test 1: single input passthrough ──────────────────────────────────

    #[test]
    fn merge_single_input_passthrough() {
        let tmp = tempfile::tempdir().unwrap();
        write_profile(
            tmp.path(),
            "a.json",
            &[
                ("auto:src/foo.rs:10:5", "src/foo.rs", 10, 5, 3, &[1, 2, 3]),
                ("auto:src/bar.rs:20:1", "src/bar.rs", 20, 1, 1, &[100]),
            ],
        );
        let out = tmp.path().join("merged.json");
        let report = run_merge(MergeArgs {
            inputs: vec![tmp.path().join("a.json")],
            output: out.clone(),
            reservoir_cap: 4096,
        })
        .unwrap();

        assert_eq!(report.inputs_count, 1);
        assert_eq!(report.unique_sites, 2);
        assert_eq!(report.total_samples_pre_reservoir, 4);
        assert_eq!(report.total_samples_post, 4);

        let raw = std::fs::read_to_string(&out).unwrap();
        let dump: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(dump["version"], 1);
        let stats = dump["stats"].as_array().unwrap();
        assert_eq!(stats.len(), 2);
        // Sorted by max(samples) desc: bar(100) before foo(3).
        assert_eq!(stats[0]["file"].as_str().unwrap(), "src/bar.rs");
        assert_eq!(stats[1]["file"].as_str().unwrap(), "src/foo.rs");
    }

    // ── test 2: two inputs with overlapping sites ─────────────────────────

    #[test]
    fn merge_two_inputs_dedup_sites() {
        let tmp = tempfile::tempdir().unwrap();
        write_profile(
            tmp.path(),
            "a.json",
            &[("auto:src/hot.rs:5:3", "src/hot.rs", 5, 3, 10, &[8, 16])],
        );
        write_profile(
            tmp.path(),
            "b.json",
            &[("auto:src/hot.rs:5:3", "src/hot.rs", 5, 3, 20, &[32, 64])],
        );
        let out = tmp.path().join("merged.json");
        let report = run_merge(MergeArgs {
            inputs: vec![tmp.path().join("a.json"), tmp.path().join("b.json")],
            output: out.clone(),
            reservoir_cap: 0,
        })
        .unwrap();

        assert_eq!(report.unique_sites, 1);
        assert_eq!(report.total_samples_pre_reservoir, 4);
        assert_eq!(report.total_samples_post, 4);

        let dump: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
        let stats = dump["stats"].as_array().unwrap();
        assert_eq!(stats.len(), 1, "must deduplicate to single site");
        assert_eq!(stats[0]["creation_count"].as_u64().unwrap(), 30, "cc must be summed");
        let samples: Vec<u64> = stats[0]["samples"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_u64().unwrap())
            .collect();
        assert_eq!(samples.len(), 4, "samples must be concatenated");
        assert!(samples.contains(&8));
        assert!(samples.contains(&64));
    }

    // ── test 3: disjoint sites produce union ──────────────────────────────

    #[test]
    fn merge_two_inputs_disjoint_sites() {
        let tmp = tempfile::tempdir().unwrap();
        write_profile(
            tmp.path(),
            "a.json",
            &[("auto:src/a.rs:1:1", "src/a.rs", 1, 1, 5, &[10])],
        );
        write_profile(
            tmp.path(),
            "b.json",
            &[("auto:src/b.rs:2:2", "src/b.rs", 2, 2, 7, &[20])],
        );
        let out = tmp.path().join("merged.json");
        let report = run_merge(MergeArgs {
            inputs: vec![tmp.path().join("a.json"), tmp.path().join("b.json")],
            output: out.clone(),
            reservoir_cap: 0,
        })
        .unwrap();

        assert_eq!(report.unique_sites, 2);

        let dump: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
        let stats = dump["stats"].as_array().unwrap();
        assert_eq!(stats.len(), 2, "disjoint sites: union must have 2 entries");
    }

    // ── test 4: reservoir sampling ─────────────────────────────────────────

    #[test]
    fn merge_with_reservoir_cap() {
        let tmp = tempfile::tempdir().unwrap();
        // Generate 10 000 samples (0..10000).
        let samples: Vec<usize> = (0..10_000).collect();
        let entries: Vec<serde_json::Value> = vec![serde_json::json!({
            "name": "auto:src/hot.rs:1:1",
            "file": "src/hot.rs",
            "line": 1u32,
            "column": 1u32,
            "creation_count": 10_000u64,
            "samples": samples
        })];
        let dump = serde_json::json!({"version": 1, "stats": entries});
        std::fs::write(
            tmp.path().join("big.json"),
            serde_json::to_string(&dump).unwrap(),
        )
        .unwrap();

        let out = tmp.path().join("merged.json");
        let cap = 100;
        let report = run_merge(MergeArgs {
            inputs: vec![tmp.path().join("big.json")],
            output: out.clone(),
            reservoir_cap: cap,
        })
        .unwrap();

        assert_eq!(report.total_samples_pre_reservoir, 10_000);
        assert_eq!(report.total_samples_post, cap);

        let dump: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
        let kept: Vec<u64> = dump["stats"][0]["samples"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_u64().unwrap())
            .collect();
        assert_eq!(kept.len(), cap);
        // All values must come from the original range.
        assert!(kept.iter().all(|&v| v < 10_000));
    }

    // ── test 5: reservoir sampling is deterministic ────────────────────────

    #[test]
    fn merge_reservoir_is_deterministic() {
        let tmp = tempfile::tempdir().unwrap();
        let samples: Vec<usize> = (0..1_000).collect();
        let entry = serde_json::json!({
            "name": "auto:src/x.rs:7:3",
            "file": "src/x.rs",
            "line": 7u32,
            "column": 3u32,
            "creation_count": 1_000u64,
            "samples": samples
        });
        let dump = serde_json::json!({"version": 1, "stats": [entry]});
        let json = serde_json::to_string(&dump).unwrap();
        std::fs::write(tmp.path().join("x.json"), &json).unwrap();

        let run = |out_name: &str| {
            let out = tmp.path().join(out_name);
            run_merge(MergeArgs {
                inputs: vec![tmp.path().join("x.json")],
                output: out.clone(),
                reservoir_cap: 50,
            })
            .unwrap();
            let raw = std::fs::read_to_string(&out).unwrap();
            let d: serde_json::Value = serde_json::from_str(&raw).unwrap();
            let s: Vec<u64> = d["stats"][0]["samples"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_u64().unwrap())
                .collect();
            s
        };

        let first = run("out1.json");
        let second = run("out2.json");
        assert_eq!(first, second, "reservoir sampling must be deterministic");
    }

    // ── test 6: empty inputs array errors gracefully ───────────────────────

    #[test]
    fn merge_empty_inputs_array() {
        let tmp = tempfile::tempdir().unwrap();
        let result = run_merge(MergeArgs {
            inputs: vec![],
            output: tmp.path().join("out.json"),
            reservoir_cap: 0,
        });
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("--inputs"),
            "error message should mention --inputs, got: {msg}"
        );
    }

    // ── test 7: glob expansion ─────────────────────────────────────────────

    #[test]
    fn merge_glob_expansion() {
        let tmp = tempfile::tempdir().unwrap();
        for (name, line) in [("p1.json", 1u32), ("p2.json", 2u32), ("p3.json", 3u32)] {
            write_profile(
                tmp.path(),
                name,
                &[(&format!("auto:src/x.rs:{}:1", line), "src/x.rs", line, 1, 1, &[line as usize])],
            );
        }
        // Use glob pattern to expand all three files.
        let pattern = format!("{}/*.json", tmp.path().display());
        let expanded = expand_inputs(&[pattern]).unwrap();
        assert_eq!(expanded.len(), 3, "glob must expand to 3 files");

        let out = tmp.path().join("merged.json");
        let report = run_merge(MergeArgs {
            inputs: expanded,
            output: out.clone(),
            reservoir_cap: 0,
        })
        .unwrap();
        assert_eq!(report.inputs_count, 3);
        assert_eq!(report.unique_sites, 3, "3 disjoint sites");
    }

    // ── test 8: utf-8 / non-ASCII path content doesn't crash ──────────────

    #[test]
    fn merge_utf8_content() {
        // Write a profile whose `name` and `file` fields contain non-ASCII chars.
        let tmp = tempfile::tempdir().unwrap();
        let dump = serde_json::json!({
            "version": 1,
            "stats": [{
                "name": "auto:src/фу-бар.rs:5:1",
                "file": "src/фу-бар.rs",
                "line": 5u32,
                "column": 1u32,
                "creation_count": 1u64,
                "samples": [42usize]
            }]
        });
        std::fs::write(
            tmp.path().join("utf8.json"),
            serde_json::to_string_pretty(&dump).unwrap(),
        )
        .unwrap();

        let out = tmp.path().join("merged.json");
        let report = run_merge(MergeArgs {
            inputs: vec![tmp.path().join("utf8.json")],
            output: out.clone(),
            reservoir_cap: 0,
        })
        .unwrap();
        assert_eq!(report.unique_sites, 1);

        let raw = std::fs::read_to_string(&out).unwrap();
        assert!(raw.contains("фу-бар"), "non-ASCII content must survive round-trip");
    }
}
