//! CLI definitions for captrack-pgo.
//!
//! Subcommands:
//!   measure        — placeholder for a future "run bench under profiler" helper.
//!   apply          — Dylint-based capacity rewrite via `cargo dylint --fix`.
//!   instrument     — Dylint-based auto-wrap via `cargo dylint --fix` +
//!                    `CAPTRACK_PGO_INSTRUMENT=1`.
//!   undo           — revert the most recent `apply` or `instrument` manifest.
//!   uninstrument   — revert the most recent `instrument` manifest (strict;
//!                    rejects `apply` manifests).

use anyhow::Context as _;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

use crate::lint_apply::{self, CapFromChoice, CapRoundChoice, HasherChoice};
use crate::lint_instrument;
use crate::wire;

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
    ///
    /// When `--hasher` is set to anything other than `none`, every matched
    /// `HashMap`/`HashSet` constructor is also upgraded to `with_capacity_and_hasher`.
    /// Note: sites with an explicit local type ascription (`let m: HashMap<K,V> = ...`)
    /// have the hasher injection skipped automatically to avoid compile errors;
    /// sites in struct fields or function return types are not detected and may
    /// need manual correction.  Remember to add the hasher crate to your Cargo.toml.
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

        /// Inject a non-default hasher into HashMap/HashSet constructors.
        ///
        /// Accepted values: `fx` (FxBuildHasher), `ahash` (ahash::RandomState),
        /// `foldhash` (foldhash::fast::RandomState), `none` (default — no change).
        ///
        /// The chosen hasher crate must be added to the target workspace's
        /// Cargo.toml manually; captrack-pgo emits a reminder after apply.
        #[arg(long, value_name = "HASHER", default_value = "none", value_parser = parse_hasher)]
        hasher: HasherChoice,

        /// Source statistic used as the base capacity value.
        ///
        /// max    — peak observed value; guarantees zero reallocations but may
        ///          waste memory when outliers are rare.
        /// mean   — arithmetic mean.  Only meaningful with captrack-backed profiles;
        ///          dhat-loaded profiles set mean=peak.
        /// median — 50th percentile (p50).
        /// p95    — 95th percentile (default; covers 95% of cases with less waste
        ///          than max).
        /// p99    — 99th percentile.
        #[arg(long, default_value = "p95", value_parser = parse_cap_from)]
        cap_from: CapFromChoice,

        /// Multiplier applied to the source statistic before rounding.
        ///
        /// Examples: --cap-from median --cap-mul 2.0 uses median×2.
        /// Must be > 0.0.  Default: 1.0.
        #[arg(long, default_value = "1.0")]
        cap_mul: f64,

        /// Rounding mode applied after the multiplier.
        ///
        /// pow2  — round up to the next power of two (default; matches Vec
        ///         doubling strategy and is cache-friendly).
        /// to8   — round up to the nearest multiple of 8.
        /// exact — no rounding; use the exact computed value (truncated to usize).
        #[arg(long, default_value = "pow2", value_parser = parse_cap_round)]
        cap_round: CapRoundChoice,
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

    /// Roll back an `instrument` run using its manifest (strict variant).
    ///
    /// Unlike `undo`, this subcommand ONLY accepts manifests produced by the
    /// `instrument` subcommand (`operation.op == "instrument"`).  If the
    /// manifest was written by `apply`, it exits with an error pointing the
    /// user to `captrack-pgo undo`.
    ///
    /// Without `--manifest`, uses the conventional instrument manifest path
    /// `target/captrack-pgo/last-instrument.json` (never falls back to
    /// `last-apply.json`).
    Uninstrument {
        /// Workspace root (defaults to current directory).
        #[arg(long)]
        workspace: Option<PathBuf>,

        /// Explicit manifest path (defaults to
        /// `<workspace>/target/captrack-pgo/last-instrument.json`).
        #[arg(long)]
        manifest: Option<PathBuf>,
    },

    /// Add `captrack` as a dependency to every Cargo.toml in the target
    /// workspace so the post-`instrument` build can resolve `::captrack::*`
    /// paths.
    ///
    /// Patches the root manifest (`[workspace.dependencies] captrack = ...`)
    /// and every member manifest (`[dependencies] captrack = { workspace = true }`).
    /// A snapshot of every modified file is written to
    /// `target/captrack-pgo/last-wire.json` for `unwire` to revert losslessly.
    ///
    /// Already-wired manifests are skipped — `wire` is idempotent.
    Wire {
        /// Workspace root to patch (defaults to current directory).
        #[arg(long)]
        workspace: Option<PathBuf>,

        /// Use a local `captrack` checkout via `path = "..."`.  If omitted, the
        /// published `version = "0.1"` from crates.io is used instead.
        #[arg(long, value_name = "PATH")]
        captrack_path: Option<PathBuf>,
    },

    /// Remove the `captrack` dependency entries that `wire` added, restoring
    /// every Cargo.toml to its pre-wire state.
    ///
    /// Reads `target/captrack-pgo/last-wire.json` (or `--manifest <path>`),
    /// verifies each file is still in the post-wire state recorded there
    /// (refuses to revert if the user has manually edited any of them), and
    /// writes back the pre-wire contents.  The manifest file is deleted on
    /// successful revert.
    Unwire {
        /// Workspace root (defaults to current directory).
        #[arg(long)]
        workspace: Option<PathBuf>,

        /// Explicit manifest path (defaults to
        /// `<workspace>/target/captrack-pgo/last-wire.json`).
        #[arg(long)]
        manifest: Option<PathBuf>,
    },
}

/// Clap value_parser for `--hasher`.
fn parse_hasher(s: &str) -> Result<HasherChoice, String> {
    match s.trim().to_ascii_lowercase().as_str() {
        "none" | "" => Ok(HasherChoice::None),
        "fx" => Ok(HasherChoice::Fx),
        "ahash" => Ok(HasherChoice::AHash),
        "foldhash" => Ok(HasherChoice::FoldHash),
        other => Err(format!(
            "unknown hasher {:?}; accepted values: fx, ahash, foldhash, none",
            other
        )),
    }
}

/// Clap value_parser for `--cap-from`.
fn parse_cap_from(s: &str) -> Result<CapFromChoice, String> {
    match s.trim().to_ascii_lowercase().as_str() {
        "max" => Ok(CapFromChoice::Max),
        "mean" => Ok(CapFromChoice::Mean),
        "median" => Ok(CapFromChoice::Median),
        "p95" => Ok(CapFromChoice::P95),
        "p99" => Ok(CapFromChoice::P99),
        other => Err(format!(
            "unknown cap-from {:?}; accepted values: max, mean, median, p95, p99",
            other
        )),
    }
}

/// Clap value_parser for `--cap-round`.
fn parse_cap_round(s: &str) -> Result<CapRoundChoice, String> {
    match s.trim().to_ascii_lowercase().as_str() {
        "pow2" => Ok(CapRoundChoice::Pow2),
        "to8" => Ok(CapRoundChoice::To8),
        "exact" => Ok(CapRoundChoice::Exact),
        other => Err(format!(
            "unknown cap-round {:?}; accepted values: pow2, to8, exact",
            other
        )),
    }
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
            hasher,
            cap_from,
            cap_mul,
            cap_round,
        } => {
            // Pre-flight: cap_mul must be positive and finite.
            if cap_mul <= 0.0 || !cap_mul.is_finite() {
                return Err(anyhow::anyhow!(
                    "--cap-mul must be > 0.0 and finite; got {}",
                    cap_mul
                ));
            }
            run_apply(
                profile, lint_path, workspace, dry_run, allow_dirty, hasher,
                cap_from, cap_mul, cap_round,
            )?;
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
        Command::Uninstrument { workspace, manifest } => {
            run_uninstrument(workspace, manifest)?;
        }
        Command::Wire {
            workspace,
            captrack_path,
        } => {
            run_wire(workspace, captrack_path)?;
        }
        Command::Unwire {
            workspace,
            manifest,
        } => {
            run_unwire(workspace, manifest)?;
        }
    }
    Ok(())
}

fn run_wire(
    workspace: Option<PathBuf>,
    captrack_path: Option<PathBuf>,
) -> anyhow::Result<()> {
    use crate::workspace as ws;
    let workspace_start = workspace.unwrap_or_else(|| std::env::current_dir().expect("cwd"));
    let workspace_root = ws::find_workspace_root(&workspace_start).with_context(|| {
        format!(
            "locate workspace root from {}",
            workspace_start.display()
        )
    })?;
    wire::run_wire(wire::WireArgs {
        workspace_root,
        captrack_path,
    })
}

fn run_unwire(
    workspace: Option<PathBuf>,
    manifest: Option<PathBuf>,
) -> anyhow::Result<()> {
    use crate::workspace as ws;
    let workspace_start = workspace.unwrap_or_else(|| std::env::current_dir().expect("cwd"));
    let workspace_root = ws::find_workspace_root(&workspace_start).with_context(|| {
        format!(
            "locate workspace root from {}",
            workspace_start.display()
        )
    })?;
    wire::run_unwire(wire::UnwireArgs {
        workspace_root,
        manifest,
    })
}

fn run_apply(
    profile: PathBuf,
    lint_path: Option<PathBuf>,
    workspace: Option<PathBuf>,
    dry_run: bool,
    allow_dirty: bool,
    hasher: HasherChoice,
    cap_from: CapFromChoice,
    cap_mul: f64,
    cap_round: CapRoundChoice,
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
        hasher,
        cap_from,
        cap_mul,
        cap_round,
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

fn run_uninstrument(
    workspace: Option<PathBuf>,
    manifest: Option<PathBuf>,
) -> anyhow::Result<()> {
    use crate::lint_apply::Operation;
    use crate::workspace as ws;

    // 1. Resolve workspace root (needed only when --manifest is omitted).
    let manifest_path = match manifest {
        Some(p) => p,
        None => {
            let workspace_start =
                workspace.unwrap_or_else(|| std::env::current_dir().expect("cwd"));
            let workspace_root =
                ws::find_workspace_root(&workspace_start).with_context(|| {
                    format!(
                        "locate workspace root from {}",
                        workspace_start.display()
                    )
                })?;
            lint_apply::default_instrument_manifest_path(&workspace_root)
        }
    };

    // 2. Read + parse the manifest; surface helpful errors.
    let manifest = lint_apply::read_manifest(&manifest_path)?;

    // 3. Validate the operation type.
    match &manifest.operation {
        Operation::Instrument => {
            // Correct type — proceed.
        }
        Operation::Apply { .. } => {
            return Err(anyhow::anyhow!(
                "manifest at {} is from `apply` (capacity rewrite), not `instrument`. \
                 Use `captrack-pgo undo` to revert it.",
                manifest_path.display()
            ));
        }
    }

    // 4. Delegate to the generic revert (re-reads + verifies the manifest).
    let n = lint_apply::undo_lint_apply(&manifest_path)?;

    // 5. Summary (manifest file is intentionally left in place).
    println!(
        "uninstrument: reverted {n} file{} from {}",
        if n == 1 { "" } else { "s" },
        manifest_path.display()
    );

    Ok(())
}
