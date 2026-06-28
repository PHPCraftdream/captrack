//! CLI definitions for captrack-pgo.
//!
//! Command stubs only; implementations are wired in by later steps
//! (propose → Step 10, apply → Step 11, undo → Step 12, auto → Step 13).
//! `measure` is currently a placeholder for an optional future "run my bench
//! with a profiler attached" convenience — primary workflow is to pass an
//! already-collected `--heap <json>` to `propose`/`apply`/`auto`.

use clap::{Parser, Subcommand};
use std::path::PathBuf;

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

    /// Print a data-driven patch plan from a profile snapshot.  Dry-run only.
    Propose {
        /// Workspace root (defaults to current directory).
        #[arg(long)]
        workspace: Option<PathBuf>,
        /// Path to a dhat-heap JSON dump.
        #[arg(long, conflicts_with = "captrack_dump")]
        heap: Option<PathBuf>,
        /// Path to a captrack dump JSON.
        #[arg(long, conflicts_with = "heap")]
        captrack_dump: Option<PathBuf>,
    },

    /// Apply the patch plan in-place, writing a manifest for `undo`.
    Apply {
        #[arg(long)]
        workspace: Option<PathBuf>,
        #[arg(long, conflicts_with = "captrack_dump")]
        heap: Option<PathBuf>,
        #[arg(long, conflicts_with = "heap")]
        captrack_dump: Option<PathBuf>,
        /// Actually write changes; without this only a dry-run plan is printed.
        #[arg(long)]
        commit: bool,
    },

    /// Roll back the most recent `apply` using its manifest.
    Undo {
        /// Manifest file (defaults to `target/captrack-pgo/last-apply.json`).
        #[arg(long)]
        manifest: Option<PathBuf>,
    },

    /// Convenience: propose + apply in one call.
    Auto {
        #[arg(long)]
        workspace: Option<PathBuf>,
        #[arg(long, conflicts_with = "captrack_dump")]
        heap: Option<PathBuf>,
        #[arg(long, conflicts_with = "heap")]
        captrack_dump: Option<PathBuf>,
        /// Without this only a dry-run plan is printed.
        #[arg(long)]
        apply: bool,
    },
}

pub fn dispatch(cli: Cli) -> anyhow::Result<()> {
    match cli.command {
        Command::Measure { bench } => {
            eprintln!("measure --bench {bench}: not yet implemented");
        }
        Command::Propose {
            workspace,
            heap,
            captrack_dump,
        } => {
            run_propose(workspace, heap, captrack_dump)?;
        }
        Command::Apply { .. } => {
            eprintln!("apply: not yet implemented");
        }
        Command::Undo { .. } => {
            eprintln!("undo: not yet implemented");
        }
        Command::Auto { .. } => {
            eprintln!("auto: not yet implemented");
        }
    }
    Ok(())
}

fn relativize(path: &std::path::Path, root: &std::path::Path) -> PathBuf {
    path.strip_prefix(root)
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|_| path.to_path_buf())
}

fn run_propose(
    workspace: Option<PathBuf>,
    heap: Option<PathBuf>,
    captrack_dump: Option<PathBuf>,
) -> anyhow::Result<()> {
    use crate::{plan, profile, report, scan, workspace as ws};
    use anyhow::Context;
    use profile::Profile;

    let start = workspace.unwrap_or_else(|| std::env::current_dir().expect("cwd"));
    let root = ws::find_workspace_root(&start)
        .with_context(|| format!("locate workspace root from {}", start.display()))?;

    // Load profile.
    let stats = match (heap, captrack_dump) {
        (Some(h), None) => {
            let p = profile::dhat::DhatProfile::new(h, root.clone());
            p.sites()?
        }
        (None, Some(c)) => {
            let p = profile::captrack::CaptrackProfile::new(c);
            p.sites()?
        }
        (None, None) => {
            anyhow::bail!("one of --heap or --captrack-dump is required");
        }
        (Some(_), Some(_)) => unreachable!("clap conflicts_with enforces this"),
    };

    // Scan workspace.
    let mut sites = Vec::new();
    for file in ws::walk_rust_files(&root) {
        match scan::scan_file(&file, false) {
            Ok(mut s) => sites.append(&mut s),
            Err(e) => eprintln!(
                "captrack-pgo: warning: skip {} (parse error: {})",
                file.display(),
                e
            ),
        }
    }

    let mut plan = plan::build_plan(sites, stats);

    // Relativize paths against workspace root for readability.
    for entry in &mut plan.entries {
        entry.key.file = relativize(&entry.key.file, &root);
    }
    for (key, _) in &mut plan.skipped {
        key.file = relativize(&key.file, &root);
    }

    print!("{}", report::render_report(&plan));
    Ok(())
}
