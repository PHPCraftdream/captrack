//! Human-readable rendering of a `PatchPlan`.
//!
//! The patcher and the planner are pure data; this module is the only
//! place that knows how the user-facing output should look.

use std::fmt::Write;

use crate::model::{CapExpr, Ctor, PatchPlan, SiteKey};

/// Render the plan as a multi-line string ready for stdout.
pub fn render_report(plan: &PatchPlan) -> String {
    let mut out = String::new();
    if plan.entries.is_empty() && plan.skipped.is_empty() {
        out.push_str("no allocation sites matched profile data — nothing to propose.\n");
        return out;
    }

    for e in &plan.entries {
        let from_str = render_call(&e.ctor, &e.from);
        let to_str = render_call(&e.ctor, &CapExpr::Literal(e.to));
        let _ = writeln!(out, "{}", render_key(&e.key));
        let _ = writeln!(out, "  {from_str}  →  {to_str}");
        let _ = writeln!(out, "  {}", e.reason);
        out.push('\n');
    }

    if !plan.entries.is_empty() {
        out.push_str("────────────────────────────────────────────\n");
    }
    let _ = writeln!(out, "{}", render_summary(plan));

    out
}

fn render_key(k: &SiteKey) -> String {
    format!("{}:{}:{}", k.file.display(), k.line, k.col)
}

/// Pretty-print a constructor call for the given `CapExpr`.
fn render_call(ctor: &Ctor, cap: &CapExpr) -> String {
    let type_name = match ctor {
        Ctor::Vec => "Vec",
        Ctor::VecDeque => "VecDeque",
        Ctor::HashMap => "HashMap",
        Ctor::HashSet => "HashSet",
        Ctor::BTreeMap => "BTreeMap",
        Ctor::BTreeSet => "BTreeSet",
    };
    match cap {
        CapExpr::Literal(n) => format!("{type_name}::with_capacity({n})"),
        CapExpr::Zero => format!("{type_name}::new()"),
        CapExpr::Dynamic(expr) => format!("{type_name}::with_capacity({expr})"),
    }
}

fn render_summary(plan: &PatchPlan) -> String {
    use std::collections::BTreeMap;
    // Group skips by a short category keyword extracted from the reason.
    let mut buckets: BTreeMap<&'static str, usize> = BTreeMap::new();
    for (_, reason) in &plan.skipped {
        let cat = if reason.contains("low frequency") {
            "low-count"
        } else if reason.contains("phantom") {
            "phantom"
        } else if reason.contains("dynamic") {
            "dynamic-expr"
        } else if reason.contains("already covers") {
            "sufficient"
        } else if reason.contains("variation") {
            "stable-cap"
        } else {
            "other"
        };
        *buckets.entry(cat).or_insert(0) += 1;
    }
    let cats: Vec<String> = buckets.iter().map(|(c, n)| format!("{n} {c}")).collect();
    if cats.is_empty() {
        format!("{} patch", plan.entries.len())
    } else {
        format!(
            "{} patch, {} skip ({})",
            plan.entries.len(),
            plan.skipped.len(),
            cats.join(", ")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{PatchEntry, SiteKey};
    use std::path::PathBuf;

    fn k(file: &str, line: u32) -> SiteKey {
        SiteKey {
            file: PathBuf::from(file),
            line,
            col: 1,
        }
    }

    #[test]
    fn renders_empty_plan() {
        let plan = PatchPlan {
            entries: vec![],
            skipped: vec![],
        };
        let s = render_report(&plan);
        assert!(s.contains("no allocation sites matched"));
    }

    #[test]
    fn renders_patch_with_from_to_and_reason() {
        let plan = PatchPlan {
            entries: vec![PatchEntry {
                key: k("a.rs", 10),
                ctor: Ctor::Vec,
                from: CapExpr::Zero,
                to: 64,
                span_bytes: 0..10,
                reason: "peak=60 p95=60 count=1000 → NextPow2".into(),
            }],
            skipped: vec![],
        };
        let s = render_report(&plan);
        assert!(s.contains("a.rs:10:1"));
        assert!(s.contains("Vec::new()"));
        assert!(s.contains("Vec::with_capacity(64)"));
        assert!(s.contains("→"));
        assert!(s.contains("peak=60"));
        assert!(s.ends_with("1 patch\n"));
    }

    #[test]
    fn summary_categorises_skips() {
        let plan = PatchPlan {
            entries: vec![],
            skipped: vec![
                (
                    k("a.rs", 1),
                    "count=3 < 10 (low frequency, not statistically sound)".into(),
                ),
                (
                    k("a.rs", 2),
                    "current cap 128 already covers observed peak 64".into(),
                ),
                (
                    k("a.rs", 3),
                    "peak=0 (phantom site, no allocation observed)".into(),
                ),
            ],
        };
        let s = render_report(&plan);
        assert!(s.contains("0 patch"));
        assert!(s.contains("3 skip"));
        assert!(s.contains("1 low-count"));
        assert!(s.contains("1 sufficient"));
        assert!(s.contains("1 phantom"));
    }

    #[test]
    fn renders_dynamic_cap_with_expression_text() {
        let plan = PatchPlan {
            entries: vec![PatchEntry {
                key: k("x.rs", 7),
                ctor: Ctor::HashMap,
                from: CapExpr::Dynamic("input.len()".into()),
                to: 128,
                span_bytes: 0..10,
                reason: "x".into(),
            }],
            skipped: vec![],
        };
        let s = render_report(&plan);
        assert!(s.contains("HashMap::with_capacity(input.len())"));
    }
}
