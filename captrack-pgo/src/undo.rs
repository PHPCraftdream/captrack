//! Rollback the most recent `apply` using its manifest.
//!
//! Refuses to act if any target file's current SHA-256 differs from the
//! `sha256_after` the manifest recorded — that means the file was edited
//! between `apply` and `undo`, and a naive splice could corrupt the
//! unrelated edits.  Resolve via git in that case.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};

use crate::apply::ApplyManifest;

/// Default manifest location, relative to a workspace root.
pub fn default_manifest_path(workspace_root: &Path) -> PathBuf {
    workspace_root
        .join("target")
        .join("captrack-pgo")
        .join("last-apply.json")
}

/// Undo the recorded apply.  Returns the number of patches reverted.
///
/// On any sha256 mismatch the function returns an error WITHOUT writing
/// any file — undo is all-or-nothing per file (we revert the entire file
/// at once via splice; if any check fails before that splice, nothing is
/// written for that file).
pub fn undo_from(manifest_path: &Path) -> Result<usize> {
    let raw = std::fs::read_to_string(manifest_path)
        .with_context(|| format!("read manifest {}", manifest_path.display()))?;
    let manifest: ApplyManifest = serde_json::from_str(&raw)
        .with_context(|| format!("parse manifest {}", manifest_path.display()))?;
    if manifest.version != 1 {
        return Err(anyhow!(
            "unsupported manifest version {}: only v1 supported",
            manifest.version
        ));
    }

    // Group entries by file.
    let mut by_file: BTreeMap<PathBuf, Vec<&crate::apply::ManifestEntry>> = BTreeMap::new();
    for e in &manifest.entries {
        by_file.entry(e.file.clone()).or_default().push(e);
    }

    let mut total = 0usize;
    for (file, mut entries) in by_file {
        let current =
            std::fs::read_to_string(&file).with_context(|| format!("read {}", file.display()))?;
        let cur_sha = hex_sha256(current.as_bytes());

        // All entries for one file share the same sha256_after (the file
        // was hashed once after all patches were spliced).  Verify.
        let expected_after = entries
            .first()
            .map(|e| e.sha256_after.as_str())
            .unwrap_or("");
        if cur_sha != expected_after {
            return Err(anyhow!(
                "file modified since apply: {}\n  expected sha256 {}\n  current sha256  {}\n  resolve via git first",
                file.display(),
                expected_after,
                cur_sha
            ));
        }

        // The manifest records byte ranges using intermediate offsets from the
        // apply pass (which spliced patches tail-first).  Entries with a
        // LOWER manifest start were applied LATER during apply, meaning their
        // length changes shift the positions of entries with HIGHER starts.
        // We must adjust each entry's range to its true position in the final
        // (post-apply) file before we can splice backwards.
        //
        // True final offset for entry i =
        //   manifest_start[i]
        //   + Σ (after_len - before_len) for all j where manifest_start[j] < manifest_start[i]
        //
        // Sort ascending to compute prefix-sum adjustments.
        entries.sort_by_key(|a| a.byte_range.start);

        // Compute the true final ranges.
        let mut adjusted: Vec<(std::ops::Range<usize>, &crate::apply::ManifestEntry)> =
            Vec::with_capacity(entries.len());
        let mut cumulative_delta: isize = 0;
        for e in &entries {
            let delta = e.after_text.len() as isize - e.before_text.len() as isize;
            let adj_start = (e.byte_range.start as isize + cumulative_delta) as usize;
            let adj_end = adj_start + e.after_text.len();
            adjusted.push((adj_start..adj_end, e));
            cumulative_delta += delta;
        }

        // Now splice tail-first using the true final ranges.
        adjusted.sort_by_key(|b| std::cmp::Reverse(b.0.start));
        let mut buf = current.into_bytes();
        for (r, e) in &adjusted {
            let r = r.clone();
            if r.end > buf.len() {
                return Err(anyhow!(
                    "manifest range {}..{} out of bounds for {} (file len {})",
                    r.start,
                    r.end,
                    file.display(),
                    buf.len()
                ));
            }
            // Defensive: the bytes at the true range must equal after_text.
            let on_disk =
                std::str::from_utf8(&buf[r.clone()]).context("non-utf8 in undo target range")?;
            if on_disk != e.after_text {
                return Err(anyhow!(
                    "undo aborted: bytes at {}..{} are {:?}, expected {:?}",
                    r.start,
                    r.end,
                    on_disk,
                    e.after_text
                ));
            }
            buf.splice(r, e.before_text.bytes());
        }

        let restored_sha = hex_sha256(&buf);
        let expected_before = entries
            .first()
            .map(|e| e.sha256_before.as_str())
            .unwrap_or("");
        if restored_sha != expected_before {
            return Err(anyhow!(
                "undo internal error: post-revert sha {} != recorded sha_before {} for {}",
                restored_sha,
                expected_before,
                file.display()
            ));
        }

        atomic_write(&file, &buf)?;
        total += entries.len();
    }

    Ok(total)
}

fn hex_sha256(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    use std::fmt::Write;
    let mut s = String::with_capacity(64);
    for b in Sha256::digest(bytes) {
        let _ = write!(s, "{b:02x}");
    }
    s
}

fn atomic_write(file: &Path, bytes: &[u8]) -> Result<()> {
    let parent = file.parent().unwrap_or_else(|| Path::new("."));
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    std::io::Write::write_all(&mut tmp, bytes)?;
    tmp.persist(file)
        .map_err(|e| anyhow!("persist tempfile: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apply::apply_plan;
    use crate::model::{CapExpr, Ctor, PatchEntry, PatchPlan, SiteKey};

    fn key(file: &Path, line: u32, col: u32) -> SiteKey {
        SiteKey {
            file: file.to_path_buf(),
            line,
            col,
        }
    }

    #[test]
    fn apply_then_undo_yields_byte_identical_source() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src.rs");
        let original = "fn main() {\n    let _v: Vec<u32> = Vec::with_capacity(8);\n}\n";
        std::fs::write(&src, original).unwrap();

        let pos = original.find('8').unwrap();
        let plan = PatchPlan {
            entries: vec![PatchEntry {
                key: key(&src, 2, 39),
                ctor: Ctor::Vec,
                from: CapExpr::Literal(8),
                to: 128,
                span_bytes: pos..pos + 1,
                reason: "test".into(),
            }],
            skipped: vec![],
        };
        apply_plan(&plan, tmp.path(), false).unwrap();
        // sanity — file changed
        let after = std::fs::read_to_string(&src).unwrap();
        assert_ne!(after, original);

        let manifest_path = default_manifest_path(tmp.path());
        let n = undo_from(&manifest_path).unwrap();
        assert_eq!(n, 1);
        let restored = std::fs::read_to_string(&src).unwrap();
        assert_eq!(restored, original);
    }

    #[test]
    fn undo_refuses_on_external_modification() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src.rs");
        let original = "let _v: Vec<u32> = Vec::with_capacity(8);\n";
        std::fs::write(&src, original).unwrap();

        let pos = original.find('8').unwrap();
        let plan = PatchPlan {
            entries: vec![PatchEntry {
                key: key(&src, 1, 1),
                ctor: Ctor::Vec,
                from: CapExpr::Literal(8),
                to: 64,
                span_bytes: pos..pos + 1,
                reason: "test".into(),
            }],
            skipped: vec![],
        };
        apply_plan(&plan, tmp.path(), false).unwrap();

        // Simulate external edit: append a line.
        let mut after = std::fs::read_to_string(&src).unwrap();
        after.push_str("// stray edit\n");
        std::fs::write(&src, after).unwrap();

        let manifest_path = default_manifest_path(tmp.path());
        let err = undo_from(&manifest_path).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("modified since apply"), "got: {msg}");
    }

    #[test]
    fn undo_handles_multiple_patches_in_one_file() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src.rs");
        let original = "let a = Vec::with_capacity(1); let b = Vec::with_capacity(2);\n";
        std::fs::write(&src, original).unwrap();

        let pos1 = original.find('1').unwrap();
        let pos2 = original.find('2').unwrap();
        let plan = PatchPlan {
            entries: vec![
                PatchEntry {
                    key: key(&src, 1, 1),
                    ctor: Ctor::Vec,
                    from: CapExpr::Literal(1),
                    to: 10,
                    span_bytes: pos1..pos1 + 1,
                    reason: "a".into(),
                },
                PatchEntry {
                    key: key(&src, 1, 2),
                    ctor: Ctor::Vec,
                    from: CapExpr::Literal(2),
                    to: 200,
                    span_bytes: pos2..pos2 + 1,
                    reason: "b".into(),
                },
            ],
            skipped: vec![],
        };
        apply_plan(&plan, tmp.path(), false).unwrap();
        let n = undo_from(&default_manifest_path(tmp.path())).unwrap();
        assert_eq!(n, 2);
        let restored = std::fs::read_to_string(&src).unwrap();
        assert_eq!(restored, original);
    }

    #[test]
    fn unsupported_manifest_version_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let m = tmp.path().join("m.json");
        std::fs::write(&m, r#"{"version": 99, "entries": []}"#).unwrap();
        let err = undo_from(&m).unwrap_err();
        assert!(err.to_string().contains("manifest version"));
    }
}
