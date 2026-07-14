// SPDX-FileCopyrightText: 2023 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

pub use self::architecture::Architecture;
pub use self::env::Env;
pub use self::paths::Paths;
pub use self::policy::BuildPolicy;
pub use self::profile::Profile;
pub use self::recipe::Recipe;
pub use self::timing::Timing;

mod architecture;
mod archive;
mod build;
mod build_lock;
pub mod cli;
mod container;
mod draft;
mod env;
mod executor;
mod generated_lock;
mod linux_fs;
mod package;
mod paths;
mod planner;
mod policy;
mod profile;
mod recipe;
pub mod source_lock;
mod timing;
mod upstream;

#[cfg(any(test, feature = "delegated-fixture-test-support"))]
pub(crate) fn private_tempdir() -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt as _;

    let directory = tempfile::tempdir().expect("create private test directory");
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))
        .expect("normalize private test directory");
    directory
}

/// Harness-free entry point for the delegated contentful execution fixture.
///
/// This API is deliberately unavailable unless Mason's narrowly scoped test
/// feature is enabled. The standalone integration target uses it instead of
/// libtest so the production `clone3` path can authenticate an exact
/// single-task supervisor through `/proc/self/task`.
#[doc(hidden)]
#[cfg(feature = "delegated-fixture-test-support")]
pub mod delegated_fixture_test_support {
    use std::process;

    /// Run the selected existing contentful execution fixture under the exact
    /// validated optional-or-required capability policy supplied by Make.
    pub fn run() {
        match std::env::var("CAST_DELEGATED_FIXTURE_RUNNER") {
            Ok(value) if value == "1" => {}
            Ok(value) => panic!("CAST_DELEGATED_FIXTURE_RUNNER must be exactly `1`, found {value:?}"),
            Err(std::env::VarError::NotPresent) => {
                panic!("CAST_DELEGATED_FIXTURE_RUNNER must be set by the delegated fixture runner")
            }
            Err(std::env::VarError::NotUnicode(_)) => {
                panic!("CAST_DELEGATED_FIXTURE_RUNNER must be the UTF-8 value `1`")
            }
        }
        assert_exact_main_task("harness-free delegated fixture startup");
        match std::env::var("CAST_REQUIRE_EXECUTION") {
            Ok(value) if value == "0" || value == "1" => {}
            Ok(value) => panic!("CAST_REQUIRE_EXECUTION must be exactly `0` or `1`, found {value:?}"),
            Err(std::env::VarError::NotPresent) => {
                panic!("CAST_REQUIRE_EXECUTION must be set to exactly `0` or `1` for the delegated fixture")
            }
            Err(std::env::VarError::NotUnicode(_)) => {
                panic!("CAST_REQUIRE_EXECUTION must be the UTF-8 value `0` or `1` for the delegated fixture")
            }
        }
        crate::planner::run_delegated_execution_fixture();
        assert_exact_main_task("harness-free delegated fixture completion");
    }

    fn assert_exact_main_task(context: &str) {
        let task_directory = format!("/proc/{}/task", process::id());
        let mut tasks = fs_err::read_dir(&task_directory)
            .unwrap_or_else(|source| panic!("enumerate {task_directory}: {source}"))
            .map(|entry| {
                let entry = entry.unwrap_or_else(|source| panic!("read {task_directory} entry: {source}"));
                entry
                    .file_name()
                    .to_str()
                    .and_then(|name| name.parse::<u32>().ok())
                    .unwrap_or_else(|| panic!("non-numeric task entry in {task_directory}"))
            })
            .collect::<Vec<_>>();
        tasks.sort_unstable();
        assert_eq!(tasks, [process::id()], "{context} was not an exact single-task process");
    }
}
