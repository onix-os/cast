//! Exact namespace authority for completing a crash-interrupted live root ABI.
//!
//! `UsrExchanged` is the durable predecessor of `RootLinksComplete`. The five
//! merged-/usr links are published individually, so interruption can leave any
//! canonical subset while the predecessor remains durable. Admission retains
//! the installation root and accepts no namespace change other than monotonic
//! publication of absent links with their exact canonical targets.

use std::io;

use crate::{
    Installation,
    transition_journal::{Phase, StorageError, TransitionJournalStore, TransitionRecord},
};

use super::{
    capture::{CaptureError, NamespaceSnapshot, capture_snapshot},
    policy::{NamespacePolicyConflict, UsrExchangeLayout, assess_snapshot_layout},
};

#[derive(Debug)]
pub(in crate::client::startup_reconciliation) enum UsrExchangedRootAbiNamespaceAdmission {
    Complete(UsrExchangedRootAbiNamespaceProof),
    Incomplete(UsrExchangedRootAbiNamespaceProof),
}

#[derive(Debug)]
pub(in crate::client::startup_reconciliation) struct UsrExchangedRootAbiNamespaceInspection {
    before: NamespaceSnapshot,
}

#[derive(Debug)]
pub(in crate::client::startup_reconciliation) struct UsrExchangedRootAbiNamespaceProof {
    before: NamespaceSnapshot,
    after: NamespaceSnapshot,
}

/// Applied publication evidence retained through the authority's database and
/// journal close. The publisher capability pins both pre-existing and newly
/// created links, preventing an exact-target ABA from being mistaken for the
/// link set that was actually published.
#[derive(Debug)]
pub(in crate::client::startup_reconciliation) struct UsrExchangedRootAbiAppliedNamespace {
    published: crate::client::RetainedRootAbi,
    completed: NamespaceSnapshot,
}

/// A complete namespace captured after the retained root directory crossed a
/// successful durability boundary.
#[derive(Debug)]
pub(in crate::client::startup_reconciliation) struct UsrExchangedRootAbiDurableNamespace {
    completed: NamespaceSnapshot,
}

impl UsrExchangedRootAbiNamespaceInspection {
    pub(in crate::client::startup_reconciliation) fn begin(
        installation: &Installation,
        journal: &TransitionJournalStore,
        expected: &TransitionRecord,
    ) -> Result<Self, UsrExchangedRootAbiNamespaceError> {
        require_exact_journal(journal, expected)?;
        let before = capture_snapshot(installation, expected)?;
        require_usr_exchanged_post(expected, &before)?;
        before.revalidate_retained()?;
        Ok(Self { before })
    }

    pub(in crate::client::startup_reconciliation) fn finish(
        self,
        installation: &Installation,
        journal: &TransitionJournalStore,
        expected: &TransitionRecord,
    ) -> Result<UsrExchangedRootAbiNamespaceAdmission, UsrExchangedRootAbiNamespaceError> {
        let after = capture_snapshot(installation, expected)?;
        self.before.revalidate_retained()?;
        after.revalidate_retained()?;
        require_exact_snapshot(&self.before, &after)?;
        require_usr_exchanged_post(expected, &after)?;
        require_exact_journal(journal, expected)?;
        installation.revalidate_mutable_namespace()?;
        let complete = after.root_abi().is_complete();
        let proof = UsrExchangedRootAbiNamespaceProof {
            before: self.before,
            after,
        };
        if complete {
            Ok(UsrExchangedRootAbiNamespaceAdmission::Complete(proof))
        } else {
            Ok(UsrExchangedRootAbiNamespaceAdmission::Incomplete(proof))
        }
    }
}

impl UsrExchangedRootAbiNamespaceProof {
    /// Revalidate the exact source immediately before an effect boundary.
    pub(in crate::client::startup_reconciliation) fn revalidate_source(
        &self,
        installation: &Installation,
        journal: &TransitionJournalStore,
        expected: &TransitionRecord,
    ) -> Result<(), UsrExchangedRootAbiNamespaceError> {
        require_exact_journal(journal, expected)?;
        installation.revalidate_mutable_namespace()?;
        self.before.revalidate_retained()?;
        self.after.revalidate_retained()?;
        require_exact_snapshot(&self.before, &self.after)?;
        require_usr_exchanged_post(expected, &self.after)?;

        let fresh = capture_snapshot(installation, expected)?;
        fresh.revalidate_retained()?;
        require_exact_snapshot(&self.after, &fresh)?;
        require_usr_exchanged_post(expected, &fresh)?;
        require_exact_journal(journal, expected)?;
        installation.revalidate_mutable_namespace()?;
        Ok(())
    }

    /// Fill absent canonical links once, then reconcile from fresh evidence on
    /// every publisher result. A raw publisher error never becomes success,
    /// even if all names are present: its directory-sync durability is unknown.
    pub(in crate::client::startup_reconciliation) fn normalize(
        self,
        installation: &Installation,
        journal: &TransitionJournalStore,
        expected: &TransitionRecord,
        final_authority_check: impl FnOnce() -> Result<(), UsrExchangedRootAbiNamespaceError>,
    ) -> Result<UsrExchangedRootAbiAppliedNamespace, UsrExchangedRootAbiNamespaceError> {
        if self.after.root_abi().is_complete() {
            return Err(UsrExchangedRootAbiNamespaceError::WrongNormalizationSource);
        }
        self.revalidate_source(installation, journal, expected)?;
        run_before_publication(journal);
        self.revalidate_source(installation, journal, expected)?;
        final_authority_check()?;
        observe_publication_attempt();
        let (root, root_path) = self.after.retained_installation_root();
        let publication = crate::client::create_root_links_retained(root_path, root)
            .and_then(|published| published.revalidate().map(|()| published));
        run_after_publication();

        // This capture is deliberately unconditional. In particular, do not
        // use `?` on the publisher result before fresh semantic reconciliation.
        let reconciliation = reconcile_after_publication(
            &self.after,
            installation,
            journal,
            expected,
        );
        match (publication, reconciliation) {
            (Ok(published), Ok(completed)) if completed.root_abi().is_complete() => {
                Ok(UsrExchangedRootAbiAppliedNamespace { published, completed })
            }
            (Ok(_), Ok(_)) => Err(UsrExchangedRootAbiNamespaceError::PublicationReportedSuccessWithoutComplete),
            (Ok(_), Err(source)) => {
                Err(UsrExchangedRootAbiNamespaceError::ReconciliationAfterPublicationSuccess(source))
            }
            (Err(publication), Ok(completed)) if completed.root_abi().is_complete() => {
                Err(UsrExchangedRootAbiNamespaceError::PublicationFailedAfterComplete {
                    source: Box::new(publication),
                })
            }
            (Err(publication), Ok(_)) => {
                Err(UsrExchangedRootAbiNamespaceError::PublicationFailedAfterCanonicalSubset {
                    source: Box::new(publication),
                })
            }
            (Err(publication), Err(reconciliation)) => {
                Err(UsrExchangedRootAbiNamespaceError::AmbiguousPublicationFailure {
                    publication: Box::new(publication),
                    reconciliation: Box::new(reconciliation),
                })
            }
        }
    }

    /// Sync a complete-at-entry root through its retained descriptor. This is
    /// required even when every link was already present, because a prior
    /// publisher may have failed after creating all links but before durability
    /// was proven.
    pub(in crate::client::startup_reconciliation) fn synchronize_complete(
        self,
        installation: &Installation,
        journal: &TransitionJournalStore,
        expected: &TransitionRecord,
        final_authority_check: impl FnOnce() -> Result<(), UsrExchangedRootAbiNamespaceError>,
    ) -> Result<UsrExchangedRootAbiDurableNamespace, UsrExchangedRootAbiNamespaceError> {
        if !self.after.root_abi().is_complete() {
            return Err(UsrExchangedRootAbiNamespaceError::WrongDurabilitySource);
        }
        self.revalidate_source(installation, journal, expected)?;
        run_before_complete_sync();
        self.revalidate_source(installation, journal, expected)?;
        final_authority_check()?;
        let (root, _) = self.after.retained_installation_root();
        let sync = sync_complete_root(root);
        run_after_complete_sync();

        // Reconcile even when sync failed; callers still fail-stop, but retain
        // a precise distinction between a stable complete set and ambiguity.
        let reconciliation = reconcile_after_complete_sync(
            &self.after,
            installation,
            journal,
            expected,
        );
        match (sync, reconciliation) {
            (Ok(()), Ok(completed)) => Ok(UsrExchangedRootAbiDurableNamespace { completed }),
            (Ok(()), Err(source)) => Err(UsrExchangedRootAbiNamespaceError::ReconciliationAfterCompleteSync(source)),
            (Err(source), Ok(_)) => Err(UsrExchangedRootAbiNamespaceError::CompleteSyncFailed { source }),
            (Err(sync), Err(reconciliation)) => Err(UsrExchangedRootAbiNamespaceError::AmbiguousCompleteSyncFailure {
                sync,
                reconciliation: Box::new(reconciliation),
            }),
        }
    }
}

impl UsrExchangedRootAbiAppliedNamespace {
    pub(in crate::client::startup_reconciliation) fn revalidate_final(
        &self,
        installation: &Installation,
        journal: &TransitionJournalStore,
        expected: &TransitionRecord,
    ) -> Result<(), UsrExchangedRootAbiNamespaceError> {
        require_exact_journal(journal, expected)?;
        installation.revalidate_mutable_namespace()?;
        self.completed.revalidate_retained()?;
        require_usr_exchanged_post(expected, &self.completed)?;
        require_exact_journal(journal, expected)?;
        installation.revalidate_mutable_namespace()?;
        // Last fallible operation before success: pin every publisher-owned
        // existing and newly created link against an exact-target ABA.
        self.published.revalidate().map_err(Into::into)
    }
}

impl UsrExchangedRootAbiDurableNamespace {
    pub(in crate::client::startup_reconciliation) fn revalidate_final(
        &self,
        installation: &Installation,
        journal: &TransitionJournalStore,
        expected: &TransitionRecord,
    ) -> Result<(), UsrExchangedRootAbiNamespaceError> {
        require_exact_journal(journal, expected)?;
        installation.revalidate_mutable_namespace()?;
        self.completed.revalidate_retained()?;
        require_usr_exchanged_post(expected, &self.completed)?;
        require_exact_journal(journal, expected)?;
        installation.revalidate_mutable_namespace()?;
        // Last fallible operation before success: retain the complete set
        // captured after the directory-sync boundary.
        self.completed.revalidate_retained().map_err(Into::into)
    }
}

fn reconcile_after_publication(
    before: &NamespaceSnapshot,
    installation: &Installation,
    journal: &TransitionJournalStore,
    expected: &TransitionRecord,
) -> Result<NamespaceSnapshot, UsrExchangedRootAbiFreshReconciliationError> {
    let completed = capture_snapshot(installation, expected)?;
    completed.revalidate_retained()?;
    require_usr_exchanged_post_fresh(expected, &completed)?;
    require_only_canonical_root_abi_growth(before, &completed)?;
    require_exact_journal_fresh(journal, expected)?;
    installation.revalidate_mutable_namespace()?;
    Ok(completed)
}

fn reconcile_after_complete_sync(
    before: &NamespaceSnapshot,
    installation: &Installation,
    journal: &TransitionJournalStore,
    expected: &TransitionRecord,
) -> Result<NamespaceSnapshot, UsrExchangedRootAbiFreshReconciliationError> {
    let completed = capture_snapshot(installation, expected)?;
    completed.revalidate_retained()?;
    require_usr_exchanged_post_fresh(expected, &completed)?;
    if !before.has_exact_fingerprint(&completed) {
        return Err(UsrExchangedRootAbiFreshReconciliationError::NamespaceChanged);
    }
    require_exact_journal_fresh(journal, expected)?;
    installation.revalidate_mutable_namespace()?;
    Ok(completed)
}

fn require_usr_exchanged_post(
    record: &TransitionRecord,
    snapshot: &NamespaceSnapshot,
) -> Result<(), UsrExchangedRootAbiNamespaceError> {
    require_usr_exchanged_post_shared(record, snapshot).map_err(Into::into)
}

fn require_usr_exchanged_post_fresh(
    record: &TransitionRecord,
    snapshot: &NamespaceSnapshot,
) -> Result<(), UsrExchangedRootAbiFreshReconciliationError> {
    require_usr_exchanged_post_shared(record, snapshot)
}

fn require_usr_exchanged_post_shared(
    record: &TransitionRecord,
    snapshot: &NamespaceSnapshot,
) -> Result<(), UsrExchangedRootAbiFreshReconciliationError> {
    if record.phase != Phase::UsrExchanged || record.rollback.is_some() {
        return Err(UsrExchangedRootAbiFreshReconciliationError::WrongSource);
    }
    let layout = assess_snapshot_layout(record, snapshot)?
        .usr_exchange_layout()
        .ok_or(UsrExchangedRootAbiFreshReconciliationError::WrongSource)?;
    if layout == UsrExchangeLayout::Post {
        Ok(())
    } else {
        Err(UsrExchangedRootAbiFreshReconciliationError::WrongLayout)
    }
}

fn require_exact_snapshot(
    expected: &NamespaceSnapshot,
    actual: &NamespaceSnapshot,
) -> Result<(), UsrExchangedRootAbiNamespaceError> {
    if expected.has_exact_fingerprint(actual) {
        Ok(())
    } else {
        Err(UsrExchangedRootAbiNamespaceError::NamespaceChanged)
    }
}

fn require_only_canonical_root_abi_growth(
    before: &NamespaceSnapshot,
    after: &NamespaceSnapshot,
) -> Result<(), UsrExchangedRootAbiFreshReconciliationError> {
    if before.admits_only_root_abi_growth(after) {
        Ok(())
    } else {
        Err(UsrExchangedRootAbiFreshReconciliationError::UnexpectedPostPublicationDelta)
    }
}

fn require_exact_journal(
    journal: &TransitionJournalStore,
    expected: &TransitionRecord,
) -> Result<(), UsrExchangedRootAbiNamespaceError> {
    require_exact_journal_shared(journal, expected).map_err(Into::into)
}

fn require_exact_journal_fresh(
    journal: &TransitionJournalStore,
    expected: &TransitionRecord,
) -> Result<(), UsrExchangedRootAbiFreshReconciliationError> {
    require_exact_journal_shared(journal, expected)
}

fn require_exact_journal_shared(
    journal: &TransitionJournalStore,
    expected: &TransitionRecord,
) -> Result<(), UsrExchangedRootAbiFreshReconciliationError> {
    match journal.load()? {
        Some(actual) if actual == *expected => Ok(()),
        Some(_) | None => Err(UsrExchangedRootAbiFreshReconciliationError::JournalChanged),
    }
}

#[derive(Debug, thiserror::Error)]
pub(in crate::client::startup_reconciliation) enum UsrExchangedRootAbiFreshReconciliationError {
    #[error("capture or revalidate fresh activation-namespace evidence")]
    Capture(#[from] CaptureError),
    #[error("assess fresh UsrExchanged activation-namespace evidence")]
    Policy(#[from] NamespacePolicyConflict),
    #[error("read the retained canonical transition journal during fresh reconciliation")]
    Journal(#[from] StorageError),
    #[error("the retained canonical transition journal changed during fresh reconciliation")]
    JournalChanged,
    #[error("fresh reconciliation requires an exact forward UsrExchanged source")]
    WrongSource,
    #[error("fresh reconciliation requires the exact post-exchange /usr layout")]
    WrongLayout,
    #[error("the activation namespace changed across complete-root synchronization")]
    NamespaceChanged,
    #[error("publication changed more than absent canonical root ABI links")]
    UnexpectedPostPublicationDelta,
    #[error("revalidate retained mutable installation namespace during fresh reconciliation")]
    Installation(#[from] crate::installation::Error),
}

#[derive(Debug, thiserror::Error)]
pub(in crate::client::startup_reconciliation) enum UsrExchangedRootAbiNamespaceError {
    #[error("capture or revalidate the exact activation namespace")]
    Capture(#[from] CaptureError),
    #[error("assess the exact UsrExchanged activation namespace")]
    Policy(#[from] NamespacePolicyConflict),
    #[error("read the retained canonical transition journal")]
    Journal(#[from] StorageError),
    #[error("the activation namespace changed before root ABI normalization")]
    NamespaceChanged,
    #[error("database or journal-binding authority changed immediately before the root ABI effect")]
    FinalAuthorityRejected,
    #[error("root ABI publication cannot consume a complete namespace")]
    WrongNormalizationSource,
    #[error("root ABI directory durability cannot consume an incomplete namespace")]
    WrongDurabilitySource,
    #[error("root ABI publication reported success without a complete canonical namespace")]
    PublicationReportedSuccessWithoutComplete,
    #[error("fresh reconciliation failed after root ABI publication reported success")]
    ReconciliationAfterPublicationSuccess(#[source] UsrExchangedRootAbiFreshReconciliationError),
    #[error("root ABI publication failed after leaving a complete canonical namespace")]
    PublicationFailedAfterComplete { source: Box<crate::client::Error> },
    #[error("root ABI publication failed after leaving an incomplete canonical subset")]
    PublicationFailedAfterCanonicalSubset { source: Box<crate::client::Error> },
    #[error("root ABI publication failed and fresh reconciliation was ambiguous")]
    AmbiguousPublicationFailure {
        publication: Box<crate::client::Error>,
        reconciliation: Box<UsrExchangedRootAbiFreshReconciliationError>,
    },
    #[error("fresh reconciliation failed after synchronizing a complete root ABI")]
    ReconciliationAfterCompleteSync(#[source] UsrExchangedRootAbiFreshReconciliationError),
    #[error("synchronize the complete retained root ABI directory")]
    CompleteSyncFailed { source: io::Error },
    #[error("complete-root synchronization failed and fresh reconciliation was ambiguous")]
    AmbiguousCompleteSyncFailure {
        sync: io::Error,
        reconciliation: Box<UsrExchangedRootAbiFreshReconciliationError>,
    },
    #[error("publish or revalidate the exact retained merged-/usr root ABI")]
    Publication(#[from] crate::client::Error),
    #[error("revalidate the retained mutable installation namespace")]
    Installation(#[from] crate::installation::Error),
    #[error("freshly reconcile a root ABI effect")]
    FreshReconciliation(#[from] UsrExchangedRootAbiFreshReconciliationError),
}

#[cfg(test)]
std::thread_local! {
    static BEFORE_PUBLICATION: std::cell::RefCell<Option<Box<dyn FnOnce(&TransitionJournalStore)>>> =
        const { std::cell::RefCell::new(None) };
    static AFTER_PUBLICATION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static BEFORE_COMPLETE_SYNC: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static AFTER_COMPLETE_SYNC: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static COMPLETE_SYNC_FAULT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static PUBLICATION_ATTEMPTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static COMPLETE_SYNC_ATTEMPTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(in crate::client) fn arm_before_usr_exchanged_root_abi_publication(
    hook: impl FnOnce(&TransitionJournalStore) + 'static,
) {
    BEFORE_PUBLICATION.with(|slot| assert!(slot.borrow_mut().replace(Box::new(hook)).is_none()));
}

#[cfg(test)]
pub(in crate::client) fn arm_after_usr_exchanged_root_abi_publication(hook: impl FnOnce() + 'static) {
    AFTER_PUBLICATION.with(|slot| assert!(slot.borrow_mut().replace(Box::new(hook)).is_none()));
}

#[cfg(test)]
pub(in crate::client) fn arm_before_usr_exchanged_root_abi_complete_sync(hook: impl FnOnce() + 'static) {
    BEFORE_COMPLETE_SYNC.with(|slot| assert!(slot.borrow_mut().replace(Box::new(hook)).is_none()));
}

#[cfg(test)]
pub(in crate::client) fn arm_after_usr_exchanged_root_abi_complete_sync(hook: impl FnOnce() + 'static) {
    AFTER_COMPLETE_SYNC.with(|slot| assert!(slot.borrow_mut().replace(Box::new(hook)).is_none()));
}

#[cfg(test)]
pub(in crate::client) fn arm_usr_exchanged_root_abi_complete_sync_fault() {
    COMPLETE_SYNC_FAULT.with(|fault| fault.set(true));
}

#[cfg(test)]
pub(in crate::client) fn reset_usr_exchanged_root_abi_effect_counts() {
    PUBLICATION_ATTEMPTS.with(|attempts| attempts.set(0));
    COMPLETE_SYNC_ATTEMPTS.with(|attempts| attempts.set(0));
}

#[cfg(test)]
pub(in crate::client) fn usr_exchanged_root_abi_publication_attempts() -> usize {
    PUBLICATION_ATTEMPTS.with(std::cell::Cell::get)
}

#[cfg(test)]
pub(in crate::client) fn usr_exchanged_root_abi_complete_sync_attempts() -> usize {
    COMPLETE_SYNC_ATTEMPTS.with(std::cell::Cell::get)
}

#[cfg(test)]
fn run_before_publication(journal: &TransitionJournalStore) {
    BEFORE_PUBLICATION.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook(journal);
        }
    });
}

#[cfg(not(test))]
fn run_before_publication(_journal: &TransitionJournalStore) {}

#[cfg(test)]
fn run_after_publication() {
    AFTER_PUBLICATION.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn run_after_publication() {}

#[cfg(test)]
fn run_before_complete_sync() {
    BEFORE_COMPLETE_SYNC.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn run_before_complete_sync() {}

#[cfg(test)]
fn run_after_complete_sync() {
    AFTER_COMPLETE_SYNC.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn run_after_complete_sync() {}

fn sync_complete_root(root: &std::fs::File) -> io::Result<()> {
    #[cfg(test)]
    {
        COMPLETE_SYNC_ATTEMPTS.with(|attempts| attempts.set(attempts.get() + 1));
        if COMPLETE_SYNC_FAULT.with(|fault| fault.replace(false)) {
            return Err(io::Error::other("injected complete-root sync failure"));
        }
    }
    root.sync_all()
}

#[cfg(test)]
fn observe_publication_attempt() {
    PUBLICATION_ATTEMPTS.with(|attempts| attempts.set(attempts.get() + 1));
}

#[cfg(not(test))]
fn observe_publication_attempt() {}
