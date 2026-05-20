//! Crates.io reservation package for the existing DemoSwarm project.
//!
//! DemoSwarm already exists as an EffortlessMetrics SDLC pack for Claude Code
//! and agentic development workflows.
//!
//! This `0.0.1` crate reserves the canonical future installer package. It does
//! not expose the production installer yet.

/// Returns the current crates.io package status.
#[must_use]
pub const fn status() -> &'static str {
    "demoswarm crates.io reservation"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_identifies_reservation_package() {
        assert_eq!(status(), "demoswarm crates.io reservation");
    }
}
