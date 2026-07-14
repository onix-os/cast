// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

//! Harness-free delegated contentful execution fixture.
//!
//! libtest owns a worker pool and therefore cannot supervise the production
//! `clone3` boundary, which deliberately audits `/proc/self/task` immediately
//! before creating a child. This target has no test harness and calls the
//! feature-gated Mason test-support entry point directly from its sole task.

fn main() {
    match std::env::var("CAST_DELEGATED_FIXTURE_RUNNER") {
        Ok(value) if value == "1" => mason::delegated_fixture_test_support::run(),
        Err(std::env::VarError::NotPresent) => {
            eprintln!("delegated execution fixture is runner-only; use `make delegated-execution-fixtures`");
        }
        Ok(value) => panic!("CAST_DELEGATED_FIXTURE_RUNNER must be exactly `1`, found {value:?}"),
        Err(std::env::VarError::NotUnicode(_)) => {
            panic!("CAST_DELEGATED_FIXTURE_RUNNER must be the UTF-8 value `1`")
        }
    }
}
