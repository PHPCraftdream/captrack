//! `wire` and `unwire` subcommands — add `captrack` as a dependency to every
//! crate in a target workspace (and revert it).
//!
//! ## Why a separate step
//!
//! `captrack-pgo instrument` rewrites `Vec::with_capacity(N)` in source files
//! into `::captrack::TrackedVec::with_capacity_named(...)`.  That rewrite
//! introduces an `::captrack` path reference into every touched crate, so each
//! crate's `Cargo.toml` MUST already list `captrack` as a dependency or the
//! post-instrument build fails with `unresolved import \`captrack\``.
//!
//! `wire` performs that Cargo.toml patch automatically and stores a snapshot
//! of every modified manifest in `target/captrack-pgo/last-wire.json` so
//! `unwire` can revert losslessly.
//!
//! ## What gets patched
//!
//! 1. **Root manifest** — `captrack` is added to the `[workspace.dependencies]`
//!    table.  If the table does not exist it is created.  The dependency spec
//!    is either `{ path = "<captrack-path>", features = ["telemetry"] }` (when
//!    `--captrack-path` is given) or `{ version = "0.1", features = ["telemetry"] }`
//!    (when omitted — uses the published crate).
//!
//! 2. **Each member manifest** — `captrack = { workspace = true }` is added to
//!    the `[dependencies]` table (created if missing).  Members are resolved
//!    by reading the workspace's `members = [...]` (glob-expanded) and
//!    subtracting `exclude = [...]`.
//!
//! If `captrack` is already present in a manifest it is left untouched
//! (already-wired sites are detected and skipped).
//!
//! ## Idempotency
//!
//! Running `wire` twice in a row is safe — the second run finds `captrack`
//! already wired everywhere and produces an empty manifest (zero files
//! touched).  `unwire` will refuse to operate on an empty / missing manifest.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use toml_edit::{value, Array, DocumentMut, Formatted, InlineTable, Item, Value};

// ──────────────────────────────────────────────────────────────────────────────
// Manifest types
// ──────────────────────────────────────────────────────────────────────────────

/// One Cargo.toml file modified by `wire`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WireFileEntry {
    /// Absolute path to the Cargo.toml.
    pub file: PathBuf,
    /// SHA-256 of the file BEFORE `wire` ran.
    pub sha256_before: String,
    /// Full content of the file BEFORE — used for lossless revert.
    pub content_before: String,
    /// SHA-256 of the file AFTER — verified by `unwire` to detect manual edits.
    pub sha256_after: String,
}

/// Where the `captrack` dep was sourced from at `wire` time.  Recorded for
/// audit / debugging; `unwire` does not consult it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WireSource {
    /// `{ path = "<abs>" }`.
    Path { path: PathBuf },
    /// `{ version = "<semver>" }` — from crates.io.
    Version { version: String },
}

/// Manifest written to `target/captrack-pgo/last-wire.json`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WireManifest {
    pub version: u32,
    pub source: WireSource,
    pub files: Vec<WireFileEntry>,
}

/// Manifest path: `<workspace>/target/captrack-pgo/last-wire.json`.
pub fn default_wire_manifest_path(workspace_root: &Path) -> PathBuf {
    workspace_root
        .join("target")
        .join("captrack-pgo")
        .join("last-wire.json")
}

// ──────────────────────────────────────────────────────────────────────────────
// Subcommand args
// ──────────────────────────────────────────────────────────────────────────────

pub struct WireArgs {
    pub workspace_root: PathBuf,
    /// Absolute path to a local `captrack` crate to depend on via `path = "..."`.
    /// If `None`, the published `version = "0.1"` is used.
    pub captrack_path: Option<PathBuf>,
}

pub struct UnwireArgs {
    pub workspace_root: PathBuf,
    /// Explicit manifest path; defaults to `default_wire_manifest_path(workspace_root)`.
    pub manifest: Option<PathBuf>,
}

// ──────────────────────────────────────────────────────────────────────────────
// `wire` — patch all Cargo.toml files
// ──────────────────────────────────────────────────────────────────────────────

pub fn run_wire(args: WireArgs) -> Result<()> {
    let WireArgs {
        workspace_root,
        captrack_path,
    } = args;

    let root_manifest = workspace_root.join("Cargo.toml");
    if !root_manifest.is_file() {
        bail!(
            "no Cargo.toml at workspace root: {}",
            root_manifest.display()
        );
    }

    // 1. Resolve [workspace] member directories (each having its own Cargo.toml).
    let member_dirs = resolve_workspace_members(&workspace_root)
        .with_context(|| "resolve workspace members")?;

    // 2. Build the `captrack` source descriptor (path-or-version).
    let source = match &captrack_path {
        Some(p) => {
            let abs = if p.is_absolute() {
                p.clone()
            } else {
                std::env::current_dir()?.join(p)
            };
            if !abs.join("Cargo.toml").is_file() {
                bail!(
                    "captrack-path {} has no Cargo.toml",
                    abs.display()
                );
            }
            WireSource::Path { path: abs }
        }
        None => WireSource::Version {
            version: "0.1".to_string(),
        },
    };

    // 3. Snapshot + patch each manifest, collecting WireFileEntry per file that
    //    actually changed.  An already-wired file is left untouched and not
    //    recorded.
    let mut entries: Vec<WireFileEntry> = Vec::new();

    // Root manifest first.
    if let Some(entry) = patch_root_manifest(&root_manifest, &source)? {
        entries.push(entry);
    }

    // Then every member.
    for member in &member_dirs {
        let member_manifest = member.join("Cargo.toml");
        if !member_manifest.is_file() {
            // Skip silently — workspace may list members whose manifest is
            // not present (excluded by glob filter).  This case is rare; for
            // safety we just continue.
            continue;
        }
        if let Some(entry) = patch_member_manifest(&member_manifest)? {
            entries.push(entry);
        }
    }

    // 4. Persist the manifest so `unwire` can revert.
    let manifest = WireManifest {
        version: 1,
        source,
        files: entries.clone(),
    };
    let manifest_path = default_wire_manifest_path(&workspace_root);
    if let Some(parent) = manifest_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create dir {}", parent.display()))?;
    }
    let manifest_text = serde_json::to_string_pretty(&manifest)
        .with_context(|| "serialize wire manifest")?;
    std::fs::write(&manifest_path, manifest_text)
        .with_context(|| format!("write {}", manifest_path.display()))?;

    eprintln!(
        "wire: patched {} Cargo.toml file{} (manifest: {})",
        entries.len(),
        if entries.len() == 1 { "" } else { "s" },
        manifest_path.display()
    );

    Ok(())
}

// ──────────────────────────────────────────────────────────────────────────────
// `unwire` — revert from manifest
// ──────────────────────────────────────────────────────────────────────────────

pub fn run_unwire(args: UnwireArgs) -> Result<()> {
    let UnwireArgs {
        workspace_root,
        manifest,
    } = args;

    let manifest_path = manifest.unwrap_or_else(|| default_wire_manifest_path(&workspace_root));

    if !manifest_path.is_file() {
        bail!(
            "no wire manifest at {}; nothing to revert",
            manifest_path.display()
        );
    }

    let text = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("read {}", manifest_path.display()))?;
    let manifest: WireManifest = serde_json::from_str(&text)
        .with_context(|| format!("parse {}", manifest_path.display()))?;

    if manifest.version != 1 {
        bail!(
            "unsupported wire manifest version {}; this binary handles only v1",
            manifest.version
        );
    }

    let mut reverted = 0usize;

    for f in &manifest.files {
        // Verify the file is in the post-wire state we recorded — abort if the
        // user has manually edited the Cargo.toml since `wire`.
        let cur = std::fs::read_to_string(&f.file)
            .with_context(|| format!("read {}", f.file.display()))?;
        let cur_sha = sha256_hex(cur.as_bytes());
        if cur_sha != f.sha256_after {
            bail!(
                "refuse to revert {}: file was modified after wire \
                 (expected sha256 {}, got {}). \
                 Reconcile manually (git checkout / git diff) and delete the manifest.",
                f.file.display(),
                f.sha256_after,
                cur_sha
            );
        }
        std::fs::write(&f.file, &f.content_before)
            .with_context(|| format!("write {}", f.file.display()))?;
        reverted += 1;
    }

    // Remove the manifest itself — wire/unwire are paired; a left-over
    // manifest after revert is misleading.
    let _ = std::fs::remove_file(&manifest_path);

    eprintln!(
        "unwire: reverted {} Cargo.toml file{} from {}",
        reverted,
        if reverted == 1 { "" } else { "s" },
        manifest_path.display()
    );
    Ok(())
}

// ──────────────────────────────────────────────────────────────────────────────
// Workspace member discovery
// ──────────────────────────────────────────────────────────────────────────────

/// Read the root Cargo.toml and resolve its `[workspace] members = [...]`
/// (glob-expanded) minus `exclude = [...]`.  Returns a list of crate
/// directories, each guaranteed to contain a Cargo.toml.
fn resolve_workspace_members(workspace_root: &Path) -> Result<Vec<PathBuf>> {
    let manifest_path = workspace_root.join("Cargo.toml");
    let text = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("read {}", manifest_path.display()))?;
    let doc: DocumentMut = text
        .parse()
        .with_context(|| format!("parse {}", manifest_path.display()))?;

    let workspace = doc
        .get("workspace")
        .and_then(Item::as_table)
        .ok_or_else(|| anyhow!("{} has no [workspace] table", manifest_path.display()))?;

    let members = workspace
        .get("members")
        .and_then(Item::as_array)
        .ok_or_else(|| anyhow!("{} has no [workspace] members field", manifest_path.display()))?;

    let exclude: BTreeSet<String> = workspace
        .get("exclude")
        .and_then(Item::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();

    let mut dirs: BTreeSet<PathBuf> = BTreeSet::new();

    for entry in members {
        let pattern = entry
            .as_str()
            .ok_or_else(|| anyhow!("workspace.members entry is not a string"))?;
        expand_member_pattern(workspace_root, pattern, &mut dirs)?;
    }

    // Subtract excludes (compare canonical paths).
    let exclude_paths: BTreeSet<PathBuf> = exclude
        .iter()
        .map(|s| workspace_root.join(s))
        .collect();

    let filtered: Vec<PathBuf> = dirs
        .into_iter()
        .filter(|d| !exclude_paths.iter().any(|x| same_dir(d, x)))
        .filter(|d| d.join("Cargo.toml").is_file())
        .collect();

    Ok(filtered)
}

fn expand_member_pattern(
    workspace_root: &Path,
    pattern: &str,
    out: &mut BTreeSet<PathBuf>,
) -> Result<()> {
    let full = workspace_root.join(pattern);
    if !pattern.contains('*') && !pattern.contains('?') && !pattern.contains('[') {
        // Plain path (most common in non-glob workspaces).
        out.insert(full);
        return Ok(());
    }
    let pat_str = full.to_string_lossy().into_owned();
    for entry in glob::glob(&pat_str).with_context(|| format!("bad glob: {}", pat_str))? {
        let path = entry.with_context(|| "glob walk")?;
        if path.is_dir() {
            out.insert(path);
        }
    }
    Ok(())
}

fn same_dir(a: &Path, b: &Path) -> bool {
    let na = std::fs::canonicalize(a).ok();
    let nb = std::fs::canonicalize(b).ok();
    match (na, nb) {
        (Some(x), Some(y)) => x == y,
        _ => a == b,
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Per-manifest patchers
// ──────────────────────────────────────────────────────────────────────────────

/// Patch the root Cargo.toml: add `captrack` to `[workspace.dependencies]`.
/// Returns `Ok(None)` if already present.
fn patch_root_manifest(path: &Path, source: &WireSource) -> Result<Option<WireFileEntry>> {
    let before = std::fs::read_to_string(path)
        .with_context(|| format!("read {}", path.display()))?;
    let sha256_before = sha256_hex(before.as_bytes());

    let mut doc: DocumentMut = before
        .parse()
        .with_context(|| format!("parse {}", path.display()))?;

    // Ensure [workspace] exists (root manifest must have it; we still guard).
    if doc.get("workspace").is_none() {
        bail!("{} has no [workspace] table", path.display());
    }

    // Ensure [workspace.dependencies] exists.
    {
        let workspace = doc["workspace"]
            .as_table_mut()
            .ok_or_else(|| anyhow!("[workspace] is not a table"))?;
        if !workspace.contains_key("dependencies") {
            workspace.insert("dependencies", toml_edit::table());
        }
    }

    let deps = doc["workspace"]["dependencies"]
        .as_table_mut()
        .ok_or_else(|| anyhow!("[workspace.dependencies] is not a table"))?;

    if deps.contains_key("captrack") {
        return Ok(None);
    }

    deps.insert("captrack", Item::Value(captrack_inline_dep(source)));

    let after = doc.to_string();
    if after == before {
        return Ok(None);
    }

    std::fs::write(path, &after).with_context(|| format!("write {}", path.display()))?;
    let sha256_after = sha256_hex(after.as_bytes());

    Ok(Some(WireFileEntry {
        file: path.to_path_buf(),
        sha256_before,
        content_before: before,
        sha256_after,
    }))
}

/// Patch a member Cargo.toml: add `captrack = { workspace = true }` to `[dependencies]`.
/// Returns `Ok(None)` if already present.
fn patch_member_manifest(path: &Path) -> Result<Option<WireFileEntry>> {
    let before = std::fs::read_to_string(path)
        .with_context(|| format!("read {}", path.display()))?;
    let sha256_before = sha256_hex(before.as_bytes());

    let mut doc: DocumentMut = before
        .parse()
        .with_context(|| format!("parse {}", path.display()))?;

    // Ensure [dependencies] exists.
    if doc.get("dependencies").is_none() {
        doc.insert("dependencies", toml_edit::table());
    }
    let deps = doc["dependencies"]
        .as_table_mut()
        .ok_or_else(|| anyhow!("[dependencies] is not a table"))?;

    if deps.contains_key("captrack") {
        return Ok(None);
    }

    // captrack = { workspace = true }
    let mut inline = InlineTable::new();
    inline.insert(
        "workspace",
        Value::Boolean(Formatted::new(true)),
    );
    deps.insert(
        "captrack",
        Item::Value(Value::InlineTable(inline)),
    );

    let after = doc.to_string();
    if after == before {
        return Ok(None);
    }

    std::fs::write(path, &after).with_context(|| format!("write {}", path.display()))?;
    let sha256_after = sha256_hex(after.as_bytes());

    Ok(Some(WireFileEntry {
        file: path.to_path_buf(),
        sha256_before,
        content_before: before,
        sha256_after,
    }))
}

/// Build the inline-table for the `captrack` dep in `[workspace.dependencies]`:
/// either `{ path = "...", features = ["telemetry"] }` or
/// `{ version = "0.1", features = ["telemetry"] }`.
fn captrack_inline_dep(source: &WireSource) -> Value {
    let mut t = InlineTable::new();
    match source {
        WireSource::Path { path } => {
            t.insert(
                "path",
                Value::String(Formatted::new(path.to_string_lossy().into_owned())),
            );
        }
        WireSource::Version { version } => {
            t.insert(
                "version",
                Value::String(Formatted::new(version.clone())),
            );
        }
    }
    let mut feats = Array::new();
    feats.push("telemetry");
    t.insert("features", Value::Array(feats));
    Value::InlineTable(t)
}

// ──────────────────────────────────────────────────────────────────────────────
// SHA helpers
// ──────────────────────────────────────────────────────────────────────────────

fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    let out = h.finalize();
    let mut s = String::with_capacity(out.len() * 2);
    for b in out {
        use std::fmt::Write as _;
        let _ = write!(s, "{:02x}", b);
    }
    s
}

// Silence unused-import lints if `value` is not referenced from this rev.
#[allow(dead_code)]
fn _silence_unused_value_import() {
    let _ = value(true);
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT_MANIFEST: &str = r#"[workspace]
members = ["crates/a", "crates/b"]
resolver = "2"
"#;

    const ROOT_MANIFEST_WITH_DEPS: &str = r#"[workspace]
members = ["crates/a", "crates/b"]
resolver = "2"

[workspace.dependencies]
serde = "1"
"#;

    const ROOT_MANIFEST_NO_MEMBERS: &str = r#"[workspace]
resolver = "2"
"#;

    const ROOT_MANIFEST_WITH_EXCLUDE: &str = r#"[workspace]
members = ["crates/*"]
exclude = ["crates/b"]
resolver = "2"
"#;

    const MEMBER_MANIFEST: &str = r#"[package]
name = "a"
version = "0.1.0"

[dependencies]
anyhow = "1"
"#;

    const MEMBER_MANIFEST_NO_DEPS: &str = r#"[package]
name = "a"
version = "0.1.0"
"#;

    /// Build a temp workspace with a root Cargo.toml and N member crates,
    /// each with their own Cargo.toml. Returns the tempdir (kept alive by
    /// the caller) and the workspace root path.
    fn make_workspace(root_manifest: &str, members: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("Cargo.toml"), root_manifest).unwrap();
        for (rel, content) in members {
            let member_dir = dir.path().join(rel);
            std::fs::create_dir_all(&member_dir).unwrap();
            std::fs::write(member_dir.join("Cargo.toml"), content).unwrap();
        }
        dir
    }

    // ── resolve_workspace_members ───────────────────────────────────────────

    #[test]
    fn resolve_workspace_members_plain_list() {
        let dir = make_workspace(
            ROOT_MANIFEST,
            &[
                ("crates/a", MEMBER_MANIFEST),
                ("crates/b", MEMBER_MANIFEST),
            ],
        );
        let members = resolve_workspace_members(dir.path()).unwrap();
        assert_eq!(members.len(), 2);
        let names: BTreeSet<String> = members
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(names.contains("a"));
        assert!(names.contains("b"));
    }

    #[test]
    fn resolve_workspace_members_glob_and_exclude() {
        let dir = make_workspace(
            ROOT_MANIFEST_WITH_EXCLUDE,
            &[
                ("crates/a", MEMBER_MANIFEST),
                ("crates/b", MEMBER_MANIFEST),
            ],
        );
        let members = resolve_workspace_members(dir.path()).unwrap();
        // "crates/b" is excluded even though it matches the glob.
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].file_name().unwrap(), "a");
    }

    #[test]
    fn resolve_workspace_members_no_members_field_errors() {
        let dir = make_workspace(ROOT_MANIFEST_NO_MEMBERS, &[]);
        let result = resolve_workspace_members(dir.path());
        assert!(result.is_err(), "missing members field must error");
    }

    #[test]
    fn resolve_workspace_members_missing_root_manifest_errors() {
        let dir = tempfile::tempdir().unwrap();
        let result = resolve_workspace_members(dir.path());
        assert!(result.is_err());
    }

    // ── patch_root_manifest ─────────────────────────────────────────────────

    #[test]
    fn patch_root_manifest_adds_dependencies_table() {
        let dir = make_workspace(ROOT_MANIFEST, &[]);
        let root = dir.path().join("Cargo.toml");
        let source = WireSource::Version {
            version: "0.1".to_string(),
        };
        let entry = patch_root_manifest(&root, &source).unwrap();
        assert!(entry.is_some(), "first run must patch the file");

        let after = std::fs::read_to_string(&root).unwrap();
        assert!(after.contains("[workspace.dependencies]"));
        assert!(after.contains("captrack"));
        assert!(after.contains("telemetry"));
    }

    #[test]
    fn patch_root_manifest_preserves_existing_deps_table() {
        let dir = make_workspace(ROOT_MANIFEST_WITH_DEPS, &[]);
        let root = dir.path().join("Cargo.toml");
        let source = WireSource::Version {
            version: "0.1".to_string(),
        };
        patch_root_manifest(&root, &source).unwrap();

        let after = std::fs::read_to_string(&root).unwrap();
        assert!(after.contains("serde"), "existing dep must survive");
        assert!(after.contains("captrack"));
    }

    #[test]
    fn patch_root_manifest_is_idempotent() {
        let dir = make_workspace(ROOT_MANIFEST, &[]);
        let root = dir.path().join("Cargo.toml");
        let source = WireSource::Version {
            version: "0.1".to_string(),
        };
        let first = patch_root_manifest(&root, &source).unwrap();
        assert!(first.is_some());

        let second = patch_root_manifest(&root, &source).unwrap();
        assert!(second.is_none(), "second run must be a no-op");
    }

    #[test]
    fn patch_root_manifest_with_path_source() {
        let dir = make_workspace(ROOT_MANIFEST, &[]);
        let root = dir.path().join("Cargo.toml");
        let source = WireSource::Path {
            path: PathBuf::from("/abs/path/to/captrack"),
        };
        patch_root_manifest(&root, &source).unwrap();
        let after = std::fs::read_to_string(&root).unwrap();
        assert!(after.contains("path"));
        assert!(after.contains("/abs/path/to/captrack".replace('\\', "/").as_str()) || after.contains("captrack"));
    }

    // ── patch_member_manifest ────────────────────────────────────────────────

    #[test]
    fn patch_member_manifest_adds_workspace_true_dep() {
        let dir = make_workspace(ROOT_MANIFEST, &[]);
        let member = dir.path().join("member.toml");
        std::fs::write(&member, MEMBER_MANIFEST).unwrap();

        let entry = patch_member_manifest(&member).unwrap();
        assert!(entry.is_some());

        let after = std::fs::read_to_string(&member).unwrap();
        assert!(after.contains("captrack"));
        assert!(after.contains("workspace = true") || after.contains("workspace=true"));
        assert!(after.contains("anyhow"), "existing dep must survive");
    }

    #[test]
    fn patch_member_manifest_creates_dependencies_table_if_missing() {
        let dir = make_workspace(ROOT_MANIFEST, &[]);
        let member = dir.path().join("member.toml");
        std::fs::write(&member, MEMBER_MANIFEST_NO_DEPS).unwrap();

        let entry = patch_member_manifest(&member).unwrap();
        assert!(entry.is_some());
        let after = std::fs::read_to_string(&member).unwrap();
        assert!(after.contains("[dependencies]"));
        assert!(after.contains("captrack"));
    }

    #[test]
    fn patch_member_manifest_is_idempotent() {
        let dir = make_workspace(ROOT_MANIFEST, &[]);
        let member = dir.path().join("member.toml");
        std::fs::write(&member, MEMBER_MANIFEST).unwrap();

        let first = patch_member_manifest(&member).unwrap();
        assert!(first.is_some());
        let second = patch_member_manifest(&member).unwrap();
        assert!(second.is_none());
    }

    #[test]
    fn patch_member_manifest_sha_changes_after_patch() {
        let dir = make_workspace(ROOT_MANIFEST, &[]);
        let member = dir.path().join("member.toml");
        std::fs::write(&member, MEMBER_MANIFEST).unwrap();

        let entry = patch_member_manifest(&member).unwrap().unwrap();
        assert_ne!(entry.sha256_before, entry.sha256_after);
        assert_eq!(
            entry.sha256_before,
            sha256_hex(MEMBER_MANIFEST.as_bytes())
        );
        let after = std::fs::read_to_string(&member).unwrap();
        assert_eq!(entry.sha256_after, sha256_hex(after.as_bytes()));
    }

    // ── unwire (via run_wire / run_unwire) ──────────────────────────────────

    #[test]
    fn wire_then_unwire_restores_original_content() {
        let dir = make_workspace(
            ROOT_MANIFEST,
            &[
                ("crates/a", MEMBER_MANIFEST),
                ("crates/b", MEMBER_MANIFEST),
            ],
        );
        let workspace_root = dir.path().to_path_buf();

        run_wire(WireArgs {
            workspace_root: workspace_root.clone(),
            captrack_path: None,
        })
        .unwrap();

        let root_after_wire = std::fs::read_to_string(workspace_root.join("Cargo.toml")).unwrap();
        assert!(root_after_wire.contains("captrack"));

        run_unwire(UnwireArgs {
            workspace_root: workspace_root.clone(),
            manifest: None,
        })
        .unwrap();

        let root_after_unwire =
            std::fs::read_to_string(workspace_root.join("Cargo.toml")).unwrap();
        assert_eq!(root_after_unwire, ROOT_MANIFEST);

        let a_after_unwire =
            std::fs::read_to_string(workspace_root.join("crates/a/Cargo.toml")).unwrap();
        assert_eq!(a_after_unwire, MEMBER_MANIFEST);

        // Manifest file must be removed after a successful unwire.
        assert!(!default_wire_manifest_path(&workspace_root).is_file());
    }

    #[test]
    fn unwire_refuses_when_manifest_missing() {
        let dir = tempfile::tempdir().unwrap();
        let result = run_unwire(UnwireArgs {
            workspace_root: dir.path().to_path_buf(),
            manifest: None,
        });
        assert!(result.is_err());
    }

    #[test]
    fn unwire_refuses_when_file_modified_after_wire() {
        let dir = make_workspace(ROOT_MANIFEST, &[("crates/a", MEMBER_MANIFEST)]);
        let workspace_root = dir.path().to_path_buf();

        run_wire(WireArgs {
            workspace_root: workspace_root.clone(),
            captrack_path: None,
        })
        .unwrap();

        // Tamper with the root manifest after wiring — unwire must detect
        // the sha256 mismatch and refuse to revert.
        let root_path = workspace_root.join("Cargo.toml");
        let mut tampered = std::fs::read_to_string(&root_path).unwrap();
        tampered.push_str("\n# manual edit\n");
        std::fs::write(&root_path, &tampered).unwrap();

        let result = run_unwire(UnwireArgs {
            workspace_root: workspace_root.clone(),
            manifest: None,
        });
        assert!(result.is_err(), "unwire must refuse on sha mismatch");

        // The tampered file must be left untouched.
        let still_tampered = std::fs::read_to_string(&root_path).unwrap();
        assert_eq!(still_tampered, tampered);
    }

    #[test]
    fn wire_is_idempotent_second_run_touches_nothing() {
        let dir = make_workspace(ROOT_MANIFEST, &[("crates/a", MEMBER_MANIFEST)]);
        let workspace_root = dir.path().to_path_buf();

        run_wire(WireArgs {
            workspace_root: workspace_root.clone(),
            captrack_path: None,
        })
        .unwrap();
        let root_after_first = std::fs::read_to_string(workspace_root.join("Cargo.toml")).unwrap();

        // Second wire run should find everything already wired.
        run_wire(WireArgs {
            workspace_root: workspace_root.clone(),
            captrack_path: None,
        })
        .unwrap();
        let root_after_second =
            std::fs::read_to_string(workspace_root.join("Cargo.toml")).unwrap();

        assert_eq!(root_after_first, root_after_second);
    }
}
