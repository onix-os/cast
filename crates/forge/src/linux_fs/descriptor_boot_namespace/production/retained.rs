//! Closed production adapter for the pure boot-namespace classifier.
//!
//! The adapter borrows one caller-authenticated retained `O_PATH` directory.
//! Every name lookup remains relative to that descriptor, and the result owns
//! only scalar destination states. No descriptor, path, reopen callback, or
//! mutation capability crosses this module boundary.
//!
//! Mount identity is captured by one descriptor-relative
//! `statx(AT_EMPTY_PATH, STATX_MNT_ID)` call. Mounted boot publication already
//! has an effective Linux >= 5.10 admission boundary; this adapter does not
//! weaken generic `linux_fs` Linux 5.6 support and instead fails closed when
//! `STATX_MNT_ID` is unavailable.

use std::{fs::File, time::Instant};

use super::super::{
    classifier::assess_with_observer_until,
    model::{
        BootNamespaceAssessmentLimits, BootNamespaceDestinationState, BootNamespaceRequest,
        ValidatedBootNamespaceAssessment,
    },
    observer::BootNamespaceNodeIdentity,
};

#[path = "retained/content.rs"]
mod content;
#[path = "retained/error.rs"]
mod error;
#[path = "retained/expected.rs"]
mod expected;
#[path = "retained/hook.rs"]
mod hook;
#[path = "retained/inventory.rs"]
mod inventory;
#[path = "retained/limits.rs"]
mod limits;
#[path = "retained/node.rs"]
mod node;
#[path = "retained/observer.rs"]
mod observer;
#[path = "retained/publication_source.rs"]
mod publication_source;
#[path = "retained/syscall.rs"]
mod syscall;

pub(crate) use error::RetainedBootNamespaceAssessmentError;
pub(crate) use expected::RetainedBootNamespaceExpectedSource;
pub(crate) use limits::RetainedBootNamespaceAssessmentLimits;
pub(in crate::linux_fs) use publication_source::BoundRetainedBootFileSource;

#[cfg(test)]
pub(crate) use hook::FixtureRetainedBootNamespaceProtocolEvent;
#[cfg(test)]
pub(crate) use limits::FixtureRetainedBootNamespaceUsage;

/// Exact test-only snapshot around one failed relative open admission.
#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct FixtureFailedOpenDescriptorSlotUsage {
    pub(crate) slots_before_failed_open: usize,
    pub(crate) slots_after_failed_open: usize,
    pub(crate) peak_descriptor_slots: usize,
}

use hook::NoopRetainedBootNamespaceHook;
use observer::RetainedBootNamespaceObserver;

/// Closed live result distinct from the injected classifier evidence.
///
/// The value contains no descriptor, path, reader, callback, or authority.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct ValidatedRetainedBootNamespaceAssessment {
    assessment: ValidatedBootNamespaceAssessment,
    observed_root_identity: Option<BootNamespaceNodeIdentity>,
    #[cfg(test)]
    usage: FixtureRetainedBootNamespaceUsage,
}

impl ValidatedRetainedBootNamespaceAssessment {
    pub(crate) fn states(&self) -> &[BootNamespaceDestinationState] {
        self.assessment.states()
    }

    /// Exact scalar identity captured from the production observer's retained
    /// root descriptor. Empty request sets intentionally perform no root
    /// observation and therefore return `None`.
    pub(crate) const fn observed_root_identity(&self) -> Option<BootNamespaceNodeIdentity> {
        self.observed_root_identity
    }

    #[cfg(test)]
    pub(crate) const fn fixture_usage(&self) -> FixtureRetainedBootNamespaceUsage {
        self.usage
    }
}

/// Assess one retained, already-authenticated boot destination directory.
///
/// `retained_root` must be the exact revalidated destination `O_PATH`
/// descriptor borrowed from the attachment aggregate. The adapter verifies
/// its scalar identity again, but does not independently establish the
/// caller's mountinfo or attachment authority.
pub(crate) fn assess_retained_boot_namespace_until<'request, 'expected, 'source>(
    retained_root: &File,
    requests: &'request [BootNamespaceRequest<'request>],
    expected: &'expected [RetainedBootNamespaceExpectedSource<'source>],
    namespace_limits: BootNamespaceAssessmentLimits,
    live_limits: RetainedBootNamespaceAssessmentLimits,
    deadline: Instant,
) -> Result<ValidatedRetainedBootNamespaceAssessment, RetainedBootNamespaceAssessmentError> {
    assess_with_hook(
        retained_root,
        requests,
        expected,
        namespace_limits,
        live_limits,
        deadline,
        NoopRetainedBootNamespaceHook,
    )
}

#[cfg(test)]
pub(crate) fn assess_retained_boot_namespace_with_hook_until<'request, 'expected, 'source>(
    retained_root: &File,
    requests: &'request [BootNamespaceRequest<'request>],
    expected: &'expected [RetainedBootNamespaceExpectedSource<'source>],
    namespace_limits: BootNamespaceAssessmentLimits,
    live_limits: RetainedBootNamespaceAssessmentLimits,
    deadline: Instant,
    hook: &mut impl FnMut(FixtureRetainedBootNamespaceProtocolEvent) -> std::io::Result<()>,
) -> Result<ValidatedRetainedBootNamespaceAssessment, RetainedBootNamespaceAssessmentError> {
    assess_with_hook(
        retained_root,
        requests,
        expected,
        namespace_limits,
        live_limits,
        deadline,
        hook::FixtureHook(hook),
    )
}

/// Test-only proof that one failed `openat2` reserves and releases exactly one
/// conservative descriptor-admission slot around the physical attempt.
#[cfg(test)]
pub(crate) fn probe_failed_open_descriptor_slot_until(
    retained_root: &File,
    missing_name: &[u8],
    live_limits: RetainedBootNamespaceAssessmentLimits,
    deadline: Instant,
) -> Result<FixtureFailedOpenDescriptorSlotUsage, RetainedBootNamespaceAssessmentError> {
    let mut ledger = limits::LiveLedger::new(live_limits, deadline)?;
    ledger.reserve_descriptor_slot("binding the borrowed failed-open fixture root")?;
    let slots_before_failed_open = ledger.fixture_descriptor_slots();
    let opened = node::open_path_component(
        retained_root,
        missing_name,
        &mut ledger,
        "probing one failed relative open descriptor slot",
    );
    let outcome = match opened {
        Ok(None) => Ok(()),
        Ok(Some(file)) => {
            file.close(&mut ledger);
            Err(node::invalid_data(
                "probing one failed relative open descriptor slot",
                "fixture component unexpectedly exists",
            ))
        }
        Err(error) => Err(error),
    };
    let slots_after_failed_open = ledger.fixture_descriptor_slots();
    let peak_descriptor_slots = ledger.usage().peak_descriptor_slots;
    ledger.release_descriptor_slot();
    ledger.checkpoint()?;
    outcome?;
    Ok(FixtureFailedOpenDescriptorSlotUsage {
        slots_before_failed_open,
        slots_after_failed_open,
        peak_descriptor_slots,
    })
}

fn assess_with_hook<'root, 'request, 'expected, 'source, Hook: hook::RetainedBootNamespaceHook>(
    retained_root: &'root File,
    requests: &'request [BootNamespaceRequest<'request>],
    expected: &'expected [RetainedBootNamespaceExpectedSource<'source>],
    namespace_limits: BootNamespaceAssessmentLimits,
    live_limits: RetainedBootNamespaceAssessmentLimits,
    deadline: Instant,
    hook: Hook,
) -> Result<ValidatedRetainedBootNamespaceAssessment, RetainedBootNamespaceAssessmentError> {
    let mut observer =
        RetainedBootNamespaceObserver::new(retained_root, requests, expected, live_limits, deadline, hook)?;
    let classified = assess_with_observer_until(requests, namespace_limits, deadline, &mut observer);
    let closed = observer.finish(classified.is_ok());
    let observed_root_identity = observer.observed_root_identity();
    let adapter_failure = observer.take_failure();

    if let Some(error) = adapter_failure {
        return Err(error);
    }
    let usage = closed?;
    let (assessment, _) = classified.map_err(RetainedBootNamespaceAssessmentError::Namespace)?;
    let observed_root_identity = match (requests.is_empty(), observed_root_identity) {
        (true, None) => None,
        (false, Some(identity)) => Some(identity),
        (true, Some(_)) => {
            return Err(RetainedBootNamespaceAssessmentError::ObserverProtocol {
                reason: "empty classification unexpectedly observed a retained root",
            });
        }
        (false, None) => {
            return Err(RetainedBootNamespaceAssessmentError::ObserverProtocol {
                reason: "successful nonempty classification omitted retained-root evidence",
            });
        }
    };
    Ok(ValidatedRetainedBootNamespaceAssessment {
        assessment,
        observed_root_identity,
        #[cfg(test)]
        usage,
    })
}
