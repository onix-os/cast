//! Retain and retire one fully prepared fixture execution.
//!
//! This boundary begins only after `Runtime::setup` returns a complete
//! `PreparedExecution`. A setup failure remains fatal to the matrix and is
//! contained by its descriptor-retained `BootstrapTempRoot`; it cannot enter
//! the continuing two-execution campaign.

use std::{error::Error as StdError, fmt};

use stone_recipe::derivation::DerivationPlan;

use super::{Planned, Publication};
use crate::{
    Timing,
    build::{PreparedExecution, Runtime},
    executor::Executor,
    package::{self, FrozenPackager},
    paths::ExecutionLock,
};

type FixtureExecutionError = Box<dyn StdError + Send + Sync>;

#[derive(Debug, thiserror::Error)]
enum FrozenExamplePayloadError {
    #[error("execute frozen example")]
    Execute(#[from] crate::executor::Error),
    #[error("package frozen example")]
    Package(#[from] package::Error),
}

#[derive(Debug, thiserror::Error)]
enum FixtureExecutionLifecycleError {
    #[error("fixture operation failed: {primary}; exact runtime cleanup also failed: {cleanup}")]
    PrimaryAndCleanup {
        #[source]
        primary: FixtureExecutionError,
        cleanup: FixtureExecutionError,
    },
    #[error("exact runtime cleanup failed: {cleanup}")]
    Cleanup {
        #[source]
        cleanup: FixtureExecutionError,
    },
}

pub(super) fn cleanup_failed(error: &(dyn StdError + 'static)) -> bool {
    error.downcast_ref::<FixtureExecutionLifecycleError>().is_some()
}

pub(super) trait FixtureCleanup {
    fn cleanup(self) -> Result<(), FixtureExecutionError>;
}

pub(super) struct RuntimeExecutionCleanup<'planned> {
    runtime: &'planned Runtime,
    plan: &'planned DerivationPlan,
    execution_lock: ExecutionLock,
    prepared: PreparedExecution,
}

impl RuntimeExecutionCleanup<'_> {
    fn execution_lock(&self) -> &ExecutionLock {
        &self.execution_lock
    }

    fn prepared(&self) -> &PreparedExecution {
        &self.prepared
    }
}

impl FixtureCleanup for RuntimeExecutionCleanup<'_> {
    fn cleanup(self) -> Result<(), FixtureExecutionError> {
        self.runtime
            .cleanup(self.plan, &self.execution_lock, self.prepared)
            .map_err(|error| Box::new(error) as FixtureExecutionError)
    }
}

struct ImmediateCleanup<C: FixtureCleanup> {
    operation: Option<C>,
}

impl<C: FixtureCleanup> ImmediateCleanup<C> {
    fn new(operation: C) -> Self {
        Self {
            operation: Some(operation),
        }
    }

    fn operation(&self) -> &C {
        self.operation
            .as_ref()
            .expect("fixture cleanup operation was already consumed")
    }

    fn cleanup_once(&mut self) -> Result<(), FixtureExecutionError> {
        self.operation
            .take()
            .expect("fixture cleanup operation was already consumed")
            .cleanup()
    }
}

impl<C: FixtureCleanup> Drop for ImmediateCleanup<C> {
    fn drop(&mut self) {
        let Some(operation) = self.operation.take() else {
            return;
        };
        if let Err(error) = operation.cleanup() {
            eprintln!("exact fixture runtime cleanup failed during drop fallback: {error}");
            if !std::thread::panicking() {
                panic!("exact fixture runtime cleanup failed during drop fallback: {error}");
            }
        }
    }
}

#[must_use = "a successful fixture execution must be explicitly cleaned after its evidence is captured"]
pub(super) struct RetainedExecutionSession<C: FixtureCleanup> {
    publication: Publication,
    cleanup: ImmediateCleanup<C>,
}

impl<C: FixtureCleanup> RetainedExecutionSession<C> {
    fn new(publication: Publication, cleanup: ImmediateCleanup<C>) -> Self {
        Self {
            publication,
            cleanup,
        }
    }

    pub(super) fn publication(&self) -> Publication {
        self.publication
    }

    pub(super) fn cleanup(mut self) -> Result<Publication, FixtureExecutionError> {
        self.cleanup.cleanup_once().map_err(|cleanup| {
            Box::new(FixtureExecutionLifecycleError::Cleanup { cleanup }) as FixtureExecutionError
        })?;
        Ok(self.publication)
    }
}

impl<C: FixtureCleanup> fmt::Debug for RetainedExecutionSession<C> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RetainedExecutionSession")
            .field("publication", &self.publication)
            .field("cleanup_pending", &self.cleanup.operation.is_some())
            .finish()
    }
}

fn operation_failed<C: FixtureCleanup>(
    primary: FixtureExecutionError,
    mut cleanup: ImmediateCleanup<C>,
) -> FixtureExecutionError {
    match cleanup.cleanup_once() {
        Ok(()) => primary,
        Err(cleanup) => Box::new(FixtureExecutionLifecycleError::PrimaryAndCleanup { primary, cleanup }),
    }
}

pub(super) fn execute_and_publish(
    planned: &Planned,
) -> Result<RetainedExecutionSession<RuntimeExecutionCleanup<'_>>, FixtureExecutionError> {
    let executor = Executor::new(&planned.plan)?;
    let packager = FrozenPackager::from_plan(&planned.runtime.paths, &planned.plan)?;
    let execution_lock = planned.runtime.acquire_execution_lock(&planned.plan)?;
    let mut timing = Timing::default();
    let initialize_timer = timing.begin(crate::timing::Kind::Initialize);
    let prepared = planned
        .runtime
        .setup(&planned.plan, &execution_lock, &mut timing, initialize_timer)?;
    let cleanup = RuntimeExecutionCleanup {
        runtime: &planned.runtime,
        plan: &planned.plan,
        execution_lock,
        prepared,
    };
    let cleanup = ImmediateCleanup::new(cleanup);

    let operation = (|| -> Result<Publication, FixtureExecutionError> {
        let retained = cleanup.operation();
        retained
            .prepared()
            .require_for(&planned.runtime.paths, &planned.plan)?;
        crate::container::exec_frozen::<FrozenExamplePayloadError>(
            &planned.runtime.paths,
            &planned.plan,
            retained.execution_lock(),
            retained.prepared().sandbox(),
            retained.prepared().root_guard(),
            |permit| {
                executor.run(&mut timing)?;
                packager.package(permit, &mut timing)?;
                Ok(())
            },
        )?;

        Ok(package::publish_artefacts(
            &planned.runtime.paths,
            &planned.plan,
            retained.execution_lock(),
            retained.prepared().artefacts()?,
            package::ManifestVerification::None,
        )?)
    })();

    match operation {
        Ok(publication) => Ok(RetainedExecutionSession::new(publication, cleanup)),
        Err(primary) => Err(operation_failed(primary, cleanup)),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        panic::{AssertUnwindSafe, catch_unwind},
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use super::*;

    #[derive(Debug, thiserror::Error)]
    #[error("{0}")]
    struct SyntheticError(&'static str);

    struct SyntheticCleanup {
        calls: Arc<AtomicUsize>,
        failure: Option<&'static str>,
    }

    impl FixtureCleanup for SyntheticCleanup {
        fn cleanup(self) -> Result<(), FixtureExecutionError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            match self.failure {
                Some(message) => Err(Box::new(SyntheticError(message))),
                None => Ok(()),
            }
        }
    }

    fn cleanup(failure: Option<&'static str>) -> (ImmediateCleanup<SyntheticCleanup>, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let cleanup = ImmediateCleanup::new(SyntheticCleanup {
            calls: Arc::clone(&calls),
            failure,
        });
        (cleanup, calls)
    }

    fn primary(message: &'static str) -> FixtureExecutionError {
        Box::new(SyntheticError(message))
    }

    fn capability_denial() -> FixtureExecutionError {
        Box::new(crate::container::Error::Container(
            ::container::Error::CloneNamespaces {
                source: nix::errno::Errno::EPERM,
            },
        ))
    }

    #[test]
    fn explicit_success_cleanup_runs_exactly_once_and_preserves_publication() {
        let (cleanup, calls) = cleanup(None);
        let session = RetainedExecutionSession::new(Publication::Published, cleanup);

        assert_eq!(session.publication(), Publication::Published);
        assert_eq!(session.cleanup().unwrap(), Publication::Published);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn execution_package_and_publication_errors_cleanup_once_and_preserve_primary() {
        for phase in ["execution failed", "package failed", "publication failed"] {
            let (cleanup, calls) = cleanup(None);
            let error = operation_failed(primary(phase), cleanup);

            assert_eq!(error.to_string(), phase);
            assert_eq!(calls.load(Ordering::SeqCst), 1, "{phase}");
        }
    }

    #[test]
    fn operation_and_cleanup_failures_are_both_preserved() {
        let (cleanup, calls) = cleanup(Some("cleanup failed"));
        let error = operation_failed(primary("package failed"), cleanup);
        let message = error.to_string();

        assert!(message.contains("package failed"));
        assert!(message.contains("cleanup failed"));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn capability_primary_with_successful_cleanup_remains_an_optional_skip() {
        let (cleanup, calls) = cleanup(None);
        let error = operation_failed(capability_denial(), cleanup);

        assert!(super::super::container_capability_unavailable(error.as_ref()));
        assert!(!cleanup_failed(error.as_ref()));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn capability_primary_with_cleanup_failure_is_fatal_and_preserves_both_diagnostics() {
        let (cleanup, calls) = cleanup(Some("cleanup failed"));
        let error = operation_failed(capability_denial(), cleanup);
        let message = error.to_string();

        assert!(!super::super::container_capability_unavailable(error.as_ref()));
        assert!(cleanup_failed(error.as_ref()));
        assert!(message.contains("clone isolated namespaces"));
        assert!(message.contains("cleanup failed"));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn explicit_cleanup_failure_blocks_a_success_result_without_retrying() {
        let (cleanup, calls) = cleanup(Some("cleanup failed"));
        let session = RetainedExecutionSession::new(Publication::Reused, cleanup);

        let error = session.cleanup().unwrap_err();
        assert!(error.to_string().contains("exact runtime cleanup failed"));
        assert!(error.to_string().contains("cleanup failed"));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn forgotten_success_session_uses_immediate_drop_fallback_exactly_once() {
        let (cleanup, calls) = cleanup(None);
        drop(RetainedExecutionSession::new(Publication::Published, cleanup));

        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn assertion_panic_uses_immediate_drop_fallback_exactly_once() {
        let (cleanup, calls) = cleanup(None);
        let panic = catch_unwind(AssertUnwindSafe(|| {
            let _session = RetainedExecutionSession::new(Publication::Published, cleanup);
            panic!("post-publication assertion failed");
        }));

        assert!(panic.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn post_publication_validation_error_uses_drop_fallback_and_preserves_the_error() {
        fn validate(cleanup: ImmediateCleanup<SyntheticCleanup>) -> Result<(), SyntheticError> {
            let _session = RetainedExecutionSession::new(Publication::Published, cleanup);
            Err(SyntheticError("bundle assertion failed"))
        }

        let (cleanup, calls) = cleanup(None);
        let error = validate(cleanup).unwrap_err();

        assert_eq!(error.to_string(), "bundle assertion failed");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn drop_cleanup_failure_panics_and_cannot_produce_success_proof() {
        let (cleanup, calls) = cleanup(Some("cleanup failed"));
        let panic = catch_unwind(AssertUnwindSafe(|| {
            drop(RetainedExecutionSession::new(Publication::Published, cleanup));
        }));

        assert!(panic.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
