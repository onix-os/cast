//! Focused contracts for the test-sealed archived child-move foundation.

mod post_move_durability;

use std::{fs, os::unix::fs::MetadataExt as _, path::PathBuf};

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_reconciliation::{
            ArchivedCandidatePreserveMoveFault, ArchivedCandidatePreserveTargetDurabilityEvent,
            ArchivedCandidatePreserveTargetDurabilityFaultPoint, UsrRollbackCandidatePreserveAdmission,
            archived_candidate_preserve_move_attempt_count, arm_archived_candidate_preserve_move_fault,
            arm_archived_candidate_preserve_target_durability_fault,
            arm_before_archived_candidate_preserve_move_reconciliation_capture,
            arm_before_archived_candidate_preserve_move_reconciliation_closing,
            arm_before_archived_candidate_preserve_pre_candidate_sync,
            arm_before_archived_candidate_preserve_pre_final_capture,
            arm_before_archived_candidate_preserve_pre_move_revalidation,
            arm_before_archived_candidate_preserve_pre_roots_parent_sync,
            arm_before_archived_candidate_preserve_pre_staging_parent_sync,
            arm_before_archived_candidate_preserve_pre_target_parent_sync,
            reset_archived_candidate_preserve_move_attempt_count,
            reset_archived_candidate_preserve_target_durability_events,
            take_archived_candidate_preserve_target_durability_events,
        },
    },
    transition_journal::{RollbackActionOutcome, TransitionJournalStore},
};

use super::super::{
    UsrRollbackArchivedCandidatePreserveAppliedEffectAuthority,
    UsrRollbackArchivedCandidatePreserveApplyReconciliation, UsrRollbackArchivedCandidatePreserveEffectLease,
    UsrRollbackArchivedCandidatePreserveEffectSeal,
};
use super::{
    fixture::{OperationKind, create_private_directory},
    support::{CandidateLayout, CandidatePreserveFixture, CandidateSource, archived_slot_path},
};

pub(super) fn archived_fixture(
    source: CandidateSource,
    usr_outcome: RollbackActionOutcome,
    layout: CandidateLayout,
) -> CandidatePreserveFixture {
    archived_fixture_at_epoch(FixtureEpoch::Current, source, usr_outcome, layout)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum FixtureEpoch {
    Current,
    Historical,
}

impl FixtureEpoch {
    pub(super) const ALL: [Self; 2] = [Self::Current, Self::Historical];
}

pub(super) fn archived_fixture_at_epoch(
    epoch: FixtureEpoch,
    source: CandidateSource,
    usr_outcome: RollbackActionOutcome,
    layout: CandidateLayout,
) -> CandidatePreserveFixture {
    match epoch {
        FixtureEpoch::Current => CandidatePreserveFixture::new(OperationKind::Archived, source, usr_outcome, layout),
        FixtureEpoch::Historical => {
            CandidatePreserveFixture::historical(OperationKind::Archived, source, usr_outcome, layout)
        }
    }
}

pub(super) fn target_path(fixture: &CandidatePreserveFixture) -> PathBuf {
    fixture
        .fixture
        .installation
        .root
        .join(".cast/root")
        .join(fixture.fixture.candidate_state.to_string())
}

pub(super) fn identity(path: impl AsRef<std::path::Path>) -> (u64, u64) {
    let metadata = fs::metadata(path).unwrap();
    (metadata.dev(), metadata.ino())
}

pub(super) fn expected_pre_events(
    fixture: &CandidatePreserveFixture,
) -> Vec<ArchivedCandidatePreserveTargetDurabilityEvent> {
    let candidate = identity(fixture.fixture.installation.staging_dir().join("usr"));
    let staging = identity(fixture.fixture.installation.staging_dir());
    let target = identity(target_path(fixture));
    let roots = identity(fixture.fixture.installation.root.join(".cast/root"));
    vec![
        ArchivedCandidatePreserveTargetDurabilityEvent::CandidateSynced {
            device: candidate.0,
            inode: candidate.1,
        },
        ArchivedCandidatePreserveTargetDurabilityEvent::StagingParentSynced {
            device: staging.0,
            inode: staging.1,
        },
        ArchivedCandidatePreserveTargetDurabilityEvent::TargetParentSynced {
            device: target.0,
            inode: target.1,
        },
        ArchivedCandidatePreserveTargetDurabilityEvent::RootsParentSynced {
            device: roots.0,
            inode: roots.1,
        },
        ArchivedCandidatePreserveTargetDurabilityEvent::FinalPreProven,
    ]
}

pub(super) fn apply_lease<'reservation>(
    fixture: &CandidatePreserveFixture,
    journal: &TransitionJournalStore,
    reservation: &'reservation ActiveStateReservation,
) -> UsrRollbackArchivedCandidatePreserveEffectLease<'reservation> {
    let UsrRollbackCandidatePreserveAdmission::Apply(authority) = fixture.capture(journal, reservation) else {
        panic!("exact archived staged-with-slot evidence did not admit Apply")
    };
    let seal = UsrRollbackArchivedCandidatePreserveEffectSeal::new_for_test();
    authority.into_archived_effect_for_test(&seal, journal).unwrap()
}

pub(super) fn reconcile_applied<'reservation>(
    fixture: &CandidatePreserveFixture,
    journal: &TransitionJournalStore,
    reservation: &'reservation ActiveStateReservation,
) -> UsrRollbackArchivedCandidatePreserveAppliedEffectAuthority<'reservation> {
    let seal = UsrRollbackArchivedCandidatePreserveEffectSeal::new_for_test();
    let UsrRollbackArchivedCandidatePreserveApplyReconciliation::Applied(authority) =
        apply_lease(fixture, journal, reservation)
            .reconcile(&seal, journal)
            .unwrap()
    else {
        panic!("exact archived child move did not reconcile Applied")
    };
    authority
}

pub(super) fn reset_observations() {
    reset_archived_candidate_preserve_move_attempt_count();
    reset_archived_candidate_preserve_target_durability_events();
}

pub(super) fn assert_preserved(fixture: &CandidatePreserveFixture) {
    let target = target_path(fixture);
    assert!(!fixture.fixture.installation.staging_dir().join("usr").exists());
    assert!(target.join("usr").is_dir());
    assert_eq!(fs::read_dir(&target).unwrap().count(), 2);
    assert_eq!(
        identity(target.join("usr/.cast-tree-id")),
        identity(archived_slot_path(&fixture.fixture, &fixture.candidate_intent)),
    );
}

#[test]
fn startup_archived_candidate_child_move_reconciles_every_raw_report_for_every_origin() {
    let cases = [
        (None, true),
        (Some(ArchivedCandidatePreserveMoveFault::ErrorAfterApply), true),
        (Some(ArchivedCandidatePreserveMoveFault::ErrorWithoutApply), false),
        (Some(ArchivedCandidatePreserveMoveFault::SuccessWithoutApply), false),
    ];
    for epoch in FixtureEpoch::ALL {
        for source in CandidateSource::ALL {
            for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
                for (fault, expected_applied) in cases {
                    let fixture = archived_fixture_at_epoch(epoch, source, usr_outcome, CandidateLayout::Staged);
                    let before = fixture.evidence_snapshots();
                    let journal = fixture.open_journal();
                    let reservation = ActiveStateReservation::acquire().unwrap();
                    let lease = apply_lease(&fixture, &journal, &reservation);
                    let expected_events = expected_pre_events(&fixture);
                    reset_observations();
                    if let Some(fault) = fault {
                        arm_archived_candidate_preserve_move_fault(fault);
                    }
                    let seal = UsrRollbackArchivedCandidatePreserveEffectSeal::new_for_test();

                    let result = lease.reconcile(&seal, &journal).unwrap();

                    assert_eq!(archived_candidate_preserve_move_attempt_count(), 1);
                    assert_eq!(
                        take_archived_candidate_preserve_target_durability_events(),
                        expected_events,
                    );
                    match (expected_applied, result) {
                        (true, UsrRollbackArchivedCandidatePreserveApplyReconciliation::Applied(authority)) => {
                            drop(authority);
                            assert_preserved(&fixture);
                        }
                        (false, UsrRollbackArchivedCandidatePreserveApplyReconciliation::NotApplied) => {
                            fixture.assert_evidence_unchanged(&before);
                        }
                        (_, UsrRollbackArchivedCandidatePreserveApplyReconciliation::Ambiguous) => {
                            panic!("stable archived raw-report case was ambiguous")
                        }
                        (true, UsrRollbackArchivedCandidatePreserveApplyReconciliation::NotApplied) => {
                            panic!("applied archived move classified NotApplied")
                        }
                        (false, UsrRollbackArchivedCandidatePreserveApplyReconciliation::Applied(_)) => {
                            panic!("unapplied archived move classified Applied")
                        }
                    }
                    fixture.assert_non_namespace_unchanged();
                }
            }
        }
    }
}

#[test]
fn startup_archived_candidate_pre_faults_stop_at_exact_ordered_prefixes_without_a_move() {
    let cases = [
        (ArchivedCandidatePreserveTargetDurabilityFaultPoint::CandidateSync, 0),
        (
            ArchivedCandidatePreserveTargetDurabilityFaultPoint::StagingParentSync,
            1,
        ),
        (ArchivedCandidatePreserveTargetDurabilityFaultPoint::TargetParentSync, 2),
        (ArchivedCandidatePreserveTargetDurabilityFaultPoint::RootsParentSync, 3),
        (ArchivedCandidatePreserveTargetDurabilityFaultPoint::FinalPreCapture, 4),
    ];
    for source in CandidateSource::ALL {
        for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
            for (fault, prefix_len) in cases {
                let fixture = archived_fixture(source, usr_outcome, CandidateLayout::Staged);
                let before = fixture.evidence_snapshots();
                let journal = fixture.open_journal();
                let reservation = ActiveStateReservation::acquire().unwrap();
                let lease = apply_lease(&fixture, &journal, &reservation);
                let expected = expected_pre_events(&fixture);
                reset_observations();
                arm_archived_candidate_preserve_target_durability_fault(fault);
                let seal = UsrRollbackArchivedCandidatePreserveEffectSeal::new_for_test();

                assert!(lease.reconcile(&seal, &journal).is_err());

                assert_eq!(archived_candidate_preserve_move_attempt_count(), 0);
                assert_eq!(
                    take_archived_candidate_preserve_target_durability_events(),
                    expected[..prefix_len],
                );
                fixture.assert_evidence_unchanged(&before);
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum PreRaceBoundary {
    Candidate,
    Staging,
    Target,
    Roots,
    FinalCapture,
    PreMove,
}

impl PreRaceBoundary {
    const CASES: [(Self, usize); 6] = [
        (Self::Candidate, 0),
        (Self::Staging, 1),
        (Self::Target, 2),
        (Self::Roots, 3),
        (Self::FinalCapture, 4),
        (Self::PreMove, 5),
    ];

    fn arm(self, hook: impl FnOnce() + 'static) {
        match self {
            Self::Candidate => arm_before_archived_candidate_preserve_pre_candidate_sync(hook),
            Self::Staging => arm_before_archived_candidate_preserve_pre_staging_parent_sync(hook),
            Self::Target => arm_before_archived_candidate_preserve_pre_target_parent_sync(hook),
            Self::Roots => arm_before_archived_candidate_preserve_pre_roots_parent_sync(hook),
            Self::FinalCapture => arm_before_archived_candidate_preserve_pre_final_capture(hook),
            Self::PreMove => arm_before_archived_candidate_preserve_pre_move_revalidation(hook),
        }
    }
}

#[test]
fn startup_archived_candidate_pre_races_fail_at_every_boundary_without_a_move() {
    for (boundary, prefix_len) in PreRaceBoundary::CASES {
        let fixture = archived_fixture(
            CandidateSource::Exchanged,
            RollbackActionOutcome::Applied,
            CandidateLayout::Staged,
        );
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        let lease = apply_lease(&fixture, &journal, &reservation);
        let expected = expected_pre_events(&fixture);
        reset_observations();
        boundary.arm(fixture.namespace_change_hook(format!("archived-pre-race-{boundary:?}")));
        let seal = UsrRollbackArchivedCandidatePreserveEffectSeal::new_for_test();

        assert!(lease.reconcile(&seal, &journal).is_err());

        assert_eq!(archived_candidate_preserve_move_attempt_count(), 0);
        assert_eq!(
            take_archived_candidate_preserve_target_durability_events(),
            expected[..prefix_len],
        );
        fixture.assert_non_namespace_unchanged();
    }
}

#[test]
fn startup_archived_candidate_final_pre_revalidation_refuses_rebound_move_parents() {
    for staging_parent in [true, false] {
        let fixture = archived_fixture(
            CandidateSource::Exchanged,
            RollbackActionOutcome::Applied,
            CandidateLayout::Staged,
        );
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        let lease = apply_lease(&fixture, &journal, &reservation);
        let expected = expected_pre_events(&fixture);
        let selected = if staging_parent {
            fixture.fixture.installation.staging_dir()
        } else {
            target_path(&fixture)
        };
        let displaced = fixture
            .fixture
            .installation
            .state_quarantine_dir()
            .join(if staging_parent {
                "displaced-archived-staging"
            } else {
                "displaced-archived-target"
            });
        arm_before_archived_candidate_preserve_pre_move_revalidation(move || {
            fs::rename(&selected, displaced).unwrap();
            create_private_directory(&selected);
        });
        reset_observations();
        let seal = UsrRollbackArchivedCandidatePreserveEffectSeal::new_for_test();

        assert!(lease.reconcile(&seal, &journal).is_err());

        assert_eq!(archived_candidate_preserve_move_attempt_count(), 0);
        assert_eq!(take_archived_candidate_preserve_target_durability_events(), expected);
        fixture.assert_non_namespace_unchanged();
    }
}

#[test]
fn startup_archived_candidate_reconciliation_uses_fresh_namespace_not_the_raw_report() {
    let fixture = archived_fixture(
        CandidateSource::Exchanged,
        RollbackActionOutcome::Applied,
        CandidateLayout::Staged,
    );
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let lease = apply_lease(&fixture, &journal, &reservation);
    reset_observations();
    arm_before_archived_candidate_preserve_move_reconciliation_capture(
        fixture.namespace_change_hook("archived-post-move-race".to_owned()),
    );
    let seal = UsrRollbackArchivedCandidatePreserveEffectSeal::new_for_test();

    let result = lease.reconcile(&seal, &journal).unwrap();

    assert!(matches!(
        result,
        UsrRollbackArchivedCandidatePreserveApplyReconciliation::Ambiguous
    ));
    assert_eq!(archived_candidate_preserve_move_attempt_count(), 1);
    assert_preserved(&fixture);
    fixture.assert_non_namespace_unchanged();
}

#[test]
fn startup_archived_candidate_reconciliation_closing_rejects_post_classification_child_moves() {
    let fixture = archived_fixture(
        CandidateSource::Exchanged,
        RollbackActionOutcome::Applied,
        CandidateLayout::Staged,
    );
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let lease = apply_lease(&fixture, &journal, &reservation);
    let staging_usr = fixture.fixture.installation.staging_dir().join("usr");
    let preserved_usr = target_path(&fixture).join("usr");
    reset_observations();
    arm_archived_candidate_preserve_move_fault(ArchivedCandidatePreserveMoveFault::ErrorWithoutApply);
    arm_before_archived_candidate_preserve_move_reconciliation_closing(move || {
        fs::rename(staging_usr, preserved_usr).unwrap();
    });
    let seal = UsrRollbackArchivedCandidatePreserveEffectSeal::new_for_test();

    let result = lease.reconcile(&seal, &journal).unwrap();

    assert!(matches!(
        result,
        UsrRollbackArchivedCandidatePreserveApplyReconciliation::Ambiguous
    ));
    assert_eq!(archived_candidate_preserve_move_attempt_count(), 1);
    assert_preserved(&fixture);
    fixture.assert_non_namespace_unchanged();
    drop(reservation);
    drop(journal);
    drop(fixture);

    let fixture = archived_fixture(
        CandidateSource::Exchanged,
        RollbackActionOutcome::Applied,
        CandidateLayout::Staged,
    );
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let lease = apply_lease(&fixture, &journal, &reservation);
    let staging_usr = fixture.fixture.installation.staging_dir().join("usr");
    let preserved_usr = target_path(&fixture).join("usr");
    reset_observations();
    arm_before_archived_candidate_preserve_move_reconciliation_closing(move || {
        fs::rename(preserved_usr, staging_usr).unwrap();
    });

    let result = lease.reconcile(&seal, &journal).unwrap();

    assert!(matches!(
        result,
        UsrRollbackArchivedCandidatePreserveApplyReconciliation::Ambiguous
    ));
    assert_eq!(archived_candidate_preserve_move_attempt_count(), 1);
    assert!(fixture.fixture.installation.staging_dir().join("usr").is_dir());
    assert!(!target_path(&fixture).join("usr").exists());
    assert_eq!(fs::read_dir(target_path(&fixture)).unwrap().count(), 1);
    assert_eq!(
        identity(fixture.fixture.installation.staging_dir().join("usr/.cast-tree-id")),
        identity(archived_slot_path(&fixture.fixture, &fixture.candidate_intent)),
    );
    fixture.assert_non_namespace_unchanged();
}

#[test]
fn startup_archived_candidate_non_namespace_races_never_escape_the_authority_sandwich() {
    let fixture = archived_fixture(
        CandidateSource::Exchanged,
        RollbackActionOutcome::Applied,
        CandidateLayout::Staged,
    );
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let lease = apply_lease(&fixture, &journal, &reservation);
    let expected = expected_pre_events(&fixture);
    reset_observations();
    arm_before_archived_candidate_preserve_pre_final_capture(fixture.journal_change_hook());
    let seal = UsrRollbackArchivedCandidatePreserveEffectSeal::new_for_test();

    assert!(lease.reconcile(&seal, &journal).is_err());
    assert_eq!(archived_candidate_preserve_move_attempt_count(), 0);
    assert_eq!(take_archived_candidate_preserve_target_durability_events(), expected);
    drop(reservation);
    drop(journal);
    drop(fixture);

    let fixture = archived_fixture(
        CandidateSource::Exchanged,
        RollbackActionOutcome::Applied,
        CandidateLayout::Staged,
    );
    let journal_bytes = fixture.fixture.canonical_bytes();
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let lease = apply_lease(&fixture, &journal, &reservation);
    reset_observations();
    let database = fixture.fixture.database.clone();
    let candidate = fixture.fixture.candidate_state;
    arm_before_archived_candidate_preserve_move_reconciliation_capture(move || {
        database.delete_metadata_provenance_for_test(candidate).unwrap();
    });

    assert!(lease.reconcile(&seal, &journal).is_err());
    assert_eq!(archived_candidate_preserve_move_attempt_count(), 1);
    assert_eq!(fixture.fixture.canonical_bytes(), journal_bytes);
    assert_preserved(&fixture);
}
