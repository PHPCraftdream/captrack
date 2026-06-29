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
use rustc_hir::def::Res;
use rustc_hir::intravisit::{self, Visitor};
use rustc_hir::{Body, BodyId, Expr, ExprKind, HirId, Node, PatKind, QPath};
use rustc_lint::{LateContext, LateLintPass};
use rustc_middle::hir::nested_filter;
use rustc_middle::ty::TyKind;
use rustc_session::declare_lint;
use rustc_span::Symbol;

use crate::{is_tracked_method, recognise_tracked_type, span_to_site_key, sym_with_capacity_and_hasher, TrackedType};

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
fn check_and_instrument<'tcx>(
    cx: &LateContext<'tcx>,
    call_expr: &Expr<'tcx>,
    fn_expr: &Expr<'tcx>,
    qpath: &QPath<'tcx>,
    args: &'tcx [Expr<'tcx>],
) {
    let span = call_expr.span;

    // Skip macro-expanded sites (e.g. `vec![]`).
    //
    // TODO(vec!-macro): attempt to detect `vec![]` expansion via
    // `span.source_callsite()` and instrument the outer user-visible span.
    // The API needed (resolving `outer.ctxt().outer_expn_data().macro_def_id`
    // to a `def_path_str` of `"std::vec"`) is available on nightly but requires
    // careful lifetime handling across `ExpnData`.  Deferred to a future pass.
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

    let (tracked_path, use_new_named) = tracked_type_to_path(tracked_ty);

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
            tracked_type_display(tracked_ty)
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

    if method_name == sym::new || method_name == sym::Default {
        // `default()` (sym::Default = interned "default") is a zero-capacity
        // constructor equivalent to `new()`.  Instrument it identically:
        // emit with_capacity_named(0, …) so the profiling run records a site
        // with initial capacity = 0.
        if use_new_named {
            // BTree types: use new_named(cap_hint=0, name, file, line, col)
            Some(format!(
                "{tracked_path}::new_named(0, {name_arg}, {meta_args})"
            ))
        } else {
            // Vec, VecDeque, HashMap, HashSet, SmallVec: with_capacity_named(0, ...)
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
    use super::{build_auto_label, tracked_type_to_path};
    use crate::{match_third_party_path, TrackedType};
    use std::path::PathBuf;

    /// The label MUST NOT contain backslashes — they are invalid char-escape
    /// triggers in a Rust string literal.  See the function-level doc on
    /// `build_auto_label` for the failure mode.
    #[test]
    fn windows_backslashes_normalised_to_forward_slashes() {
        let path: PathBuf = ["crates", "shamir-server", "src", "lib.rs"].iter().collect();
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
        assert_eq!(
            match_third_party_path("smallvec::SmallVecData"),
            None
        );
    }

    /// tracked_type_to_path returns the correct captrack path for SmallVec
    /// and marks it as using with_capacity_named (not new_named).
    #[test]
    fn smallvec_to_path_is_correct() {
        let (path, use_new_named) = tracked_type_to_path(TrackedType::SmallVec);
        assert_eq!(path, "::captrack::TrackedSmallVec::<_>");
        assert!(!use_new_named, "SmallVec uses with_capacity_named, not new_named");
    }
}
