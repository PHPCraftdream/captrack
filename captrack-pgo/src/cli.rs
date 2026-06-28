//! CLI definitions for captrack-pgo.
//!
//! Subcommands:
//!   measure      — placeholder for a future "run bench under profiler" helper.
//!   apply        — Dylint-based capacity rewrite via `cargo dylint --fix`.
//!   instrument   — Dylint-based auto-wrap via `cargo dylint --fix` +
//!                  `CAPTRACK_PGO_INSTRUMENT=1`.
//!   undo         — revert the most recent `apply` or `instrument` manifest.

use anyhow::Context as _;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

use crate::lint_apply;
use crate::lint_instrument;

#[derive(Parser, Debug)]
#[command(name = "captrack-pgo", version, about, long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Run a bench under a profiler and collect a heap snapshot (not yet implemented).
    Measure {
        /// Bench target name (e.g. `tx_pipeline`).
        #[arg(long)]
        bench: String,
    },

    /// Apply capacity suggestions via `cargo dylint --fix` (Dylint-based rewrite).
    ///
    /// Runs the captrack-pgo-lint Dylint plugin against the target workspace with
    /// `--fix`, then records before/after file snapshots in a manifest that `undo`
    /// can revert.
    ///
    /// Requires: `cargo install cargo-dylint dylint-link` and a nightly toolchain
    /// pinned in `captrack-pgo-lint/rust-toolchain.toml`.
    Apply {
        /// Path to a captrack profile JSON (required).
        #[arg(long)]
        profile: PathBuf,

        /// Path to the captrack-pgo-lint crate root.
        /// Defaults to a sibling `captrack-pgo-lint/` directory.
        #[arg(long)]
        lint_path: Option<PathBuf>,

        /// Workspace root to patch (defaults to current directory).
        #[arg(long)]
        workspace: Option<PathBuf>,

        /// Show suggestions without applying or writing a manifest.
        #[arg(long)]
        dry_run: bool,

        /// Pass --allow-dirty to cargo dylint (allow dirty git trees).
        #[arg(long)]
        allow_dirty: bool,
    },

    /// Auto-instrument every bare std collection constructor with
    /// `::captrack::TrackedX::with_capacity_named(...)` via `cargo dylint --fix`.
    ///
    /// Sets `CAPTRACK_PGO_INSTRUMENT=1` in the child environment and explicitly
    /// unsets `CAPTRACK_PGO_PROFILE` so the two lint modes can't conflict.
    ///
    /// After the run a manifest is written to
    /// `target/captrack-pgo/last-instrument.json` which `undo` (or a future
    /// `uninstrument` subcommand) can revert.
    ///
    /// Requires: `cargo install cargo-dylint dylint-link` and a nightly toolchain
    /// pinned in `captrack-pgo-lint/rust-toolchain.toml`.
    Instrument {
        /// Path to the captrack-pgo-lint crate root.
        /// Defaults to a sibling `captrack-pgo-lint/` directory.
        #[arg(long)]
        lint_path: Option<PathBuf>,

        /// Workspace root to instrument (defaults to current directory).
        #[arg(long)]
        workspace: Option<PathBuf>,

        /// Run `cargo dylint` WITHOUT `--fix` so suggestions appear in stderr
        /// without modifying any file; no manifest is written.
        #[arg(long)]
        dry_run: bool,

        /// Pass --allow-dirty to cargo dylint (allow dirty git trees).
        #[arg(long)]
        allow_dirty: bool,
    },

    /// Roll back the most recent `apply` or `instrument` using its manifest.
    ///
    /// Without `--manifest`, picks the most recently modified manifest among
    /// `target/captrack-pgo/last-apply.json` and
    /// `target/captrack-pgo/last-instrument.json`.
    Undo {
        /// Explicit manifest file to revert.
        #[arg(long)]
        manifest: Option<PathBuf>,
    },
}

pub fn dispatch(cli: Cli) -> anyhow::Result<()> {
    match cli.command {
        Command::Measure { bench } => {
            eprintln!("measure --bench {bench}: not yet implemented");
        }
        Command::Apply {
            profile,
            lint_path,
            workspace,
            dry_run,
            allow_dirty,
        } => {
            run_apply(profile, lint_path, workspace, dry_run, allow_dirty)?;
        }
        Command::Instrument {
            lint_path,
            workspace,
            dry_run,
            allow_dirty,
        } => {
            run_instrument(lint_path, workspace, dry_run, allow_dirty)?;
        }
        Command::Undo { manifest } => {
            run_undo(manifest)?;
        }
    }
    Ok(())
}

fn run_apply(
    profile: PathBuf,
    lint_path: Option<PathBuf>,
    workspace: Option<PathBuf>,
    dry_run: bool,
    allow_dirty: bool,
) -> anyhow::Result<()> {
    use crate::workspace as ws;

    // Resolve workspace root.
    let workspace_start = workspace.unwrap_or_else(|| std::env::current_dir().expect("cwd"));
    let workspace_root = ws::find_workspace_root(&workspace_start).with_context(|| {
        format!(
            "locate workspace root from {}",
            workspace_start.display()
        )
    })?;

    // Resolve lint-path (use provided or discover default).
    let resolved_lint_path = match lint_path {
        Some(p) => p,
        None => lint_apply::resolve_default_lint_path()?,
    };

    lint_apply::run_lint_apply(lint_apply::LintApplyArgs {
        profile_path: profile,
        lint_path: resolved_lint_path,
        workspace_root,
        dry_run,
        allow_dirty,
    })
}

fn run_instrument(
    lint_path: Option<PathBuf>,
    workspace: Option<PathBuf>,
    dry_run: bool,
    allow_dirty: bool,
) -> anyhow::Result<()> {
    use crate::workspace as ws;

    // Resolve workspace root.
    let workspace_start = workspace.unwrap_or_else(|| std::env::current_dir().expect("cwd"));
    let workspace_root = ws::find_workspace_root(&workspace_start).with_context(|| {
        format!(
            "locate workspace root from {}",
            workspace_start.display()
        )
    })?;

    // Resolve lint-path.
    let resolved_lint_path = lint_instrument::resolve_lint_path(lint_path)?;

    lint_instrument::run_lint_instrument(lint_instrument::LintInstrumentArgs {
        lint_path: resolved_lint_path,
        workspace_root,
        dry_run,
        allow_dirty,
    })
}

fn run_undo(manifest: Option<PathBuf>) -> anyhow::Result<()> {
    use crate::workspace as ws;

    let path = match manifest {
        Some(p) => p,
        None => {
            let root = ws::find_workspace_root(&std::env::current_dir()?)?;
            match lint_apply::latest_manifest_path(&root) {
                Some(p) => p,
                None => {
                    return Err(anyhow::anyhow!(
                        "no manifest found; expected one of:\n  \
                         {}\n  {}",
                        lint_apply::default_apply_manifest_path(&root).display(),
                        lint_apply::default_instrument_manifest_path(&root).display()
                    ));
                }
            }
        }
    };

    let n = lint_apply::undo_lint_apply(&path)?;
    println!(
        "reverted {n} file{} from {}",
        if n == 1 { "" } else { "s" },
        path.display()
    );

    Ok(())
}
