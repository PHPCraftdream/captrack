//! Planner: match scanned `AllocSite`s against profile `SiteStats`,
//! apply `rules::propose_cap` to each, return a `PatchPlan` with both
//! the patches and the skips (and reasons).
//!
//! Profile entries that don't match any AST site are warned (likely
//! generated code or macro expansion — captrack-pgo can't patch what
//! isn't in source).  AST sites without profile data are silently
//! skipped — they simply weren't exercised by the measurement run.

use std::collections::HashMap;

use crate::model::{AllocSite, PatchEntry, PatchPlan, SiteKey, SiteStats};
use crate::rules::{propose_cap_with, Decision, RuleConfig};

/// Build a patch plan from scanned sites + profile stats using the default rules.
pub fn build_plan(sites: Vec<AllocSite>, stats: Vec<SiteStats>) -> PatchPlan {
    build_plan_with(sites, stats, RuleConfig::default())
}

/// Same as `build_plan` with explicit rule configuration.
pub fn build_plan_with(sites: Vec<AllocSite>, stats: Vec<SiteStats>, cfg: RuleConfig) -> PatchPlan {
    let mut by_key: HashMap<SiteKey, SiteStats> =
        stats.into_iter().map(|s| (s.key.clone(), s)).collect();
    let mut entries = Vec::new();
    let mut skipped = Vec::new();

    for site in sites {
        let key = site.key.clone();
        let Some(stat) = by_key.remove(&key) else {
            // Site exists in source but profile saw no allocation — not
            // exercised by this measurement run.  Silent skip (matches
            // plan §3 Шаг 9 — "молча, не аллоцировали в этом прогоне").
            continue;
        };
        match propose_cap_with(&stat, &site.current_cap, cfg) {
            Decision::Patch { to, reason } => {
                entries.push(PatchEntry {
                    key,
                    ctor: site.ctor,
                    from: site.current_cap,
                    to,
                    span_bytes: site.span_bytes,
                    reason,
                });
            }
            Decision::Skip { reason } => {
                skipped.push((key, reason));
            }
        }
    }

    // Whatever remains in `by_key` is profile-only — measured but not in AST.
    for (key, _) in by_key {
        eprintln!(
            "captrack-pgo: warning: measured site not found in AST — possibly generated/macro: {}:{}:{}",
            key.file.display(),
            key.line,
            key.col,
        );
    }

    PatchPlan { entries, skipped }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CapExpr, Ctor, Unit};
    use std::path::PathBuf;

    fn key(file: &str, line: u32, col: u32) -> SiteKey {
        SiteKey {
            file: PathBuf::from(file),
            line,
            col,
        }
    }

    fn site_zero(file: &str, line: u32, col: u32) -> AllocSite {
        AllocSite {
            key: key(file, line, col),
            ctor: Ctor::Vec,
            current_cap: CapExpr::Zero,
            span_bytes: 100..110,
        }
    }

    fn site_literal(file: &str, line: u32, col: u32, n: usize) -> AllocSite {
        AllocSite {
            key: key(file, line, col),
            ctor: Ctor::Vec,
            current_cap: CapExpr::Literal(n),
            span_bytes: 100..110,
        }
    }

    fn stats(key: SiteKey, peak: usize, p95: usize, count: u64) -> SiteStats {
        SiteStats {
            key,
            unit: Unit::Elements,
            peak,
            p50: p95,
            p95,
            count,
        }
    }

    #[test]
    fn match_produces_patch_for_zero_current() {
        let sites = vec![site_zero("a.rs", 1, 1)];
        let stat = vec![stats(key("a.rs", 1, 1), 60, 60, 1000)];
        let plan = build_plan(sites, stat);
        assert_eq!(plan.entries.len(), 1, "should have one patch");
        let e = &plan.entries[0];
        assert_eq!(e.to, 64); // p95=60 → next_pow2 = 64
        assert!(plan.skipped.is_empty());
    }

    #[test]
    fn match_produces_skip_when_literal_is_sufficient() {
        let sites = vec![site_literal("a.rs", 5, 1, 128)];
        let stat = vec![stats(key("a.rs", 5, 1), 64, 64, 1000)];
        let plan = build_plan(sites, stat);
        assert!(plan.entries.is_empty(), "should produce no patches");
        assert_eq!(plan.skipped.len(), 1);
        assert!(plan.skipped[0].1.contains("already covers"));
    }

    #[test]
    fn site_without_profile_data_is_silently_skipped() {
        let sites = vec![site_zero("a.rs", 1, 1)];
        let plan = build_plan(sites, Vec::new());
        assert!(plan.entries.is_empty());
        assert!(plan.skipped.is_empty(), "no profile data → silent skip");
    }

    #[test]
    fn profile_without_ast_site_is_dropped_not_patched() {
        let sites: Vec<AllocSite> = Vec::new();
        let stat = vec![stats(key("ghost.rs", 1, 1), 100, 90, 1000)];
        let plan = build_plan(sites, stat);
        // Warning prints to stderr; not asserted here.  Nothing in the plan.
        assert!(plan.entries.is_empty());
        assert!(plan.skipped.is_empty());
    }

    #[test]
    fn multiple_sites_independent_decisions() {
        let sites = vec![
            site_zero("a.rs", 1, 1),
            site_literal("a.rs", 2, 1, 8),
            site_zero("b.rs", 3, 1),
        ];
        let stat = vec![
            stats(key("a.rs", 1, 1), 60, 60, 1000),  // → Patch(64)
            stats(key("a.rs", 2, 1), 100, 90, 1000), // peak >= 4*8 → Patch(128)
            stats(key("b.rs", 3, 1), 3, 3, 1000),    // peak < 4 → Skip
        ];
        let plan = build_plan(sites, stat);
        assert_eq!(plan.entries.len(), 2);
        assert_eq!(plan.skipped.len(), 1);
        // Patches preserve span_bytes from the AllocSite (not 0..0).
        assert!(plan.entries.iter().all(|e| !e.span_bytes.is_empty()));
    }

    #[test]
    fn patch_entry_carries_ctor_and_from() {
        let sites = vec![site_zero("a.rs", 1, 1)];
        let stat = vec![stats(key("a.rs", 1, 1), 60, 60, 1000)];
        let plan = build_plan(sites, stat);
        let e = &plan.entries[0];
        assert_eq!(e.ctor, Ctor::Vec);
        assert_eq!(e.from, CapExpr::Zero);
    }
}
