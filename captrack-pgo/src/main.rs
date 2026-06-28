//! captrack-pgo — profile-guided capacity optimization.
//!
//! See `docs/CAPACITY-PGO-PLAN.md` for the design rationale.

mod apply;
mod cli;
mod model;
mod plan;
mod profile;
mod report;
mod rules;
mod scan;
mod workspace;

fn main() -> anyhow::Result<()> {
    use clap::Parser;
    let args = cli::Cli::parse();
    cli::dispatch(args)
}
