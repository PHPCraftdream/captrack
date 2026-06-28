#![feature(rustc_private)]
#![warn(unused_extern_crates)]

extern crate rustc_hir;

use rustc_hir::Expr;
use rustc_lint::{LateContext, LateLintPass};

dylint_linting::impl_late_lint! {
    /// ### What it does
    ///
    /// Placeholder lint — does nothing yet. Real lint logic comes in M2-M3.
    ///
    /// ### Why is this bad?
    ///
    /// It is not bad; this is a scaffold to verify the Dylint setup compiles on nightly.
    ///
    /// ### Example
    ///
    /// ```rust
    /// fn main() {}
    /// ```
    pub CAPTRACK_PGO_PLACEHOLDER,
    Warn,
    "captrack-pgo placeholder lint — replaced in M2",
    CaptrackPgoPlaceholder
}

#[derive(Default)]
pub struct CaptrackPgoPlaceholder;

impl<'tcx> LateLintPass<'tcx> for CaptrackPgoPlaceholder {
    fn check_expr(&mut self, _cx: &LateContext<'tcx>, _expr: &'tcx Expr<'tcx>) {
        // Intentionally empty — M2 will add HIR walking, M3 suggestions.
    }
}
