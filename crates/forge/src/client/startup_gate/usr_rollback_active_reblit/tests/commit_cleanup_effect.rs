//! Focused one-shot effect, durability, and re-entry contracts.

use std::{fs, os::unix::fs::{MetadataExt as _, PermissionsExt as _}};

use crate::client::{
    active_state_snapshot::ActiveStateReservation,
    startup_reconciliation::{
        ActiveReblitCommitCleanupAdmission, ActiveReblitCommitCleanupApplyReconciliation,
        ActiveReblitCommitCleanupAuthority, ActiveReblitCommitCleanupDurabilityEvent,
        ActiveReblitCommitCleanupDurabilityFaultPoint, ActiveReblitCommitCleanupExchangeFault,
        ActiveReblitCommitCleanupPendingDurabilityAuthority,
        active_reblit_commit_cleanup_exchange_attempt_count,
        arm_active_reblit_commit_cleanup_durability_fault,
        arm_active_reblit_commit_cleanup_exchange_fault,
        arm_before_active_reblit_commit_cleanup_reconciliation_capture,
        reset_active_reblit_commit_cleanup_durability_events,
        reset_active_reblit_commit_cleanup_exchange_attempt_count,
        take_active_reblit_commit_cleanup_durability_events,
    },
};
use crate::transition_journal::{Phase, TransitionJournalStore};

use super::{
    boot_sync_complete_support::{boot_sync_complete_fixture, open_boot_sync_complete_journal},
    support::{BootRepairFixture, Epoch},
};

#[derive(Clone, Copy)]
pub(super) enum CleanupLayout {
    Apply,
    Finish,
}

pub(super) fn commit_decided_fixture(epoch: Epoch, layout: CleanupLayout) -> BootRepairFixture {
    let mut fixture = boot_sync_complete_fixture(epoch, true);
    let decided = fixture.fixture.source.forward_successor(None).unwrap();
    assert_eq!(decided.phase, Phase::CommitDecided);
    let journal = open_boot_sync_complete_journal(&fixture);
    journal.advance(&fixture.fixture.source, &decided).unwrap();
    drop(journal);
    fixture.fixture.source = decided;
    if matches!(layout, CleanupLayout::Finish) {
        exchange_cleanup_wrappers(&fixture);
    }
    fixture
}

fn exchange_cleanup_wrappers(fixture: &BootRepairFixture) {
    let staging = fixture.fixture.installation.root.join(".cast/root/staging");
    let replacement = fixture.fixture.active_reblit_reservation.as_ref().unwrap();
    let temporary = fixture.fixture.installation.root.join(".cast/root/.cleanup-effect-swap");
    fs::rename(&staging, &temporary).unwrap();
    fs::rename(replacement, &staging).unwrap();
    fs::rename(&temporary, replacement).unwrap();
}

pub(super) fn capture_apply_pending<'reservation>(
    fixture: &BootRepairFixture,
    journal: &TransitionJournalStore,
    reservation: &'reservation ActiveStateReservation,
) -> ActiveReblitCommitCleanupPendingDurabilityAuthority<'reservation> {
    let authority = match ActiveReblitCommitCleanupAuthority::capture(
        &fixture.fixture.installation,
        journal,
        &fixture.fixture.database,
        reservation,
        &fixture.fixture.source,
    )
    .unwrap()
    {
        ActiveReblitCommitCleanupAdmission::Apply(authority) => authority,
        _ => panic!("exact cleanup Apply layout did not admit"),
    };
    let effect = authority.into_effect_authority(journal).unwrap();
    match effect.reconcile(journal).unwrap() {
        ActiveReblitCommitCleanupApplyReconciliation::Applied(pending) => pending,
        _ => panic!("exact cleanup exchange was not freshly classified Applied"),
    }
}

pub(super) fn capture_finish_pending<'reservation>(
    fixture: &BootRepairFixture,
    journal: &TransitionJournalStore,
    reservation: &'reservation ActiveStateReservation,
) -> ActiveReblitCommitCleanupPendingDurabilityAuthority<'reservation> {
    let authority = match ActiveReblitCommitCleanupAuthority::capture(
        &fixture.fixture.installation,
        journal,
        &fixture.fixture.database,
        reservation,
        &fixture.fixture.source,
    )
    .unwrap()
    {
        ActiveReblitCommitCleanupAdmission::Finish(authority) => authority,
        _ => panic!("exact cleanup Finish layout did not admit"),
    };
    authority
        .into_effect_authority(journal)
        .unwrap()
        .into_durability(journal)
        .unwrap()
}

#[derive(Clone, Copy)]
enum ExpectedOutcome {
    Applied,
    NotApplied,
}

#[test]
fn single_exchange_report_is_never_the_semantic_outcome() {
    let cases = [
        (None, ExpectedOutcome::Applied),
        (
            Some(ActiveReblitCommitCleanupExchangeFault::ErrorWithoutApply),
            ExpectedOutcome::NotApplied,
        ),
        (
            Some(ActiveReblitCommitCleanupExchangeFault::SuccessWithoutApply),
            ExpectedOutcome::NotApplied,
        ),
        (
            Some(ActiveReblitCommitCleanupExchangeFault::ErrorAfterApply),
            ExpectedOutcome::Applied,
        ),
    ];
    for (fault, expected) in cases {
        let fixture = commit_decided_fixture(Epoch::Current, CleanupLayout::Apply);
        let journal = open_boot_sync_complete_journal(&fixture);
        let reservation = ActiveStateReservation::acquire().unwrap();
        reset_active_reblit_commit_cleanup_exchange_attempt_count();
        if let Some(fault) = fault {
            arm_active_reblit_commit_cleanup_exchange_fault(fault);
        }
        let authority = match ActiveReblitCommitCleanupAuthority::capture(
            &fixture.fixture.installation,
            &journal,
            &fixture.fixture.database,
            &reservation,
            &fixture.fixture.source,
        )
        .unwrap()
        {
            ActiveReblitCommitCleanupAdmission::Apply(authority) => authority,
            _ => panic!("Apply fixture did not admit"),
        };
        let result = authority
            .into_effect_authority(&journal)
            .unwrap()
            .reconcile(&journal)
            .unwrap();
        assert_eq!(active_reblit_commit_cleanup_exchange_attempt_count(), 1);
        assert!(matches!(
            (expected, result),
            (
                ExpectedOutcome::Applied,
                ActiveReblitCommitCleanupApplyReconciliation::Applied(_)
            ) | (
                ExpectedOutcome::NotApplied,
                ActiveReblitCommitCleanupApplyReconciliation::NotApplied
            )
        ));
        assert_eq!(fixture.fixture.canonical_record(), fixture.fixture.source);
    }

    let fixture = commit_decided_fixture(Epoch::Current, CleanupLayout::Apply);
    let journal = open_boot_sync_complete_journal(&fixture);
    let reservation = ActiveStateReservation::acquire().unwrap();
    reset_active_reblit_commit_cleanup_exchange_attempt_count();
    let staging = fixture.fixture.installation.root.join(".cast/root/staging");
    arm_before_active_reblit_commit_cleanup_reconciliation_capture(move || {
        fs::set_permissions(staging, fs::Permissions::from_mode(0o755)).unwrap();
    });
    let authority = match ActiveReblitCommitCleanupAuthority::capture(
        &fixture.fixture.installation,
        &journal,
        &fixture.fixture.database,
        &reservation,
        &fixture.fixture.source,
    )
    .unwrap()
    {
        ActiveReblitCommitCleanupAdmission::Apply(authority) => authority,
        _ => panic!("Apply fixture did not admit"),
    };
    assert!(matches!(
        authority
            .into_effect_authority(&journal)
            .unwrap()
            .reconcile(&journal)
            .unwrap(),
        ActiveReblitCommitCleanupApplyReconciliation::Ambiguous
    ));
    assert_eq!(active_reblit_commit_cleanup_exchange_attempt_count(), 1);
    assert_eq!(fixture.fixture.canonical_record(), fixture.fixture.source);
}

#[test]
fn apply_and_finish_use_the_same_exact_durability_suffix() {
    for epoch in Epoch::ALL {
        for layout in [CleanupLayout::Apply, CleanupLayout::Finish] {
            let fixture = commit_decided_fixture(epoch, layout);
            let database_before = fixture.fixture.database_snapshot();
            let receipt_before = fixture.fixture.database.boot_publication_receipt_state().unwrap();
            let journal = open_boot_sync_complete_journal(&fixture);
            let reservation = ActiveStateReservation::acquire().unwrap();
            reset_active_reblit_commit_cleanup_exchange_attempt_count();
            reset_active_reblit_commit_cleanup_durability_events();
            let pending = match layout {
                CleanupLayout::Apply => capture_apply_pending(&fixture, &journal, &reservation),
                CleanupLayout::Finish => capture_finish_pending(&fixture, &journal, &reservation),
            };
            let expected_attempts = usize::from(matches!(layout, CleanupLayout::Apply));
            assert_eq!(active_reblit_commit_cleanup_exchange_attempt_count(), expected_attempts);
            let durable = pending.complete(&journal).unwrap();
            durable.revalidate(&journal).unwrap();
            assert_exact_suffix(&fixture, take_active_reblit_commit_cleanup_durability_events());
            assert_eq!(fixture.fixture.database_snapshot(), database_before);
            assert_eq!(fixture.fixture.database.boot_publication_receipt_state().unwrap(), receipt_before);
            assert_eq!(fixture.fixture.canonical_record(), fixture.fixture.source);
            assert_eq!(active_reblit_commit_cleanup_exchange_attempt_count(), expected_attempts);
        }
    }
}

#[test]
fn post_exchange_durability_fault_reenters_finish_without_second_exchange() {
    let points = [
        ActiveReblitCommitCleanupDurabilityFaultPoint::PreviousTreeSync,
        ActiveReblitCommitCleanupDurabilityFaultPoint::PreviousWrapperSync,
        ActiveReblitCommitCleanupDurabilityFaultPoint::ReplacementWrapperSync,
        ActiveReblitCommitCleanupDurabilityFaultPoint::RootsParentSync,
        ActiveReblitCommitCleanupDurabilityFaultPoint::QuarantineParentSync,
        ActiveReblitCommitCleanupDurabilityFaultPoint::FinalFinishCapture,
    ];
    for point in points {
        let fixture = commit_decided_fixture(Epoch::Current, CleanupLayout::Apply);
        let database_before = fixture.fixture.database_snapshot();
        let journal = open_boot_sync_complete_journal(&fixture);
        let reservation = ActiveStateReservation::acquire().unwrap();
        reset_active_reblit_commit_cleanup_exchange_attempt_count();
        reset_active_reblit_commit_cleanup_durability_events();
        let pending = capture_apply_pending(&fixture, &journal, &reservation);
        assert_eq!(active_reblit_commit_cleanup_exchange_attempt_count(), 1);
        arm_active_reblit_commit_cleanup_durability_fault(point);
        assert!(pending.complete(&journal).is_err());
        assert_eq!(fixture.fixture.canonical_record(), fixture.fixture.source);

        drop(reservation);
        drop(journal);
        let journal = open_boot_sync_complete_journal(&fixture);
        let reservation = ActiveStateReservation::acquire().unwrap();

        reset_active_reblit_commit_cleanup_durability_events();
        let finish = capture_finish_pending(&fixture, &journal, &reservation);
        assert_eq!(active_reblit_commit_cleanup_exchange_attempt_count(), 1);
        let durable = finish.complete(&journal).unwrap();
        durable.revalidate(&journal).unwrap();
        assert_exact_suffix(&fixture, take_active_reblit_commit_cleanup_durability_events());
        assert_eq!(active_reblit_commit_cleanup_exchange_attempt_count(), 1);
        assert_eq!(fixture.fixture.database_snapshot(), database_before);
        assert_eq!(fixture.fixture.canonical_record(), fixture.fixture.source);
    }
}

fn assert_exact_suffix(
    fixture: &BootRepairFixture,
    events: Vec<ActiveReblitCommitCleanupDurabilityEvent>,
) {
    assert_eq!(events.len(), 6);
    let target = fixture.fixture.active_reblit_reservation.as_ref().unwrap();
    let previous = identity(&target.join("usr"));
    let previous_wrapper = identity(target);
    let replacement = identity(&fixture.fixture.installation.root.join(".cast/root/staging"));
    let roots = identity(&fixture.fixture.installation.root.join(".cast/root"));
    let quarantine = identity(&fixture.fixture.installation.root.join(".cast/quarantine"));
    assert!(matches!(events[0], ActiveReblitCommitCleanupDurabilityEvent::PreviousTreeSynced { device, inode } if (device, inode) == previous));
    assert!(matches!(events[1], ActiveReblitCommitCleanupDurabilityEvent::PreviousWrapperSynced { device, inode } if (device, inode) == previous_wrapper));
    assert!(matches!(events[2], ActiveReblitCommitCleanupDurabilityEvent::ReplacementWrapperSynced { device, inode } if (device, inode) == replacement));
    assert!(matches!(events[3], ActiveReblitCommitCleanupDurabilityEvent::RootsParentSynced { device, inode } if (device, inode) == roots));
    assert!(matches!(events[4], ActiveReblitCommitCleanupDurabilityEvent::QuarantineParentSynced { device, inode } if (device, inode) == quarantine));
    assert!(matches!(events[5], ActiveReblitCommitCleanupDurabilityEvent::FinalFinishProven));
}

fn identity(path: &std::path::Path) -> (u64, u64) {
    let metadata = fs::symlink_metadata(path).unwrap();
    (metadata.dev(), metadata.ino())
}
