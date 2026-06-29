//! Per-type integration tests for the captrack-pgo-lint capacity rewrite and hasher injection.
//!
//! ## Design
//!
//! ### Std types (Vec, VecDeque, HashMap, HashSet, BTreeMap, BTreeSet)
//!
//! These are covered by `ui_test` on the `ui_per_type/` directory.  Each fixture file has one
//! call-site; a profile entry is generated at test time from the fixture's actual path + the
//! call-site's line/col (found by scanning the source text for the pattern).
//!
//! ### Third-party types (BytesMut, IndexMap, IndexSet, DashMap, scc::HashMap, scc::HashSet,
//!     scc::TreeIndex, SmallVec)
//!
//! For these types `ui_test` can't compile the fixture directly because the crates are not
//! on the default rustc search path.  Instead we locate the compiled rlib artifacts that Cargo
//! put into `target/debug/deps/` when it built this test binary, and pass them as `--extern`
//! flags via `dylint_testing::ui::Test::rustc_flags`.
//!
//! ### Hasher matrix
//!
//! Hash-keyed types (HashMap, HashSet, IndexMap, IndexSet, DashMap, scc::HashMap, scc::HashSet)
//! get a hasher-injection test (`CAPTRACK_PGO_HASHER=fx` → `with_capacity_and_hasher`).
//! Non-hashing types (Vec, VecDeque, BytesMut, SmallVec, BTreeMap, BTreeSet, scc::TreeIndex)
//! get a capacity-only test.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Serialize all per-type tests that mutate env vars.
static ENV_MUTEX: Mutex<()> = Mutex::new(());

// ── Profile JSON helpers ──────────────────────────────────────────────────────

/// Profile values used for all per-type tests:
/// peak=100, p50=60, p95=60, count=50 → next_pow2(60) = 64.
const PROFILE_PEAK: usize = 100;
const PROFILE_P50: usize = 60;
const PROFILE_P95: usize = 60;
const PROFILE_COUNT: u64 = 50;

#[derive(serde::Serialize)]
struct SiteStats {
    key: SiteKey,
    unit: Unit,
    peak: usize,
    p50: usize,
    p95: usize,
    count: u64,
}

#[derive(serde::Serialize)]
struct SiteKey {
    file: PathBuf,
    line: u32,
    col: u32,
}

#[derive(serde::Serialize)]
enum Unit {
    Elements,
}

/// Scan a fixture source file for `pattern` (skipping lines starting with `//`
/// or containing `// no-match`) and return the first matching `SiteStats`.
///
/// Panics if the pattern is not found — a bug in the test fixture.
fn make_single_entry(file: &Path, source: &str, pattern: &str) -> SiteStats {
    for (line_idx, line_str) in source.lines().enumerate() {
        let trimmed = line_str.trim_start();
        if trimmed.starts_with("//") || line_str.contains("// no-match") {
            continue;
        }
        if let Some(byte_offset) = line_str.find(pattern) {
            return SiteStats {
                key: SiteKey {
                    file: file.to_path_buf(),
                    line: (line_idx + 1) as u32,
                    col: byte_offset as u32 + 1,
                },
                unit: Unit::Elements,
                peak: PROFILE_PEAK,
                p50: PROFILE_P50,
                p95: PROFILE_P95,
                count: PROFILE_COUNT,
            };
        }
    }
    panic!("pattern {:?} not found in {}", pattern, file.display());
}

/// Write a profile JSON covering the given sites to a file in the fixtures dir.
///
/// Returns the profile file path.
fn write_profile(sites: &[SiteStats], name: &str) -> PathBuf {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let fixtures_dir = manifest_dir.join("tests").join("fixtures");
    std::fs::create_dir_all(&fixtures_dir).expect("create fixtures dir");
    let profile_path = fixtures_dir.join(format!("profile_{name}.json"));
    let json = serde_json::to_string_pretty(sites).expect("serialise profile");
    std::fs::write(&profile_path, &json).expect("write profile JSON");
    profile_path
}

// ── Extern-flag helpers for third-party types ────────────────────────────────

/// Find all rlib / dylib / so files in `target/debug/deps/` that correspond
/// to the given crate name (underscored).  Returns `--extern name=path` pairs
/// and the `-L` dep directory path.
///
/// Cargo compiles dev-dependencies before running tests; by the time this
/// function runs those artifacts exist in the deps directory.
fn extern_flags_for(crate_names: &[&str]) -> Vec<String> {
    let target_dir = PathBuf::from(
        std::env::var("CARGO_TARGET_DIR")
            .or_else(|_| std::env::var("CARGO_MANIFEST_DIR").map(|d| format!("{d}/../target")))
            .unwrap_or_else(|_| "target".to_string()),
    );
    let deps_dir = target_dir.join("debug").join("deps");

    let mut flags: Vec<String> = vec![
        "-L".to_string(),
        format!("dependency={}", deps_dir.display()),
    ];

    for &name in crate_names {
        let name_under = name.replace('-', "_");
        // Find the most recently modified matching rlib artifact.
        let artifact = find_newest_rlib(&deps_dir, &name_under);
        if let Some(path) = artifact {
            flags.push("--extern".to_string());
            flags.push(format!("{name_under}={}", path.display()));
        }
    }

    flags
}

/// Find the most recently modified rlib for the given crate name in `deps_dir`.
///
/// Cargo may produce multiple versions (hash suffixes); take the newest.
fn find_newest_rlib(deps_dir: &Path, name_under: &str) -> Option<PathBuf> {
    let prefix = format!("lib{name_under}-");
    let read_dir = std::fs::read_dir(deps_dir).ok()?;
    let mut candidates: Vec<(std::time::SystemTime, PathBuf)> = read_dir
        .filter_map(|e| e.ok())
        .filter(|e| {
            let fname = e.file_name();
            let s = fname.to_string_lossy();
            (s.starts_with(&prefix) || s.starts_with(&format!("{name_under}-")))
                && (s.ends_with(".rlib") || s.ends_with(".rmeta"))
        })
        .filter_map(|e| {
            let mt = e.metadata().ok()?.modified().ok()?;
            Some((mt, e.path()))
        })
        .collect();
    candidates.sort_by(|a, b| b.0.cmp(&a.0)); // newest first
    candidates.into_iter().next().map(|(_, p)| p)
}

// ── Std-type tests (ui_test on ui_per_type/) ─────────────────────────────────

/// Run a ui_test for a single std-type fixture in `ui_per_type/`.
///
/// Creates a profile JSON for that file only, sets env vars, and calls
/// `dylint_testing::ui_test`.  The `.stderr` file in `ui_per_type/` defines
/// the expected lint output.
///
/// `test_name` should be the `#[test]` function name — used as the unique key
/// for the temp directory and profile file so parallel tests don't race.
fn run_std_type_test(
    test_name: &str,
    fixture_stem: &str,
    ctor_pattern: &str,
    hasher: Option<&str>,
) {
    run_per_type_test(test_name, fixture_stem, ctor_pattern, hasher, &[]);
}

/// Core runner: copy the fixture to an isolated temp dir, generate a profile
/// pointing to the copy, and run `dylint_testing::ui_test` (with optional
/// extra `--extern` / `-L` flags for third-party crates).
///
/// Isolation is necessary because `ui_test` runs ALL `.rs` files in the target
/// directory — putting all fixtures in one directory would require one giant
/// profile covering every fixture simultaneously, making each test a witness
/// to every other test's profile entry.  Per-fixture temp dirs avoid this.
///
/// `test_name` uniquely identifies this test invocation (e.g. the Rust
/// `#[test]` function name).  Multiple tests may share the same `fixture_stem`
/// (e.g. `vec_cap` is used by both the cap-only test and the hasher-env test).
/// Using `test_name` in the temp-dir path prevents parallel tests from racing
/// on the same directory.
fn run_per_type_test(
    test_name: &str,
    fixture_stem: &str,
    ctor_pattern: &str,
    hasher: Option<&str>,
    extern_crates: &[&str],
) {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let ui_per_type_dir = manifest_dir.join("ui_per_type");
    let rs_src = ui_per_type_dir.join(format!("{fixture_stem}.rs"));
    let stderr_src = ui_per_type_dir.join(format!("{fixture_stem}.stderr"));

    // Create an isolated subdirectory so compiletest sees only this fixture.
    // Use test_name (not fixture_stem) so tests that share a fixture don't
    // race when run in parallel.
    let tmp_dir = manifest_dir
        .join("tests")
        .join("fixtures")
        .join(format!("ui_{test_name}"));
    std::fs::create_dir_all(&tmp_dir).expect("create per-fixture ui dir");

    let rs_dst = tmp_dir.join(format!("{fixture_stem}.rs"));
    let stderr_dst = tmp_dir.join(format!("{fixture_stem}.stderr"));
    std::fs::copy(&rs_src, &rs_dst).unwrap_or_else(|e| panic!("copy .rs failed: {e}"));
    std::fs::copy(&stderr_src, &stderr_dst).unwrap_or_else(|e| panic!("copy .stderr failed: {e}"));

    // Generate profile using the COPY path — the compiler will see rs_dst as
    // the source file, so the profile key must use that path.
    // Use test_name for the profile filename to avoid races between tests that
    // share a fixture stem (each test gets its own profile JSON).
    let source = std::fs::read_to_string(&rs_dst).unwrap();
    let entry = make_single_entry(&rs_dst, &source, ctor_pattern);
    let profile_path = write_profile(&[entry], test_name);

    let extra_flags = if extern_crates.is_empty() {
        vec![]
    } else {
        extern_flags_for(extern_crates)
    };

    let _guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        std::env::remove_var("CAPTRACK_PGO_INSTRUMENT");
        std::env::set_var("CAPTRACK_PGO_PROFILE", &profile_path);
        match hasher {
            Some(h) => std::env::set_var("CAPTRACK_PGO_HASHER", h),
            None => std::env::remove_var("CAPTRACK_PGO_HASHER"),
        }
    }

    if extra_flags.is_empty() {
        dylint_testing::ui_test(env!("CARGO_PKG_NAME"), &tmp_dir);
    } else {
        dylint_testing::ui::Test::src_base(env!("CARGO_PKG_NAME"), &tmp_dir)
            .rustc_flags(extra_flags)
            .run();
    }
}

#[test]
fn per_type_vec_cap() {
    run_std_type_test(
        "per_type_vec_cap",
        "vec_cap",
        "Vec::<u32>::with_capacity(0)",
        None,
    );
}

#[test]
fn per_type_vecdeque_cap() {
    run_std_type_test(
        "per_type_vecdeque_cap",
        "vecdeque_cap",
        "VecDeque::<u32>::with_capacity(0)",
        None,
    );
}

#[test]
fn per_type_hashmap_cap_only() {
    run_std_type_test(
        "per_type_hashmap_cap_only",
        "hashmap_cap",
        "HashMap::<u32, u32>::with_capacity(0)",
        None,
    );
}

#[test]
fn per_type_hashset_cap_only() {
    run_std_type_test(
        "per_type_hashset_cap_only",
        "hashset_cap",
        "HashSet::<u32>::with_capacity(0)",
        None,
    );
}

#[test]
fn per_type_btreemap_cap() {
    run_std_type_test(
        "per_type_btreemap_cap",
        "btreemap_cap",
        "BTreeMap::<u32, u32>::new()",
        None,
    );
}

#[test]
fn per_type_btreeset_cap() {
    run_std_type_test(
        "per_type_btreeset_cap",
        "btreeset_cap",
        "BTreeSet::<u32>::new()",
        None,
    );
}

#[test]
fn per_type_hashmap_hasher_fx() {
    run_std_type_test(
        "per_type_hashmap_hasher_fx",
        "hashmap_hasher",
        "HashMap::<u32, u32>::with_capacity(0)",
        Some("fx"),
    );
}

#[test]
fn per_type_hashset_hasher_fx() {
    run_std_type_test(
        "per_type_hashset_hasher_fx",
        "hashset_hasher",
        "HashSet::<u32>::with_capacity(0)",
        Some("fx"),
    );
}

// ── Third-party type tests (ui_test with extern flags) ───────────────────────

/// Run a ui_test for a fixture in `ui_per_type/` that uses third-party types.
///
/// Same as `run_std_type_test` but also passes `--extern` / `-L` flags so
/// the compiler can resolve the third-party crate imports.
///
/// `test_name` should be the `#[test]` function name — used as the unique key
/// for the temp directory and profile file so parallel tests don't race.
fn run_third_party_type_test(
    test_name: &str,
    fixture_stem: &str,
    ctor_pattern: &str,
    hasher: Option<&str>,
    extern_crates: &[&str],
) {
    run_per_type_test(test_name, fixture_stem, ctor_pattern, hasher, extern_crates);
}

#[test]
fn per_type_bytes_mut_cap() {
    run_third_party_type_test(
        "per_type_bytes_mut_cap",
        "bytes_mut_cap",
        "bytes::BytesMut::with_capacity(0)",
        None,
        &["bytes"],
    );
}

#[test]
fn per_type_indexmap_cap() {
    run_third_party_type_test(
        "per_type_indexmap_cap",
        "indexmap_cap",
        "indexmap::IndexMap::<u32, u32>::with_capacity(0)",
        None,
        &["indexmap"],
    );
}

#[test]
fn per_type_indexset_cap() {
    run_third_party_type_test(
        "per_type_indexset_cap",
        "indexset_cap",
        "indexmap::IndexSet::<u32>::with_capacity(0)",
        None,
        &["indexmap"],
    );
}

#[test]
fn per_type_dashmap_cap() {
    run_third_party_type_test(
        "per_type_dashmap_cap",
        "dashmap_cap",
        "dashmap::DashMap::<u32, u32>::with_capacity(0)",
        None,
        &["dashmap"],
    );
}

#[test]
fn per_type_scc_hashmap_cap() {
    run_third_party_type_test(
        "per_type_scc_hashmap_cap",
        "scc_hashmap_cap",
        "scc::HashMap::<u32, u32>::with_capacity(0)",
        None,
        &["scc"],
    );
}

#[test]
fn per_type_scc_hashset_cap() {
    run_third_party_type_test(
        "per_type_scc_hashset_cap",
        "scc_hashset_cap",
        "scc::HashSet::<u32>::with_capacity(0)",
        None,
        &["scc"],
    );
}

#[test]
fn per_type_scc_treeindex_cap() {
    run_third_party_type_test(
        "per_type_scc_treeindex_cap",
        "scc_treeindex_cap",
        "scc::TreeIndex::<u32, u32>::new()",
        None,
        &["scc"],
    );
}

#[test]
fn per_type_smallvec_cap() {
    run_third_party_type_test(
        "per_type_smallvec_cap",
        "smallvec_cap",
        "smallvec::SmallVec::<[u32; 4]>::with_capacity(0)",
        None,
        &["smallvec"],
    );
}

// ── Hasher-injection tests for third-party hashing types ─────────────────────

#[test]
fn per_type_indexmap_hasher_fx() {
    run_third_party_type_test(
        "per_type_indexmap_hasher_fx",
        "indexmap_hasher",
        "indexmap::IndexMap::<u32, u32>::with_capacity(0)",
        Some("fx"),
        &["indexmap"],
    );
}

#[test]
fn per_type_indexset_hasher_fx() {
    run_third_party_type_test(
        "per_type_indexset_hasher_fx",
        "indexset_hasher",
        "indexmap::IndexSet::<u32>::with_capacity(0)",
        Some("fx"),
        &["indexmap"],
    );
}

#[test]
fn per_type_dashmap_hasher_fx() {
    run_third_party_type_test(
        "per_type_dashmap_hasher_fx",
        "dashmap_hasher",
        "dashmap::DashMap::<u32, u32>::with_capacity(0)",
        Some("fx"),
        &["dashmap"],
    );
}

#[test]
fn per_type_scc_hashmap_hasher_fx() {
    run_third_party_type_test(
        "per_type_scc_hashmap_hasher_fx",
        "scc_hashmap_hasher",
        "scc::HashMap::<u32, u32>::with_capacity(0)",
        Some("fx"),
        &["scc"],
    );
}

#[test]
fn per_type_scc_hashset_hasher_fx() {
    run_third_party_type_test(
        "per_type_scc_hashset_hasher_fx",
        "scc_hashset_hasher",
        "scc::HashSet::<u32>::with_capacity(0)",
        Some("fx"),
        &["scc"],
    );
}

// ── Phase N: type-ascribed hasher multi-span tests ───────────────────────────

/// HashMap with explicit type ascription `HashMap<u32, u32>` (hasher omitted)
/// → Phase N multi-span suggestion: extend ascription + rewrite ctor.
#[test]
fn per_type_hashmap_ascribed_hasher_fx() {
    run_std_type_test(
        "per_type_hashmap_ascribed_hasher_fx",
        "hashmap_ascribed_hasher",
        "HashMap::<u32, u32>::with_capacity(0)",
        Some("fx"),
    );
}

// ── Verification: non-hashing types do NOT get hasher injected ────────────────

/// Verify that Vec with CAPTRACK_PGO_HASHER=fx still gets capacity-ONLY rewrite
/// (the hasher suggestion for Vec would produce invalid code).
///
/// This reuses the `vec_cap.rs` fixture but sets HASHER=fx.  The expected
/// .stderr is the same as the capacity-only output — hasher must NOT appear.
#[test]
fn per_type_vec_with_hasher_env_still_cap_only() {
    run_std_type_test(
        "per_type_vec_with_hasher_env_still_cap_only",
        "vec_cap",
        "Vec::<u32>::with_capacity(0)",
        Some("fx"),
    );
}

/// Same verification for VecDeque.
#[test]
fn per_type_vecdeque_with_hasher_env_still_cap_only() {
    run_std_type_test(
        "per_type_vecdeque_with_hasher_env_still_cap_only",
        "vecdeque_cap",
        "VecDeque::<u32>::with_capacity(0)",
        Some("fx"),
    );
}

/// Same verification for BTreeMap (warning-only, no hasher injection).
#[test]
fn per_type_btreemap_with_hasher_env_still_warn_only() {
    run_std_type_test(
        "per_type_btreemap_with_hasher_env_still_warn_only",
        "btreemap_cap",
        "BTreeMap::<u32, u32>::new()",
        Some("fx"),
    );
}

/// Same verification for BTreeSet (warning-only, no hasher injection).
#[test]
fn per_type_btreeset_with_hasher_env_still_warn_only() {
    run_std_type_test(
        "per_type_btreeset_with_hasher_env_still_warn_only",
        "btreeset_cap",
        "BTreeSet::<u32>::new()",
        Some("fx"),
    );
}
