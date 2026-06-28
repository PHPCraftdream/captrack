use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Mutex to serialize UI tests that mutate environment variables.
///
/// `dylint_testing::ui_test` internally takes its own mutex around the
/// `compiletest` call, but our `set_var` calls happen *before* that lock.
/// We use this outer mutex to ensure that env-var mutation and the subsequent
/// `ui_test` call are atomic from the perspective of the other test thread.
static ENV_MUTEX: Mutex<()> = Mutex::new(());

/// Generate a combined profile JSON that covers both `detect.rs` and
/// `suggest.rs` fixture files.  Each fixture's entries are generated
/// independently and then concatenated into a single slice before
/// serialisation, so that `dylint_testing::ui_test` can run both files under
/// the same `CAPTRACK_PGO_PROFILE` env var.
fn make_profile_json(detect_rs: &Path, suggest_rs: &Path) -> String {
    let detect_src = std::fs::read_to_string(detect_rs).expect("detect.rs must be readable");
    let suggest_src = std::fs::read_to_string(suggest_rs).expect("suggest.rs must be readable");

    let mut sites = find_detect_ctors(&detect_src, detect_rs);
    sites.extend(find_suggest_ctors(&suggest_src, suggest_rs));

    serde_json::to_string_pretty(&sites).expect("serialise profile")
}

/// Description of one profile entry — mirrors `model::SiteStats` but is a
/// plain struct so we don't need to import the lint crate types here.
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

/// Scan `detect.rs` for the Vec constructors that the test profile should
/// match, using the same pattern-matching logic as M2.
///
/// We include `Vec::new()` and `Vec::with_capacity(` (excluding comment-only
/// lines).
///
/// Profile values: peak=1024, p50=64, p95=512, count=100 — as in M2.
fn find_detect_ctors(source: &str, file: &Path) -> Vec<SiteStats> {
    let targets: &[&str] = &["Vec::new()", "Vec::with_capacity("];

    let mut results = Vec::new();

    for (line_idx, line_str) in source.lines().enumerate() {
        let line_no = (line_idx + 1) as u32;
        let trimmed = line_str.trim_start();
        if trimmed.starts_with("//") {
            continue;
        }
        for &pattern in targets {
            if let Some(byte_offset) = line_str.find(pattern) {
                let col = byte_offset as u32 + 1;
                results.push(SiteStats {
                    key: SiteKey {
                        file: file.to_path_buf(),
                        line: line_no,
                        col,
                    },
                    unit: Unit::Elements,
                    peak: 1024,
                    p50: 64,
                    p95: 512,
                    count: 100,
                });
            }
        }
    }

    results
}

/// Scan `suggest.rs` for the constructor sites that should have profile
/// entries.
///
/// Rules:
/// - Lines containing `// no-match` are excluded (intentionally unmatched).
/// - Lines that are pure comments (`//` after trimming) are excluded.
/// - The remaining lines are scanned for the constructor patterns below.
///
/// Profile values are chosen so that `propose_cap` returns `Decision::Patch`
/// for Vec/HashMap sites and the resulting N values match `suggest.fixed`.
///
/// | Site                              | peak | p95 | count | N (next_pow2(p95)) |
/// |-----------------------------------|------|-----|-------|--------------------|
/// | `Vec::new()`                      |  100 |  60 |    50 | 64                 |
/// | `Vec::with_capacity(4)`           |  100 |  80 |    50 | 128                |
/// | `HashMap::with_capacity_and_hasher(4, …)` | 100 | 30 | 50 | 32          |
/// | `BTreeMap::new()`                 |  100 |  80 |    50 | —  (warn only)     |
fn find_suggest_ctors(source: &str, file: &Path) -> Vec<SiteStats> {
    // Define the patterns to look for and their associated profile values.
    // Each tuple: (pattern_to_search, peak, p50, p95, count).
    let targets: &[(&str, usize, usize, usize, u64)] = &[
        ("Vec::new()", 100, 60, 60, 50),
        ("Vec::with_capacity(4)", 100, 80, 80, 50),
        ("HashMap::with_capacity_and_hasher(4,", 100, 30, 30, 50),
        ("BTreeMap::new()", 100, 80, 80, 50),
    ];

    let mut results = Vec::new();

    for (line_idx, line_str) in source.lines().enumerate() {
        let line_no = (line_idx + 1) as u32;
        let trimmed = line_str.trim_start();

        // Skip pure comment lines.
        if trimmed.starts_with("//") {
            continue;
        }
        // Skip intentionally-unmatched lines.
        if line_str.contains("// no-match") {
            continue;
        }

        for &(pattern, peak, p50, p95, count) in targets {
            if let Some(byte_offset) = line_str.find(pattern) {
                let col = byte_offset as u32 + 1;
                results.push(SiteStats {
                    key: SiteKey {
                        file: file.to_path_buf(),
                        line: line_no,
                        col,
                    },
                    unit: Unit::Elements,
                    peak,
                    p50,
                    p95,
                    count,
                });
                // One match per line is enough.
                break;
            }
        }
    }

    results
}

#[test]
fn ui() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let ui_dir = manifest_dir.join("ui");
    let detect_rs = ui_dir.join("detect.rs");
    let suggest_rs = ui_dir.join("suggest.rs");

    // Generate the combined profile JSON with the machine's absolute paths.
    let profile_json = make_profile_json(&detect_rs, &suggest_rs);

    // Write it to tests/fixtures/profile.json (created fresh on every run).
    let fixtures_dir = manifest_dir.join("tests").join("fixtures");
    std::fs::create_dir_all(&fixtures_dir).expect("create fixtures dir");
    let profile_path = fixtures_dir.join("profile.json");
    std::fs::write(&profile_path, &profile_json).expect("write profile.json");

    // Take the env mutex to prevent the `instrument` test from clobbering env
    // vars while we are setting up and running this test.
    let _guard = ENV_MUTEX.lock().unwrap();

    // Set the env var so the child compiler process (spawned by compiletest)
    // inherits it.
    //
    // Safety: we hold ENV_MUTEX; only one of {ui, instrument} mutates env
    // vars at a time.  The child process is spawned inside `ui_test` while
    // we still hold the lock.
    unsafe {
        std::env::remove_var("CAPTRACK_PGO_INSTRUMENT");
        std::env::remove_var("CAPTRACK_PGO_HASHER");
        std::env::set_var("CAPTRACK_PGO_PROFILE", &profile_path);
    }

    dylint_testing::ui_test(env!("CARGO_PKG_NAME"), &ui_dir);
}

/// Scan `suggest_hasher.rs` for the constructor sites that should have profile
/// entries (M9 hasher-injection test).
///
/// Profile values trigger `Decision::Patch` for all matched sites:
///
/// | Site                              | peak | p95 | count | N (next_pow2(p95)) |
/// |-----------------------------------|------|-----|-------|--------------------|
/// | `HashMap::<u8, u8>::new()`        |  100 |  60 |    50 | 64                 |
/// | `HashMap::with_capacity(4)`       |  100 |  80 |    50 | 128                |
/// | `HashSet::<u8>::new()`            |  100 |  60 |    50 | 64                 |
/// | `Vec::<u8>::new()`                |  100 |  60 |    50 | 64                 |
fn find_suggest_hasher_ctors(source: &str, file: &Path) -> Vec<SiteStats> {
    let targets: &[(&str, usize, usize, usize, u64)] = &[
        ("HashMap::<u8, u8>::new()", 100, 60, 60, 50),
        ("HashMap::with_capacity(4)", 100, 80, 80, 50),
        ("HashSet::<u8>::new()", 100, 60, 60, 50),
        ("Vec::<u8>::new()", 100, 60, 60, 50),
    ];

    let mut results = Vec::new();

    for (line_idx, line_str) in source.lines().enumerate() {
        let line_no = (line_idx + 1) as u32;
        let trimmed = line_str.trim_start();

        // Skip pure comment lines.
        if trimmed.starts_with("//") {
            continue;
        }
        // Skip intentionally-unmatched lines.
        if line_str.contains("// no-match") {
            continue;
        }

        for &(pattern, peak, p50, p95, count) in targets {
            if let Some(byte_offset) = line_str.find(pattern) {
                let col = byte_offset as u32 + 1;
                results.push(SiteStats {
                    key: SiteKey {
                        file: file.to_path_buf(),
                        line: line_no,
                        col,
                    },
                    unit: Unit::Elements,
                    peak,
                    p50,
                    p95,
                    count,
                });
                // One match per line is enough.
                break;
            }
        }
    }

    results
}

#[test]
fn suggest_hasher() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let ui_hasher_dir = manifest_dir.join("ui_hasher");
    let suggest_hasher_rs = ui_hasher_dir.join("suggest_hasher.rs");

    let source =
        std::fs::read_to_string(&suggest_hasher_rs).expect("suggest_hasher.rs must be readable");

    // Build a profile JSON covering only suggest_hasher.rs sites.
    let sites = find_suggest_hasher_ctors(&source, &suggest_hasher_rs);
    let profile_json = serde_json::to_string_pretty(&sites).expect("serialise hasher profile");

    let fixtures_dir = manifest_dir.join("tests").join("fixtures");
    std::fs::create_dir_all(&fixtures_dir).expect("create fixtures dir");
    let profile_path = fixtures_dir.join("profile_hasher.json");
    std::fs::write(&profile_path, &profile_json).expect("write profile_hasher.json");

    // Take the env mutex to prevent other tests from clobbering env vars.
    let _guard = ENV_MUTEX.lock().unwrap();

    unsafe {
        std::env::remove_var("CAPTRACK_PGO_INSTRUMENT");
        std::env::set_var("CAPTRACK_PGO_PROFILE", &profile_path);
        std::env::set_var("CAPTRACK_PGO_HASHER", "fx");
    }

    dylint_testing::ui_test(env!("CARGO_PKG_NAME"), &ui_hasher_dir);
}

#[test]
fn instrument() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let ui_instrument_dir = manifest_dir.join("ui_instrument");

    // Take the env mutex to prevent the `ui` test from clobbering env vars
    // while we are setting up and running this test.
    let _guard = ENV_MUTEX.lock().unwrap();

    // Clear the profile env var (the capacity lint must be a no-op during
    // the instrument pass) and activate the instrument lint.
    //
    // Safety: we hold ENV_MUTEX; only one of {ui, instrument} mutates env
    // vars at a time.
    unsafe {
        std::env::remove_var("CAPTRACK_PGO_PROFILE");
        std::env::remove_var("CAPTRACK_PGO_HASHER");
        std::env::set_var("CAPTRACK_PGO_INSTRUMENT", "1");
    }

    dylint_testing::ui_test(env!("CARGO_PKG_NAME"), &ui_instrument_dir);
}
