#![feature(rustc_private)]
#![warn(unused_extern_crates)]

extern crate rustc_hir;
extern crate rustc_middle;
extern crate rustc_span;

use std::path::PathBuf;

use clippy_utils::diagnostics::span_lint;
use clippy_utils::sym;
use rustc_hir::{Expr, ExprKind, QPath};
use rustc_lint::{LateContext, LateLintPass};
use rustc_middle::ty::TyKind;
use rustc_span::{FileName, Symbol};

mod loader;
mod model;

use loader::Profile;
use model::SiteKey;

dylint_linting::impl_late_lint! {
    /// ### What it does
    ///
    /// Detects collection constructor call-sites (`Vec::new()`,
    /// `HashMap::with_capacity(N)`, etc.) whose source location matches an
    /// entry in a captrack-pgo profile JSON and emits a warning with peak
    /// capacity data from the profile.
    ///
    /// ### Configuration
    ///
    /// Set `CAPTRACK_PGO_PROFILE=/path/to/profile.json` before building.
    /// Without this env var the lint is a no-op.
    ///
    /// ### Why is this bad?
    ///
    /// A collection allocated without a capacity hint causes repeated
    /// reallocations.  Profile data from a real workload can guide
    /// pre-sizing decisions.
    ///
    /// ### Example
    ///
    /// ```rust
    /// let v: Vec<u8> = Vec::new(); // profile shows peak=1024 — use with_capacity
    /// ```
    pub CAPTRACK_PGO_CAPACITY,
    Warn,
    "collection constructor whose call-site has profile data suggesting a capacity hint",
    CaptrackPgoCapacity
}

#[derive(Default)]
pub struct CaptrackPgoCapacity;

/// Symbols for the collection types we care about.
/// `Vec`, `HashMap`, `HashSet`, `BTreeMap` are in `rustc_span::sym::*`.
/// `BTreeSet` and `VecDeque` are added by clippy_utils.
const TRACKED_TYPES: &[Symbol] = &[
    sym::Vec,
    sym::VecDeque,
    sym::HashMap,
    sym::HashSet,
    sym::BTreeMap,
    sym::BTreeSet,
];

/// The `with_capacity_and_hasher` symbol — not in rustc predefined set, so we
/// intern it on first use.  We use a `std::sync::OnceLock` so we don't call
/// `Symbol::intern` in a hot path.
fn sym_with_capacity_and_hasher() -> Symbol {
    use std::sync::OnceLock;
    static SYM: OnceLock<Symbol> = OnceLock::new();
    *SYM.get_or_init(|| Symbol::intern("with_capacity_and_hasher"))
}

/// Return true if `name` is one of the constructor method names we track.
fn is_tracked_method(name: Symbol) -> bool {
    name == sym::new || name == sym::with_capacity || name == sym_with_capacity_and_hasher()
}

impl<'tcx> LateLintPass<'tcx> for CaptrackPgoCapacity {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        // Fast-path: profile is empty (env var unset) → no work.
        let profile = loader::profile();
        if profile.is_empty() {
            return;
        }

        // ------------------------------------------------------------------ //
        // Form 1: Static path call — `Vec::new()`, `HashMap::with_capacity(N)`,
        //         alias-routed calls like `Map::new()` (HIR resolves aliases),
        //         and `Default::default()` returning a tracked collection.
        // ------------------------------------------------------------------ //
        if let ExprKind::Call(fn_expr, _args) = &expr.kind {
            if let ExprKind::Path(qpath) = &fn_expr.kind {
                check_call_site(cx, expr, fn_expr, qpath, profile);
            }
        }

        // ------------------------------------------------------------------ //
        // Form 2: Method call — `receiver.with_capacity(N)` (uncommon for std
        // collections, but possible for custom wrappers).
        // ------------------------------------------------------------------ //
        if let ExprKind::MethodCall(method_name, _receiver, _args, _span) = &expr.kind {
            if is_tracked_method(method_name.ident.name) {
                let typeck = cx.typeck_results();
                let ret_ty = typeck.expr_ty(expr);
                if let TyKind::Adt(adt_def, _) = ret_ty.kind() {
                    let diag_name = cx.tcx.get_diagnostic_name(adt_def.did());
                    if diag_name.is_some_and(|n| TRACKED_TYPES.contains(&n)) {
                        let call_span = expr.span;
                        if !call_span.from_expansion() {
                            emit_if_matched(cx, call_span, profile);
                        }
                    }
                }
            }
        }
    }
}

/// Check an `ExprKind::Call` node whose callee is a path expression.
fn check_call_site<'tcx>(
    cx: &LateContext<'tcx>,
    call_expr: &Expr<'tcx>,
    fn_expr: &Expr<'tcx>,
    qpath: &QPath<'tcx>,
    profile: &Profile,
) {
    // Skip macro-expanded sites — the span points into the macro body, not
    // the user's call site.  vec![] detection is deferred (TODO M2-followup).
    let span = call_expr.span;
    if span.from_expansion() {
        return;
    }

    let typeck = cx.typeck_results();

    // Resolve the path to a DefId.
    let res = typeck.qpath_res(qpath, fn_expr.hir_id);
    let Some(def_id) = res.opt_def_id() else {
        return;
    };

    // Extract the method/function name from the last path segment.
    let method_name = match qpath {
        QPath::TypeRelative(_, segment) => segment.ident.name,
        QPath::Resolved(_, path) => {
            let Some(last) = path.segments.last() else {
                return;
            };
            last.ident.name
        }
    };

    // -----------------------------------------------------------------------
    // Strategy A — method is a known constructor on a tracked collection type.
    //
    // Use `impl_of_assoc` to get the impl block and then inspect the `Self`
    // type's diagnostic name.  This works for:
    //   • `Vec::new()` — TypeRelative, method = "new"
    //   • `HashMap::with_capacity(n)` — TypeRelative, method = "with_capacity"
    //   • Alias form `use HashMap as Map; Map::new()` — HIR resolves the alias
    //     so `qpath_res` gives us the real DefId of `HashMap::new`.
    // -----------------------------------------------------------------------
    if is_tracked_method(method_name) {
        if let Some(impl_did) = cx.tcx.impl_of_assoc(def_id) {
            let self_ty = cx.tcx.type_of(impl_did).instantiate_identity();
            if let TyKind::Adt(adt_def, _) = self_ty.kind() {
                let diag_name = cx.tcx.get_diagnostic_name(adt_def.did());
                if diag_name.is_some_and(|n| TRACKED_TYPES.contains(&n)) {
                    emit_if_matched(cx, span, profile);
                    return;
                }
            }
        }
        // impl_of_assoc returned None (free function).  Fall through to the
        // return-type check below.
    }

    // -----------------------------------------------------------------------
    // Strategy B — check the inferred return type of the call expression.
    //
    // Catches:
    //   • `Default::default()` returning a tracked collection.
    //   • Any constructor-like free function whose result is one of our types.
    //   • Also catches non-constructor named functions if they accidentally
    //     share the same name — acceptable false positive rate for M2.
    //
    // TODO: `Default::default()` detection deferred to a follow-up; the
    // return-type check below already handles it if method_name == "default".
    // -----------------------------------------------------------------------
    let ret_ty = typeck.expr_ty(call_expr);
    if let TyKind::Adt(adt_def, _) = ret_ty.kind() {
        let diag_name = cx.tcx.get_diagnostic_name(adt_def.did());
        if diag_name.is_some_and(|n| TRACKED_TYPES.contains(&n)) {
            emit_if_matched(cx, span, profile);
        }
    }
}

/// Emit `CAPTRACK_PGO_CAPACITY` if `span` maps to a profile entry.
fn emit_if_matched(cx: &LateContext<'_>, span: rustc_span::Span, profile: &Profile) {
    let key = span_to_site_key(cx, span);
    if let Some(stats) = profile.get(&key) {
        let unit_str = match stats.unit {
            model::Unit::Elements => "elements",
            model::Unit::Bytes => "bytes",
        };
        span_lint(
            cx,
            CAPTRACK_PGO_CAPACITY,
            span,
            format!(
                "captrack-pgo: profile shows peak={peak} {unit}, p95={p95}, count={count} \
                 — consider with_capacity",
                peak = stats.peak,
                unit = unit_str,
                p95 = stats.p95,
                count = stats.count,
            ),
        );
    }
}

/// Convert a `Span` to the `SiteKey` that captrack's registry uses for the
/// same call site.
///
/// # Column offset convention
///
/// `captrack` macros call `file!()` / `line!()` / `column!()` at expansion time.
/// Both `line!()` and `column!()` are **1-based** in Rust.
///
/// `source_map.lookup_char_pos(span.lo())` returns a `Loc` where:
/// - `loc.line` is 1-based (matches `line!()`).
/// - `loc.col` is 0-based `CharPos` (does NOT match `column!()` — must add 1).
///
/// So: `col = loc.col.0 as u32 + 1`.
///
/// # File path
///
/// `file!()` gives the path the compiler was given for the source file,
/// typically workspace-relative (e.g. `src/main.rs`).  We extract the local
/// (non-remapped) path from `RealFileName::local_path()`.  On Windows, paths
/// may use backslashes — the profile must use the same representation that
/// the compiler produces (not normalised).
fn span_to_site_key(cx: &LateContext<'_>, span: rustc_span::Span) -> SiteKey {
    let sm = cx.tcx.sess.source_map();
    let loc = sm.lookup_char_pos(span.lo());

    let file: PathBuf = match &loc.file.name {
        FileName::Real(real) => {
            // `local_path()` returns the non-remapped local path, matching
            // what `file!()` expands to in normal builds.  When
            // `--remap-path-prefix` is active, `local_path()` may be `None`;
            // in that case we return an empty path so the key simply won't
            // match any profile entry (acceptable: remapped builds are an
            // edge case not covered by captrack's normal instrumentation).
            real.local_path()
                .map(|p| p.to_path_buf())
                .unwrap_or_default()
        }
        FileName::Custom(s) => PathBuf::from(s.as_str()),
        other => {
            // Non-real files (macros, doctests, …).  Build a placeholder path
            // so lookup always produces a `SiteKey` — it just won't match.
            PathBuf::from(format!("{other:?}"))
        }
    };

    SiteKey {
        file,
        line: loc.line as u32,
        // col is 0-based; column!() is 1-based → add 1.
        col: loc.col.0 as u32 + 1,
    }
}
