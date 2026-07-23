//! Harness-free delegated contentful execution fixture.
//!
//! libtest owns a worker pool and therefore cannot supervise the production
//! `clone3` boundary, which deliberately audits `/proc/self/task` immediately
//! before creating a child. This target has no test harness and calls the
//! feature-gated Mason test-support entry point directly from its sole task.

use std::env;

const PREFLIGHT_ONLY_ENV: &str = "CAST_DELEGATED_PREFLIGHT_ONLY";

fn main() {
    match env::var("CAST_DELEGATED_FIXTURE_RUNNER") {
        Ok(value) if value == "1" => match env::var(PREFLIGHT_ONLY_ENV) {
            Ok(value) if value == "1" => mason::delegated_fixture_test_support::preflight(),
            Ok(value) if value == "0" => mason::delegated_fixture_test_support::run(),
            Ok(value) => {
                panic!("{PREFLIGHT_ONLY_ENV} must be exactly `0` or `1`, found {value:?}")
            }
            Err(env::VarError::NotPresent) => {
                panic!("{PREFLIGHT_ONLY_ENV} must be set explicitly by the delegated fixture runner")
            }
            Err(env::VarError::NotUnicode(_)) => {
                panic!("{PREFLIGHT_ONLY_ENV} must be the UTF-8 value `0` or `1`")
            }
        },
        Err(env::VarError::NotPresent) => {
            eprintln!("delegated execution fixture is runner-only; use `make delegated-execution-fixtures`");
        }
        Ok(value) => panic!("CAST_DELEGATED_FIXTURE_RUNNER must be exactly `1`, found {value:?}"),
        Err(env::VarError::NotUnicode(_)) => {
            panic!("CAST_DELEGATED_FIXTURE_RUNNER must be the UTF-8 value `1`")
        }
    }
}
