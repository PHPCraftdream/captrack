//! In-place byte-splice patcher.
//!
//! Applies a `PatchPlan` to disk via precise byte-range substitution.
//! Никакого prettyplease / rustfmt — diff минимальный, только изменённое
//! число.  Writes an `ApplyManifest` so `undo` can revert.

use std::collections::BTreeMap;
use std::fmt::Write as FmtWrite;
use std::ops::Range;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::model::{CapExpr, Ctor, PatchEntry, PatchPlan};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApplyManifest {
    pub version: u32,
    pub entries: Vec<ManifestEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManifestEntry {
    pub file: PathBuf,
    pub byte_range: Range<usize>,
    pub before_text: String,
    pub after_text: String,
    pub sha256_before: String,
    pub sha256_after: String,
    pub reason: String,
}

/// Apply a plan in-place.  If `dry_run` is true, returns the would-be
/// manifest without touching files or writing the manifest file.
pub fn apply_plan(plan: &PatchPlan, workspace_root: &Path, dry_run: bool) -> Result<ApplyManifest> {
    // Group entries by their source file.
    let mut by_file: BTreeMap<PathBuf, Vec<&PatchEntry>> = BTreeMap::new();
    for e in &plan.entries {
        if matches!(e.from, CapExpr::Dynamic(_)) {
            return Err(anyhow!(
                "internal error: planner produced a Dynamic patch entry for {}",
                render_loc(e)
            ));
        }
        let abs = if e.key.file.is_absolute() {
            e.key.file.clone()
        } else {
            workspace_root.join(&e.key.file)
        };
        by_file.entry(abs).or_default().push(e);
    }

    let mut manifest_entries: Vec<ManifestEntry> = Vec::new();
    for (file, mut entries) in by_file {
        // Sort by descending start offset — splice from the tail forward so
        // earlier offsets stay valid after each replacement.
        entries.sort_by_key(|e| std::cmp::Reverse(e.span_bytes.start));

        let original =
            std::fs::read_to_string(&file).with_context(|| format!("read {}", file.display()))?;
        let sha_before = hex_sha256(original.as_bytes());

        // Apply.  We build a mutable Vec<u8> and splice.
        let mut buf: Vec<u8> = original.into_bytes();
        // Records: (new_range, before_text, after_text, patch_entry_index)
        // We store them in reverse application order so we can later sort ascending.
        let mut local_records: Vec<(Range<usize>, String, String, usize)> = Vec::new();
        for (idx, e) in entries.iter().enumerate() {
            let r = e.span_bytes.clone();
            if r.end > buf.len() {
                return Err(anyhow!(
                    "span {}..{} out of bounds (file len {}) for {}",
                    r.start,
                    r.end,
                    buf.len(),
                    file.display()
                ));
            }
            let before = std::str::from_utf8(&buf[r.clone()])
                .with_context(|| format!("non-utf8 in span at {}", file.display()))?
                .to_string();
            let after = replacement_for(e);
            // Splice.
            buf.splice(r.clone(), after.as_bytes().iter().copied());
            // Record with the NEW byte range (start unchanged, end = start + after.len()).
            local_records.push((r.start..(r.start + after.len()), before, after, idx));
        }

        let sha_after = hex_sha256(&buf);
        if !dry_run {
            atomic_write(&file, &buf)?;
        }

        // Sort manifest entries by ascending span start for determinism.
        local_records.sort_by_key(|r| r.0.start);
        // `entries` was sorted descending; entries[entries.len()-1-idx] is the
        // ascending-order partner — but we stored the original descending index.
        // Re-map: entries is in descending order; local_records[i].3 is the index
        // into that descending slice.  For the reason field we need the PatchEntry.
        for (rng, before, after, desc_idx) in local_records {
            let pe = entries[desc_idx];
            manifest_entries.push(ManifestEntry {
                file: file.clone(),
                byte_range: rng,
                before_text: before,
                after_text: after,
                sha256_before: sha_before.clone(),
                sha256_after: sha_after.clone(),
                reason: pe.reason.clone(),
            });
        }
    }

    let manifest = ApplyManifest {
        version: 1,
        entries: manifest_entries,
    };

    if !dry_run {
        let manifest_path = workspace_root
            .join("target")
            .join("captrack-pgo")
            .join("last-apply.json");
        if let Some(parent) = manifest_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        let json = serde_json::to_string_pretty(&manifest)?;
        std::fs::write(&manifest_path, json)
            .with_context(|| format!("write {}", manifest_path.display()))?;
    }

    Ok(manifest)
}

fn render_loc(e: &PatchEntry) -> String {
    format!("{}:{}:{}", e.key.file.display(), e.key.line, e.key.col)
}

fn type_name(c: &Ctor) -> &'static str {
    match c {
        Ctor::Vec => "Vec",
        Ctor::VecDeque => "VecDeque",
        Ctor::HashMap => "HashMap",
        Ctor::HashSet => "HashSet",
        Ctor::BTreeMap => "BTreeMap",
        Ctor::BTreeSet => "BTreeSet",
    }
}

/// What to substitute into the source for one patch entry, derived from
/// `ctor` and `from` (the patcher knows nothing else).
fn replacement_for(e: &PatchEntry) -> String {
    match &e.from {
        // `with_capacity(N)` — span covers only the literal; new text is the
        // new number alone.
        CapExpr::Literal(_) => e.to.to_string(),
        // `Vec::new()` / `vec![]` / `HashMap::new()` — span covers the whole
        // call; replace with `{Type}::with_capacity({to})`.
        CapExpr::Zero => format!("{}::with_capacity({})", type_name(&e.ctor), e.to),
        CapExpr::Dynamic(_) => {
            // Rejected by Planner, but defensive.
            unreachable!("dynamic-cap entries are filtered upstream")
        }
    }
}

fn hex_sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut s = String::with_capacity(64);
    for b in digest {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Atomic file replace: write to a sibling temp + rename over the target.
fn atomic_write(file: &Path, bytes: &[u8]) -> Result<()> {
    let parent = file.parent().unwrap_or_else(|| Path::new("."));
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    std::io::Write::write_all(&mut tmp, bytes)?;
    tmp.persist(file)
        .map_err(|e| anyhow::anyhow!("persist tempfile: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CapExpr, Ctor, PatchEntry, PatchPlan, SiteKey};
    use std::path::Path;

    fn key(file: &Path, line: u32, col: u32) -> SiteKey {
        SiteKey {
            file: file.to_path_buf(),
            line,
            col,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn make_entry(
        file: &Path,
        line: u32,
        col: u32,
        ctor: Ctor,
        from: CapExpr,
        to: usize,
        span: Range<usize>,
        reason: &str,
    ) -> PatchEntry {
        PatchEntry {
            key: key(file, line, col),
            ctor,
            from,
            to,
            span_bytes: span,
            reason: reason.to_string(),
        }
    }

    #[test]
    fn replacement_for_literal_substitutes_number_only() {
        let e = make_entry(
            Path::new("x.rs"),
            1,
            1,
            Ctor::Vec,
            CapExpr::Literal(0),
            64,
            0..1,
            "x",
        );
        assert_eq!(replacement_for(&e), "64");
    }

    #[test]
    fn replacement_for_zero_emits_with_capacity_call() {
        let e = make_entry(
            Path::new("x.rs"),
            1,
            1,
            Ctor::Vec,
            CapExpr::Zero,
            32,
            0..1,
            "x",
        );
        assert_eq!(replacement_for(&e), "Vec::with_capacity(32)");
    }

    #[test]
    fn replacement_for_hashmap_zero_uses_correct_type_name() {
        let e = make_entry(
            Path::new("x.rs"),
            1,
            1,
            Ctor::HashMap,
            CapExpr::Zero,
            16,
            0..1,
            "x",
        );
        assert_eq!(replacement_for(&e), "HashMap::with_capacity(16)");
    }

    #[test]
    fn replacement_for_vecdeque_zero() {
        let e = make_entry(
            Path::new("x.rs"),
            1,
            1,
            Ctor::VecDeque,
            CapExpr::Zero,
            8,
            0..1,
            "x",
        );
        assert_eq!(replacement_for(&e), "VecDeque::with_capacity(8)");
    }

    #[test]
    fn replacement_for_hashset_zero() {
        let e = make_entry(
            Path::new("x.rs"),
            1,
            1,
            Ctor::HashSet,
            CapExpr::Zero,
            4,
            0..1,
            "x",
        );
        assert_eq!(replacement_for(&e), "HashSet::with_capacity(4)");
    }

    #[test]
    fn replacement_for_btreemap_zero() {
        let e = make_entry(
            Path::new("x.rs"),
            1,
            1,
            Ctor::BTreeMap,
            CapExpr::Zero,
            4,
            0..1,
            "x",
        );
        assert_eq!(replacement_for(&e), "BTreeMap::with_capacity(4)");
    }

    #[test]
    fn replacement_for_btreeset_zero() {
        let e = make_entry(
            Path::new("x.rs"),
            1,
            1,
            Ctor::BTreeSet,
            CapExpr::Zero,
            4,
            0..1,
            "x",
        );
        assert_eq!(replacement_for(&e), "BTreeSet::with_capacity(4)");
    }

    #[test]
    fn apply_plan_e2e_replaces_literal_in_place() {
        let tmp = tempfile::tempdir().unwrap();
        let src_file = tmp.path().join("src.rs");
        let body = "fn main() {\n    let _v: Vec<u32> = Vec::with_capacity(8);\n}\n";
        std::fs::write(&src_file, body).unwrap();

        // span_bytes points to the literal "8" in the file.
        let pos = body.find('8').expect("literal in body");
        let plan = PatchPlan {
            entries: vec![make_entry(
                &src_file,
                2,
                39,
                Ctor::Vec,
                CapExpr::Literal(8),
                128,
                pos..pos + 1,
                "test",
            )],
            skipped: vec![],
        };
        let manifest = apply_plan(&plan, tmp.path(), false).unwrap();
        let after = std::fs::read_to_string(&src_file).unwrap();
        assert!(after.contains("Vec::with_capacity(128)"));
        assert!(!after.contains("Vec::with_capacity(8)"));

        // Manifest stored.
        let mpath = tmp
            .path()
            .join("target")
            .join("captrack-pgo")
            .join("last-apply.json");
        assert!(mpath.is_file(), "manifest must be written");
        let parsed: ApplyManifest =
            serde_json::from_str(&std::fs::read_to_string(&mpath).unwrap()).unwrap();
        assert_eq!(parsed, manifest);
        assert_eq!(manifest.entries.len(), 1);
        let me = &manifest.entries[0];
        assert_eq!(me.before_text, "8");
        assert_eq!(me.after_text, "128");
    }

    #[test]
    fn apply_plan_zero_ctor_replaces_whole_call() {
        let tmp = tempfile::tempdir().unwrap();
        let src_file = tmp.path().join("src.rs");
        let body = "let v = Vec::new();\n";
        std::fs::write(&src_file, body).unwrap();
        let pos = body.find("Vec::new()").unwrap();
        let plan = PatchPlan {
            entries: vec![make_entry(
                &src_file,
                1,
                9,
                Ctor::Vec,
                CapExpr::Zero,
                64,
                pos..pos + "Vec::new()".len(),
                "zero-ctor",
            )],
            skipped: vec![],
        };
        let manifest = apply_plan(&plan, tmp.path(), false).unwrap();
        let after = std::fs::read_to_string(&src_file).unwrap();
        assert!(after.contains("Vec::with_capacity(64)"));
        assert!(!after.contains("Vec::new()"));
        assert_eq!(manifest.entries[0].before_text, "Vec::new()");
        assert_eq!(manifest.entries[0].after_text, "Vec::with_capacity(64)");
    }

    #[test]
    fn apply_plan_multiple_in_one_file_uses_tail_first() {
        let tmp = tempfile::tempdir().unwrap();
        let src_file = tmp.path().join("src.rs");
        let body = "let a = Vec::with_capacity(1); let b = Vec::with_capacity(2);\n";
        std::fs::write(&src_file, body).unwrap();
        let pos1 = body.find('1').unwrap();
        let pos2 = body.find('2').unwrap();
        // Two patches: first → 10, second → 200; lengths differ, so
        // tail-first order is required to keep offsets valid.
        let plan = PatchPlan {
            entries: vec![
                make_entry(
                    &src_file,
                    1,
                    1,
                    Ctor::Vec,
                    CapExpr::Literal(1),
                    10,
                    pos1..pos1 + 1,
                    "a",
                ),
                make_entry(
                    &src_file,
                    1,
                    2,
                    Ctor::Vec,
                    CapExpr::Literal(2),
                    200,
                    pos2..pos2 + 1,
                    "b",
                ),
            ],
            skipped: vec![],
        };
        apply_plan(&plan, tmp.path(), false).unwrap();
        let after = std::fs::read_to_string(&src_file).unwrap();
        assert!(after.contains("Vec::with_capacity(10)"));
        assert!(after.contains("Vec::with_capacity(200)"));
        assert!(!after.contains("(1)"));
        assert!(!after.contains("(2)"));
    }

    #[test]
    fn dry_run_does_not_modify_file_or_write_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        let src_file = tmp.path().join("src.rs");
        let body = "Vec::with_capacity(8);\n";
        std::fs::write(&src_file, body).unwrap();
        let pos = body.find('8').unwrap();
        let plan = PatchPlan {
            entries: vec![make_entry(
                &src_file,
                1,
                1,
                Ctor::Vec,
                CapExpr::Literal(8),
                64,
                pos..pos + 1,
                "test",
            )],
            skipped: vec![],
        };
        let _ = apply_plan(&plan, tmp.path(), true).unwrap();
        let after = std::fs::read_to_string(&src_file).unwrap();
        assert_eq!(after, body, "dry_run must not modify file");
        let mpath = tmp
            .path()
            .join("target")
            .join("captrack-pgo")
            .join("last-apply.json");
        assert!(!mpath.exists(), "dry_run must not write manifest");
    }

    #[test]
    fn dynamic_cap_in_plan_is_internal_error() {
        let tmp = tempfile::tempdir().unwrap();
        let src_file = tmp.path().join("src.rs");
        let plan = PatchPlan {
            entries: vec![make_entry(
                &src_file,
                1,
                1,
                Ctor::Vec,
                CapExpr::Dynamic("input.len()".into()),
                64,
                0..1,
                "x",
            )],
            skipped: vec![],
        };
        let err = apply_plan(&plan, tmp.path(), false).unwrap_err();
        assert!(err.to_string().contains("Dynamic"));
    }

    #[test]
    fn sha256_is_deterministic_and_correct_length() {
        let data = b"hello world";
        // Same input → same output.
        assert_eq!(hex_sha256(data), hex_sha256(data));
        // Different inputs → different outputs.
        assert_ne!(hex_sha256(data), hex_sha256(b"different"));
        // SHA-256 digest is always 32 bytes = 64 hex chars.
        assert_eq!(hex_sha256(data).len(), 64);
        // All chars are valid lowercase hex.
        assert!(hex_sha256(data).chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn manifest_sha256_before_after_differ() {
        let tmp = tempfile::tempdir().unwrap();
        let src_file = tmp.path().join("src.rs");
        let body = "Vec::with_capacity(4);\n";
        std::fs::write(&src_file, body).unwrap();
        let pos = body.find('4').unwrap();
        let plan = PatchPlan {
            entries: vec![make_entry(
                &src_file,
                1,
                1,
                Ctor::Vec,
                CapExpr::Literal(4),
                128,
                pos..pos + 1,
                "test",
            )],
            skipped: vec![],
        };
        let manifest = apply_plan(&plan, tmp.path(), false).unwrap();
        let me = &manifest.entries[0];
        assert_ne!(
            me.sha256_before, me.sha256_after,
            "before/after hashes must differ"
        );
        assert_eq!(me.sha256_before.len(), 64);
        assert_eq!(me.sha256_after.len(), 64);
    }

    #[test]
    fn empty_plan_produces_empty_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        let plan = PatchPlan {
            entries: vec![],
            skipped: vec![],
        };
        let manifest = apply_plan(&plan, tmp.path(), false).unwrap();
        assert!(manifest.entries.is_empty());
        assert_eq!(manifest.version, 1);
        // No manifest file written for empty plan (no entries → no files processed).
        // The manifest dir need not exist.
    }

    #[test]
    fn out_of_bounds_span_returns_error() {
        let tmp = tempfile::tempdir().unwrap();
        let src_file = tmp.path().join("src.rs");
        std::fs::write(&src_file, "abc").unwrap();
        let plan = PatchPlan {
            entries: vec![make_entry(
                &src_file,
                1,
                1,
                Ctor::Vec,
                CapExpr::Literal(0),
                64,
                0..100, // out of bounds
                "x",
            )],
            skipped: vec![],
        };
        let err = apply_plan(&plan, tmp.path(), false).unwrap_err();
        assert!(err.to_string().contains("out of bounds"), "got: {}", err);
    }
}
