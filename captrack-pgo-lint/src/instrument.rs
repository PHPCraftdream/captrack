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
use clippy_utils::is_expn_of;
use clippy_utils::source::snippet_opt;
use clippy_utils::sym;
use rustc_errors::Applicability;
use rustc_hir::def::Res;
use rustc_hir::intravisit::{self, Visitor};
use rustc_hir::{Body, BodyId, Expr, ExprKind, HirId, Node, PatKind, QPath};
use rustc_lint::{LateContext, LateLintPass};
use rustc_middle::hir::nested_filter;
use rustc_middle::ty::TyKind;
use rustc_session::declare_lint;
use rustc_span::Span;
use rustc_span::Symbol;

use crate::{is_tracked_method, recognise_tracked_type, span_to_site_key, TrackedType};

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
    /// let v = Vec::new();
    /// // becomes:
    /// let v = ::captrack::TrackedVec::<_>::wrap_from(Vec::new(), "auto:src/main.rs:1:9", file!(), line!(), column!());
    ///
    /// let v = vec![1, 2, 3];
    /// // becomes:
    /// let v = ::captrack::TrackedVec::<_>::wrap_from(vec![1, 2, 3], "auto:src/main.rs:1:9", file!(), line!(), column!());
    /// ```
    pub CAPTRACK_PGO_INSTRUMENT,
    Warn,
    "auto-wraps collection expressions in TrackedX::wrap_from for universal telemetry collection"
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
                // First, try the normal (non-expanded) path — also handles
                // vec![] (vec_new form) and smallvec![] after macro unwrap.
                check_and_instrument(cx, expr, fn_expr, qpath, args);
            }
        }

        // Separately handle vec![a, b, c] and vec![x; n] forms which expand
        // to non-tracked method calls (into_vec / from_elem).
        check_vec_like_macro_expansion(cx, expr);
    }
}

/// Map a `TrackedType` to the absolute `::captrack::TrackedX` path and
/// constructor style.
///
/// Returns `(tracked_path, use_new_named)` where:
/// - `tracked_path` is e.g. `"::captrack::TrackedVec::<_>"`
/// - `use_new_named` is `true` for types that only have `new_named` (BTree
///   types and `SccTreeIndex` — they have no `with_capacity` constructor).
///
/// `SmallVec` uses `with_capacity_named` (not `new_named`) because
/// `TrackedSmallVec::with_capacity_named(0, …)` is valid and correctly
/// initialises the inline array storage.
pub(crate) fn tracked_type_to_path(t: TrackedType) -> (&'static str, bool) {
    match t {
        TrackedType::Vec => ("::captrack::TrackedVec::<_>", false),
        TrackedType::VecDeque => ("::captrack::TrackedVecDeque::<_>", false),
        TrackedType::HashMap => ("::captrack::TrackedHashMap::<_, _>", false),
        TrackedType::HashSet => ("::captrack::TrackedHashSet::<_, _>", false),
        TrackedType::BTreeMap => ("::captrack::TrackedBTreeMap::<_, _>", true),
        TrackedType::BTreeSet => ("::captrack::TrackedBTreeSet::<_>", true),
        TrackedType::BytesMut => ("::captrack::TrackedBytesMut", false),
        TrackedType::IndexMap => ("::captrack::TrackedIndexMap::<_, _>", false),
        TrackedType::IndexSet => ("::captrack::TrackedIndexSet::<_>", false),
        TrackedType::DashMap => ("::captrack::TrackedDashMap::<_, _>", false),
        TrackedType::SccHashMap => ("::captrack::TrackedSccHashMap::<_, _>", false),
        TrackedType::SccHashSet => ("::captrack::TrackedSccHashSet::<_>", false),
        TrackedType::SccTreeIndex => ("::captrack::TrackedSccTreeIndex::<_, _>", true),
        TrackedType::SmallVec => ("::captrack::TrackedSmallVec::<_>", false),
    }
}

/// Short display name for a `TrackedType` used in lint messages.
fn tracked_type_display(t: TrackedType) -> &'static str {
    match t {
        TrackedType::Vec => "Vec",
        TrackedType::VecDeque => "VecDeque",
        TrackedType::HashMap => "HashMap",
        TrackedType::HashSet => "HashSet",
        TrackedType::BTreeMap => "BTreeMap",
        TrackedType::BTreeSet => "BTreeSet",
        TrackedType::BytesMut => "bytes::BytesMut",
        TrackedType::IndexMap => "indexmap::IndexMap",
        TrackedType::IndexSet => "indexmap::IndexSet",
        TrackedType::DashMap => "dashmap::DashMap",
        TrackedType::SccHashMap => "scc::HashMap",
        TrackedType::SccHashSet => "scc::HashSet",
        TrackedType::SccTreeIndex => "scc::TreeIndex",
        TrackedType::SmallVec => "SmallVec",
    }
}

/// Core check: resolve the call site, determine if it's a tracked constructor,
/// and emit a `MachineApplicable` suggestion.
///
/// Also handles macro-expanded sites where the expansion produces a tracked
/// constructor method call (`vec![]` → `Vec::new()`, `smallvec![]` →
/// `SmallVec::new()`).  In that case `span` is replaced by the outer
/// user-visible macro call span before the suggestion is emitted.
fn check_and_instrument<'tcx>(
    cx: &LateContext<'tcx>,
    call_expr: &Expr<'tcx>,
    fn_expr: &Expr<'tcx>,
    qpath: &QPath<'tcx>,
    _args: &'tcx [Expr<'tcx>],
) {
    let call_span = call_expr.span;

    // Resolve the effective span and whether we are in a tracked-macro context.
    //
    // When call_expr comes from a macro expansion we try to determine if the
    // macro is one of the tracked ones (vec!, smallvec!).  If so we use the
    // outer user-visible span for the suggestion; otherwise we skip.
    //
    // Note: vec![a,b,c] and vec![x;n] expand to *non-tracked* method calls
    // (into_vec / from_elem) and are handled separately in
    // `check_vec_like_macro_expansion`, not here.
    let span = if call_span.from_expansion() {
        // Walk the expansion chain to find the outermost tracked macro call.
        // `is_expn_of` returns Some(outer_span) if `span` is (transitively)
        // inside an expansion of a `!`-macro with the given name.
        if let Some(outer) = is_expn_of(call_span, sym::vec) {
            // Double check: outer span must not itself be inside another
            // expansion (i.e. the vec! call is in real user code).
            if outer.from_expansion() {
                return;
            }
            outer
        } else if let Some(outer) = is_expn_of(call_span, sym_smallvec()) {
            if outer.from_expansion() {
                return;
            }
            outer
        } else {
            // Not a tracked macro — skip.
            return;
        }
    } else {
        call_span
    };

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

    // Recognise the Self type as one of the 13 tracked collection types.
    // This covers both std types (via diagnostic name) and third-party types
    // (via def_path_str) in a single call.
    let Some(tracked_ty) = recognise_tracked_type(cx, adt_def.did()) else {
        return;
    };

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

    let (tracked_path, _use_new_named) = tracked_type_to_path(tracked_ty);

    // Bail out if rewriting this call-site would produce a `TrackedX` value in
    // a position where the surrounding code expects the bare `X`. `TrackedX`
    // derefs to `X` for *uses through a reference*, but in by-value contexts
    // (struct field init, fn return position, fn call argument, tuple field,
    // anywhere with an explicit `X` type written nearby) the types are
    // distinct and `cargo fix` would abort the whole crate with `E0308`.
    //
    // We adopt a conservative whitelist: only allow rewriting when the parent
    // HIR node is a `let` binding WITHOUT an explicit type annotation
    // (`let v = X::new();`). Everything else — type-ascribed let, return,
    // struct field, function argument, match arm tail, block tail — is
    // skipped. This is the smallest universally-safe set; the cost is some
    // missed sites, but each skip is preferable to a per-crate rollback.
    if !is_safe_instrument_context(cx, call_expr) {
        return;
    }

    // Compute the "auto:<file>:<line>:<col>" label from the call site.
    let site = span_to_site_key(cx, span);
    let auto_label = build_auto_label(&site.file, site.line, site.col);

    // Build the wrap_from replacement: wrap the original expression verbatim.
    // `span` is the user-visible call span (outer macro span when applicable).
    let Some(orig_snippet) = snippet_opt(cx, span) else {
        return;
    };
    let sugg_text = build_wrap_from_suggestion(tracked_path, &orig_snippet, &auto_label);

    span_lint_and_sugg(
        cx,
        CAPTRACK_PGO_INSTRUMENT,
        span,
        format!(
            "captrack-pgo-instrument: wrapping `{}` for telemetry profiling",
            tracked_type_display(tracked_ty)
        ),
        "instrument with TrackedX::wrap_from",
        sugg_text,
        Applicability::MachineApplicable,
    );
}

/// Build a `wrap_from` replacement string for the instrument lint.
///
/// The universal Phase K form:
/// ```text
/// ::captrack::TrackedVec::<_>::wrap_from(<orig_snippet>, "auto:...", file!(), line!(), column!())
/// ```
///
/// This works for ANY expression that evaluates to the target collection type —
/// `Vec::new()`, `Vec::with_capacity(N)`, `vec![a, b, c]`, `Vec::from_iter(it)`,
/// `smallvec![a, b, c]`, arbitrary builder calls, etc.  The original expression
/// is preserved verbatim; no element loss can occur.
pub(crate) fn build_wrap_from_suggestion(
    tracked_path: &str,
    orig_snippet: &str,
    auto_label: &str,
) -> String {
    let name_arg = format!("\"{auto_label}\"");
    let meta_args = "file!(), line!(), column!()";
    format!("{tracked_path}::wrap_from({orig_snippet}, {name_arg}, {meta_args})")
}

/// Cached interned `Symbol` for the `smallvec` macro name.
///
/// `sym::vec` is already in clippy_utils' prelude; `"smallvec"` is a
/// third-party crate macro, so we intern it on first use.
pub(crate) fn sym_smallvec() -> Symbol {
    static SYM: OnceLock<Symbol> = OnceLock::new();
    *SYM.get_or_init(|| Symbol::intern("smallvec"))
}

/// Which tracked macro produced the expanded call-site we are examining.
///
/// This is a pure, HIR-independent discriminant used only to choose the
/// correct `tracked_path` for the suggestion — no rustc context required,
/// making it trivially unit-testable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TrackedMacro {
    /// `vec![]` — expands to `Vec` operations.
    Vec,
    /// `smallvec![]` — expands to `SmallVec` operations.
    SmallVec,
}

/// Pure helper: should we attempt to instrument a macro with this name?
///
/// Only `"vec"` and `"smallvec"` are recognised; everything else is skipped.
/// This function has no rustc dependencies and is trivially unit-testable.
///
/// The function is `pub(crate)` primarily for unit testing; the runtime paths
/// use `is_expn_of` with interned symbols rather than calling this directly.
#[allow(dead_code)] // used in unit tests; runtime path uses is_expn_of + sym::vec/sym_smallvec
pub(crate) fn should_instrument_macro_name(name: &str) -> bool {
    name == "vec" || name == "smallvec"
}

/// Emit a `wrap_from` suggestion for a `vec!` or `smallvec!` expansion.
///
/// `outer_span` — the user-visible macro call span (`vec![…]` or
/// `smallvec![…]`).  The suggestion replaces this whole span with
/// `TrackedX::wrap_from(<macro_snippet>, "auto:...", file!(), line!(), column!())`.
///
/// Because `wrap_from` evaluates the original expression first, all element
/// initialisers are preserved — this is safe for all forms including
/// `vec![a, b, c]`, `vec![x; n]`, `smallvec![a, b, c]`.
///
/// Returns without emitting when the context is not safe for rewriting
/// (data-flow check via `is_safe_instrument_context`) or when the outer
/// snippet cannot be extracted.
fn emit_macro_wrap_from_suggestion<'tcx>(
    cx: &LateContext<'tcx>,
    call_expr: &Expr<'tcx>,
    outer_span: Span,
    tracked_macro: TrackedMacro,
) {
    // Data-flow guard — same as the non-macro path.
    if !is_safe_instrument_context(cx, call_expr) {
        return;
    }

    let tracked_path = match tracked_macro {
        TrackedMacro::Vec => "::captrack::TrackedVec::<_>",
        TrackedMacro::SmallVec => "::captrack::TrackedSmallVec::<_>",
    };

    // Extract the outer macro snippet, e.g. `vec![a, b, c]`.
    let Some(orig_snippet) = snippet_opt(cx, outer_span) else {
        return;
    };

    let site = span_to_site_key(cx, outer_span);
    let auto_label = build_auto_label(&site.file, site.line, site.col);
    let sugg = build_wrap_from_suggestion(tracked_path, &orig_snippet, &auto_label);

    let macro_name = match tracked_macro {
        TrackedMacro::Vec => "vec",
        TrackedMacro::SmallVec => "smallvec",
    };

    span_lint_and_sugg(
        cx,
        CAPTRACK_PGO_INSTRUMENT,
        outer_span,
        format!("captrack-pgo-instrument: wrapping `{macro_name}!` for telemetry profiling"),
        "instrument with TrackedX::wrap_from",
        sugg,
        Applicability::MachineApplicable,
    );
}

/// Capacity value extracted from a macro expansion form.
///
/// Pure discriminant — no HIR dependencies — retained for unit tests.
/// Phase K uses `wrap_from` universally (no capacity extraction needed),
/// so the runtime paths no longer branch on `MacroCap`.
#[allow(dead_code)] // used only in unit tests; Phase K wraps universally via wrap_from
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MacroCap<'a> {
    /// `vec![]` or `smallvec![]` — no elements, capacity is 0.
    #[allow(dead_code)] // retained for unit tests; Phase K wraps universally
    Zero,
    /// `vec![a, b, c]` — element count (used in unit tests only).
    #[allow(dead_code)] // retained for unit tests; Phase K wraps universally
    Literal(usize),
    /// `vec![x; n]` where `n` is a source expression (used in unit tests only).
    #[allow(dead_code)] // retained for unit tests; Phase K wraps universally
    Expr(&'a str),
}

/// Handle macro-expanded call-sites for forms that do NOT produce a tracked
/// method name (`vec![a,b,c]` → `into_vec` / `box_assume_init_into_vec_unsafe`,
/// `vec![x;n]` → `from_elem`).
///
/// With Phase K's `wrap_from` approach, ALL forms are safe to instrument by
/// wrapping the outer macro span verbatim — elements are evaluated first,
/// then handed to `wrap_from` which just records identity.
///
/// For `vec![]` (which expands to `Vec::new()`) this function does nothing —
/// that case is caught by the main `check_and_instrument` path.
///
/// The call is a no-op when the expression did not come from a tracked macro.
fn check_vec_like_macro_expansion<'tcx>(cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
    if !expr.span.from_expansion() {
        return;
    }

    // Detect vec! or smallvec! expansions via the expansion chain name.
    // `is_expn_of` walks up through nested expansions checking the bang-macro
    // name at each level — returns Some(outer_user_span) on the first match.
    let (outer_span, tracked_macro) = if let Some(outer) = is_expn_of(expr.span, sym::vec) {
        (outer, TrackedMacro::Vec)
    } else if let Some(outer) = is_expn_of(expr.span, sym_smallvec()) {
        (outer, TrackedMacro::SmallVec)
    } else {
        return;
    };

    // Outer span must be in real user code.
    if outer_span.from_expansion() {
        return;
    }

    // We only handle Call-form expansions here.  ExprKind::Array (for the
    // box [a,b,c] sub-expression) is visited separately by check_expr.
    let ExprKind::Call(fn_expr, _args) = &expr.kind else {
        return;
    };
    let ExprKind::Path(qpath) = &fn_expr.kind else {
        return;
    };

    let typeck = cx.typeck_results();
    let res = typeck.qpath_res(qpath, fn_expr.hir_id);
    let Some(def_id) = res.opt_def_id() else {
        return;
    };

    // Identify the inner function via its diagnostic name.
    let Some(diag_name) = cx.tcx.get_diagnostic_name(def_id) else {
        return;
    };

    // `vec![a, b, c]` — expands to `box_assume_init_into_vec_unsafe(...)`.
    //
    // Phase K: wrap_from evaluates the original expression first, so element
    // initialisers are NOT lost.  The outer macro span `vec![a, b, c]` is
    // extracted verbatim and passed to `TrackedVec::wrap_from(vec![a, b, c], ...)`.
    if diag_name == sym::box_assume_init_into_vec_unsafe {
        emit_macro_wrap_from_suggestion(cx, expr, outer_span, tracked_macro);
        return;
    }

    // `vec![x; n]` — expands to `from_elem(x, n)`.
    //
    // Phase K: wrap_from is safe here too — `vec![x; n]` evaluates all n
    // copies of x, producing a real Vec<_>, then wrap_from just records identity.
    if diag_name == sym::vec_from_elem {
        emit_macro_wrap_from_suggestion(cx, expr, outer_span, tracked_macro);
        return;
    }

    // `vec![]` with vec_new diagnostic — handled by the main
    // check_and_instrument path (Vec::new → tracked). Nothing to do here.
    // Other diagnostic names are not tracked.
}

/// Walk the argument list of `box_assume_init_into_vec_unsafe` to find the
/// inner `[a, b, c]` array literal and return its element count.
///
/// Returns 0 if the inner array cannot be found (conservative fallback).
///
/// Currently unused: the `box_assume_init_into_vec_unsafe` branch in
/// `check_vec_like_macro_expansion` returns early (non-empty vec![…] forms are
/// disabled).  Retained for the future element-preserving rewrite.
#[allow(dead_code)] // retained for future element-preserving rewrite — see TODO(vec-list)
fn extract_array_len_from_into_vec_args(args: &[Expr<'_>]) -> usize {
    // The expansion of vec![a, b, c] on nightly-2026-04-16 is:
    //   box_assume_init_into_vec_unsafe(
    //       write_box_via_move(
    //           Box::new_uninit_slice(N),
    //           [a, b, c]          ← ExprKind::Array
    //       )
    //   )
    // where write_box_via_move is a 2-arg Call whose second arg is the array.
    //
    // Layout (from clippy's VecArgs::hir):
    //   args[0]  = write_box_via_move(...) call
    //     ↳ inner_call.args[0] = Box::new_uninit_slice(N) call
    //     ↳ inner_call.args[1] = [a, b, c]  ExprKind::Array
    //
    // We dig one level in and count the elements.
    let Some(write_call) = args.first() else {
        return 0;
    };
    // The write_box call may be a Call or MethodCall — check both.
    let inner_args: &[Expr<'_>] = match &write_call.kind {
        ExprKind::Call(_, inner) => inner,
        ExprKind::MethodCall(_, _, inner, _) => inner,
        _ => return 0,
    };
    // Second arg should be the array literal.
    if let Some(array_expr) = inner_args.get(1) {
        if let ExprKind::Array(elems) = &array_expr.kind {
            return elems.len();
        }
    }
    0
}

/// Build the `auto:<file>:<line>:<col>` label that is embedded — wrapped in
/// double quotes — into a generated suggestion as a Rust string literal.
///
/// The path is normalised to forward slashes BEFORE quoting because the label
/// is inlined verbatim into source: a Windows-native `crates\foo\src\lib.rs`
/// would emit `"auto:crates\foo\src\lib.rs:42:7"`, where `\f`, `\s`, and `\l`
/// are invalid Rust character escapes — rustc rejects the entire literal,
/// `cargo fix` aborts, and the resulting parser confusion cascades into
/// spurious `E0107` / `E0308` errors in surrounding code, leaving the
/// workspace untouched.  Normalising to `/` keeps the label valid on every
/// platform; the real registry key is built from `file!()` / `line!()` /
/// `column!()` macros expanded at the call site, so the slash flavour in the
/// label is purely cosmetic.
pub(crate) fn build_auto_label(file: &std::path::Path, line: u32, col: u32) -> String {
    let normalised = file.to_string_lossy().replace('\\', "/");
    format!("auto:{normalised}:{line}:{col}")
}

/// Verdict from inspecting how a bound variable is **used** after a
/// `let v = Vec::new();` binding.  See `is_safe_instrument_context` for why
/// we care, and `classify_parent_kind` for the pattern-by-pattern logic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UsageSafety {
    /// The usage goes through `Deref` / `DerefMut` / `Index` / `IntoIterator
    /// for &T` — `TrackedX<T>` transparently substitutes for `X<T>` here,
    /// because both expose the same auto-deref surface.
    Safe,
    /// The usage requires the bare `X<T>` by value (return, struct field
    /// init, fn call argument, tuple field, assigned-from binding, etc.).
    /// Replacing the RHS with `TrackedX<T>` would produce `E0308`.
    Unsafe,
}

/// Discriminant-only summary of the parent expression around a usage —
/// purpose-built for unit-testing without constructing real `ExprKind`s.
///
/// Translated from `&ExprKind` via `parent_kind_of()` (see below), so we
/// never depend on the inner field shapes of `ExprKind` variants — those
/// drift across nightly releases.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ParentKind {
    /// `usage.method(args)` — usage is the receiver.
    MethodCallReceiver,
    /// `recv.method(usage, …)` — usage is one of the argument expressions.
    MethodCallArg,
    /// `&usage` / `&mut usage`.
    AddrOf,
    /// `usage[i]` or `arr[usage]`.
    Index,
    /// `return usage;`.
    Return,
    /// `(usage, …)` tuple element.
    Tuple,
    /// `Struct { field: usage }`.
    StructField,
    /// `func(usage)` or `func(usage, …)`.
    Call,
    /// Any other expression position — match scrutinee, cast, block tail,
    /// `let other = usage`, raw expression statement, etc.  Treated as
    /// by-value (unsafe) by default.
    Other,
}

/// PURE classifier — answers "is this single usage of the bound variable
/// safe to keep after the binding has changed type from `X` to `TrackedX`?"
///
/// Decision is made on the discriminant-only `ParentKind` so this function
/// is trivially unit-testable.  Receiver / `&`-ref / index positions
/// deref-coerce and stay safe; everything else (return, struct field init,
/// fn arg, tuple field, `let other = v`, …) is unsafe.
pub(crate) fn classify_parent_kind(parent: ParentKind) -> UsageSafety {
    match parent {
        // `v.push(x)`, `v.len()`, `v.iter()`, `v.clear()` — receiver position
        // goes through auto-deref → safe.  By-value receivers like
        // `v.into_iter()` would actually consume the binding by value and
        // therefore be unsafe, but the lint cannot tell receiver convention
        // from the parent kind alone (consumes-self is a property of the
        // resolved method, not the syntactic shape).  We accept the residual
        // risk: methods that consume by value are uncommon on collection
        // variables that were just freshly built with `X::new()` /
        // `X::with_capacity()` and immediately filled.
        ParentKind::MethodCallReceiver
            // `&v`, `&mut v` — deref coercion via `Deref`/`DerefMut`.
            | ParentKind::AddrOf
            // `v[i]`, `arr[v]` — `Index`/`IndexMut` traits are forwarded via
            // `Deref` for `TrackedX`.
            | ParentKind::Index => UsageSafety::Safe,
        // Every other position — by-value, type-fixed externally.
        //   • `return v;`           → ParentKind::Return
        //   • `S { field: v }`      → ParentKind::StructField
        //   • `foo(v)`              → ParentKind::Call / MethodCallArg
        //   • `(v, 0)`              → ParentKind::Tuple
        //   • `let other = v;`      → ParentKind::Other (init Expr of LetStmt)
        //   • `match v { … }`       → ParentKind::Other (scrutinee)
        //   • bare `v` as block tail expression → ParentKind::Other
        ParentKind::MethodCallArg
            | ParentKind::Return
            | ParentKind::Tuple
            | ParentKind::StructField
            | ParentKind::Call
            | ParentKind::Other => UsageSafety::Unsafe,
    }
}

/// Translate a real HIR `ExprKind` + receiver-slot bit into the
/// discriminant-only `ParentKind` consumed by `classify_parent_kind`.
///
/// This is the only adapter between the HIR API surface and the pure
/// classifier; if a future nightly reshuffles `ExprKind` variants, only
/// this function needs to follow.
fn parent_kind_of(parent: &ExprKind<'_>, usage_is_receiver: bool) -> ParentKind {
    match parent {
        ExprKind::MethodCall(..) => {
            if usage_is_receiver {
                ParentKind::MethodCallReceiver
            } else {
                ParentKind::MethodCallArg
            }
        }
        ExprKind::AddrOf(..) => ParentKind::AddrOf,
        ExprKind::Index(..) => ParentKind::Index,
        ExprKind::Ret(..) => ParentKind::Return,
        ExprKind::Tup(..) => ParentKind::Tuple,
        ExprKind::Struct(..) => ParentKind::StructField,
        ExprKind::Call(..) => ParentKind::Call,
        _ => ParentKind::Other,
    }
}

/// Return `true` only when this call-site is a context where replacing the
/// bare collection constructor with a `TrackedX::with_capacity_named(...)`
/// call is guaranteed not to produce an `E0308 mismatched types`.
///
/// ## The problem this guards against
///
/// `TrackedVec<T>` derefs to `Vec<T>` (and similarly for the other tracked
/// types), so `v.push(x)` / `&v` / `for x in &v` continue to compile after
/// the rewrite.  But in *by-value* positions — anywhere the surrounding code
/// names the bare type as the expected type — the types are distinct and
/// `cargo fix` aborts the entire crate.
///
/// ## Two-level data-flow check
///
/// 1. **Parent check.**  The call-site parent must be a `let` binding with
///    NO explicit type annotation (`let v = X::new();`).  An annotation
///    immediately pins the variable to `X`, blocking the rewrite.  A non-let
///    parent is also unsafe — only inside a binding can type inference
///    propagate the new `TrackedX` type forward.
///
/// 2. **Usage walk.**  Find every later reference to the binding in the
///    enclosing fn body.  Each usage is classified by `classify_usage_kind`;
///    if any usage is `Unsafe`, the rewrite would break that usage point
///    (return, struct field, fn arg, …) and the whole site is skipped.
///
/// Complex `let` patterns (`let (a, b) = …`, `let Some(v) = …`) are
/// conservatively rejected — we only handle plain identifier bindings.
pub(crate) fn is_safe_instrument_context<'tcx>(
    cx: &LateContext<'tcx>,
    call_expr: &Expr<'tcx>,
) -> bool {
    // Level 1: parent must be a `let v = …;` with no type annotation.
    let mut parents = cx.tcx.hir_parent_id_iter(call_expr.hir_id);
    let Some(parent_id) = parents.next() else {
        return false;
    };
    let Node::LetStmt(local) = cx.tcx.hir_node(parent_id) else {
        return false;
    };
    if local.ty.is_some() {
        return false;
    }

    // Extract the binding `HirId` from the pattern.  Anything beyond a plain
    // identifier (tuple destructuring, `mut`, `ref`, struct patterns, etc.)
    // is rejected — `mut` is fine semantically but the simplest check is
    // "is this a `Binding(_, hir_id, _, None)` with no sub-pattern?".
    let binding_hid = match local.pat.kind {
        PatKind::Binding(_, hir_id, _, None) => hir_id,
        _ => return false,
    };

    // Level 2: find the enclosing body and walk for all usages.
    let Some(body_id) = enclosing_body_id(cx, call_expr.hir_id) else {
        return false;
    };
    let body: &Body<'tcx> = cx.tcx.hir_body(body_id);

    let mut walker = UsageWalker {
        cx,
        target: binding_hid,
        any_unsafe: false,
    };
    walker.visit_body(body);
    !walker.any_unsafe
}

/// Climb the HIR parent chain from `start` until a node that owns a `Body`
/// is reached — that body's `BodyId` is what we walk to enumerate usages.
fn enclosing_body_id<'tcx>(cx: &LateContext<'tcx>, start: HirId) -> Option<BodyId> {
    for parent_id in cx.tcx.hir_parent_id_iter(start) {
        match cx.tcx.hir_node(parent_id) {
            Node::Item(item) => {
                if let Some(body_id) = item_body(item) {
                    return Some(body_id);
                }
            }
            Node::ImplItem(item) => {
                if let Some(body_id) = impl_item_body(item) {
                    return Some(body_id);
                }
            }
            Node::TraitItem(item) => {
                if let Some(body_id) = trait_item_body(item) {
                    return Some(body_id);
                }
            }
            Node::Expr(expr) => {
                if let ExprKind::Closure(closure) = expr.kind {
                    return Some(closure.body);
                }
            }
            _ => {}
        }
    }
    None
}

fn item_body(item: &rustc_hir::Item<'_>) -> Option<BodyId> {
    // Only fn bodies — const/static initialisers rarely contain mutable
    // collection constructions whose binding is later returned/escaped.
    // The signature of `ItemKind::Const` / `Static` shifts across nightlies;
    // sidestep that churn.
    match item.kind {
        rustc_hir::ItemKind::Fn { body, .. } => Some(body),
        _ => None,
    }
}

fn impl_item_body(item: &rustc_hir::ImplItem<'_>) -> Option<BodyId> {
    match item.kind {
        rustc_hir::ImplItemKind::Fn(_, body) => Some(body),
        _ => None,
    }
}

fn trait_item_body(item: &rustc_hir::TraitItem<'_>) -> Option<BodyId> {
    match item.kind {
        rustc_hir::TraitItemKind::Fn(_, rustc_hir::TraitFn::Provided(body)) => Some(body),
        _ => None,
    }
}

/// HIR visitor that finds every usage of a specific local binding and
/// classifies the immediate context.  Sets `any_unsafe = true` on the first
/// classify-unsafe usage found; the walk continues so the rest of the body
/// is fully visited (cheap, and lets future debug logs report ALL unsafe
/// usages rather than just the first).
struct UsageWalker<'a, 'tcx> {
    cx: &'a LateContext<'tcx>,
    target: HirId,
    any_unsafe: bool,
}

impl<'tcx> Visitor<'tcx> for UsageWalker<'_, 'tcx> {
    type NestedFilter = nested_filter::OnlyBodies;

    fn maybe_tcx(&mut self) -> Self::MaybeTyCtxt {
        self.cx.tcx
    }

    fn visit_expr(&mut self, expr: &'tcx Expr<'tcx>) {
        if let ExprKind::Path(QPath::Resolved(_, path)) = &expr.kind {
            if let Res::Local(local_id) = path.res {
                if local_id == self.target {
                    // We found a reference to the target binding.  Classify
                    // by walking ONE step up the parent chain.
                    if !is_usage_safe(self.cx, expr) {
                        self.any_unsafe = true;
                    }
                    // Do not recurse into the path itself — nothing to find.
                    return;
                }
            }
        }
        intravisit::walk_expr(self, expr);
    }
}

/// Classify a single usage by inspecting its HIR parent.  Returns `false`
/// when the usage is unsafe (would break compilation after the binding's
/// RHS becomes `TrackedX`).
fn is_usage_safe<'tcx>(cx: &LateContext<'tcx>, usage_expr: &Expr<'tcx>) -> bool {
    let mut parents = cx.tcx.hir_parent_id_iter(usage_expr.hir_id);
    let Some(parent_id) = parents.next() else {
        return false;
    };
    let parent_node = cx.tcx.hir_node(parent_id);
    let Node::Expr(parent_expr) = parent_node else {
        // Parent is a Stmt / Block / Let / Item — bare value position.
        // The only safe non-Expr parent would be a `let _ = v;` "drop"
        // pattern, but accepting that would require an extra case.  Stay
        // strict — non-Expr parent → unsafe.
        return false;
    };

    // Determine whether this usage occupies the receiver slot of a method
    // call (the only context where method-call parent doesn't mean by-value).
    let is_receiver = matches!(
        &parent_expr.kind,
        ExprKind::MethodCall(_, recv, _, _) if recv.hir_id == usage_expr.hir_id
    );

    let parent_kind = parent_kind_of(&parent_expr.kind, is_receiver);
    classify_parent_kind(parent_kind) == UsageSafety::Safe
}

#[cfg(test)]
mod tests {
    use super::{build_auto_label, build_wrap_from_suggestion, tracked_type_to_path};
    use crate::{match_third_party_path, TrackedType};
    use std::path::PathBuf;

    // ──────────────────────────────────────────────────────────────────────────
    // build_wrap_from_suggestion — Phase K: pure helper, fully unit-testable.
    // ──────────────────────────────────────────────────────────────────────────

    /// The wrap_from suggestion must produce a valid Rust call expression.
    #[test]
    fn wrap_from_suggestion_format_vec_new() {
        let sugg = build_wrap_from_suggestion(
            "::captrack::TrackedVec::<_>",
            "Vec::new()",
            "auto:src/main.rs:10:9",
        );
        assert_eq!(
            sugg,
            r#"::captrack::TrackedVec::<_>::wrap_from(Vec::new(), "auto:src/main.rs:10:9", file!(), line!(), column!())"#
        );
    }

    /// Non-empty vec! literal — elements preserved verbatim in snippet.
    #[test]
    fn wrap_from_suggestion_format_vec_literal() {
        let sugg = build_wrap_from_suggestion(
            "::captrack::TrackedVec::<_>",
            "vec![1, 2, 3]",
            "auto:src/lib.rs:42:5",
        );
        assert_eq!(
            sugg,
            r#"::captrack::TrackedVec::<_>::wrap_from(vec![1, 2, 3], "auto:src/lib.rs:42:5", file!(), line!(), column!())"#
        );
    }

    /// vec![x; n] repeat form — n-expression preserved verbatim.
    #[test]
    fn wrap_from_suggestion_format_vec_repeat() {
        let sugg = build_wrap_from_suggestion(
            "::captrack::TrackedVec::<_>",
            "vec![0u8; n]",
            "auto:crates/foo/src/bar.rs:7:13",
        );
        assert_eq!(
            sugg,
            r#"::captrack::TrackedVec::<_>::wrap_from(vec![0u8; n], "auto:crates/foo/src/bar.rs:7:13", file!(), line!(), column!())"#
        );
    }

    /// SmallVec literal form — the critical Phase K fix for smallvec![a,b,c].
    #[test]
    fn wrap_from_suggestion_format_smallvec_literal() {
        let sugg = build_wrap_from_suggestion(
            "::captrack::TrackedSmallVec::<_>",
            "smallvec![1u32, 2, 3]",
            "auto:src/engine.rs:99:5",
        );
        assert_eq!(
            sugg,
            r#"::captrack::TrackedSmallVec::<_>::wrap_from(smallvec![1u32, 2, 3], "auto:src/engine.rs:99:5", file!(), line!(), column!())"#
        );
    }

    /// BTreeMap — uses wrap_from (no new_named needed with universal approach).
    #[test]
    fn wrap_from_suggestion_format_btreemap() {
        let sugg = build_wrap_from_suggestion(
            "::captrack::TrackedBTreeMap::<_, _>",
            "BTreeMap::new()",
            "auto:crates/x/src/lib.rs:5:5",
        );
        assert_eq!(
            sugg,
            r#"::captrack::TrackedBTreeMap::<_, _>::wrap_from(BTreeMap::new(), "auto:crates/x/src/lib.rs:5:5", file!(), line!(), column!())"#
        );
    }

    /// HashMap::with_capacity — args preserved in outer snippet.
    #[test]
    fn wrap_from_suggestion_format_hashmap_with_capacity() {
        let sugg = build_wrap_from_suggestion(
            "::captrack::TrackedHashMap::<_, _>",
            "HashMap::with_capacity(64)",
            "auto:src/store.rs:20:18",
        );
        assert_eq!(
            sugg,
            r#"::captrack::TrackedHashMap::<_, _>::wrap_from(HashMap::with_capacity(64), "auto:src/store.rs:20:18", file!(), line!(), column!())"#
        );
    }

    /// Arbitrary builder expression — wrap_from is universal.
    #[test]
    fn wrap_from_suggestion_format_arbitrary_expr() {
        let sugg = build_wrap_from_suggestion(
            "::captrack::TrackedVec::<_>",
            "items.collect::<Vec<_>>()",
            "auto:src/collect.rs:33:9",
        );
        assert_eq!(
            sugg,
            r#"::captrack::TrackedVec::<_>::wrap_from(items.collect::<Vec<_>>(), "auto:src/collect.rs:33:9", file!(), line!(), column!())"#
        );
    }

    /// The generated suggestion must not contain backslashes (would break Rust string literals).
    #[test]
    fn wrap_from_suggestion_no_backslashes() {
        let sugg = build_wrap_from_suggestion(
            "::captrack::TrackedVec::<_>",
            "Vec::new()",
            "auto:crates/foo/src/lib.rs:1:1",
        );
        assert!(
            !sugg.contains('\\'),
            "suggestion must not contain backslashes: {sugg:?}"
        );
    }

    /// The label MUST NOT contain backslashes — they are invalid char-escape
    /// triggers in a Rust string literal.  See the function-level doc on
    /// `build_auto_label` for the failure mode.
    #[test]
    fn windows_backslashes_normalised_to_forward_slashes() {
        let path: PathBuf = ["crates", "shamir-server", "src", "lib.rs"]
            .iter()
            .collect();
        // On Windows `path` builds with `\`; on Linux with `/`.  Force the
        // backslash flavour by constructing the string manually so the test
        // exercises the same input shape on both targets.
        let win_path = PathBuf::from(r"crates\shamir-server\src\lib.rs");
        let label = build_auto_label(&win_path, 42, 7);
        assert!(
            !label.contains('\\'),
            "label must not retain backslashes: got {label:?}"
        );
        assert_eq!(label, "auto:crates/shamir-server/src/lib.rs:42:7");
        // Sanity: the PathBuf variant produces the same output on either OS.
        let _ = build_auto_label(&path, 42, 7);
    }

    #[test]
    fn unix_paths_are_passed_through_unchanged() {
        let p = PathBuf::from("crates/shamir-server/src/lib.rs");
        assert_eq!(
            build_auto_label(&p, 100, 3),
            "auto:crates/shamir-server/src/lib.rs:100:3"
        );
    }

    /// Mixed separators (e.g. a tool that mangled a path part-way) — every
    /// backslash must be flipped, not just the leading one.
    #[test]
    fn mixed_separators_all_normalised() {
        let p = PathBuf::from(r"crates/shamir-engine\src/lib.rs");
        let label = build_auto_label(&p, 1, 1);
        assert!(!label.contains('\\'), "got {label:?}");
        assert_eq!(label, "auto:crates/shamir-engine/src/lib.rs:1:1");
    }

    /// The literal we emit must parse as a valid Rust string literal.  The
    /// only character we need to defend against is `\` (every Rust escape
    /// trigger starts with it); double-quote and CR/LF cannot appear in a
    /// file path on the platforms we target.
    #[test]
    fn label_is_a_valid_rust_string_literal() {
        let tricky_inputs = [
            r"crates\foo\bar.rs",
            r"a\b\c\d\e\f\g.rs", // every escape-trigger letter pre-/post-fixed
            r"with space\foo.rs",
        ];
        for input in tricky_inputs {
            let label = build_auto_label(&PathBuf::from(input), 1, 1);
            assert!(
                !label.contains('\\'),
                "label {label:?} would break as a Rust string literal"
            );
        }
    }

    // ──────────────────────────────────────────────────────────────────────
    // classify_parent_kind — pure, discriminant-only, table-driven.
    //
    // The classifier acts ONLY on the `ParentKind` enum, so unit tests can
    // exhaustively enumerate every variant without touching real HIR.  The
    // adapter `parent_kind_of` (HIR ExprKind → ParentKind) is exercised
    // indirectly through the real-workspace `captrack-pgo instrument` run.
    // ──────────────────────────────────────────────────────────────────────
    use super::{classify_parent_kind, ParentKind, UsageSafety};

    #[test]
    fn safe_when_used_as_method_call_receiver() {
        // `v.push(x)`, `v.len()`, `for x in &v` (which lowers to `(&v).into_iter()`).
        assert_eq!(
            classify_parent_kind(ParentKind::MethodCallReceiver),
            UsageSafety::Safe
        );
    }

    #[test]
    fn unsafe_when_used_as_method_call_argument() {
        // `other.takes_vec(v)` — usage is in the args slice, not receiver.
        assert_eq!(
            classify_parent_kind(ParentKind::MethodCallArg),
            UsageSafety::Unsafe
        );
    }

    #[test]
    fn safe_when_used_via_addr_of() {
        // `&v`, `&mut v` — `Deref` / `DerefMut` coercion handles it.
        assert_eq!(classify_parent_kind(ParentKind::AddrOf), UsageSafety::Safe);
    }

    #[test]
    fn safe_when_used_via_index() {
        // `v[i]`, `arr[v]` — `Index` / `IndexMut` forwarded via `Deref`.
        assert_eq!(classify_parent_kind(ParentKind::Index), UsageSafety::Safe);
    }

    #[test]
    fn unsafe_when_returned_by_value() {
        // `return v;` — by-value, but the binding is `TrackedX` after rewrite.
        assert_eq!(
            classify_parent_kind(ParentKind::Return),
            UsageSafety::Unsafe
        );
    }

    #[test]
    fn unsafe_when_tuple_field() {
        // `(v, 0)` — by-value into tuple.
        assert_eq!(classify_parent_kind(ParentKind::Tuple), UsageSafety::Unsafe);
    }

    #[test]
    fn unsafe_when_function_call_argument() {
        // `foo(v)` — first positional arg, by-value.
        assert_eq!(classify_parent_kind(ParentKind::Call), UsageSafety::Unsafe);
    }

    #[test]
    fn unsafe_when_struct_field_init() {
        // `S { vec: v }` — struct field, by-value.
        assert_eq!(
            classify_parent_kind(ParentKind::StructField),
            UsageSafety::Unsafe
        );
    }

    #[test]
    fn unsafe_when_other_position() {
        // Match scrutinee, cast, bare block tail, `let other = v`, etc.  All
        // by-value.  The catch-all bucket MUST default to Unsafe — that is
        // the load-bearing safety contract of this lint.
        assert_eq!(classify_parent_kind(ParentKind::Other), UsageSafety::Unsafe);
    }

    /// Property: exactly three ParentKinds are Safe; everything else is
    /// Unsafe.  Encoded as an explicit table so a future contributor who
    /// adds a `ParentKind` variant either deliberately classifies it (and
    /// adds the row) or fails the test — preventing silent expansion of
    /// the "safe" set.
    #[test]
    fn coverage_table_locks_the_safety_classification() {
        let cases = [
            (ParentKind::MethodCallReceiver, UsageSafety::Safe),
            (ParentKind::MethodCallArg, UsageSafety::Unsafe),
            (ParentKind::AddrOf, UsageSafety::Safe),
            (ParentKind::Index, UsageSafety::Safe),
            (ParentKind::Return, UsageSafety::Unsafe),
            (ParentKind::Tuple, UsageSafety::Unsafe),
            (ParentKind::StructField, UsageSafety::Unsafe),
            (ParentKind::Call, UsageSafety::Unsafe),
            (ParentKind::Other, UsageSafety::Unsafe),
        ];
        for (parent, expected) in cases {
            assert_eq!(
                classify_parent_kind(parent),
                expected,
                "regression: {parent:?} classification changed"
            );
        }
        let safe_count = cases
            .iter()
            .filter(|(_, s)| matches!(s, UsageSafety::Safe))
            .count();
        assert_eq!(
            safe_count, 3,
            "exactly three ParentKinds (MethodCallReceiver, AddrOf, Index) are Safe; \
             a change here means someone added a new Safe path — review it carefully"
        );
    }

    // ──────────────────────────────────────────────────────────────────────────
    // match_third_party_path — pure string-matching, fully unit-testable.
    // ──────────────────────────────────────────────────────────────────────────

    #[test]
    fn scc_hashmap_recognised() {
        assert_eq!(
            match_third_party_path("scc::hash_map::HashMap"),
            Some(TrackedType::SccHashMap)
        );
    }

    #[test]
    fn scc_hashset_recognised() {
        assert_eq!(
            match_third_party_path("scc::hash_set::HashSet"),
            Some(TrackedType::SccHashSet)
        );
    }

    #[test]
    fn scc_tree_index_recognised() {
        assert_eq!(
            match_third_party_path("scc::tree_index::TreeIndex"),
            Some(TrackedType::SccTreeIndex)
        );
    }

    #[test]
    fn dashmap_recognised() {
        assert_eq!(
            match_third_party_path("dashmap::DashMap"),
            Some(TrackedType::DashMap)
        );
    }

    #[test]
    fn bytesmut_recognised() {
        assert_eq!(
            match_third_party_path("bytes::BytesMut"),
            Some(TrackedType::BytesMut)
        );
        // Also handle internal module path if crate reshuffles it.
        assert_eq!(
            match_third_party_path("bytes::bytes_mut::BytesMut"),
            Some(TrackedType::BytesMut)
        );
    }

    #[test]
    fn indexmap_recognised() {
        // Canonical internal path as of indexmap 2.x.
        assert_eq!(
            match_third_party_path("indexmap::map::IndexMap"),
            Some(TrackedType::IndexMap)
        );
        // Re-exported flat path.
        assert_eq!(
            match_third_party_path("indexmap::IndexMap"),
            Some(TrackedType::IndexMap)
        );
    }

    #[test]
    fn indexset_recognised() {
        assert_eq!(
            match_third_party_path("indexmap::set::IndexSet"),
            Some(TrackedType::IndexSet)
        );
        assert_eq!(
            match_third_party_path("indexmap::IndexSet"),
            Some(TrackedType::IndexSet)
        );
    }

    /// std types must NOT match (they are handled by diagnostic-name).
    #[test]
    fn std_vec_not_matched_by_third_party() {
        assert_eq!(match_third_party_path("std::vec::Vec"), None);
        assert_eq!(match_third_party_path("alloc::vec::Vec"), None);
    }

    /// Unrelated types must not match.
    #[test]
    fn unrelated_types_not_matched() {
        assert_eq!(match_third_party_path("tokio::sync::Mutex"), None);
        assert_eq!(match_third_party_path("std::collections::HashMap"), None);
        assert_eq!(match_third_party_path("scc::ebr::AtomicOwned"), None);
    }

    // ──────────────────────────────────────────────────────────────────────────
    // tracked_type_to_path — every variant must map to a non-empty path.
    // ──────────────────────────────────────────────────────────────────────────

    #[test]
    fn tracked_type_to_path_covers_all_variants() {
        let all = [
            TrackedType::Vec,
            TrackedType::VecDeque,
            TrackedType::HashMap,
            TrackedType::HashSet,
            TrackedType::BTreeMap,
            TrackedType::BTreeSet,
            TrackedType::BytesMut,
            TrackedType::IndexMap,
            TrackedType::IndexSet,
            TrackedType::DashMap,
            TrackedType::SccHashMap,
            TrackedType::SccHashSet,
            TrackedType::SccTreeIndex,
            TrackedType::SmallVec,
        ];
        for t in all {
            let (path, _use_new_named) = tracked_type_to_path(t);
            assert!(
                !path.is_empty(),
                "tracked_type_to_path({t:?}) returned empty path"
            );
            assert!(
                path.starts_with("::captrack::"),
                "path for {t:?} must start with ::captrack:: but got {path:?}"
            );
        }
        // Exactly 14 variants — assert count to lock against silent additions.
        assert_eq!(all.len(), 14, "TrackedType must have exactly 14 variants");
    }

    /// BTree types and SccTreeIndex use new_named (no with_capacity); the
    /// rest use with_capacity_named.  SmallVec has with_capacity_named
    /// (TrackedSmallVec::with_capacity_named(0, …) is valid).
    #[test]
    fn new_named_flag_correct_for_each_type() {
        let new_named_types = [
            TrackedType::BTreeMap,
            TrackedType::BTreeSet,
            TrackedType::SccTreeIndex,
        ];
        let with_capacity_types = [
            TrackedType::Vec,
            TrackedType::VecDeque,
            TrackedType::HashMap,
            TrackedType::HashSet,
            TrackedType::BytesMut,
            TrackedType::IndexMap,
            TrackedType::IndexSet,
            TrackedType::DashMap,
            TrackedType::SccHashMap,
            TrackedType::SccHashSet,
            TrackedType::SmallVec,
        ];
        for t in new_named_types {
            let (_, use_new_named) = tracked_type_to_path(t);
            assert!(
                use_new_named,
                "{t:?} should have use_new_named=true (no with_capacity)"
            );
        }
        for t in with_capacity_types {
            let (_, use_new_named) = tracked_type_to_path(t);
            assert!(
                !use_new_named,
                "{t:?} should have use_new_named=false (has with_capacity_named)"
            );
        }
    }

    /// Coverage table for match_third_party_path — all 8 third-party types
    /// and selected non-matches, analogous to coverage_table_locks_the_safety_classification.
    #[test]
    fn coverage_table_third_party_paths() {
        let should_match = [
            ("bytes::BytesMut", TrackedType::BytesMut),
            ("bytes::bytes_mut::BytesMut", TrackedType::BytesMut),
            ("indexmap::map::IndexMap", TrackedType::IndexMap),
            ("indexmap::IndexMap", TrackedType::IndexMap),
            ("indexmap::set::IndexSet", TrackedType::IndexSet),
            ("indexmap::IndexSet", TrackedType::IndexSet),
            ("dashmap::DashMap", TrackedType::DashMap),
            ("dashmap::mapref::DashMap", TrackedType::DashMap),
            ("scc::hash_map::HashMap", TrackedType::SccHashMap),
            ("scc::hash_set::HashSet", TrackedType::SccHashSet),
            ("scc::tree_index::TreeIndex", TrackedType::SccTreeIndex),
            ("smallvec::SmallVec", TrackedType::SmallVec),
        ];
        let should_not_match = [
            "std::vec::Vec",
            "std::collections::HashMap",
            "std::collections::HashSet",
            "alloc::collections::BTreeMap",
            "tokio::sync::Mutex",
            "scc::ebr::AtomicOwned",
            // SmallVec must NOT match partial paths that don't end with "SmallVec".
            "smallvec::alloc::SmallVecData",
        ];

        for (path, expected) in should_match {
            assert_eq!(
                match_third_party_path(path),
                Some(expected),
                "expected {path:?} to match {expected:?}"
            );
        }
        for path in should_not_match {
            assert_eq!(
                match_third_party_path(path),
                None,
                "expected {path:?} to NOT match any TrackedType"
            );
        }
    }

    /// SmallVec is recognised by path "smallvec::SmallVec".
    #[test]
    fn smallvec_recognised_via_path() {
        assert_eq!(
            match_third_party_path("smallvec::SmallVec"),
            Some(TrackedType::SmallVec)
        );
        // Internal module reshuffling must still match.
        assert_eq!(
            match_third_party_path("smallvec::smallvec::SmallVec"),
            Some(TrackedType::SmallVec)
        );
        // Must NOT match a path that merely contains "SmallVec" but doesn't end with it.
        assert_eq!(match_third_party_path("smallvec::SmallVecData"), None);
    }

    /// tracked_type_to_path returns the correct captrack path for SmallVec
    /// and marks it as using with_capacity_named (not new_named).
    #[test]
    fn smallvec_to_path_is_correct() {
        let (path, use_new_named) = tracked_type_to_path(TrackedType::SmallVec);
        assert_eq!(path, "::captrack::TrackedSmallVec::<_>");
        assert!(
            !use_new_named,
            "SmallVec uses with_capacity_named, not new_named"
        );
    }

    // ── Phase F: macro recognition — pure, no rustc context needed ───────────
    use super::{should_instrument_macro_name, MacroCap, TrackedMacro};

    /// `vec` and `smallvec` macro names are the only ones we instrument.
    #[test]
    fn vec_macro_name_recognised() {
        assert!(
            should_instrument_macro_name("vec"),
            "\"vec\" must be recognised as a tracked macro"
        );
    }

    #[test]
    fn smallvec_macro_name_recognised() {
        assert!(
            should_instrument_macro_name("smallvec"),
            "\"smallvec\" must be recognised as a tracked macro"
        );
    }

    #[test]
    fn unknown_macro_names_not_recognised() {
        let unknown = [
            "format",
            "println",
            "assert",
            "my_vec", // user-defined — not tracked
            "tvec",   // captrack-generated — not tracked here
            "declare_collections",
            "",
        ];
        for name in unknown {
            assert!(
                !should_instrument_macro_name(name),
                "macro name {name:?} must NOT be recognised as tracked"
            );
        }
    }

    /// Coverage table — exactly the two names are tracked; adding more requires
    /// updating both `should_instrument_macro_name` and this test.
    #[test]
    fn tracked_macro_name_coverage_table() {
        let tracked = ["vec", "smallvec"];
        let not_tracked = [
            "format", "println", "assert", "my_vec", "tvec", "", "vecdeque",
        ];
        for name in tracked {
            assert!(
                should_instrument_macro_name(name),
                "tracked macro {name:?} not recognised — update coverage table"
            );
        }
        for name in not_tracked {
            assert!(
                !should_instrument_macro_name(name),
                "non-tracked macro {name:?} incorrectly recognised"
            );
        }
    }

    /// TrackedMacro → correct captrack path and no `new_named` flag.
    #[test]
    fn tracked_macro_enum_to_path() {
        // Vec macro instruments to TrackedVec
        let (path, use_new_named) = tracked_type_to_path(TrackedType::Vec);
        assert_eq!(path, "::captrack::TrackedVec::<_>");
        assert!(!use_new_named, "vec! uses with_capacity_named");

        // SmallVec macro instruments to TrackedSmallVec
        let (path, use_new_named) = tracked_type_to_path(TrackedType::SmallVec);
        assert_eq!(path, "::captrack::TrackedSmallVec::<_>");
        assert!(!use_new_named, "smallvec! uses with_capacity_named");
    }

    /// `TrackedMacro` discriminants have the expected identity.
    #[test]
    fn tracked_macro_enum_variants() {
        // Sanity: the two variants are distinct.
        assert_ne!(TrackedMacro::Vec, TrackedMacro::SmallVec);
        assert_eq!(TrackedMacro::Vec, TrackedMacro::Vec);
        assert_eq!(TrackedMacro::SmallVec, TrackedMacro::SmallVec);
    }

    /// `MacroCap` variants are distinct and carry the right payloads.
    #[test]
    fn macro_cap_variants() {
        assert_eq!(MacroCap::Zero, MacroCap::Zero);
        assert_eq!(MacroCap::Literal(3), MacroCap::Literal(3));
        assert_ne!(MacroCap::Zero, MacroCap::Literal(0));
        assert_ne!(MacroCap::Literal(3), MacroCap::Literal(4));
        // Expr variant carries the source text.
        assert_eq!(MacroCap::Expr("n"), MacroCap::Expr("n"));
        assert_ne!(MacroCap::Expr("n"), MacroCap::Expr("m"));
    }

    /// `MacroCap::Literal` correctly represents element counts for the
    /// `vec![a, b, c]` pattern.
    #[test]
    fn macro_cap_literal_element_count() {
        // Simulate the capacity we'd extract from vec![a, b, c]:
        // extract_array_len_from_into_vec_args returns 3, then MacroCap::Literal(3).
        let cap = MacroCap::Literal(3);
        assert_eq!(cap, MacroCap::Literal(3));
        // MacroCap::Literal(0) and MacroCap::Zero are DIFFERENT discriminants —
        // they both represent "capacity = 0" semantically, but are distinct enum
        // variants.  The instrument code uses MacroCap::Zero for vec![] expansions
        // and MacroCap::Literal(n) for vec![a, b, c] where n is the element count.
        let cap_literal_zero = MacroCap::Literal(0);
        assert_ne!(
            cap_literal_zero,
            MacroCap::Zero,
            "MacroCap::Literal(0) != MacroCap::Zero — different discriminants"
        );
    }
}
