//! captrack-pgo — profile-guided capacity optimization.
//!
//! See `docs/CAPACITY-PGO-PLAN.md` for the design rationale and
//! `docs/CAPACITY-PGO-PLAN.md#path-b-migration-completed` for the
//! description of the current Dylint-based pipeline.

mod analyze;
mod cli;
mod lint_apply;
mod lint_instrument;
mod measure;
mod merge;
mod model;
mod profile;
mod staleness;
mod wire;
mod workspace;

fn main() -> anyhow::Result<()> {
    use clap::Parser;
    let args = cli::Cli::parse();
    cli::dispatch(args)
}
