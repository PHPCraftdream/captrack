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
        Command::Apply {
            workspace,
            heap,
            captrack_dump,
            commit,
        } => {
            run_apply(workspace, heap, captrack_dump, commit)?;
        }
        Command::Undo { manifest } => {
            run_undo(manifest)?;
        }
        Command::Auto {
            workspace,
            heap,
            captrack_dump,
            apply,
        } => {
            run_auto(workspace, heap, captrack_dump, apply)?;
        }
    }
    Ok(())
}

fn relativize(path: &std::path::Path, root: &std::path::Path) -> PathBuf {
    path.strip_prefix(root)
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|_| path.to_path_buf())
}

/// Shared helper: resolve workspace root, load profile, scan AST, build plan.
/// Returns (workspace_root, plan).
fn build_plan_from_profile(
    workspace: Option<PathBuf>,
    heap: Option<PathBuf>,
    captrack_dump: Option<PathBuf>,
) -> anyhow::Result<(PathBuf, crate::model::PatchPlan)> {
    use crate::{plan, profile, scan, workspace as ws};
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

    let built = plan::build_plan(sites, stats);
    Ok((root, built))
}

fn run_propose(
    workspace: Option<PathBuf>,
    heap: Option<PathBuf>,
    captrack_dump: Option<PathBuf>,
) -> anyhow::Result<()> {
    use crate::report;

    let (root, mut plan) = build_plan_from_profile(workspace, heap, captrack_dump)?;

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

fn run_undo(manifest: Option<PathBuf>) -> anyhow::Result<()> {
    use crate::workspace as ws;
    let path = match manifest {
        Some(p) => p,
        None => {
            let root = ws::find_workspace_root(&std::env::current_dir()?)?;
            crate::undo::default_manifest_path(&root)
        }
    };
    let n = crate::undo::undo_from(&path)?;
    println!(
        "reverted {n} patch{} from {}",
        if n == 1 { "" } else { "es" },
        path.display()
    );
    Ok(())
}

fn run_auto(
    workspace: Option<PathBuf>,
    heap: Option<PathBuf>,
    captrack_dump: Option<PathBuf>,
    apply: bool,
) -> anyhow::Result<()> {
    // 1. Build plan via the shared helper.
    let (root, plan) = build_plan_from_profile(workspace, heap, captrack_dump)?;

    // 2. Always print the report (with relativized paths for readability).
    {
        let mut display_plan = plan.clone();
        for entry in &mut display_plan.entries {
            entry.key.file = relativize(&entry.key.file, &root);
        }
        for (key, _) in &mut display_plan.skipped {
            key.file = relativize(&key.file, &root);
        }
        print!("{}", crate::report::render_report(&display_plan));
    }

    // 3. If --apply and there are entries, run apply.
    if apply && !plan.entries.is_empty() {
        let manifest = crate::apply::apply_plan(&plan, &root, false)?;
        let manifest_path = root
            .join("target")
            .join("captrack-pgo")
            .join("last-apply.json");
        println!();
        println!(
            "applied {} patch{}",
            manifest.entries.len(),
            if manifest.entries.len() == 1 {
                ""
            } else {
                "es"
            }
        );
        println!("  manifest: {}", manifest_path.display());
        println!("  undo with: captrack-pgo undo");
    } else if apply {
        println!();
        println!("(plan is empty — nothing to apply)");
    } else {
        println!();
        println!("(dry-run; pass --apply to commit changes)");
    }

    Ok(())
}

fn run_apply(
    workspace: Option<PathBuf>,
    heap: Option<PathBuf>,
    captrack_dump: Option<PathBuf>,
    commit: bool,
) -> anyhow::Result<()> {
    use crate::{apply, report};

    let (root, plan) = build_plan_from_profile(workspace, heap, captrack_dump)?;

    // Always print the human-readable report so the user sees what would change.
    {
        let mut display_plan = plan.clone();
        for entry in &mut display_plan.entries {
            entry.key.file = relativize(&entry.key.file, &root);
        }
        for (key, _) in &mut display_plan.skipped {
            key.file = relativize(&key.file, &root);
        }
        print!("{}", report::render_report(&display_plan));
    }

    if commit && !plan.entries.is_empty() {
        let manifest = apply::apply_plan(&plan, &root, false)?;
        let manifest_path = root
            .join("target")
            .join("captrack-pgo")
            .join("last-apply.json");
        println!(
            "Applied {} patch(es). Manifest written to {}",
            manifest.entries.len(),
            manifest_path.display()
        );
    } else if plan.entries.is_empty() {
        println!("Nothing to apply.");
    } else {
        // dry-run
        let manifest = apply::apply_plan(&plan, &root, true)?;
        println!(
            "(dry-run; {} patch(es) would be applied — pass --commit to write changes)",
            manifest.entries.len()
        );
    }

    Ok(())
}
