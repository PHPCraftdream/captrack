#![feature(rustc_private)]
#![warn(unused_extern_crates)]

extern crate rustc_ast;
extern crate rustc_errors;
extern crate rustc_hir;
extern crate rustc_middle;
extern crate rustc_span;

use std::path::PathBuf;
use std::sync::OnceLock;

use clippy_utils::diagnostics::{span_lint, span_lint_and_sugg, span_lint_and_then};
use clippy_utils::source::snippet_opt;
use clippy_utils::sym;
use rustc_ast::ast::LitKind;
use rustc_errors::Applicability;
use rustc_hir::{Expr, ExprKind, Node, QPath, TyKind as HirTyKind};
use rustc_lint::{LateContext, LateLintPass};
use rustc_middle::ty::{GenericArgsRef, TyKind};
use rustc_span::def_id::DefId;
use rustc_span::{FileName, Symbol};

pub mod instrument;
mod loader;
mod model;
mod rules;

use loader::Profile;
use model::{CapExpr, CapFrom, CapRound, Ctor, SiteKey};
use rules::{Decision, PolicyDefaults};

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
    /// Optionally set `CAPTRACK_PGO_HASHER=fx|ahash|foldhash` to also inject a
    /// hasher into `HashMap`/`HashSet` constructors.  Supported values:
    /// - `fx`       → `::fxhash::FxBuildHasher::default()`
    /// - `ahash`    → `::ahash::RandomState::new()`
    /// - `foldhash` → `::foldhash::fast::RandomState::default()`
    ///
    /// ### Hasher-injection and type ascriptions (Phase N)
    ///
    /// When `CAPTRACK_PGO_HASHER` is set, the lint handles explicit type
    /// ascriptions on `let` bindings intelligently:
    ///
    /// - `let m: HashMap<K, V> = HashMap::new()` — hasher omitted in ascription
    ///   → **multi-span suggestion**: extends the ascription to
    ///   `HashMap<K, V, FxBuildHasher>` and rewrites the ctor simultaneously.
    /// - `let m: HashMap<K, V, MyHasher> = HashMap::new()` — hasher pinned
    ///   → capacity-only rewrite (user's hasher is preserved).
    /// - `let m: HashMap = HashMap::new()` — no generics written
    ///   → capacity-only rewrite (insufficient information to safely add hasher).
    ///
    /// Sites in struct fields, function return types, or `const`/`static` items
    /// with explicit types are **not** detected and may produce compile errors if
    /// the suggestion is accepted.  Remember to add the chosen hasher crate to
    /// your `Cargo.toml`.
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

// ── TrackedType enum — full 14-type recognition ───────────────────────────────

/// All 14 collection types that captrack can track.
///
/// The first 6 are standard-library types recognised via `get_diagnostic_name`.
/// The remaining 8 are third-party types recognised via `def_path_str`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TrackedType {
    // std
    Vec,
    VecDeque,
    HashMap,
    HashSet,
    BTreeMap,
    BTreeSet,
    // third-party
    BytesMut,
    IndexMap,
    IndexSet,
    DashMap,
    SccHashMap,
    SccHashSet,
    SccTreeIndex,
    SmallVec,
}

/// Pure string-match helper for third-party paths.
///
/// Accepts the string produced by `cx.tcx.def_path_str(did)` and returns
/// the matching `TrackedType` if it is a third-party collection we track.
///
/// Std types (`std::vec::Vec`, etc.) are handled separately via
/// `get_diagnostic_name` — this function returns `None` for them so the
/// caller can fall through to the diagnostic-name branch.
///
/// The matching strategy is a **prefix + suffix** check so that internal
/// module reshuffling inside a crate (e.g. `indexmap::map::IndexMap` vs
/// `indexmap::IndexMap`) does not break recognition:
///
/// - path starts with `"bytes::"` AND contains `"BytesMut"` → BytesMut
/// - path starts with `"indexmap::"` AND contains `"::IndexMap"` → IndexMap
/// - path starts with `"indexmap::"` AND contains `"::IndexSet"` → IndexSet
/// - path starts with `"dashmap::"` AND contains `"DashMap"` → DashMap
/// - path starts with `"scc::"` AND contains `"::HashMap"` → SccHashMap
/// - path starts with `"scc::"` AND contains `"::HashSet"` → SccHashSet
/// - path starts with `"scc::"` AND (contains `"::TreeIndex"` OR ends with `"TreeIndex"`) → SccTreeIndex
/// - path starts with `"smallvec::"` AND ends with `"SmallVec"` → SmallVec
pub(crate) fn match_third_party_path(path: &str) -> Option<TrackedType> {
    if path.starts_with("bytes::") && path.contains("BytesMut") {
        return Some(TrackedType::BytesMut);
    }
    if path.starts_with("indexmap::") {
        if path.contains("IndexMap") && !path.contains("IndexSet") {
            return Some(TrackedType::IndexMap);
        }
        if path.contains("IndexSet") {
            return Some(TrackedType::IndexSet);
        }
    }
    if path.starts_with("dashmap::") && path.contains("DashMap") {
        return Some(TrackedType::DashMap);
    }
    if path.starts_with("scc::") {
        // Must test TreeIndex before HashMap/HashSet (no overlap, but be explicit).
        if path.contains("TreeIndex") {
            return Some(TrackedType::SccTreeIndex);
        }
        if path.contains("HashMap") {
            return Some(TrackedType::SccHashMap);
        }
        if path.contains("HashSet") {
            return Some(TrackedType::SccHashSet);
        }
    }
    if path.starts_with("smallvec::") && path.ends_with("SmallVec") {
        return Some(TrackedType::SmallVec);
    }
    None
}

/// Recognise a `DefId` as one of the 13 tracked collection types.
///
/// Two-step:
/// 1. Try `get_diagnostic_name` — fast, stable, covers the 6 std types.
/// 2. Fall back to `def_path_str` + `match_third_party_path` for the 7
///    third-party types.
pub(crate) fn recognise_tracked_type(cx: &LateContext<'_>, did: DefId) -> Option<TrackedType> {
    // Step 1 — diagnostic names for std types.
    if let Some(diag) = cx.tcx.get_diagnostic_name(did) {
        if diag == sym::Vec {
            return Some(TrackedType::Vec);
        }
        if diag == sym::VecDeque {
            return Some(TrackedType::VecDeque);
        }
        if diag == sym::HashMap {
            return Some(TrackedType::HashMap);
        }
        if diag == sym::HashSet {
            return Some(TrackedType::HashSet);
        }
        if diag == sym::BTreeMap {
            return Some(TrackedType::BTreeMap);
        }
        if diag == sym::BTreeSet {
            return Some(TrackedType::BTreeSet);
        }
    }
    // Step 2 — path-based recognition for third-party types.
    let path = cx.tcx.def_path_str(did);
    match_third_party_path(&path)
}

/// Map a `TrackedType` to the `Ctor` enum used by the capacity lint.
pub(crate) fn tracked_type_to_ctor(t: TrackedType) -> Ctor {
    match t {
        TrackedType::Vec => Ctor::Vec,
        TrackedType::VecDeque => Ctor::VecDeque,
        TrackedType::HashMap => Ctor::HashMap,
        TrackedType::HashSet => Ctor::HashSet,
        TrackedType::BTreeMap => Ctor::BTreeMap,
        TrackedType::BTreeSet => Ctor::BTreeSet,
        TrackedType::BytesMut => Ctor::BytesMut,
        TrackedType::IndexMap => Ctor::IndexMap,
        TrackedType::IndexSet => Ctor::IndexSet,
        TrackedType::DashMap => Ctor::DashMap,
        TrackedType::SccHashMap => Ctor::SccHashMap,
        TrackedType::SccHashSet => Ctor::SccHashSet,
        TrackedType::SccTreeIndex => Ctor::SccTreeIndex,
        TrackedType::SmallVec => Ctor::SmallVec,
    }
}

/// The `with_capacity_and_hasher` symbol — not in rustc predefined set, so we
/// intern it on first use.  We use a `std::sync::OnceLock` so we don't call
/// `Symbol::intern` in a hot path.
pub(crate) fn sym_with_capacity_and_hasher() -> Symbol {
    use std::sync::OnceLock;
    static SYM: OnceLock<Symbol> = OnceLock::new();
    *SYM.get_or_init(|| Symbol::intern("with_capacity_and_hasher"))
}

/// Return true if `name` is one of the constructor method names we track.
///
/// `sym::Default` (the interned string `"default"`) is included because
/// `Vec::default()` / `HashMap::default()` are zero-capacity constructors
/// equivalent to `new()`.  The capacity lint handles them the same as `new()`
/// (capacity = 0).
pub(crate) fn is_tracked_method(name: Symbol) -> bool {
    name == sym::new
        || name == sym::with_capacity
        || name == sym_with_capacity_and_hasher()
        || name == sym::Default
}

// ── Hasher choice (M9) ───────────────────────────────────────────────────────

/// Which hasher the user wants injected via `--hasher`.
///
/// Read from `CAPTRACK_PGO_HASHER` env var once per plugin load.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HasherChoice {
    /// `::fxhash::FxBuildHasher`
    Fx,
    /// `::ahash::RandomState`
    AHash,
    /// `::foldhash::fast::RandomState`
    FoldHash,
}

impl HasherChoice {
    /// Default-constructor expression for use in `with_capacity_and_hasher(N, <expr>)`.
    pub(crate) fn default_expr(self) -> &'static str {
        match self {
            HasherChoice::Fx => "::fxhash::FxBuildHasher::default()",
            HasherChoice::AHash => "::ahash::RandomState::new()",
            HasherChoice::FoldHash => "::foldhash::fast::RandomState::default()",
        }
    }

    /// Fully-qualified type path for use as a generic type argument in an
    /// ascription, e.g. `HashMap<K, V, ::fxhash::FxBuildHasher>`.
    ///
    /// Note: this is the *type* (not the value constructor) — it must be a
    /// valid type expression in angle brackets.
    pub(crate) fn hasher_type_path(self) -> &'static str {
        match self {
            HasherChoice::Fx => "::fxhash::FxBuildHasher",
            HasherChoice::AHash => "::ahash::RandomState",
            HasherChoice::FoldHash => "::foldhash::fast::RandomState",
        }
    }
}

/// Read `CAPTRACK_PGO_HASHER` once and cache the result.
///
/// Returns `None` when the env var is unset or set to "none" (default
/// behaviour — capacity-only rewrite, no hasher change).
///
/// Unknown values produce an `eprintln!` warning and fall back to `None`.
pub(crate) fn read_hasher_choice() -> Option<HasherChoice> {
    static CHOICE: OnceLock<Option<HasherChoice>> = OnceLock::new();
    *CHOICE.get_or_init(|| {
        let val = match std::env::var("CAPTRACK_PGO_HASHER") {
            Ok(v) if !v.is_empty() => v,
            _ => return None,
        };
        match val.trim().to_ascii_lowercase().as_str() {
            "none" | "" => None,
            "fx" => Some(HasherChoice::Fx),
            "ahash" => Some(HasherChoice::AHash),
            "foldhash" => Some(HasherChoice::FoldHash),
            other => {
                eprintln!(
                    "captrack-pgo-lint: unknown CAPTRACK_PGO_HASHER value {:?}; \
                     known values: fx, ahash, foldhash, none — treating as none",
                    other
                );
                None
            }
        }
    })
}

// ── Capacity policy env-var readers (M11) ────────────────────────────────────

/// Read `CAPTRACK_PGO_CAP_FROM` once and cache the result.
///
/// Accepted values (case-insensitive): `max`, `mean`, `median`, `p95`, `p99`.
/// Default: `P95`.  Unknown values warn to stderr and fall back to the default.
pub(crate) fn read_cap_from() -> CapFrom {
    static CHOICE: OnceLock<CapFrom> = OnceLock::new();
    *CHOICE.get_or_init(|| {
        let val = match std::env::var("CAPTRACK_PGO_CAP_FROM") {
            Ok(v) if !v.is_empty() => v,
            _ => return CapFrom::P95,
        };
        match val.trim().to_ascii_lowercase().as_str() {
            "max" => CapFrom::Max,
            "mean" => CapFrom::Mean,
            "median" => CapFrom::Median,
            "p95" => CapFrom::P95,
            "p99" => CapFrom::P99,
            other => {
                eprintln!(
                    "captrack-pgo-lint: unknown CAPTRACK_PGO_CAP_FROM value {:?}; \
                     known values: max, mean, median, p95, p99 — using p95",
                    other
                );
                CapFrom::P95
            }
        }
    })
}

/// Read `CAPTRACK_PGO_CAP_MUL` once and cache the result.
///
/// Must be a positive finite float.  Default: `1.0`.  Invalid or non-positive
/// values warn to stderr and fall back to `1.0`.
pub(crate) fn read_cap_mul() -> f64 {
    static CHOICE: OnceLock<f64> = OnceLock::new();
    *CHOICE.get_or_init(|| {
        let val = match std::env::var("CAPTRACK_PGO_CAP_MUL") {
            Ok(v) if !v.is_empty() => v,
            _ => return 1.0,
        };
        match val.trim().parse::<f64>() {
            Ok(v) if v > 0.0 && v.is_finite() => v,
            Ok(v) => {
                eprintln!(
                    "captrack-pgo-lint: CAPTRACK_PGO_CAP_MUL value {:?} must be > 0 and finite — using 1.0",
                    v
                );
                1.0
            }
            Err(_) => {
                eprintln!(
                    "captrack-pgo-lint: could not parse CAPTRACK_PGO_CAP_MUL {:?} as f64 — using 1.0",
                    val.trim()
                );
                1.0
            }
        }
    })
}

/// Read `CAPTRACK_PGO_CAP_ROUND` once and cache the result.
///
/// Accepted values (case-insensitive): `pow2`, `to8`, `exact`.
/// Default: `Pow2`.  Unknown values warn to stderr and fall back to the default.
pub(crate) fn read_cap_round() -> CapRound {
    static CHOICE: OnceLock<CapRound> = OnceLock::new();
    *CHOICE.get_or_init(|| {
        let val = match std::env::var("CAPTRACK_PGO_CAP_ROUND") {
            Ok(v) if !v.is_empty() => v,
            _ => return CapRound::Pow2,
        };
        match val.trim().to_ascii_lowercase().as_str() {
            "pow2" => CapRound::Pow2,
            "to8" => CapRound::To8,
            "exact" => CapRound::Exact,
            other => {
                eprintln!(
                    "captrack-pgo-lint: unknown CAPTRACK_PGO_CAP_ROUND value {:?}; \
                     known values: pow2, to8, exact — using pow2",
                    other
                );
                CapRound::Pow2
            }
        }
    })
}

// ── Phase O: HasherKind — detect already-fast hashers ────────────────────────

/// Set of path substrings (or prefixes) that identify already-fast hashers.
///
/// Matched via `contains` on the source snippet of the hasher type argument.
/// Ordering does not matter — each path is distinct.
const FAST_HASHER_SNIPPETS: &[&str] = &[
    // fxhash crate
    "fxhash::FxBuildHasher",
    "FxBuildHasher",
    // rustc_hash crate
    "rustc_hash::",
    // ahash crate (its `RandomState` is fast; disambiguation: `ahash::` prefix required)
    "ahash::",
    // foldhash crate
    "foldhash::",
    // BuildHasherDefault<FxHasher> — catches THasher alias form
    "FxHasher",
];

/// Categorise how fast (or slow) an already-pinned hasher type is.
///
/// Used by the `HasherPinned` branch in `emit_with_suggestion` to decide
/// whether to emit a "already fast, skipping swap" message or a generic
/// "user pinned hasher, skipping swap" message.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub(crate) enum HasherKind {
    /// `std::collections::hash_map::RandomState` or bare `RandomState`.
    /// This is the **slow** DOS-safe default — swap would be useful, but
    /// the user explicitly wrote it.  We still skip the swap (user choice),
    /// but the label mentions the slow hasher.
    SlowDefault,
    /// `fxhash::FxBuildHasher`, `rustc_hash::FxBuildHasher`,
    /// `ahash::RandomState`, `foldhash::fast::RandomState`,
    /// `BuildHasherDefault<FxHasher>` (THasher form).
    /// Already a fast hasher — skip swap, emit an "already fast" label.
    FastKnown,
    /// Custom / unrecognised hasher type path.  Conservatively skip swap.
    Unknown,
}

/// Classify the hasher type from a **source snippet** of the type argument.
///
/// This is a pure string-match function (no HIR context needed) so it is
/// fully unit-testable.  It is called with the snippet of the hasher's type
/// argument extracted from the `let` binding's explicit type ascription.
///
/// Decision order:
/// 1. If the snippet contains any of `FAST_HASHER_SNIPPETS` → `FastKnown`.
/// 2. If the snippet contains `"RandomState"` (without a fast-hasher prefix
///    such as `"ahash"` or `"foldhash"`) → `SlowDefault`.
/// 3. Otherwise → `Unknown`.
///
/// Note: `ahash::RandomState` hits rule 1 first (contains `"ahash::"`),
/// so it is classified as `FastKnown`, not `SlowDefault`.
pub(crate) fn classify_hasher_snippet(snippet: &str) -> HasherKind {
    let s = snippet.trim();
    // Rule 1: known fast hashers (checked first so ahash::RandomState
    // does not fall through to the RandomState slow-default rule).
    for &fast in FAST_HASHER_SNIPPETS {
        if s.contains(fast) {
            return HasherKind::FastKnown;
        }
    }
    // Rule 2: bare RandomState (std's slow default).
    if s.contains("RandomState") {
        return HasherKind::SlowDefault;
    }
    // Rule 3: unrecognised custom hasher.
    HasherKind::Unknown
}

/// Extract the source snippet for the **hasher type argument** from a HIR
/// type that has been classified as `AscriptionForm::HasherPinned`.
///
/// For `HashMap<K, V, S>` the hasher is the last (3rd) non-lifetime generic
/// arg.  For `HashSet<T, S>` it is the last (2nd) non-lifetime generic arg.
///
/// Returns `None` when:
/// - The HIR type is not a path type.
/// - There are no generic args (unexpected — callers should have verified
///   `HasherPinned` first).
/// - `snippet_opt` fails (virtual or remapped source files).
fn extract_hasher_arg_snippet<'tcx>(
    cx: &LateContext<'tcx>,
    hir_ty: &rustc_hir::Ty<'tcx>,
) -> Option<String> {
    let HirTyKind::Path(qpath) = &hir_ty.kind else {
        return None;
    };
    let last_segment = match qpath {
        QPath::Resolved(_, path) => path.segments.last()?,
        QPath::TypeRelative(_, segment) => Some(*segment)?,
    };
    let generic_args = last_segment.args?;
    // The hasher is the **last** non-lifetime type arg.
    let last_type_arg = generic_args
        .args
        .iter()
        .filter(|a| !matches!(a, rustc_hir::GenericArg::Lifetime(_)))
        .last()?;
    if let rustc_hir::GenericArg::Type(ty_ref) = last_type_arg {
        snippet_opt(cx, ty_ref.span)
    } else {
        None
    }
}

// ─────────────────────────────────────────────────────────────────────────────

/// How the type ascription in a `let` binding specifies (or omits) the hasher
/// parameter for a hash-keyed collection.
///
/// Used by `classify_type_ascription_hir` to decide whether Phase N can emit a
/// multi-span rewrite that extends the ascription with a hasher type argument.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub(crate) enum AscriptionForm {
    /// `HashMap<K, V>` — hasher omitted (defaults to `RandomState`).  We CAN
    /// rewrite to `HashMap<K, V, ::fxhash::FxBuildHasher>` safely.
    HasherOmitted,
    /// `HashMap<K, V, MyHasher>` — hasher pinned by the user.  Skip swap.
    HasherPinned,
    /// `HashMap` (no generics written at all) — not enough information to
    /// safely insert a hasher.  Skip.
    Wildcard,
    /// Not a hashing type (Vec, BytesMut, …) or unrecognised generic count.
    NotApplicable,
}

/// How many generic args a hashing collection requires when the hasher is
/// omitted vs when it is specified (by-value comparison, not HIR inspection).
///
/// Returns `(args_without_hasher, args_with_hasher)` for the given
/// `TrackedType`, or `None` for non-hashing types.
pub(crate) fn hasher_arg_counts(t: TrackedType) -> Option<(usize, usize)> {
    match t {
        // HashMap<K, V>     — 2 without hasher, 3 with hasher
        TrackedType::HashMap => Some((2, 3)),
        // HashSet<T>        — 1 without hasher, 2 with hasher
        TrackedType::HashSet => Some((1, 2)),
        // IndexMap<K, V, S> — 2 without hasher, 3 with hasher
        TrackedType::IndexMap => Some((2, 3)),
        // IndexSet<T, S>    — 1 without hasher, 2 with hasher
        TrackedType::IndexSet => Some((1, 2)),
        // DashMap<K, V, S>  — 2 without hasher, 3 with hasher
        TrackedType::DashMap => Some((2, 3)),
        // scc::HashMap<K, V, S> — 2 without hasher, 3 with hasher
        TrackedType::SccHashMap => Some((2, 3)),
        // scc::HashSet<T, S>    — 1 without hasher, 2 with hasher
        TrackedType::SccHashSet => Some((1, 2)),
        _ => None,
    }
}

/// Inspect the HIR type node from a `let` binding's explicit annotation and
/// classify whether the hasher parameter is omitted, pinned, or absent.
///
/// Called only when the `let` has an explicit type (`local.ty.is_some()`).
///
/// The function walks the type's HIR structure to find the last path segment
/// and counts how many generic args are written:
///
/// - 0 written args   → `Wildcard` (e.g. `let m: HashMap = …`)
/// - args_without == N → `HasherOmitted` (e.g. `HashMap<K, V>` for HashMap)
/// - args_with    == N → `HasherPinned`  (e.g. `HashMap<K, V, S>` for HashMap)
/// - anything else    → `NotApplicable`
pub(crate) fn classify_type_ascription_hir<'tcx>(
    hir_ty: &rustc_hir::Ty<'tcx>,
    tracked: TrackedType,
) -> AscriptionForm {
    let Some((without_hasher, with_hasher)) = hasher_arg_counts(tracked) else {
        return AscriptionForm::NotApplicable;
    };

    // The type must be a path type (TyKind::Path).
    let HirTyKind::Path(qpath) = &hir_ty.kind else {
        return AscriptionForm::NotApplicable;
    };

    // Extract the last path segment to get the generic args.
    let last_segment = match qpath {
        QPath::Resolved(_, path) => path.segments.last(),
        QPath::TypeRelative(_, segment) => Some(*segment),
    };

    let Some(segment) = last_segment else {
        return AscriptionForm::NotApplicable;
    };

    // Count the written generic args (skip lifetime args — they don't count
    // toward the K/V/S hasher parameter position).
    let n_args = match segment.args {
        None => 0,
        Some(generic_args) => generic_args
            .args
            .iter()
            .filter(|a| !matches!(a, rustc_hir::GenericArg::Lifetime(_)))
            .count(),
    };

    if n_args == 0 {
        AscriptionForm::Wildcard
    } else if n_args == without_hasher {
        AscriptionForm::HasherOmitted
    } else if n_args == with_hasher {
        AscriptionForm::HasherPinned
    } else {
        AscriptionForm::NotApplicable
    }
}

/// Determine whether the enclosing `let` binding has an explicit type
/// ascription that resolves to `HashMap` or `HashSet`.
///
/// When `true`, changing `HashMap::new()` to
/// `HashMap::with_capacity_and_hasher(N, H)` would produce a type-mismatch
/// because the ascription `HashMap<K, V>` defaults `S = RandomState` while
/// the RHS would be `HashMap<K, V, H>`.  We skip the hasher rewrite in that
/// case (the capacity rewrite is still emitted).
///
/// ## What we detect
///
/// Walk up the HIR parent chain looking for the immediately enclosing node.
/// If that node is a `Local` (let binding) **and** the local has an explicit
/// type annotation (`.ty.is_some()`), we return `true` → skip.
///
/// ## Known false-negative cases
///
/// - Struct fields with explicit type: `struct S { m: HashMap<K,V> }` — the
///   parent is a `Field`, not a `Local`; we do NOT detect this.  The user may
///   get a compile error if they accept the hasher suggestion in such a context.
/// - Function return types, `const`/`static` items — similarly not detected.
///
/// For M9, only the local-binding case is handled programmatically.
fn has_local_type_ascription<'tcx>(cx: &LateContext<'tcx>, call_expr: &Expr<'tcx>) -> bool {
    // Walk up parent nodes. The immediate parent of the call_expr in a
    // `let x: T = call_expr;` is an `Expr` wrapper (usually the Init expr),
    // but the HIR parent chain gives us the enclosing node.
    //
    // Layout in HIR for `let _m: HashMap<u8,u8> = HashMap::new();`:
    //   Local { ty: Some(...), init: Some(call_expr), ... }
    //
    // The call_expr's parent HirId points directly to the Local node.
    //
    // In nightly-2026-04-16, `tcx.hir()` is gone; use `tcx.hir_parent_id_iter`
    // and `tcx.hir_node(id)` instead.
    let call_hir_id = call_expr.hir_id;
    // Take only the first parent (immediate enclosing node).
    if let Some(parent_id) = cx.tcx.hir_parent_id_iter(call_hir_id).next() {
        match cx.tcx.hir_node(parent_id) {
            Node::LetStmt(local) => {
                // A `let` binding: check if there's an explicit type annotation.
                local.ty.is_some()
            }
            _ => false,
        }
    } else {
        false
    }
}

/// Retrieve the HIR type from the enclosing `let` binding's type annotation,
/// together with the ascription type node itself.
///
/// Returns `Some((&Ty, AscriptionForm))` when:
/// - The immediate parent of `call_expr` is a `LetStmt` with an explicit type.
/// - The `AscriptionForm` is determined by `classify_type_ascription_hir`.
///
/// Returns `None` when there is no enclosing `let`, or no explicit type.
fn get_let_ascription<'tcx>(
    cx: &LateContext<'tcx>,
    call_expr: &Expr<'tcx>,
    tracked: TrackedType,
) -> Option<(&'tcx rustc_hir::Ty<'tcx>, AscriptionForm)> {
    let parent_id = cx.tcx.hir_parent_id_iter(call_expr.hir_id).next()?;
    if let Node::LetStmt(local) = cx.tcx.hir_node(parent_id) {
        let ty = local.ty?;
        let form = classify_type_ascription_hir(ty, tracked);
        Some((ty, form))
    } else {
        None
    }
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
                    if recognise_tracked_type(cx, adt_def.did()).is_some() {
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
                if let Some(tracked_ty) = recognise_tracked_type(cx, adt_def.did()) {
                    let ctor = tracked_type_to_ctor(tracked_ty);
                    let cap_expr = extract_cap_expr(cx, method_name, args);
                    // Build PolicyDefaults once per call-site (OnceLock
                    // ensures env vars are read at most once per process).
                    let policy_defaults = PolicyDefaults {
                        cap_from: read_cap_from(),
                        cap_mul: read_cap_mul(),
                        cap_round: read_cap_round(),
                    };
                    emit_with_suggestion(
                        cx,
                        call_expr,
                        fn_expr,
                        ctor,
                        tracked_ty,
                        method_name,
                        args,
                        &cap_expr,
                        span,
                        profile,
                        read_hasher_choice(),
                        policy_defaults,
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
    if let TyKind::Adt(adt_def, generic_args) = ret_ty.kind() {
        if let Some(tracked) = recognise_tracked_type(cx, adt_def.did()) {
            let policy_defaults = PolicyDefaults {
                cap_from: read_cap_from(),
                cap_mul: read_cap_mul(),
                cap_round: read_cap_round(),
            };
            emit_with_default_dispatch_suggestion(
                cx,
                call_expr,
                tracked,
                adt_def.did(),
                generic_args,
                span,
                profile,
                read_hasher_choice(),
                policy_defaults,
            );
        }
    }
}

/// Determine the current `CapExpr` from the method name and argument list.
fn extract_cap_expr<'tcx>(
    cx: &LateContext<'tcx>,
    method_name: Symbol,
    args: &'tcx [Expr<'tcx>],
) -> CapExpr {
    // `new()` and `default()` are zero-capacity constructors.
    // `sym::Default` is the interned string "default" — the method name from HIR.
    if method_name == sym::new || method_name == sym::Default {
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

/// Map a `TrackedType` to its static fully-qualified type path prefix for use
/// in a `::with_capacity(N)` or `::with_capacity_and_hasher(N, H)` suggestion.
///
/// Returns `None` for types that have no `with_capacity` constructor
/// (`BTreeMap`, `BTreeSet`, `SccTreeIndex`) — callers must fall back to
/// warning-only for these.
///
/// The returned path does NOT include generic arguments; Rust's type inference
/// fills them in from the surrounding context (e.g. a `let` type ascription or
/// the site's inferred type).
pub(crate) fn tracked_type_to_static_path(t: TrackedType) -> Option<&'static str> {
    match t {
        TrackedType::Vec => Some("::std::vec::Vec"),
        TrackedType::VecDeque => Some("::std::collections::VecDeque"),
        TrackedType::HashMap => Some("::std::collections::HashMap"),
        TrackedType::HashSet => Some("::std::collections::HashSet"),
        // BTreeMap / BTreeSet / SccTreeIndex have no with_capacity.
        TrackedType::BTreeMap | TrackedType::BTreeSet | TrackedType::SccTreeIndex => None,
        TrackedType::BytesMut => Some("::bytes::BytesMut"),
        TrackedType::IndexMap => Some("::indexmap::IndexMap"),
        TrackedType::IndexSet => Some("::indexmap::IndexSet"),
        TrackedType::DashMap => Some("::dashmap::DashMap"),
        TrackedType::SccHashMap => Some("::scc::HashMap"),
        TrackedType::SccHashSet => Some("::scc::HashSet"),
        // SmallVec::with_capacity() exists — use the static prefix.
        // NOTE: SmallVec is generic over the array type `A`; without explicit
        // turbofish the compiler infers `A` from context (type ascription or
        // usage).  The suggestion is still MachineApplicable only when the
        // target binding has an explicit type annotation.
        TrackedType::SmallVec => Some("::smallvec::SmallVec"),
    }
}

/// Return `true` if the type supports `with_capacity_and_hasher` (and thus
/// can accept a hasher injection from `CAPTRACK_PGO_HASHER`).
pub(crate) fn tracked_type_supports_hasher(t: TrackedType) -> bool {
    matches!(
        t,
        TrackedType::HashMap
            | TrackedType::HashSet
            | TrackedType::IndexMap
            | TrackedType::IndexSet
            | TrackedType::DashMap
            | TrackedType::SccHashMap
            | TrackedType::SccHashSet
    )
}

/// Emit a lint warning with a MachineApplicable suggestion for
/// `Default::default()` and other Strategy-B call sites (return-type dispatch).
///
/// Decision path:
/// - BTreeMap / BTreeSet / SccTreeIndex → no `with_capacity` → warning only.
/// - All other TrackedTypes → compute capacity via `propose_cap` and emit a
///   `span_lint_and_sugg` with the static qualified path.
///
/// Hasher injection:
/// - When `hasher` is `Some(H)` and the type supports `with_capacity_and_hasher`,
///   the suggestion uses `with_capacity_and_hasher(N, H::default())`.
/// - When the enclosing `let` binding has an explicit type ascription that
///   names a plain HashMap/HashSet (no third-party hasher), the hasher swap is
///   skipped and only the capacity is updated (same guard as Strategy A).
///
/// The suggestion uses a generic-free path (e.g. `::std::vec::Vec`) and
/// relies on Rust's type inference to fill in the generic arguments from the
/// surrounding context (type ascription on the `let` binding, or usage
/// patterns elsewhere).  This is the "alternative" approach described in the
/// Phase-G brief; `def_path_str_with_args` would produce the same result but
/// is unnecessarily complex for this use-case.
#[allow(clippy::too_many_arguments)]
fn emit_with_default_dispatch_suggestion<'tcx>(
    cx: &LateContext<'tcx>,
    call_expr: &Expr<'tcx>,
    tracked: TrackedType,
    _adt_did: DefId,
    _generic_args: GenericArgsRef<'tcx>,
    span: rustc_span::Span,
    profile: &Profile,
    hasher: Option<HasherChoice>,
    policy_defaults: PolicyDefaults,
) {
    let key = span_to_site_key(cx, span);
    let Some(stats) = profile.get(&key) else {
        return;
    };

    let unit_str = match stats.unit {
        model::Unit::Elements => "elements",
        model::Unit::Bytes => "bytes",
    };

    // BTreeMap / BTreeSet / SccTreeIndex: no with_capacity — warn only.
    let Some(type_path) = tracked_type_to_static_path(tracked) else {
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
    };

    // `Default::default()` has no capacity argument — the current cap is Zero.
    let cap_expr = CapExpr::Zero;
    let decision = rules::propose_cap(stats, &cap_expr, policy_defaults);

    match decision {
        Decision::Skip { reason } => {
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
            let base_applicability = match rules_applicability {
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

            // Determine hasher injection eligibility.
            let inject_hasher = hasher.filter(|_| tracked_type_supports_hasher(tracked));

            let (sugg, applicability, help_label) = if let Some(h) = inject_hasher {
                // Check for explicit local type ascription that would block
                // inference of the new hasher type parameter.
                let ascription_detected = has_local_type_ascription(cx, call_expr);
                if ascription_detected {
                    // Skip hasher swap; capacity-only suggestion.
                    let cap_sugg = format!("{type_path}::with_capacity({to})");
                    let label = format!(
                        "use with_capacity({to}) (skipping hasher swap — explicit type \
                         ascription would prevent inference)"
                    );
                    (cap_sugg, base_applicability, label)
                } else {
                    // Safe to inject hasher.
                    let sugg = format!(
                        "{type_path}::with_capacity_and_hasher({to}, {})",
                        h.default_expr()
                    );
                    let label = format!("use with_capacity_and_hasher({to}, {})", h.default_expr());
                    (sugg, base_applicability, label)
                }
            } else {
                // Capacity-only path.
                let sugg = format!("{type_path}::with_capacity({to})");
                let label = format!("use with_capacity({to})");
                (sugg, base_applicability, label)
            };

            span_lint_and_sugg(
                cx,
                CAPTRACK_PGO_CAPACITY,
                span,
                msg,
                help_label,
                sugg,
                applicability,
            );
        }
    }
}

/// Emit a lint warning only (no suggestion).
///
/// Used for: method-call form (deferred) and macro-expanded sites.
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
///
/// Phase N: when `hasher` is `Some(H)` and the enclosing `let` has a type
/// ascription with the hasher **omitted** (e.g. `let m: HashMap<K, V> = …`),
/// emits a **multi-span** suggestion that simultaneously:
/// - Extends the ascription: `HashMap<K, V>` → `HashMap<K, V, ::fxhash::FxBuildHasher>`
/// - Rewrites the ctor: `HashMap::new()` → `HashMap::with_capacity_and_hasher(N, …)`
///
/// When the ascription already pins the hasher (`HasherPinned`) or has no
/// generics at all (`Wildcard`), falls back to the capacity-only rewrite.
#[allow(clippy::too_many_arguments)]
fn emit_with_suggestion<'tcx>(
    cx: &LateContext<'tcx>,
    call_expr: &Expr<'tcx>,
    fn_expr: &Expr<'tcx>,
    ctor: Ctor,
    tracked_ty: TrackedType,
    method_name: Symbol,
    args: &'tcx [Expr<'tcx>],
    cap_expr: &CapExpr,
    span: rustc_span::Span,
    profile: &Profile,
    hasher: Option<HasherChoice>,
    policy_defaults: PolicyDefaults,
) {
    let key = span_to_site_key(cx, span);
    let Some(stats) = profile.get(&key) else {
        return;
    };

    let unit_str = match stats.unit {
        model::Unit::Elements => "elements",
        model::Unit::Bytes => "bytes",
    };

    // BTreeMap / BTreeSet / SccTreeIndex have no with_capacity — warn only.
    if matches!(ctor, Ctor::BTreeMap | Ctor::BTreeSet | Ctor::SccTreeIndex) {
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
    let decision = rules::propose_cap(stats, cap_expr, policy_defaults);

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
            let base_applicability = match rules_applicability {
                rules::Applicability::MachineApplicable => Applicability::MachineApplicable,
                rules::Applicability::MaybeIncorrect => Applicability::MaybeIncorrect,
            };

            // Determine whether we should also inject a new hasher.
            // HashMap, HashSet, IndexMap, IndexSet, DashMap, SccHashMap, SccHashSet all
            // support `with_capacity_and_hasher`.
            let inject_hasher = hasher.filter(|_| {
                matches!(
                    ctor,
                    Ctor::HashMap
                        | Ctor::HashSet
                        | Ctor::IndexMap
                        | Ctor::IndexSet
                        | Ctor::DashMap
                        | Ctor::SccHashMap
                        | Ctor::SccHashSet
                )
            });

            let msg = format!(
                "captrack-pgo: profile shows peak={peak} {unit}, p95={p95}, count={count} \
                 — pre-allocate to {to}",
                peak = stats.peak,
                unit = unit_str,
                p95 = stats.p95,
                count = stats.count,
                to = to,
            );

            if let Some(h) = inject_hasher {
                // Check for explicit local type ascription.
                if let Some((hir_ty, form)) = get_let_ascription(cx, call_expr, tracked_ty) {
                    match form {
                        AscriptionForm::HasherOmitted => {
                            // Phase N: multi-span suggestion — extend ascription AND
                            // rewrite ctor simultaneously.
                            //
                            // Span A: the generic-args clause in the ascription.
                            // Span B: the ctor call expression.
                            if let Some(ctor_sugg) =
                                build_suggestion(cx, fn_expr, method_name, args, to, Some(h))
                            {
                                if let Some(new_ty_snip) =
                                    build_ascription_with_hasher(cx, hir_ty, h)
                                {
                                    // The ascription span covers the whole type node
                                    // (e.g. `HashMap<K, V>`).  We replace the full type
                                    // text with the new type including the hasher param.
                                    let asc_span = hir_ty.span;
                                    let label = format!(
                                        "use with_capacity_and_hasher + extend type ascription \
                                         with hasher (Phase N)"
                                    );
                                    span_lint_and_then(
                                        cx,
                                        CAPTRACK_PGO_CAPACITY,
                                        span,
                                        msg,
                                        |diag| {
                                            diag.multipart_suggestion(
                                                label,
                                                vec![(asc_span, new_ty_snip), (span, ctor_sugg)],
                                                base_applicability,
                                            );
                                        },
                                    );
                                    return;
                                }
                            }
                            // Fallback: snippet unavailable — capacity-only.
                            let Some(cap_sugg) =
                                build_suggestion(cx, fn_expr, method_name, args, to, None)
                            else {
                                span_lint(cx, CAPTRACK_PGO_CAPACITY, span, msg);
                                return;
                            };
                            span_lint_and_sugg(
                                cx,
                                CAPTRACK_PGO_CAPACITY,
                                span,
                                msg,
                                format!(
                                    "use with_capacity({to}) (ascription snippet unavailable \
                                     — hasher skipped)"
                                ),
                                cap_sugg,
                                base_applicability,
                            );
                            return;
                        }
                        AscriptionForm::HasherPinned => {
                            // User explicitly pinned a hasher.
                            // Phase O: classify it so we can tailor the label.
                            let hasher_kind = extract_hasher_arg_snippet(cx, hir_ty)
                                .as_deref()
                                .map(classify_hasher_snippet)
                                .unwrap_or(HasherKind::Unknown);

                            let label = match hasher_kind {
                                HasherKind::FastKnown => format!(
                                    "use with_capacity({to}) (skipping hasher swap — \
                                     type ascription already pins a fast hasher)"
                                ),
                                HasherKind::SlowDefault => format!(
                                    "use with_capacity({to}) (skipping hasher swap — \
                                     type ascription pins RandomState; consider removing \
                                     the explicit hasher to enable swap)"
                                ),
                                HasherKind::Unknown => format!(
                                    "use with_capacity({to}) (skipping hasher swap — \
                                     user pinned hasher in type ascription)"
                                ),
                            };

                            let Some(cap_sugg) =
                                build_suggestion(cx, fn_expr, method_name, args, to, None)
                            else {
                                span_lint(cx, CAPTRACK_PGO_CAPACITY, span, msg);
                                return;
                            };
                            span_lint_and_sugg(
                                cx,
                                CAPTRACK_PGO_CAPACITY,
                                span,
                                msg,
                                label,
                                cap_sugg,
                                base_applicability,
                            );
                            return;
                        }
                        AscriptionForm::Wildcard | AscriptionForm::NotApplicable => {
                            // No generics written or unrecognised pattern — capacity only.
                            let Some(cap_sugg) =
                                build_suggestion(cx, fn_expr, method_name, args, to, None)
                            else {
                                span_lint(cx, CAPTRACK_PGO_CAPACITY, span, msg);
                                return;
                            };
                            span_lint_and_sugg(
                                cx,
                                CAPTRACK_PGO_CAPACITY,
                                span,
                                msg,
                                format!(
                                    "use with_capacity({to}) (skipping hasher swap — explicit \
                                     type ascription would prevent inference)"
                                ),
                                cap_sugg,
                                base_applicability,
                            );
                            return;
                        }
                    }
                }

                // No local type ascription — inject hasher normally (existing M9 path).
                let sugg_opt = build_suggestion(cx, fn_expr, method_name, args, to, Some(h));
                let (sugg, applicability) = match sugg_opt {
                    None => {
                        span_lint(cx, CAPTRACK_PGO_CAPACITY, span, msg);
                        return;
                    }
                    Some(s) => (s, base_applicability),
                };
                let label = format!("use with_capacity_and_hasher({to}, {})", h.default_expr());
                span_lint_and_sugg(
                    cx,
                    CAPTRACK_PGO_CAPACITY,
                    span,
                    msg,
                    label,
                    sugg,
                    applicability,
                );
            } else {
                // No hasher injection: capacity-only path (existing M3 behaviour).
                let Some(cap_sugg) = build_suggestion(cx, fn_expr, method_name, args, to, None)
                else {
                    span_lint(cx, CAPTRACK_PGO_CAPACITY, span, msg);
                    return;
                };
                let label = format!("use with_capacity({to})");
                span_lint_and_sugg(
                    cx,
                    CAPTRACK_PGO_CAPACITY,
                    span,
                    msg,
                    label,
                    cap_sugg,
                    base_applicability,
                );
            }
        }
    }
}

/// Build a new type-ascription snippet that extends the existing type with a
/// hasher parameter.
///
/// For `HashMap<K, V>` returns `HashMap<K, V, ::fxhash::FxBuildHasher>`.
///
/// Strategy:
/// 1. Take the source snippet of the HIR type node (the entire ascription text).
/// 2. Find the last `>` and insert `, <hasher_type>` before it.
///
/// This approach works for any nested path form (qualified or unqualified).
/// Returns `None` when the snippet is unavailable or doesn't end with `>`.
fn build_ascription_with_hasher<'tcx>(
    cx: &LateContext<'tcx>,
    hir_ty: &rustc_hir::Ty<'tcx>,
    h: HasherChoice,
) -> Option<String> {
    let snippet = snippet_opt(cx, hir_ty.span)?;
    // The snippet must end with `>` to be a generic type like `HashMap<K, V>`.
    // For bare `HashMap` (Wildcard) there is no `>`, but we guard against
    // Wildcard before calling this function.
    let pos = snippet.rfind('>')?;
    let hasher_ty = h.hasher_type_path();
    let new_snip = format!("{}, {}{}", &snippet[..pos], hasher_ty, &snippet[pos..]);
    Some(new_snip)
}

/// Build the replacement source string for the call expression.
///
/// Strategy: take the source text of `fn_expr` (the callee path, e.g.
/// `Vec::new` or `std::collections::HashMap::with_capacity`), strip the
/// trailing constructor name, append the new constructor name and arguments.
///
/// When `inject_hasher` is `Some(h)`, the replacement uses
/// `with_capacity_and_hasher(to, h.default_expr())` regardless of the current
/// constructor form.  This covers:
/// - `new()` → `with_capacity_and_hasher(N, <default_expr>)`
/// - `with_capacity(K)` → `with_capacity_and_hasher(N, <default_expr>)`
/// - `with_capacity_and_hasher(K, h)` where `h` is one of our known
///   defaulted expressions → replace both K and h (idempotent).
/// - `with_capacity_and_hasher(K, h)` where `h` is NOT a known expression
///   → preserve the user's hasher (replace K only).
///
/// Returns `None` if the source snippet is unavailable (e.g. virtual files).
fn build_suggestion<'tcx>(
    cx: &LateContext<'tcx>,
    fn_expr: &Expr<'tcx>,
    method_name: Symbol,
    args: &'tcx [Expr<'tcx>],
    to: usize,
    inject_hasher: Option<HasherChoice>,
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
    if let Some(h) = inject_hasher {
        // Hasher-injection path: always emit with_capacity_and_hasher.
        if method_name == sym_with_capacity_and_hasher() {
            // Current call is already with_capacity_and_hasher(K, existing_hasher).
            // Check if existing_hasher is one of our known default expressions
            // (so we can idempotently replace it).
            let hasher_span = args.get(1)?.span;
            let existing_hasher_text = snippet_opt(cx, hasher_span)?;

            let known_defaults = [
                HasherChoice::Fx.default_expr(),
                HasherChoice::AHash.default_expr(),
                HasherChoice::FoldHash.default_expr(),
            ];
            if known_defaults.contains(&existing_hasher_text.trim()) {
                // Idempotent replacement: swap to the chosen hasher.
                Some(format!(
                    "{prefix}::with_capacity_and_hasher({to}, {})",
                    h.default_expr()
                ))
            } else {
                // User's custom hasher — preserve it, only update capacity.
                Some(format!(
                    "{prefix}::with_capacity_and_hasher({to}, {existing_hasher_text})"
                ))
            }
        } else {
            // new() or with_capacity(K) → upgrade to with_capacity_and_hasher.
            Some(format!(
                "{prefix}::with_capacity_and_hasher({to}, {})",
                h.default_expr()
            ))
        }
    } else {
        // Capacity-only path (existing M3 behaviour).
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

// ── Unit tests for pure helpers (no HIR / rustc context needed) ──────────────
#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CapExpr, SiteKey, SiteStats, Unit};
    use std::path::PathBuf;

    fn key() -> SiteKey {
        SiteKey {
            file: PathBuf::from("x.rs"),
            line: 1,
            col: 1,
        }
    }

    fn stats_for(peak: usize, p95: usize, count: u64) -> SiteStats {
        SiteStats {
            key: key(),
            unit: Unit::Elements,
            peak,
            p50: p95 / 2,
            p95,
            count,
            mean: None,
            p99: None,
            policy: None,
        }
    }

    // ── tracked_type_to_static_path ──────────────────────────────────────────

    /// Vec maps to ::std::vec::Vec.
    #[test]
    fn static_path_vec() {
        assert_eq!(
            tracked_type_to_static_path(TrackedType::Vec),
            Some("::std::vec::Vec")
        );
    }

    /// HashMap maps to ::std::collections::HashMap.
    #[test]
    fn static_path_hashmap() {
        assert_eq!(
            tracked_type_to_static_path(TrackedType::HashMap),
            Some("::std::collections::HashMap")
        );
    }

    /// HashSet maps to ::std::collections::HashSet.
    #[test]
    fn static_path_hashset() {
        assert_eq!(
            tracked_type_to_static_path(TrackedType::HashSet),
            Some("::std::collections::HashSet")
        );
    }

    /// VecDeque maps to ::std::collections::VecDeque.
    #[test]
    fn static_path_vecdeque() {
        assert_eq!(
            tracked_type_to_static_path(TrackedType::VecDeque),
            Some("::std::collections::VecDeque")
        );
    }

    /// BytesMut maps to ::bytes::BytesMut.
    #[test]
    fn static_path_bytesmut() {
        assert_eq!(
            tracked_type_to_static_path(TrackedType::BytesMut),
            Some("::bytes::BytesMut")
        );
    }

    /// IndexMap maps to ::indexmap::IndexMap.
    #[test]
    fn static_path_indexmap() {
        assert_eq!(
            tracked_type_to_static_path(TrackedType::IndexMap),
            Some("::indexmap::IndexMap")
        );
    }

    /// IndexSet maps to ::indexmap::IndexSet.
    #[test]
    fn static_path_indexset() {
        assert_eq!(
            tracked_type_to_static_path(TrackedType::IndexSet),
            Some("::indexmap::IndexSet")
        );
    }

    /// DashMap maps to ::dashmap::DashMap.
    #[test]
    fn static_path_dashmap() {
        assert_eq!(
            tracked_type_to_static_path(TrackedType::DashMap),
            Some("::dashmap::DashMap")
        );
    }

    /// SccHashMap maps to ::scc::HashMap.
    #[test]
    fn static_path_scc_hashmap() {
        assert_eq!(
            tracked_type_to_static_path(TrackedType::SccHashMap),
            Some("::scc::HashMap")
        );
    }

    /// SccHashSet maps to ::scc::HashSet.
    #[test]
    fn static_path_scc_hashset() {
        assert_eq!(
            tracked_type_to_static_path(TrackedType::SccHashSet),
            Some("::scc::HashSet")
        );
    }

    /// SmallVec maps to ::smallvec::SmallVec.
    #[test]
    fn static_path_smallvec() {
        assert_eq!(
            tracked_type_to_static_path(TrackedType::SmallVec),
            Some("::smallvec::SmallVec")
        );
    }

    /// BTreeMap returns None — no with_capacity.
    #[test]
    fn default_default_synthesises_btreemap_warning_only() {
        assert_eq!(
            tracked_type_to_static_path(TrackedType::BTreeMap),
            None,
            "BTreeMap has no with_capacity — must return None"
        );
    }

    /// BTreeSet returns None — no with_capacity.
    #[test]
    fn static_path_btreeset_none() {
        assert_eq!(tracked_type_to_static_path(TrackedType::BTreeSet), None);
    }

    /// SccTreeIndex returns None — no with_capacity.
    #[test]
    fn static_path_scc_tree_index_none() {
        assert_eq!(tracked_type_to_static_path(TrackedType::SccTreeIndex), None);
    }

    // ── tracked_type_supports_hasher ─────────────────────────────────────────

    /// Hasher-capable types.
    #[test]
    fn hasher_support_yes() {
        for t in [
            TrackedType::HashMap,
            TrackedType::HashSet,
            TrackedType::IndexMap,
            TrackedType::IndexSet,
            TrackedType::DashMap,
            TrackedType::SccHashMap,
            TrackedType::SccHashSet,
        ] {
            assert!(
                tracked_type_supports_hasher(t),
                "{t:?} should support hasher injection"
            );
        }
    }

    /// Non-hasher types.
    #[test]
    fn hasher_support_no() {
        for t in [
            TrackedType::Vec,
            TrackedType::VecDeque,
            TrackedType::BTreeMap,
            TrackedType::BTreeSet,
            TrackedType::BytesMut,
            TrackedType::SccTreeIndex,
            TrackedType::SmallVec,
        ] {
            assert!(
                !tracked_type_supports_hasher(t),
                "{t:?} should NOT support hasher injection"
            );
        }
    }

    // ── Suggestion-string synthesis (pure, no HIR) ───────────────────────────

    /// Vec: Default::default() with profile → ::std::vec::Vec::with_capacity(N).
    #[test]
    fn default_default_synthesises_vec_with_capacity() {
        let path = tracked_type_to_static_path(TrackedType::Vec).unwrap();
        let s = stats_for(60, 60, 50);
        let defaults = PolicyDefaults::default();
        let decision = rules::propose_cap(&s, &CapExpr::Zero, defaults);
        if let Decision::Patch { to, .. } = decision {
            let sugg = format!("{path}::with_capacity({to})");
            assert_eq!(sugg, "::std::vec::Vec::with_capacity(64)");
        } else {
            panic!("expected Patch for Vec, got {:?}", decision);
        }
    }

    /// HashMap: Default::default() with profile → ::std::collections::HashMap::with_capacity(N).
    #[test]
    fn default_default_synthesises_hashmap_with_capacity() {
        let path = tracked_type_to_static_path(TrackedType::HashMap).unwrap();
        let s = stats_for(60, 60, 50);
        let defaults = PolicyDefaults::default();
        let decision = rules::propose_cap(&s, &CapExpr::Zero, defaults);
        if let Decision::Patch { to, .. } = decision {
            let sugg = format!("{path}::with_capacity({to})");
            assert_eq!(sugg, "::std::collections::HashMap::with_capacity(64)");
        } else {
            panic!("expected Patch for HashMap, got {:?}", decision);
        }
    }

    /// HashMap: with Fx hasher → ::std::collections::HashMap::with_capacity_and_hasher(N, <expr>).
    #[test]
    fn default_default_with_hasher_fx_works() {
        let path = tracked_type_to_static_path(TrackedType::HashMap).unwrap();
        let s = stats_for(60, 60, 50);
        let defaults = PolicyDefaults::default();
        let decision = rules::propose_cap(&s, &CapExpr::Zero, defaults);
        let h = HasherChoice::Fx;
        if let Decision::Patch { to, .. } = decision {
            let sugg = format!(
                "{path}::with_capacity_and_hasher({to}, {})",
                h.default_expr()
            );
            assert_eq!(
                sugg,
                "::std::collections::HashMap::with_capacity_and_hasher(64, ::fxhash::FxBuildHasher::default())"
            );
        } else {
            panic!("expected Patch for HashMap+Fx, got {:?}", decision);
        }
    }

    /// BTreeMap: no static path → suggestion not synthesised (warning-only path).
    #[test]
    fn btreemap_no_path_means_warning_only() {
        // tracked_type_to_static_path returns None; callers fall back to span_lint.
        assert!(
            tracked_type_to_static_path(TrackedType::BTreeMap).is_none(),
            "BTreeMap must return None so callers fall back to warning-only"
        );
        // Verify propose_cap itself would have produced a Patch (the Decision
        // pipeline works; only the emitter bails out because there's no path).
        let s = stats_for(60, 60, 50);
        let d = rules::propose_cap(&s, &CapExpr::Zero, PolicyDefaults::default());
        assert!(
            matches!(d, Decision::Patch { .. }),
            "propose_cap should still produce Patch for BTreeMap stats"
        );
    }

    /// SccHashMap suggestion path.
    #[test]
    fn default_default_synthesises_scc_hashmap_with_capacity() {
        let path = tracked_type_to_static_path(TrackedType::SccHashMap).unwrap();
        let s = stats_for(60, 60, 50);
        let defaults = PolicyDefaults::default();
        let decision = rules::propose_cap(&s, &CapExpr::Zero, defaults);
        if let Decision::Patch { to, .. } = decision {
            let sugg = format!("{path}::with_capacity({to})");
            assert_eq!(sugg, "::scc::HashMap::with_capacity(64)");
        } else {
            panic!("expected Patch for SccHashMap, got {:?}", decision);
        }
    }

    /// BytesMut suggestion path.
    #[test]
    fn default_default_synthesises_bytesmut_with_capacity() {
        let path = tracked_type_to_static_path(TrackedType::BytesMut).unwrap();
        let s = stats_for(60, 60, 50);
        let defaults = PolicyDefaults::default();
        let decision = rules::propose_cap(&s, &CapExpr::Zero, defaults);
        if let Decision::Patch { to, .. } = decision {
            let sugg = format!("{path}::with_capacity({to})");
            assert_eq!(sugg, "::bytes::BytesMut::with_capacity(64)");
        } else {
            panic!("expected Patch for BytesMut, got {:?}", decision);
        }
    }

    /// Generic-args round-trip: the suggestion string uses a generic-free path,
    /// relying on type inference.  Verify the suggestion format for a type with
    /// generic parameters (Vec<u8> → Vec::with_capacity(N), not Vec::<u8>::with_capacity(N)).
    #[test]
    fn generic_args_round_trip_uses_generic_free_path() {
        // The static-path approach always uses generic-free paths.
        // Vec<u8> and Vec<String> both get "::std::vec::Vec::with_capacity(N)".
        let path = tracked_type_to_static_path(TrackedType::Vec).unwrap();
        assert!(
            !path.contains('<'),
            "static path must not contain generic args"
        );
        assert!(
            !path.contains('>'),
            "static path must not contain generic args"
        );
    }

    // ── Phase N: hasher_arg_counts + AscriptionForm pure helpers ─────────────

    /// HashMap requires (2, 3): 2 generics without hasher, 3 with.
    #[test]
    fn hasher_arg_counts_hashmap() {
        assert_eq!(hasher_arg_counts(TrackedType::HashMap), Some((2, 3)));
    }

    /// HashSet requires (1, 2): 1 generic without hasher, 2 with.
    #[test]
    fn hasher_arg_counts_hashset() {
        assert_eq!(hasher_arg_counts(TrackedType::HashSet), Some((1, 2)));
    }

    /// IndexMap requires (2, 3): same shape as HashMap.
    #[test]
    fn hasher_arg_counts_indexmap() {
        assert_eq!(hasher_arg_counts(TrackedType::IndexMap), Some((2, 3)));
    }

    /// IndexSet requires (1, 2): same shape as HashSet.
    #[test]
    fn hasher_arg_counts_indexset() {
        assert_eq!(hasher_arg_counts(TrackedType::IndexSet), Some((1, 2)));
    }

    /// DashMap requires (2, 3): K, V [, S].
    #[test]
    fn hasher_arg_counts_dashmap() {
        assert_eq!(hasher_arg_counts(TrackedType::DashMap), Some((2, 3)));
    }

    /// scc::HashMap requires (2, 3).
    #[test]
    fn hasher_arg_counts_scc_hashmap() {
        assert_eq!(hasher_arg_counts(TrackedType::SccHashMap), Some((2, 3)));
    }

    /// scc::HashSet requires (1, 2).
    #[test]
    fn hasher_arg_counts_scc_hashset() {
        assert_eq!(hasher_arg_counts(TrackedType::SccHashSet), Some((1, 2)));
    }

    /// Vec is not a hashing type — returns None.
    #[test]
    fn hasher_arg_counts_vec_not_applicable() {
        assert_eq!(hasher_arg_counts(TrackedType::Vec), None);
    }

    /// VecDeque is not a hashing type — returns None.
    #[test]
    fn hasher_arg_counts_vecdeque_not_applicable() {
        assert_eq!(hasher_arg_counts(TrackedType::VecDeque), None);
    }

    /// BTreeMap is not a hashing type — returns None.
    #[test]
    fn hasher_arg_counts_btreemap_not_applicable() {
        assert_eq!(hasher_arg_counts(TrackedType::BTreeMap), None);
    }

    /// BTreeSet is not a hashing type — returns None.
    #[test]
    fn hasher_arg_counts_btreeset_not_applicable() {
        assert_eq!(hasher_arg_counts(TrackedType::BTreeSet), None);
    }

    /// BytesMut is not a hashing type — returns None.
    #[test]
    fn hasher_arg_counts_bytesmut_not_applicable() {
        assert_eq!(hasher_arg_counts(TrackedType::BytesMut), None);
    }

    /// scc::TreeIndex is not a hashing type — returns None.
    #[test]
    fn hasher_arg_counts_scc_treeindex_not_applicable() {
        assert_eq!(hasher_arg_counts(TrackedType::SccTreeIndex), None);
    }

    /// SmallVec is not a hashing type — returns None.
    #[test]
    fn hasher_arg_counts_smallvec_not_applicable() {
        assert_eq!(hasher_arg_counts(TrackedType::SmallVec), None);
    }

    // ── HasherChoice::hasher_type_path ────────────────────────────────────────

    /// Fx hasher_type_path gives the type (not constructor) form.
    #[test]
    fn hasher_type_path_fx() {
        assert_eq!(
            HasherChoice::Fx.hasher_type_path(),
            "::fxhash::FxBuildHasher"
        );
    }

    /// AHash hasher_type_path.
    #[test]
    fn hasher_type_path_ahash() {
        assert_eq!(
            HasherChoice::AHash.hasher_type_path(),
            "::ahash::RandomState"
        );
    }

    /// FoldHash hasher_type_path.
    #[test]
    fn hasher_type_path_foldhash() {
        assert_eq!(
            HasherChoice::FoldHash.hasher_type_path(),
            "::foldhash::fast::RandomState"
        );
    }

    // ── build_ascription_with_hasher pure logic (snippet-level) ──────────────
    //
    // `build_ascription_with_hasher` needs a real LateContext to call
    // `snippet_opt`, so we can't unit-test it directly without HIR.  Instead
    // we test the core string-manipulation logic it relies on: inserting
    // `, HasherType` before the last `>`.

    /// Simulated snippet insertion: `HashMap<K, V>` → `HashMap<K, V, ::fxhash::FxBuildHasher>`.
    #[test]
    fn ascription_snippet_insertion_hashmap_2_generics() {
        let snippet = "HashMap<K, V>";
        let hasher_ty = "::fxhash::FxBuildHasher";
        let pos = snippet.rfind('>').unwrap();
        let result = format!("{}, {}{}", &snippet[..pos], hasher_ty, &snippet[pos..]);
        assert_eq!(result, "HashMap<K, V, ::fxhash::FxBuildHasher>");
    }

    /// Simulated snippet insertion: `HashSet<T>` → `HashSet<T, ::ahash::RandomState>`.
    #[test]
    fn ascription_snippet_insertion_hashset_1_generic() {
        let snippet = "HashSet<T>";
        let hasher_ty = "::ahash::RandomState";
        let pos = snippet.rfind('>').unwrap();
        let result = format!("{}, {}{}", &snippet[..pos], hasher_ty, &snippet[pos..]);
        assert_eq!(result, "HashSet<T, ::ahash::RandomState>");
    }

    /// Nested generics: `HashMap<String, Vec<u8>>` →
    /// `HashMap<String, Vec<u8>, ::fxhash::FxBuildHasher>`.
    /// Uses `rfind('>')` so we insert before the last `>`, not an inner one.
    #[test]
    fn ascription_snippet_insertion_nested_generics() {
        let snippet = "HashMap<String, Vec<u8>>";
        let hasher_ty = "::fxhash::FxBuildHasher";
        let pos = snippet.rfind('>').unwrap();
        let result = format!("{}, {}{}", &snippet[..pos], hasher_ty, &snippet[pos..]);
        assert_eq!(result, "HashMap<String, Vec<u8>, ::fxhash::FxBuildHasher>");
    }

    /// Fully-qualified path: `std::collections::HashMap<u32, String>` →
    /// `std::collections::HashMap<u32, String, ::fxhash::FxBuildHasher>`.
    #[test]
    fn ascription_snippet_insertion_qualified_path() {
        let snippet = "std::collections::HashMap<u32, String>";
        let hasher_ty = "::fxhash::FxBuildHasher";
        let pos = snippet.rfind('>').unwrap();
        let result = format!("{}, {}{}", &snippet[..pos], hasher_ty, &snippet[pos..]);
        assert_eq!(
            result,
            "std::collections::HashMap<u32, String, ::fxhash::FxBuildHasher>"
        );
    }

    /// Bare `HashMap` (no generics) — `rfind('>')` returns `None`, no insertion.
    /// This ensures `build_ascription_with_hasher` returns `None` for Wildcard.
    #[test]
    fn ascription_snippet_insertion_bare_type_no_angle_bracket() {
        let snippet = "HashMap";
        assert!(
            snippet.rfind('>').is_none(),
            "bare type has no '>' — build_ascription_with_hasher returns None"
        );
    }

    // ── Coverage table for hasher_arg_counts (all 14 types) ──────────────────

    /// Every non-hashing tracked type returns None.
    #[test]
    fn non_hashing_types_all_return_none() {
        for t in [
            TrackedType::Vec,
            TrackedType::VecDeque,
            TrackedType::BTreeMap,
            TrackedType::BTreeSet,
            TrackedType::BytesMut,
            TrackedType::SccTreeIndex,
            TrackedType::SmallVec,
        ] {
            assert_eq!(
                hasher_arg_counts(t),
                None,
                "{t:?} should not have hasher arg counts"
            );
        }
    }

    /// Every hashing tracked type returns Some with consistent without/with values.
    #[test]
    fn hashing_types_all_return_some() {
        for (t, expected) in [
            (TrackedType::HashMap, (2, 3)),
            (TrackedType::HashSet, (1, 2)),
            (TrackedType::IndexMap, (2, 3)),
            (TrackedType::IndexSet, (1, 2)),
            (TrackedType::DashMap, (2, 3)),
            (TrackedType::SccHashMap, (2, 3)),
            (TrackedType::SccHashSet, (1, 2)),
        ] {
            assert_eq!(
                hasher_arg_counts(t),
                Some(expected),
                "{t:?} should have hasher arg counts {expected:?}"
            );
        }
    }

    // ── Phase O: classify_hasher_snippet ─────────────────────────────────────
    //
    // Coverage table (locked by spec):
    //   classify_hasher_kind_random_state_returns_slow
    //   classify_hasher_kind_fx_returns_fast
    //   classify_hasher_kind_ahash_returns_fast
    //   classify_hasher_kind_unknown_custom_returns_unknown
    //   classify_hasher_kind_build_hasher_default_fx_returns_fast

    /// Bare `RandomState` (std's slow default) → SlowDefault.
    #[test]
    fn classify_hasher_kind_random_state_returns_slow() {
        assert_eq!(
            classify_hasher_snippet("RandomState"),
            HasherKind::SlowDefault,
        );
    }

    /// Fully-qualified std RandomState → SlowDefault.
    #[test]
    fn classify_hasher_kind_std_random_state_returns_slow() {
        assert_eq!(
            classify_hasher_snippet("std::collections::hash_map::RandomState"),
            HasherKind::SlowDefault,
        );
    }

    /// `fxhash::FxBuildHasher` → FastKnown.
    #[test]
    fn classify_hasher_kind_fx_returns_fast() {
        assert_eq!(
            classify_hasher_snippet("fxhash::FxBuildHasher"),
            HasherKind::FastKnown,
        );
    }

    /// `::fxhash::FxBuildHasher` (fully-rooted path) → FastKnown.
    #[test]
    fn classify_hasher_kind_rooted_fx_returns_fast() {
        assert_eq!(
            classify_hasher_snippet("::fxhash::FxBuildHasher"),
            HasherKind::FastKnown,
        );
    }

    /// `rustc_hash::FxBuildHasher` → FastKnown.
    #[test]
    fn classify_hasher_kind_rustc_hash_returns_fast() {
        assert_eq!(
            classify_hasher_snippet("rustc_hash::FxBuildHasher"),
            HasherKind::FastKnown,
        );
    }

    /// `ahash::RandomState` → FastKnown (ahash:: prefix wins over RandomState slow rule).
    #[test]
    fn classify_hasher_kind_ahash_returns_fast() {
        assert_eq!(
            classify_hasher_snippet("ahash::RandomState"),
            HasherKind::FastKnown,
        );
    }

    /// `::ahash::RandomState` → FastKnown.
    #[test]
    fn classify_hasher_kind_rooted_ahash_returns_fast() {
        assert_eq!(
            classify_hasher_snippet("::ahash::RandomState"),
            HasherKind::FastKnown,
        );
    }

    /// `foldhash::fast::RandomState` → FastKnown.
    #[test]
    fn classify_hasher_kind_foldhash_fast_returns_fast() {
        assert_eq!(
            classify_hasher_snippet("foldhash::fast::RandomState"),
            HasherKind::FastKnown,
        );
    }

    /// `foldhash::quality::RandomState` → FastKnown.
    #[test]
    fn classify_hasher_kind_foldhash_quality_returns_fast() {
        assert_eq!(
            classify_hasher_snippet("foldhash::quality::RandomState"),
            HasherKind::FastKnown,
        );
    }

    /// `BuildHasherDefault<FxHasher>` (THasher alias form) → FastKnown.
    #[test]
    fn classify_hasher_kind_build_hasher_default_fx_returns_fast() {
        assert_eq!(
            classify_hasher_snippet("BuildHasherDefault<FxHasher>"),
            HasherKind::FastKnown,
        );
    }

    /// `std::hash::BuildHasherDefault<fxhash::FxHasher>` → FastKnown.
    #[test]
    fn classify_hasher_kind_qualified_build_hasher_default_fx_returns_fast() {
        assert_eq!(
            classify_hasher_snippet("std::hash::BuildHasherDefault<fxhash::FxHasher>"),
            HasherKind::FastKnown,
        );
    }

    /// Custom unknown hasher → Unknown.
    #[test]
    fn classify_hasher_kind_unknown_custom_returns_unknown() {
        assert_eq!(
            classify_hasher_snippet("MyCustomHasher"),
            HasherKind::Unknown,
        );
    }

    /// `SipHasher` → Unknown (not in our fast-hasher set, not RandomState).
    #[test]
    fn classify_hasher_kind_siphash_returns_unknown() {
        assert_eq!(
            classify_hasher_snippet("SipHasher"),
            HasherKind::Unknown,
        );
    }

    /// Whitespace is trimmed before matching.
    #[test]
    fn classify_hasher_kind_trims_whitespace() {
        assert_eq!(
            classify_hasher_snippet("  fxhash::FxBuildHasher  "),
            HasherKind::FastKnown,
        );
        assert_eq!(
            classify_hasher_snippet("  RandomState  "),
            HasherKind::SlowDefault,
        );
    }
}
