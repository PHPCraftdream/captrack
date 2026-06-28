use std::path::{Path, PathBuf};

/// Generate a minimal profile JSON with two Vec call-sites from `detect.rs`.
///
/// The `file` field must exactly match what the compiler reports for the span
/// — i.e., the absolute path of `detect.rs` as passed to rustc by compiletest.
///
/// Line/col offsets are computed by scanning the detect.rs source file to find
/// the exact byte positions of `Vec::new()` and `Vec::with_capacity`, so the
/// profile remains correct even if the fixture file is reformatted.
fn make_profile_json(detect_rs: &Path) -> String {
    let source = std::fs::read_to_string(detect_rs).expect("detect.rs must be readable");

    let sites = find_vec_ctors(&source, detect_rs);
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

/// Scan `source` for constructor patterns that the test profile should match
/// and return `SiteStats` entries.
///
/// We look for the first occurrence of `Vec::new()` (the one on the "matched"
/// line 13) and `Vec::with_capacity` (line 31), extracting their 1-based line
/// and column numbers.
///
/// Column arithmetic: `CharPos` from `lookup_char_pos` is 0-based;
/// `column!()` is 1-based.  The lint adds 1 (`col.0 as u32 + 1`), so we do
/// the same here: scan to find byte offset of the pattern start, then + 1.
fn find_vec_ctors(source: &str, file: &Path) -> Vec<SiteStats> {
    // Build a list of (line_no, col_1based, pattern) for each Vec constructor
    // we want to include in the test profile.  We include only the "matched"
    // occurrences; the other constructors intentionally have no profile entry.
    let targets: &[&str] = &["Vec::new()", "Vec::with_capacity("];

    let mut results = Vec::new();

    for (line_idx, line_str) in source.lines().enumerate() {
        let line_no = (line_idx + 1) as u32; // 1-based
        for &pattern in targets {
            // Only match the FIRST occurrence per line (there's at most one).
            // Also skip lines that are pure comments.
            let trimmed = line_str.trim_start();
            if trimmed.starts_with("//") {
                continue;
            }
            if let Some(byte_offset) = line_str.find(pattern) {
                // byte_offset is 0-based byte index within the line.
                // The lint adds 1 to match `column!()` (1-based), so we do too.
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
                // Don't break — in theory there could be multiple occurrences,
                // but in our fixture each matched line has exactly one.
            }
        }
    }

    results
}

#[test]
fn ui() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let detect_rs = manifest_dir.join("ui").join("detect.rs");

    // Generate the profile JSON with the machine's absolute path to detect.rs.
    let profile_json = make_profile_json(&detect_rs);

    // Write it to tests/fixtures/profile.json (created fresh on every run).
    let fixtures_dir = manifest_dir.join("tests").join("fixtures");
    std::fs::create_dir_all(&fixtures_dir).expect("create fixtures dir");
    let profile_path = fixtures_dir.join("profile.json");
    std::fs::write(&profile_path, &profile_json).expect("write profile.json");

    // Set the env var so the child compiler process (spawned by compiletest)
    // inherits it.  The OnceLock in the plugin loader ensures it is read once
    // per compiler invocation (each compiletest run is a fresh process).
    //
    // Safety: tests run single-threaded here; the env var is set before
    // compiletest spawns any child processes.
    unsafe {
        std::env::set_var("CAPTRACK_PGO_PROFILE", &profile_path);
    }

    dylint_testing::ui_test(env!("CARGO_PKG_NAME"), &manifest_dir.join("ui"));
}
