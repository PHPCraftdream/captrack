//! `CAPTRACK_PGO_INSTRUMENT` lint — auto-wraps bare std collection constructors
//! in `::captrack::TrackedX::with_capacity_named(...)` so a profile run can be
//! done without any hand-written macro calls.
//!
//! Active when `CAPTRACK_PGO_INSTRUMENT=1` (or any truthy value: "1", "true",
//! case-insensitive).  When both `CAPTRACK_PGO_INSTRUMENT` and
//! `CAPTRACK_PGO_PROFILE` are set, this lint runs and the capacity lint is
//! suppressed; a one-time `eprintln!` warning is emitted.

use std::sync::OnceLock;

use clippy_utils::diagnostics::span_lint_and_sugg;
use clippy_utils::source::snippet_opt;
use clippy_utils::sym;
use rustc_errors::Applicability;
use rustc_hir::{Expr, ExprKind, QPath};
use rustc_lint::{LateContext, LateLintPass};
use rustc_middle::ty::TyKind;
use rustc_session::declare_lint;
use rustc_span::Symbol;

use crate::{is_tracked_method, span_to_site_key, sym_with_capacity_and_hasher, TRACKED_TYPES};

declare_lint! {
    /// ### What it does
    ///
    /// Replaces every bare std collection constructor (`Vec::new()`,
    /// `HashMap::with_capacity(N)`, etc.) with the corresponding
    /// `::captrack::TrackedX::with_capacity_named(...)` call so that a
    /// profiling run records peak capacities without any hand-written macro
    /// calls.
    ///
    /// ### Configuration
    ///
    /// Set `CAPTRACK_PGO_INSTRUMENT=1` before building.
    /// Without this env var the lint is a no-op.
    ///
    /// ### Why is this bad?
    ///
    /// Bare constructors produce no telemetry.  This lint instruments them
    /// for a single profiling pass; the instrumentation is reverted by
    /// `captrack-pgo uninstrument`.
    ///
    /// ### Example
    ///
    /// ```rust
    /// let v: Vec<u8> = Vec::new();
    /// // becomes:
    /// let v: Vec<u8> = ::captrack::TrackedVec::<_>::with_capacity_named(0, "auto:src/main.rs:1:18", file!(), line!(), column!());
    /// ```
    pub CAPTRACK_PGO_INSTRUMENT,
    Warn,
    "auto-wraps std collection constructors in TrackedX::with_capacity_named for telemetry collection"
}

/// Returns `true` when `CAPTRACK_PGO_INSTRUMENT` is set to a truthy value.
///
/// Evaluated once and cached.
pub(crate) fn instrument_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("CAPTRACK_PGO_INSTRUMENT")
            .map(|v| {
                let v = v.trim().to_ascii_lowercase();
                v == "1" || v == "true" || v == "yes"
            })
            .unwrap_or(false)
    })
}

/// Emitted at most once when both env vars are set.
static DUAL_WARNING_EMITTED: OnceLock<()> = OnceLock::new();

pub(crate) fn maybe_warn_dual_vars() {
    if std::env::var("CAPTRACK_PGO_PROFILE").is_ok() && instrument_enabled() {
        DUAL_WARNING_EMITTED.get_or_init(|| {
            eprintln!(
                "captrack-pgo-lint: both INSTRUMENT and PROFILE env vars set; \
                 using INSTRUMENT and ignoring PROFILE"
            );
        });
    }
}

/// The lint pass struct.
#[derive(Default)]
pub struct CaptrackPgoInstrument;

impl<'tcx> LateLintPass<'tcx> for CaptrackPgoInstrument {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        if !instrument_enabled() {
            return;
        }

        // Only handle ExprKind::Call with a path callee (Form 1).
        // Method-call form is uncommon for std constructors.
        if let ExprKind::Call(fn_expr, args) = &expr.kind {
            if let ExprKind::Path(qpath) = &fn_expr.kind {
                check_and_instrument(cx, expr, fn_expr, qpath, args);
            }
        }
    }
}

/// Map a diagnostic type symbol to the absolute `::captrack::TrackedX` path
/// and the constructor method to call.
///
/// Returns `(tracked_path, use_new_named)` where:
/// - `tracked_path` is e.g. `"::captrack::TrackedVec::<_>"`
/// - `use_new_named` is true for BTree types that only have `new_named`
fn symbol_to_tracked(sym: Symbol) -> Option<(&'static str, bool)> {
    if sym == sym::Vec {
        Some(("::captrack::TrackedVec::<_>", false))
    } else if sym == sym::VecDeque {
        Some(("::captrack::TrackedVecDeque::<_>", false))
    } else if sym == sym::HashMap {
        Some(("::captrack::TrackedHashMap::<_, _>", false))
    } else if sym == sym::HashSet {
        Some(("::captrack::TrackedHashSet::<_, _>", false))
    } else if sym == sym::BTreeMap {
        Some(("::captrack::TrackedBTreeMap::<_, _>", true))
    } else if sym == sym::BTreeSet {
        Some(("::captrack::TrackedBTreeSet::<_>", true))
    } else {
        None
    }
}

/// Core check: resolve the call site, determine if it's a tracked constructor,
/// and emit a `MachineApplicable` suggestion.
fn check_and_instrument<'tcx>(
    cx: &LateContext<'tcx>,
    call_expr: &Expr<'tcx>,
    fn_expr: &Expr<'tcx>,
    qpath: &QPath<'tcx>,
    args: &'tcx [Expr<'tcx>],
) {
    let span = call_expr.span;

    // Skip macro-expanded sites (e.g. `vec![]`).
    if span.from_expansion() {
        return;
    }

    let typeck = cx.typeck_results();

    // Resolve the path to a DefId.
    let res = typeck.qpath_res(qpath, fn_expr.hir_id);
    let Some(def_id) = res.opt_def_id() else {
        return;
    };

    // Extract method name from the last path segment.
    let method_name = match qpath {
        QPath::TypeRelative(_, segment) => segment.ident.name,
        QPath::Resolved(_, path) => {
            let Some(last) = path.segments.last() else {
                return;
            };
            last.ident.name
        }
    };

    if !is_tracked_method(method_name) {
        return;
    }

    // Use impl_of_assoc to find the Self type.
    let Some(impl_did) = cx.tcx.impl_of_assoc(def_id) else {
        return;
    };
    let self_ty = cx.tcx.type_of(impl_did).instantiate_identity();
    let TyKind::Adt(adt_def, _) = self_ty.kind() else {
        return;
    };
    let Some(diag_name) = cx.tcx.get_diagnostic_name(adt_def.did()) else {
        return;
    };
    if !TRACKED_TYPES.contains(&diag_name) {
        return;
    }

    // Skip if the receiver type is already a TrackedX type (double-instrument
    // guard). We detect this by checking whether the return type of the call
    // lives in the `::captrack` path. Since TrackedX types are not in std,
    // we check that the ADT's crate name is "captrack".
    let ret_ty = typeck.expr_ty(call_expr);
    if let TyKind::Adt(ret_adt, _) = ret_ty.kind() {
        let krate = cx.tcx.crate_name(ret_adt.did().krate);
        if krate.as_str() == "captrack" {
            return;
        }
    }

    let Some((tracked_path, use_new_named)) = symbol_to_tracked(diag_name) else {
        return;
    };

    // Compute the "auto:<file>:<line>:<col>" label from the call site.
    let site = span_to_site_key(cx, span);
    // Use the path as-is (OS-native separators).  On Windows this means
    // backslashes, which compiletest's normalizer will handle correctly:
    // the JSON-escaped `\\` form is matched by `normalize_path` and
    // replaced with `$DIR` before the backslash→slash pass.
    let file_str = site.file.to_string_lossy();
    let auto_label = format!("auto:{}:{}:{}", file_str, site.line, site.col);

    // Build the replacement expression.
    let suggestion = build_instrument_suggestion(
        cx,
        fn_expr,
        method_name,
        args,
        tracked_path,
        use_new_named,
        &auto_label,
    );
    let Some(sugg_text) = suggestion else {
        return;
    };

    span_lint_and_sugg(
        cx,
        CAPTRACK_PGO_INSTRUMENT,
        span,
        format!(
            "captrack-pgo-instrument: wrapping `{}` constructor for telemetry profiling",
            diag_name.as_str()
        ),
        "instrument with TrackedX::with_capacity_named",
        sugg_text,
        Applicability::MachineApplicable,
    );
}

/// Build the replacement source string for the instrument lint.
///
/// For most constructors (`Vec::new`, `Vec::with_capacity`, etc.) the form is:
/// ```text
/// ::captrack::TrackedVec::<_>::with_capacity_named(cap, "auto:...", file!(), line!(), column!())
/// ```
///
/// For `BTreeMap::new` / `BTreeSet::new` the form uses `new_named`:
/// ```text
/// ::captrack::TrackedBTreeMap::<_, _>::new_named(0, "auto:...", file!(), line!(), column!())
/// ```
///
/// For `HashMap::with_capacity_and_hasher(K, h)`:
/// ```text
/// ::captrack::TrackedHashMap::<_, _, _>::with_capacity_and_hasher_named(K, h, "auto:...", file!(), line!(), column!())
/// ```
/// Note: `with_capacity_and_hasher_named` exists on `TrackedHashMap` and
/// `TrackedHashSet` (verified in `src/tracked/hashmap.rs` and `hashset.rs`).
fn build_instrument_suggestion<'tcx>(
    cx: &LateContext<'tcx>,
    _fn_expr: &Expr<'tcx>,
    method_name: Symbol,
    args: &'tcx [Expr<'tcx>],
    tracked_path: &str,
    use_new_named: bool,
    auto_label: &str,
) -> Option<String> {
    let name_arg = format!("\"{auto_label}\"");
    let meta_args = format!("file!(), line!(), column!()");

    if method_name == sym::new {
        if use_new_named {
            // BTree types: use new_named(cap_hint=0, name, file, line, col)
            Some(format!(
                "{tracked_path}::new_named(0, {name_arg}, {meta_args})"
            ))
        } else {
            // Vec, VecDeque, HashMap, HashSet: with_capacity_named(0, ...)
            Some(format!(
                "{tracked_path}::with_capacity_named(0, {name_arg}, {meta_args})"
            ))
        }
    } else if method_name == sym::with_capacity {
        // `with_capacity(K)` — extract the cap argument as source text.
        let cap_arg = args.first()?;
        let cap_text = snippet_opt(cx, cap_arg.span)?;
        Some(format!(
            "{tracked_path}::with_capacity_named({cap_text}, {name_arg}, {meta_args})"
        ))
    } else if method_name == sym_with_capacity_and_hasher() {
        // `with_capacity_and_hasher(K, h)` — both args preserved.
        // Verified: TrackedHashMap and TrackedHashSet both have
        // `with_capacity_and_hasher_named(cap, hasher, name, file, line, col)`.
        // BTree types don't have this method; they're excluded by use_new_named=true
        // and by not being hash-keyed.
        let cap_arg = args.first()?;
        let cap_text = snippet_opt(cx, cap_arg.span)?;
        let hasher_arg = args.get(1)?;
        let hasher_text = snippet_opt(cx, hasher_arg.span)?;
        // Adjust the tracked_path for the 3-param variant:
        // HashMap::<_, _> → HashMap::<_, _, _>
        // HashSet::<_, _> → HashSet::<_, _, _>
        let path_3 = tracked_path.replace("::<_, _>", "::<_, _, _>");
        Some(format!(
            "{path_3}::with_capacity_and_hasher_named({cap_text}, {hasher_text}, {name_arg}, {meta_args})"
        ))
    } else {
        None
    }
}
