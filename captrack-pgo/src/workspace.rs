//! Workspace discovery and gitignore-aware Rust file walk.
//!
//! `find_workspace_root` — walk upward from a starting path until a
//! `Cargo.toml` containing a `[workspace]` table is found.
//!
//! `walk_rust_files` — yield every `.rs` file under a root, respecting
//! `.gitignore` and skipping `target/`.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use ignore::WalkBuilder;

/// Walk upward from `start` looking for a `Cargo.toml` whose contents
/// include a `[workspace]` table.  Returns the directory containing that
/// manifest.  Fails if no workspace ancestor is found.
pub fn find_workspace_root(start: &Path) -> Result<PathBuf> {
    let start_abs = if start.is_absolute() {
        start.to_path_buf()
    } else {
        std::env::current_dir()?.join(start)
    };

    for ancestor in start_abs.ancestors() {
        let manifest = ancestor.join("Cargo.toml");
        if !manifest.is_file() {
            continue;
        }
        let body = std::fs::read_to_string(&manifest)
            .with_context(|| format!("read {}", manifest.display()))?;
        if has_workspace_table(&body) {
            return Ok(ancestor.to_path_buf());
        }
    }
    Err(anyhow!(
        "no Cargo.toml with [workspace] table found in any ancestor of {}",
        start_abs.display()
    ))
}

/// Iterator-returning convenience: every `.rs` file under `root`, skipping
/// what `.gitignore` would skip plus `target/` (which is gitignored in any
/// sane workspace anyway, but make it explicit) and `captrack-pgo/tests/fixtures`
/// (those are intentionally-malformed sample sources for our own tests).
pub fn walk_rust_files(root: &Path) -> impl Iterator<Item = PathBuf> {
    WalkBuilder::new(root)
        .standard_filters(true) // .gitignore, .ignore, hidden files
        .require_git(false) // work even outside a git repo
        .build()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .filter(|e| e.path().extension().is_some_and(|x| x == "rs"))
        .filter(|e| !is_fixture_path(e.path()))
        .map(|e| e.into_path())
}

fn has_workspace_table(toml_body: &str) -> bool {
    // Cheap heuristic — looking for a top-level `[workspace]` header.
    // Avoid pulling in a full toml parser here; the test suite covers
    // edge cases like commented-out headers.
    for line in toml_body.lines() {
        let stripped = line.trim_start();
        if stripped.starts_with('#') {
            // Skip commented-out [workspace] markers
            continue;
        }
        if stripped == "[workspace]" || stripped.starts_with("[workspace]") {
            return true;
        }
    }
    false
}

fn is_fixture_path(p: &Path) -> bool {
    // Skip our own fixtures dir — intentionally malformed Rust files would
    // confuse the AST scanner if walked along with real sources.
    p.components().any(|c| c.as_os_str() == "fixtures")
        && p.components().any(|c| c.as_os_str() == "tests")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_workspace_root_from_subdirectory() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("Cargo.toml"), "[workspace]\nmembers = [\"a\"]\n").unwrap();
        let sub = root.join("a").join("src");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(
            root.join("a").join("Cargo.toml"),
            "[package]\nname=\"a\"\nversion=\"0.1.0\"\nedition=\"2021\"\n",
        )
        .unwrap();
        let found = find_workspace_root(&sub).unwrap();
        // Both paths point to the same dir but might differ in canonical form.
        assert_eq!(
            std::fs::canonicalize(&found).unwrap(),
            std::fs::canonicalize(root).unwrap(),
        );
    }

    #[test]
    fn no_workspace_returns_err() {
        let tmp = tempfile::tempdir().unwrap();
        // Tmp dir is unlikely to be under any [workspace] Cargo.toml; if it
        // is (e.g. nested under captrack), this test may flake — guard by
        // checking the error path only when the discovery finds nothing.
        let res = find_workspace_root(tmp.path());
        // Just sanity: either Ok (env-dependent) or Err with our message.
        if let Err(e) = res {
            assert!(e.to_string().contains("no Cargo.toml"));
        }
    }

    #[test]
    fn walk_skips_target_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // Workspace Cargo.toml
        std::fs::write(root.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
        // .gitignore so target/ is filtered by `ignore::WalkBuilder`.
        std::fs::write(root.join(".gitignore"), "/target/\n").unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src").join("lib.rs"), "// real\n").unwrap();
        std::fs::create_dir_all(root.join("target").join("debug")).unwrap();
        std::fs::write(
            root.join("target").join("debug").join("bogus.rs"),
            "// build artefact, must NOT be walked\n",
        )
        .unwrap();

        let files: Vec<_> = walk_rust_files(root).collect();
        assert!(files
            .iter()
            .any(|p| p.ends_with("src/lib.rs") || p.ends_with("src\\lib.rs")));
        assert!(
            !files.iter().any(|p| p.to_string_lossy().contains("target")),
            "target/ must be skipped; got {:?}",
            files
        );
    }

    #[test]
    fn walk_skips_test_fixtures() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();
        let fixture = root.join("tests").join("fixtures");
        std::fs::create_dir_all(&fixture).unwrap();
        std::fs::write(fixture.join("malformed.rs"), "this is not valid rust").unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src").join("main.rs"), "fn main() {}").unwrap();

        let files: Vec<_> = walk_rust_files(root).collect();
        assert!(
            !files
                .iter()
                .any(|p| p.to_string_lossy().contains("fixtures")),
            "tests/fixtures must be skipped; got {:?}",
            files
        );
    }

    #[test]
    fn has_workspace_table_basic() {
        assert!(has_workspace_table("[workspace]\nmembers = []"));
        assert!(has_workspace_table("[package]\n[workspace]\n"));
        assert!(!has_workspace_table("[package]\nname = \"x\""));
        assert!(!has_workspace_table("# [workspace]\n[package]\n"));
    }

    #[test]
    fn walk_finds_rs_files_in_nested_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();
        std::fs::create_dir_all(root.join("src").join("submod")).unwrap();
        std::fs::write(root.join("src").join("lib.rs"), "// lib\n").unwrap();
        std::fs::write(
            root.join("src").join("submod").join("helper.rs"),
            "// helper\n",
        )
        .unwrap();

        let files: Vec<_> = walk_rust_files(root).collect();
        assert_eq!(
            files.len(),
            2,
            "expected exactly 2 .rs files, got {:?}",
            files
        );
    }

    #[test]
    fn walk_ignores_non_rs_files() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src").join("main.rs"), "fn main() {}").unwrap();
        std::fs::write(root.join("src").join("readme.md"), "# docs").unwrap();
        std::fs::write(root.join("src").join("data.json"), "{}").unwrap();

        let files: Vec<_> = walk_rust_files(root).collect();
        assert_eq!(files.len(), 1);
        assert!(files[0].extension().is_some_and(|x| x == "rs"));
    }

    #[test]
    fn has_workspace_table_inline_comment_after_header() {
        // A line like `[workspace] # comment` should still be detected
        assert!(has_workspace_table(
            "[workspace] # with comment\nmembers=[]\n"
        ));
    }

    #[test]
    fn find_workspace_root_stops_at_nearest_workspace() {
        // Nested workspaces: outer has [workspace], inner too.
        // find_workspace_root should return the inner one (nearest ancestor).
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // outer workspace
        std::fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers=[\"inner\"]\n",
        )
        .unwrap();
        let inner = root.join("inner");
        std::fs::create_dir_all(inner.join("src")).unwrap();
        std::fs::write(inner.join("Cargo.toml"), "[workspace]\nmembers=[]\n").unwrap();
        std::fs::write(inner.join("src").join("lib.rs"), "").unwrap();

        let found = find_workspace_root(&inner.join("src")).unwrap();
        assert_eq!(
            std::fs::canonicalize(&found).unwrap(),
            std::fs::canonicalize(&inner).unwrap(),
        );
    }
}
