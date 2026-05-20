//! Reserved package spelling for the existing DemoSwarm project.
//!
//! The canonical future installer crate is `demoswarm`.
//!
//! This `0.0.1` crate reserves the repository-style `demo-swarm` spelling and
//! does not expose the production installer.

/// Returns the current crates.io package status.
#[must_use]
pub const fn status() -> &'static str {
    "demo-swarm crates.io reservation"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_identifies_reservation_package() {
        assert_eq!(status(), "demo-swarm crates.io reservation");
    }
}
