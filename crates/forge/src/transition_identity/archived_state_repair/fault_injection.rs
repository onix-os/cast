use super::ArchivedStateRepairError;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ArchivedStateRepairNamespaceMove {
    PublishExisting,
    PublishMissing,
    CleanupExisting,
    CleanupMissing,
    PreserveFailedCandidate,
}

impl ArchivedStateRepairNamespaceMove {
    #[cfg(test)]
    const fn index(self) -> usize {
        match self {
            Self::PublishExisting => 0,
            Self::PublishMissing => 1,
            Self::CleanupExisting => 2,
            Self::CleanupMissing => 3,
            Self::PreserveFailedCandidate => 4,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ArchivedStateRepairFaultPoint {
    ReplacementPostCreate,
    ReplacementPreparationSync,
    QuarantinePreparationSync,
    FinalPreparationRevalidation,
    BeforePreparationReservationRetirement,
    CandidatePreSync,
    StagingPreSync,
    CanonicalPreSync,
    ReplacementPreSync,
    BeforePublication,
    AfterPublication,
    BeforeCleanup,
    AfterCleanup,
    CandidatePostSync,
    StagingPostSync,
    RetainedPayloadPostSync,
    RootsParentSync,
    QuarantineParentSync,
    FinalRevalidation,
    BeforePreservation,
    AfterPreservation,
    PreservedWrapperPostSync,
    PreservedReplacementPostSync,
    PreservationRootsSync,
    PreservationQuarantineSync,
    FinalPreservationRevalidation,
}

#[cfg(test)]
std::thread_local! {
    static FAULTS: std::cell::RefCell<Vec<ArchivedStateRepairFaultPoint>> =
        const { std::cell::RefCell::new(Vec::new()) };
    static BEFORE_PUBLICATION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static BEFORE_CLEANUP: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static BEFORE_PRESERVATION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static BETWEEN_LAYOUT_READS: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static BEFORE_SUFFIX_RETRY: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static BEFORE_NAMESPACE_SYSCALL: std::cell::RefCell<Option<(
        ArchivedStateRepairNamespaceMove,
        Box<dyn FnOnce()>,
    )>> = const { std::cell::RefCell::new(None) };
    static NAMESPACE_SYSCALL_COUNTS: std::cell::RefCell<[usize; 5]> =
        const { std::cell::RefCell::new([0; 5]) };
}

#[cfg(test)]
pub(crate) fn arm_archived_state_repair_faults(points: impl IntoIterator<Item = ArchivedStateRepairFaultPoint>) {
    let mut points = points.into_iter().collect::<Vec<_>>();
    points.reverse();
    FAULTS.with(|faults| *faults.borrow_mut() = points);
}

#[cfg(test)]
pub(crate) fn arm_before_archived_state_repair_publication(hook: impl FnOnce() + 'static) {
    BEFORE_PUBLICATION.with(|armed| *armed.borrow_mut() = Some(Box::new(hook)));
}

#[cfg(test)]
pub(crate) fn arm_before_archived_state_repair_cleanup(hook: impl FnOnce() + 'static) {
    BEFORE_CLEANUP.with(|armed| *armed.borrow_mut() = Some(Box::new(hook)));
}

#[cfg(test)]
pub(crate) fn arm_before_archived_state_repair_preservation(hook: impl FnOnce() + 'static) {
    BEFORE_PRESERVATION.with(|armed| *armed.borrow_mut() = Some(Box::new(hook)));
}

#[cfg(test)]
#[allow(dead_code)]
pub(crate) fn arm_between_archived_state_repair_layout_reads(hook: impl FnOnce() + 'static) {
    BETWEEN_LAYOUT_READS.with(|armed| *armed.borrow_mut() = Some(Box::new(hook)));
}

#[cfg(test)]
#[allow(dead_code)]
pub(crate) fn arm_before_archived_state_repair_suffix_retry(hook: impl FnOnce() + 'static) {
    BEFORE_SUFFIX_RETRY.with(|armed| *armed.borrow_mut() = Some(Box::new(hook)));
}

#[cfg(test)]
pub(crate) fn arm_before_archived_state_repair_namespace_syscall(
    operation: ArchivedStateRepairNamespaceMove,
    hook: impl FnOnce() + 'static,
) {
    NAMESPACE_SYSCALL_COUNTS.with(|counts| *counts.borrow_mut() = [0; 5]);
    BEFORE_NAMESPACE_SYSCALL.with(|armed| *armed.borrow_mut() = Some((operation, Box::new(hook))));
}

#[cfg(test)]
pub(crate) fn archived_state_repair_namespace_syscall_count(operation: ArchivedStateRepairNamespaceMove) -> usize {
    NAMESPACE_SYSCALL_COUNTS.with(|counts| counts.borrow()[operation.index()])
}

pub(super) fn before_publication() {
    #[cfg(test)]
    BEFORE_PUBLICATION.with(|armed| {
        if let Some(hook) = armed.borrow_mut().take() {
            hook();
        }
    });
}

pub(super) fn before_cleanup() {
    #[cfg(test)]
    BEFORE_CLEANUP.with(|armed| {
        if let Some(hook) = armed.borrow_mut().take() {
            hook();
        }
    });
}

pub(super) fn before_preservation() {
    #[cfg(test)]
    BEFORE_PRESERVATION.with(|armed| {
        if let Some(hook) = armed.borrow_mut().take() {
            hook();
        }
    });
}

pub(super) fn between_layout_reads() {
    #[cfg(test)]
    BETWEEN_LAYOUT_READS.with(|armed| {
        if let Some(hook) = armed.borrow_mut().take() {
            hook();
        }
    });
}

pub(super) fn before_suffix_retry() {
    #[cfg(test)]
    BEFORE_SUFFIX_RETRY.with(|armed| {
        if let Some(hook) = armed.borrow_mut().take() {
            hook();
        }
    });
}

pub(super) fn before_namespace_syscall(operation: ArchivedStateRepairNamespaceMove) {
    #[cfg(test)]
    {
        NAMESPACE_SYSCALL_COUNTS.with(|counts| counts.borrow_mut()[operation.index()] += 1);
        let hook = BEFORE_NAMESPACE_SYSCALL.with(|armed| {
            let mut armed = armed.borrow_mut();
            if armed.as_ref().is_some_and(|(expected, _)| *expected == operation) {
                armed.take().map(|(_, hook)| hook)
            } else {
                None
            }
        });
        if let Some(hook) = hook {
            hook();
        }
    }
    let _ = operation;
}

pub(super) fn checkpoint(point: ArchivedStateRepairFaultPoint) -> Result<(), ArchivedStateRepairError> {
    #[cfg(test)]
    if FAULTS.with(|faults| faults.borrow().last().copied()) == Some(point) {
        FAULTS.with(|faults| {
            faults.borrow_mut().pop();
        });
        return Err(ArchivedStateRepairError::InjectedFault { point });
    }
    let _ = point;
    Ok(())
}
