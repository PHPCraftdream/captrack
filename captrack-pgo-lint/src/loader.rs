//! Profile loader for the captrack-pgo Dylint plugin.
//!
//! Reads `CAPTRACK_PGO_PROFILE` on first use (lazy, via `OnceLock`) and
//! builds an in-memory lookup table indexed by `SiteKey`.
//!
//! # Error handling
//!
//! - Env var unset → empty profile, lint is a no-op (silent).
//! - File missing or invalid JSON → one `eprintln!` warning, then empty
//!   profile. We never panic because that would abort the build.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::OnceLock;

use crate::model::{SiteKey, SiteStats};

/// The loaded profile: a map from `SiteKey` to `SiteStats`.
///
/// Empty when the env var is unset or the file cannot be parsed.
pub type Profile = HashMap<SiteKey, SiteStats>;

static PROFILE: OnceLock<Profile> = OnceLock::new();

/// Return a reference to the process-global profile, loading it on first call.
pub fn profile() -> &'static Profile {
    PROFILE.get_or_init(load_profile)
}

fn load_profile() -> Profile {
    let path = match std::env::var("CAPTRACK_PGO_PROFILE") {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => {
            // Env var unset or empty — no-op mode.
            return Profile::new();
        }
    };

    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "captrack-pgo-lint: warning: cannot read profile `{}`: {e}",
                path.display()
            );
            return Profile::new();
        }
    };

    let stats: Vec<SiteStats> = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            eprintln!(
                "captrack-pgo-lint: warning: cannot parse profile `{}`: {e}",
                path.display()
            );
            return Profile::new();
        }
    };

    stats.into_iter().map(|s| (s.key.clone(), s)).collect()
}
