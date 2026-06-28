#![feature(rustc_private)]
#![warn(unused_extern_crates)]

extern crate rustc_ast;
extern crate rustc_errors;
extern crate rustc_hir;
extern crate rustc_middle;
extern crate rustc_span;

use std::path::PathBuf;

use clippy_utils::diagnostics::{span_lint, span_lint_and_sugg};
use clippy_utils::source::snippet_opt;
use clippy_utils::sym;
use rustc_ast::ast::LitKind;
use rustc_errors::Applicability;
use rustc_hir::{Expr, ExprKind, QPath};
use rustc_lint::{LateContext, LateLintPass};
use rustc_middle::ty::TyKind;
use rustc_span::{FileName, Symbol};

pub mod instrument;
mod loader;
mod model;
mod rules;

use loader::Profile;
use model::{CapExpr, Ctor, SiteKey};
use rules::Decision;

pub use instrument::CAPTRACK_PGO_INSTRUMENT;

// ── Dual-lint registration ────────────────────────────────────────────────────
//
// `impl_late_lint!` generates `register_lints` and can be used only once per
// cdylib.  Since this plugin hosts TWO lints (`CAPTRACK_PGO_CAPACITY` and
// `CAPTRACK_PGO_INSTRUMENT`), we write `register_lints` by hand and use
// `dylint_library!()` + `declare_lint!` / `impl_lint_pass!` directly.
//
// The `constituent` feature of `dylint_linting` is intentionally NOT used
// because this crate is a self-contained cdylib, not a constituent of a larger
// library.

extern crate rustc_lint;
extern crate rustc_session;

dylint_linting::dylint_library!();

#[unsafe(no_mangle)]
pub fn register_lints(sess: &rustc_session::Session, lint_store: &mut rustc_lint::LintStore) {
    dylint_linting::init_config(sess);

    // Check for dual-env-var conflict once per compilation.
    instrument::maybe_warn_dual_vars();

    // Register both lints so they appear in `cargo dylint --list`.
    lint_store.register_lints(&[CAPTRACK_PGO_CAPACITY, CAPTRACK_PGO_INSTRUMENT]);

    // Register the capacity lint pass — active only when PROFILE is set and
    // INSTRUMENT is NOT set (instrument takes priority).
    lint_store.register_late_pass(|_| Box::new(CaptrackPgoCapacity));

    // Register the instrument lint pass — active only when INSTRUMENT is set.
    lint_store.register_late_pass(|_| Box::new(instrument::CaptrackPgoInstrument));
}

rustc_session::declare_lint! {
    /// ### What it does
    ///
    /// Detects collection constructor call-sites (`Vec::new()`,
    /// `HashMap::with_capacity(N)`, etc.) whose source location matches an
    /// entry in a captrack-pgo profile JSON and emits a warning — with a
    /// rustfix-compatible suggestion where possible — based on peak capacity
    /// data from the profile.
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
    "collection constructor whose call-site has profile data suggesting a capacity hint"
}

rustc_session::impl_lint_pass!(CaptrackPgoCapacity => [CAPTRACK_PGO_CAPACITY]);
rustc_session::impl_lint_pass!(instrument::CaptrackPgoInstrument => [CAPTRACK_PGO_INSTRUMENT]);

#[derive(Default)]
pub struct CaptrackPgoCapacity;

/// Symbols for the collection types we care about.
/// `Vec`, `HashMap`, `HashSet`, `BTreeMap` are in `rustc_span::sym::*`.
/// `BTreeSet` and `VecDeque` are added by clippy_utils.
pub(crate) const TRACKED_TYPES: &[Symbol] = &[
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
pub(crate) fn sym_with_capacity_and_hasher() -> Symbol {
    use std::sync::OnceLock;
    static SYM: OnceLock<Symbol> = OnceLock::new();
    *SYM.get_or_init(|| Symbol::intern("with_capacity_and_hasher"))
}

/// Return true if `name` is one of the constructor method names we track.
pub(crate) fn is_tracked_method(name: Symbol) -> bool {
    name == sym::new || name == sym::with_capacity || name == sym_with_capacity_and_hasher()
}

impl<'tcx> LateLintPass<'tcx> for CaptrackPgoCapacity {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        // When INSTRUMENT is active, suppress the capacity lint — the user is
        // doing an instrumentation pass, not a capacity-patch pass.
        if instrument::instrument_enabled() {
            return;
        }

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
        if let ExprKind::Call(fn_expr, args) = &expr.kind {
            if let ExprKind::Path(qpath) = &fn_expr.kind {
                check_call_site(cx, expr, fn_expr, qpath, args, profile);
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
                            // Method-call form: emit warning only (span extraction
                            // for the receiver is more complex; deferred to M4).
                            emit_warning_only(cx, call_span, profile);
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
    args: &'tcx [Expr<'tcx>],
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
                if let Some(type_sym) = diag_name.filter(|n| TRACKED_TYPES.contains(n)) {
                    let ctor = symbol_to_ctor(type_sym);
                    let cap_expr = extract_cap_expr(cx, method_name, args);
                    emit_with_suggestion(
                        cx,
                        call_expr,
                        fn_expr,
                        ctor,
                        method_name,
                        args,
                        &cap_expr,
                        span,
                        profile,
                    );
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
    // -----------------------------------------------------------------------
    let ret_ty = typeck.expr_ty(call_expr);
    if let TyKind::Adt(adt_def, _) = ret_ty.kind() {
        let diag_name = cx.tcx.get_diagnostic_name(adt_def.did());
        if diag_name.is_some_and(|n| TRACKED_TYPES.contains(&n)) {
            // Strategy B: emit warning only (no suggestion for Default::default()
            // — we'd need to synthesise a type path, deferred to a later milestone).
            emit_warning_only(cx, span, profile);
        }
    }
}

/// Map a diagnostic type symbol to our `Ctor` enum.
fn symbol_to_ctor(sym: Symbol) -> Ctor {
    if sym == sym::Vec {
        Ctor::Vec
    } else if sym == sym::VecDeque {
        Ctor::VecDeque
    } else if sym == sym::HashMap {
        Ctor::HashMap
    } else if sym == sym::HashSet {
        Ctor::HashSet
    } else if sym == sym::BTreeMap {
        Ctor::BTreeMap
    } else {
        Ctor::BTreeSet
    }
}

/// Determine the current `CapExpr` from the method name and argument list.
fn extract_cap_expr<'tcx>(
    cx: &LateContext<'tcx>,
    method_name: Symbol,
    args: &'tcx [Expr<'tcx>],
) -> CapExpr {
    if method_name == sym::new {
        return CapExpr::Zero;
    }
    // `with_capacity(K)` and `with_capacity_and_hasher(K, h)` — cap is args[0].
    if let Some(cap_arg) = args.first() {
        if let ExprKind::Lit(lit) = &cap_arg.kind {
            if let LitKind::Int(n, _) = lit.node {
                return CapExpr::Literal(n.get() as usize);
            }
        }
        // Non-literal expression.
        let snip = snippet_opt(cx, cap_arg.span).unwrap_or_else(|| "<expr>".to_string());
        return CapExpr::Dynamic(snip);
    }
    // Fallback: no args (shouldn't happen for with_capacity).
    CapExpr::Zero
}

/// Emit a lint warning only (no suggestion).
///
/// Used for: BTreeMap/BTreeSet (no `with_capacity`), `Default::default()`,
/// and method-call form (deferred).
fn emit_warning_only(cx: &LateContext<'_>, span: rustc_span::Span, profile: &Profile) {
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

/// Emit a lint warning with an optional rustfix suggestion.
///
/// For `BTreeMap`/`BTreeSet` — no `with_capacity` exists — falls back to
/// `emit_warning_only`.  For `Default::default()` the caller should use
/// `emit_warning_only` directly.
#[allow(clippy::too_many_arguments)]
fn emit_with_suggestion<'tcx>(
    cx: &LateContext<'tcx>,
    _call_expr: &Expr<'tcx>,
    fn_expr: &Expr<'tcx>,
    ctor: Ctor,
    method_name: Symbol,
    args: &'tcx [Expr<'tcx>],
    cap_expr: &CapExpr,
    span: rustc_span::Span,
    profile: &Profile,
) {
    let key = span_to_site_key(cx, span);
    let Some(stats) = profile.get(&key) else {
        return;
    };

    let unit_str = match stats.unit {
        model::Unit::Elements => "elements",
        model::Unit::Bytes => "bytes",
    };

    // BTreeMap / BTreeSet have no with_capacity — warn only.
    if matches!(ctor, Ctor::BTreeMap | Ctor::BTreeSet) {
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
        return;
    }

    // Run the capacity-decision rules.
    let decision = rules::propose_cap(stats, cap_expr);

    match decision {
        Decision::Skip { reason } => {
            // Rules say no change needed — still emit a diagnostic so the
            // developer sees the profile data, but without a suggestion.
            span_lint(
                cx,
                CAPTRACK_PGO_CAPACITY,
                span,
                format!(
                    "captrack-pgo: profile shows peak={peak} {unit}, p95={p95}, count={count} \
                     — no change suggested ({reason})",
                    peak = stats.peak,
                    unit = unit_str,
                    p95 = stats.p95,
                    count = stats.count,
                    reason = reason,
                ),
            );
        }
        Decision::Patch {
            to,
            applicability: rules_applicability,
            ..
        } => {
            // Map our local Applicability to rustc_errors::Applicability.
            let applicability = match rules_applicability {
                rules::Applicability::MachineApplicable => Applicability::MachineApplicable,
                rules::Applicability::MaybeIncorrect => Applicability::MaybeIncorrect,
            };

            let msg = format!(
                "captrack-pgo: profile shows peak={peak} {unit}, p95={p95}, count={count} \
                 — pre-allocate to {to}",
                peak = stats.peak,
                unit = unit_str,
                p95 = stats.p95,
                count = stats.count,
                to = to,
            );

            // Build the suggestion string.
            let Some(sugg) = build_suggestion(cx, fn_expr, method_name, args, to) else {
                // Couldn't get source snippet — fall back to warning only.
                span_lint(cx, CAPTRACK_PGO_CAPACITY, span, msg);
                return;
            };

            span_lint_and_sugg(
                cx,
                CAPTRACK_PGO_CAPACITY,
                span,
                msg,
                format!("use with_capacity({to})"),
                sugg,
                applicability,
            );
        }
    }
}

/// Build the replacement source string for the call expression.
///
/// Strategy: take the source text of `fn_expr` (the callee path, e.g.
/// `Vec::new` or `std::collections::HashMap::with_capacity`), strip the
/// trailing constructor name, append the new constructor name and arguments.
///
/// Returns `None` if the source snippet is unavailable (e.g. virtual files).
fn build_suggestion<'tcx>(
    cx: &LateContext<'tcx>,
    fn_expr: &Expr<'tcx>,
    method_name: Symbol,
    args: &'tcx [Expr<'tcx>],
    to: usize,
) -> Option<String> {
    let fn_text = snippet_opt(cx, fn_expr.span)?;

    // Strip the trailing constructor name from the path text to get the
    // qualifying prefix (e.g. `Vec` or `std::collections::HashMap`).
    let method_str = method_name.as_str();

    // The fn_text ends with "::<method_name>" or just the type name if
    // using TypeRelative form that clippy_utils desugars.  We look for
    // the last occurrence of "::" followed by the method name.
    let prefix = if let Some(pos) = fn_text.rfind("::") {
        // Everything before the last "::" separator.
        &fn_text[..pos]
    } else {
        // No "::" — shouldn't happen for associated functions, but fall back.
        return None;
    };

    // Verify that what follows "::" matches the expected method name.
    let after_colons = &fn_text[fn_text.rfind("::").unwrap() + 2..];
    // after_colons might have generic turbofish, e.g. `new::<T>`.
    // We only care that it starts with the method name.
    if !after_colons.starts_with(method_str) {
        return None;
    }

    // Determine the replacement constructor call.
    match method_name {
        n if n == sym::new || n == sym::with_capacity => {
            // `<Prefix>::with_capacity(<to>)`
            Some(format!("{prefix}::with_capacity({to})"))
        }
        n if n == sym_with_capacity_and_hasher() => {
            // `<Prefix>::with_capacity_and_hasher(<to>, <hasher>)`
            // The hasher is args[1] — preserve its source text verbatim.
            let hasher_span = args.get(1)?.span;
            let hasher_text = snippet_opt(cx, hasher_span)?;
            Some(format!(
                "{prefix}::with_capacity_and_hasher({to}, {hasher_text})"
            ))
        }
        _ => None,
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
pub(crate) fn span_to_site_key(cx: &LateContext<'_>, span: rustc_span::Span) -> SiteKey {
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
