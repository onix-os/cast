//! Focused contracts for exact read-only ActiveReblit commit cleanup.

use std::{fs, os::unix::fs::PermissionsExt as _};

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_reconciliation::{
            ActiveReblitCommitCleanupAdmission, ActiveReblitCommitCleanupAuthority,
            arm_before_active_reblit_commit_cleanup_fresh_namespace_capture,
            arm_between_active_reblit_commit_cleanup_database_captures,
        },
    },
    state,
    transition_journal::{Phase, encode},
};

use super::{
    boot_sync_complete_support::{
        BootSyncCompleteReadOnlySnapshot, boot_sync_complete_fixture, open_boot_sync_complete_journal,
        same_byte_different_inode_hook,
    },
    support::{BootRepairFixture, Epoch},
};

#[derive(Clone, Copy)]
enum CleanupLayout {
    Apply,
    Finish,
}

fn commit_decided_fixture(epoch: Epoch, layout: CleanupLayout) -> BootRepairFixture {
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
    let replacement = fixture
        .fixture
        .active_reblit_reservation
        .as_ref()
        .expect("ActiveReblit cleanup fixture retains its replacement wrapper");
    let temporary = fixture.fixture.installation.root.join(".cast/root/.cleanup-test-swap");
    fs::rename(&staging, &temporary).unwrap();
    fs::rename(replacement, &staging).unwrap();
    fs::rename(&temporary, replacement).unwrap();
}

#[test]
fn exact_apply_and_finish_authorities_are_read_only_and_consumable() {
    for epoch in Epoch::ALL {
        for layout in [CleanupLayout::Apply, CleanupLayout::Finish] {
            let fixture = commit_decided_fixture(epoch, layout);
            let before = BootSyncCompleteReadOnlySnapshot::capture(&fixture);
            let journal = open_boot_sync_complete_journal(&fixture);
            let reservation = ActiveStateReservation::acquire().unwrap();
            let admission = ActiveReblitCommitCleanupAuthority::capture(
                &fixture.fixture.installation,
                &journal,
                &fixture.fixture.database,
                &reservation,
                &fixture.fixture.source,
            )
            .unwrap();
            match (layout, admission) {
                (CleanupLayout::Apply, ActiveReblitCommitCleanupAdmission::Apply(authority)) => {
                    authority.revalidate(&journal).unwrap();
                    let _effect = authority.into_effect_authority(&journal).unwrap();
                }
                (CleanupLayout::Finish, ActiveReblitCommitCleanupAdmission::Finish(authority)) => {
                    authority.revalidate(&journal).unwrap();
                    let _effect = authority.into_effect_authority(&journal).unwrap();
                }
                _ => panic!("exact CommitDecided cleanup layout admitted the wrong typestate"),
            }
            before.assert_unchanged(&fixture);
            assert_eq!(fixture.fixture.canonical_record(), fixture.fixture.source);
        }
    }
}

#[test]
fn stable_receipt_selection_and_wrapper_mismatches_defer() {
    let mut legacy = commit_decided_fixture(Epoch::Current, CleanupLayout::Apply);
    legacy.fixture.source.version = 2;
    legacy.fixture.source.boot_publication_receipts = None;
    fs::write(
        legacy.fixture.installation.root.join(".cast/journal/state-transition"),
        encode(&legacy.fixture.source).unwrap(),
    )
    .unwrap();
    let journal = open_boot_sync_complete_journal(&legacy);
    let reservation = ActiveStateReservation::acquire().unwrap();
    assert!(matches!(
        ActiveReblitCommitCleanupAuthority::capture(
            &legacy.fixture.installation,
            &journal,
            &legacy.fixture.database,
            &reservation,
            &legacy.fixture.source,
        )
        .unwrap(),
        ActiveReblitCommitCleanupAdmission::Deferred
    ));
    drop(reservation);
    drop(journal);

    let mut wrong_selection = commit_decided_fixture(Epoch::Current, CleanupLayout::Apply);
    let other = state::Id::from(i32::from(wrong_selection.fixture.candidate_state) + 100);
    wrong_selection.fixture.installation.active_state = Some(other);
    fs::write(
        wrong_selection.fixture.installation.root.join("usr/.stateID"),
        i32::from(other).to_string(),
    )
    .unwrap();
    let journal = open_boot_sync_complete_journal(&wrong_selection);
    let reservation = ActiveStateReservation::acquire().unwrap();
    assert!(matches!(
        ActiveReblitCommitCleanupAuthority::capture(
            &wrong_selection.fixture.installation,
            &journal,
            &wrong_selection.fixture.database,
            &reservation,
            &wrong_selection.fixture.source,
        )
        .unwrap(),
        ActiveReblitCommitCleanupAdmission::Deferred
    ));
    drop(reservation);
    drop(journal);

    let wrong_wrapper = commit_decided_fixture(Epoch::Current, CleanupLayout::Apply);
    let replacement = wrong_wrapper.fixture.active_reblit_reservation.as_ref().unwrap();
    fs::rename(replacement, replacement.with_extension("wrong-name")).unwrap();
    let journal = open_boot_sync_complete_journal(&wrong_wrapper);
    let reservation = ActiveStateReservation::acquire().unwrap();
    assert!(matches!(
        ActiveReblitCommitCleanupAuthority::capture(
            &wrong_wrapper.fixture.installation,
            &journal,
            &wrong_wrapper.fixture.database,
            &reservation,
            &wrong_wrapper.fixture.source,
        )
        .unwrap(),
        ActiveReblitCommitCleanupAdmission::Deferred
    ));
}

#[test]
fn database_change_inside_admission_fails_stop() {
    let fixture = commit_decided_fixture(Epoch::Current, CleanupLayout::Apply);
    let database = fixture.fixture.database.clone();
    let candidate = fixture.fixture.candidate_state;
    arm_between_active_reblit_commit_cleanup_database_captures(move || {
        database
            .change_summary_for_test(candidate, Some("changed inside commit cleanup sandwich"))
            .unwrap();
    });
    let journal = open_boot_sync_complete_journal(&fixture);
    let reservation = ActiveStateReservation::acquire().unwrap();
    assert!(
        ActiveReblitCommitCleanupAuthority::capture(
            &fixture.fixture.installation,
            &journal,
            &fixture.fixture.database,
            &reservation,
            &fixture.fixture.source,
        )
        .is_err()
    );
    assert_eq!(fixture.fixture.canonical_record(), fixture.fixture.source);
}

#[test]
fn fresh_namespace_and_same_byte_record_replacements_fail_stop() {
    let fixture = commit_decided_fixture(Epoch::Current, CleanupLayout::Apply);
    let journal = open_boot_sync_complete_journal(&fixture);
    let reservation = ActiveStateReservation::acquire().unwrap();
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
        _ => panic!("exact Apply cleanup evidence did not admit"),
    };
    let changed = fixture.fixture.active_reblit_reservation.as_ref().unwrap().clone();
    let original_mode = fs::metadata(&changed).unwrap().permissions().mode() & 0o7777;
    let changed_mode = if original_mode == 0o700 { 0o755 } else { 0o700 };
    arm_before_active_reblit_commit_cleanup_fresh_namespace_capture(move || {
        fs::set_permissions(changed, fs::Permissions::from_mode(changed_mode)).unwrap();
    });
    assert!(authority.revalidate(&journal).is_err());
    drop(authority);
    drop(reservation);
    drop(journal);

    let fixture = commit_decided_fixture(Epoch::Current, CleanupLayout::Finish);
    let journal = open_boot_sync_complete_journal(&fixture);
    let reservation = ActiveStateReservation::acquire().unwrap();
    let authority = match ActiveReblitCommitCleanupAuthority::capture(
        &fixture.fixture.installation,
        &journal,
        &fixture.fixture.database,
        &reservation,
        &fixture.fixture.source,
    )
    .unwrap()
    {
        ActiveReblitCommitCleanupAdmission::Finish(authority) => authority,
        _ => panic!("exact Finish cleanup evidence did not admit"),
    };
    arm_before_active_reblit_commit_cleanup_fresh_namespace_capture(
        same_byte_different_inode_hook(&fixture, "commit-cleanup-record-race"),
    );
    assert!(authority.into_effect_authority(&journal).is_err());
}

#[test]
fn non_commit_decided_sources_are_not_applicable() {
    let fixture = boot_sync_complete_fixture(Epoch::Current, true);
    let journal = open_boot_sync_complete_journal(&fixture);
    let reservation = ActiveStateReservation::acquire().unwrap();
    assert!(matches!(
        ActiveReblitCommitCleanupAuthority::capture(
            &fixture.fixture.installation,
            &journal,
            &fixture.fixture.database,
            &reservation,
            &fixture.fixture.source,
        )
        .unwrap(),
        ActiveReblitCommitCleanupAdmission::NotApplicable
    ));
}
