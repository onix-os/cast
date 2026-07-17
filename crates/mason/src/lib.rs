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

#[cfg(any(
    test,
    feature = "cache-clean-test-support",
    feature = "delegated-fixture-test-support"
))]
pub(crate) fn private_tempdir() -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt as _;

    let directory = tempfile::tempdir().expect("create private test directory");
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))
        .expect("normalize private test directory");
    directory
}

/// Harness-free entry point for the descriptor-anchored cache-clean proof.
///
/// This API is deliberately unavailable unless Mason's narrowly scoped test
/// feature is enabled. The standalone integration target uses it instead of
/// libtest so the production container boundary can authenticate an exact
/// single-task supervisor immediately before its fork-like clone.
#[doc(hidden)]
#[cfg(feature = "cache-clean-test-support")]
pub mod cache_clean_test_support {
    /// Prove cache cleaning preserves both its retained root and the target of
    /// a symlink stored inside that root.
    pub fn run() {
        match std::env::var("CAST_CACHE_CLEAN_TEST_RUNNER") {
            Ok(value) if value == "1" => {}
            Ok(value) => panic!("CAST_CACHE_CLEAN_TEST_RUNNER must be exactly `1`, found {value:?}"),
            Err(std::env::VarError::NotPresent) => {
                panic!("CAST_CACHE_CLEAN_TEST_RUNNER must be set by the cache-clean test runner")
            }
            Err(std::env::VarError::NotUnicode(_)) => {
                panic!("CAST_CACHE_CLEAN_TEST_RUNNER must be the UTF-8 value `1`")
            }
        }
        crate::cli::run_harness_free_cache_clean_test();
    }
}

/// Harness-free entry point for the delegated contentful execution fixture.
///
/// This API is deliberately unavailable unless Mason's narrowly scoped test
/// feature is enabled. The standalone integration target uses it instead of
/// libtest so the production `clone3` path can authenticate an exact
/// single-task supervisor through `/proc/self/task`.
#[cfg(any(test, feature = "delegated-fixture-test-support"))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ExecutionRequirement {
    Optional,
    Required,
}

#[cfg(any(test, feature = "delegated-fixture-test-support"))]
#[derive(Debug)]
enum DelegatedPreflightOutcome<T> {
    Executed(T),
    Skipped(container::Error),
}

#[cfg(any(test, feature = "delegated-fixture-test-support"))]
fn parse_execution_requirement(value: Option<&std::ffi::OsStr>) -> Result<ExecutionRequirement, String> {
    let Some(value) = value else {
        return Err("CAST_REQUIRE_EXECUTION must be set to exactly `0` or `1`".to_owned());
    };
    let value = value
        .to_str()
        .ok_or_else(|| "CAST_REQUIRE_EXECUTION must be the UTF-8 value `0` or `1`".to_owned())?;
    match value {
        "0" => Ok(ExecutionRequirement::Optional),
        "1" => Ok(ExecutionRequirement::Required),
        value => Err(format!(
            "CAST_REQUIRE_EXECUTION must be exactly `0` or `1`, found {value:?}"
        )),
    }
}

#[cfg(any(test, feature = "delegated-fixture-test-support"))]
fn run_after_delegated_preflight<T>(
    requirement: ExecutionRequirement,
    preflight: Result<(), container::Error>,
    execute: impl FnOnce() -> T,
) -> Result<DelegatedPreflightOutcome<T>, container::Error> {
    match preflight {
        Ok(()) => Ok(DelegatedPreflightOutcome::Executed(execute())),
        Err(error)
            if requirement == ExecutionRequirement::Optional
                && container::execution_namespace_capability_unavailable(&error) =>
        {
            Ok(DelegatedPreflightOutcome::Skipped(error))
        }
        Err(error) => Err(error),
    }
}

#[doc(hidden)]
#[cfg(feature = "delegated-fixture-test-support")]
pub mod delegated_fixture_test_support {
    use std::process;

    use super::{DelegatedPreflightOutcome, ExecutionRequirement};

    /// Exercise the exact production clone3, cgroup, ID-map, credential, and
    /// mount path without reading or materializing a bootstrap package store.
    /// This is required-only: a fast host gate must never turn an unavailable
    /// production capability into a successful pre-download result.
    pub fn preflight() {
        require_runner_marker();
        assert_exact_main_task("harness-free delegated capability preflight startup");
        let execution_requirement =
            super::parse_execution_requirement(std::env::var_os("CAST_REQUIRE_EXECUTION").as_deref())
                .unwrap_or_else(|message| panic!("{message}"));
        assert_eq!(
            execution_requirement,
            ExecutionRequirement::Required,
            "delegated capability preflight requires CAST_REQUIRE_EXECUTION=1"
        );
        match crate::container::preflight_delegated_execution_capability() {
            Ok(()) => {}
            Err(error) if crate::container::execution_namespace_capability_unavailable(&error) => {
                panic!(
                    "required execution-capability preflight failed before package/root materialization; enable unprivileged user namespaces and permit isolated setgroups and mount setup for the delegated service: {}",
                    error_chain(&error)
                );
            }
            Err(error) => panic!(
                "delegated execution-capability preflight failed before package/root materialization: {}",
                error_chain(&error)
            ),
        }
        assert_exact_main_task("harness-free delegated capability preflight completion");
    }

    /// Run the selected existing contentful execution fixture under the exact
    /// validated optional-or-required capability policy supplied by Make.
    pub fn run() {
        require_runner_marker();
        assert_exact_main_task("harness-free delegated fixture startup");
        let execution_requirement =
            super::parse_execution_requirement(std::env::var_os("CAST_REQUIRE_EXECUTION").as_deref())
                .unwrap_or_else(|message| panic!("{message}"));
        let outcome = super::run_after_delegated_preflight(
            execution_requirement,
            crate::container::preflight_delegated_execution_capability(),
            crate::planner::run_delegated_execution_fixture,
        );
        match outcome {
            Ok(DelegatedPreflightOutcome::Executed(())) => {}
            Ok(DelegatedPreflightOutcome::Skipped(error)) => {
                assert_exact_main_task("after optional delegated execution-capability denial");
                eprintln!(
                    "SKIP delegated execution fixture: host denied required production user/mount namespace setup before package/root materialization: {}",
                    error_chain(&error)
                );
                return;
            }
            Err(error)
                if execution_requirement == ExecutionRequirement::Required
                    && crate::container::execution_namespace_capability_unavailable(&error) =>
            {
                panic!(
                    "required execution-capability preflight failed before package/root materialization; enable unprivileged user namespaces and permit isolated setgroups and mount setup for the delegated service: {}",
                    error_chain(&error)
                );
            }
            Err(error) => panic!(
                "delegated execution-capability preflight failed before package/root materialization: {}",
                error_chain(&error)
            ),
        }
        assert_exact_main_task("harness-free delegated fixture completion");
    }

    fn require_runner_marker() {
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
    }

    fn error_chain(error: &(dyn std::error::Error + 'static)) -> String {
        let mut messages = vec![error.to_string()];
        let mut source = error.source();
        while let Some(error) = source {
            messages.push(error.to_string());
            source = error.source();
        }
        messages.join(": ")
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

#[cfg(test)]
mod delegated_preflight_tests {
    use std::{cell::Cell, os::unix::ffi::OsStrExt as _};

    use super::*;

    fn setgroups_denial() -> container::Error {
        container::Error::Container(::container::Error::Failure {
            message: "clear inherited supplementary groups: EPERM: Operation not permitted".to_owned(),
        })
    }

    #[test]
    fn execution_requirement_rejects_missing_or_invalid_values() {
        assert!(parse_execution_requirement(None).is_err());
        assert_eq!(
            parse_execution_requirement(Some(std::ffi::OsStr::new("0"))).unwrap(),
            ExecutionRequirement::Optional
        );
        assert_eq!(
            parse_execution_requirement(Some(std::ffi::OsStr::new("1"))).unwrap(),
            ExecutionRequirement::Required
        );
        assert!(parse_execution_requirement(Some(std::ffi::OsStr::new(""))).is_err());
        assert!(parse_execution_requirement(Some(std::ffi::OsStr::new("yes"))).is_err());
        assert!(parse_execution_requirement(Some(std::ffi::OsStr::from_bytes(&[0xff]))).is_err());
    }

    #[test]
    fn successful_preflight_executes_fixture_materialization_once_for_both_policies() {
        for requirement in [ExecutionRequirement::Optional, ExecutionRequirement::Required] {
            let materializations = Cell::new(0_u8);
            let outcome = run_after_delegated_preflight(requirement, Ok(()), || {
                materializations.set(materializations.get() + 1);
            })
            .unwrap();

            assert!(matches!(outcome, DelegatedPreflightOutcome::Executed(())));
            assert_eq!(materializations.get(), 1);
        }
    }

    #[test]
    fn optional_capability_denial_short_circuits_before_fixture_materialization() {
        let materialized = Cell::new(false);
        let outcome = run_after_delegated_preflight(ExecutionRequirement::Optional, Err(setgroups_denial()), || {
            materialized.set(true)
        })
        .unwrap();

        let DelegatedPreflightOutcome::Skipped(error) = outcome else {
            panic!("optional denial did not report a skipped preflight");
        };
        assert!(container::execution_namespace_capability_unavailable(&error));
        assert!(!materialized.get(), "optional denial entered fixture materialization");
    }

    #[test]
    fn required_capability_denial_fails_before_fixture_materialization() {
        let materialized = Cell::new(false);
        let outcome = run_after_delegated_preflight(ExecutionRequirement::Required, Err(setgroups_denial()), || {
            materialized.set(true)
        });

        assert!(outcome.is_err());
        assert!(!materialized.get(), "required denial entered fixture materialization");
    }
}
