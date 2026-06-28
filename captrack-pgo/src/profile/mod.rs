//! Profile backends — sources of `SiteStats` for the planner.
//!
//! Each backend implements [`Profile::sites`] returning aggregated per-site
//! statistics.  The planner consumes the trait, so the rest of the pipeline
//! is backend-agnostic.

use crate::model::SiteStats;

pub mod captrack;
pub mod dhat;

pub trait Profile {
    fn sites(&self) -> anyhow::Result<Vec<SiteStats>>;
}
