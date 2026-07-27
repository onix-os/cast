//! One place that decides how absolute boot-path budgets scale under test.
//!
//! Every timeout on the boot publication path is an *absolute* budget: it is
//! armed once and then inherited by each nested stage, so an outer budget that
//! is nearly spent shows up as `DeadlineExceeded` deep inside an inner stage
//! (typically boot-topology intent, with a few milliseconds remaining at
//! admission).
//!
//! Production policy is sized for what production does: one boot publication at
//! a time. The test suite instead runs this path in ~24 concurrent tests on a
//! shared machine, where contention alone can exhaust a 30s budget on work that
//! is not slow. That produced a persistent cluster of "only fails in a full
//! run" failures in `receipt_promotion::completion`, reproducible at
//! `--test-threads=24` and clean at 16 or below.
//!
//! Relaxing the production constants to stabilise the suite would hide a real
//! bound behind a test artefact, so the scaling lives here instead: production
//! values stay exactly as written at their definitions, and only the test build
//! multiplies them.
//!
//! This scales *absolute* budgets. It deliberately does not touch values that
//! feed an evaluation identity — see `MAX_EVALUATION_TIME`, which must stay a
//! fixed constant because it is hashed into `resource_policy_sha256`.

use std::time::Duration;

/// How much longer a test build may take for the same absolute budget.
#[cfg(test)]
const TEST_BUDGET_SCALE: u32 = 20;

/// Scale one absolute boot-path budget for the current build.
#[must_use]
pub(in crate::client) const fn boot_budget(production: Duration) -> Duration {
    #[cfg(test)]
    {
        match production.checked_mul(TEST_BUDGET_SCALE) {
            Some(scaled) => scaled,
            // Unreachable for any realistic budget; saturate rather than panic
            // in a const context.
            None => Duration::MAX,
        }
    }
    #[cfg(not(test))]
    {
        production
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_test_build_actually_scales_budgets() {
        assert_eq!(
            boot_budget(Duration::from_secs(30)),
            Duration::from_secs(30 * u64::from(TEST_BUDGET_SCALE)),
            "boot budgets are not being scaled in the test build",
        );
    }
}
