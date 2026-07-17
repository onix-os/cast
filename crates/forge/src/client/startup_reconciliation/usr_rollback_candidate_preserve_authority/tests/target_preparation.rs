//! Disjoint, read-only target-preparation lease selection contracts.

use std::{fs, os::unix::fs::MetadataExt as _};

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_reconciliation::{
            UsrRollbackCandidatePreserveAdmission, UsrRollbackCandidatePreserveApplyEffectSelection,
            active_reblit_candidate_preserve_exchange_attempt_count, new_state_candidate_preserve_move_attempt_count,
            reset_active_reblit_candidate_preserve_exchange_attempt_count,
            reset_new_state_candidate_preserve_move_attempt_count,
        },
        startup_recovery::UsrRollbackCandidatePreserveEffectSeal,
    },
    transition_journal::RollbackActionOutcome,
};

use super::{
    fixture::OperationKind,
    support::{CandidateLayout, CandidatePreserveFixture, CandidateSource, transition_quarantine_path},
};

const RESTRICTIVE_RESIDUE_MODES: [u32; 7] = [0o000, 0o100, 0o200, 0o300, 0o400, 0o500, 0o600];

#[test]
fn startup_candidate_target_preparation_selects_every_new_state_prefix_for_every_origin() {
    for source in CandidateSource::ALL {
        for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
            let fixture =
                CandidatePreserveFixture::new(OperationKind::NewState, source, usr_outcome, CandidateLayout::Staged);
            let before = fixture.evidence_snapshots();
            let journal = fixture.open_journal();
            let reservation = ActiveStateReservation::acquire().unwrap();
            let authority = apply_authority(&fixture, &journal, &reservation);
            let seal = UsrRollbackCandidatePreserveEffectSeal::new_for_test();
            reset_new_state_candidate_preserve_move_attempt_count();
            let UsrRollbackCandidatePreserveApplyEffectSelection::CreateNewStateTarget(lease) =
                authority.into_effect_selection(&seal, &journal).unwrap()
            else {
                panic!("absent target did not select CreateNewStateTarget for {source:?} {usr_outcome:?}");
            };
            drop(lease);
            assert_eq!(new_state_candidate_preserve_move_attempt_count(), 0);
            fixture.assert_evidence_unchanged(&before);
            drop(reservation);
            drop(journal);

            for mode in RESTRICTIVE_RESIDUE_MODES {
                let fixture = CandidatePreserveFixture::new_state_target_residue(source, usr_outcome, mode);
                let target = transition_quarantine_path(&fixture.fixture, &fixture.candidate_intent);
                let target_before = target_metadata(&target);
                let journal = fixture.open_journal();
                let reservation = ActiveStateReservation::acquire().unwrap();
                let authority = apply_authority(&fixture, &journal, &reservation);
                let seal = UsrRollbackCandidatePreserveEffectSeal::new_for_test();
                reset_new_state_candidate_preserve_move_attempt_count();
                let UsrRollbackCandidatePreserveApplyEffectSelection::NormalizeNewStateTarget(lease) =
                    authority.into_effect_selection(&seal, &journal).unwrap()
                else {
                    panic!(
                        "target residue {mode:04o} did not select NormalizeNewStateTarget for {source:?} {usr_outcome:?}"
                    );
                };
                drop(lease);
                assert_eq!(new_state_candidate_preserve_move_attempt_count(), 0);
                assert_eq!(target_metadata(&target), target_before);
                fixture.assert_non_namespace_unchanged();
                drop(reservation);
                drop(journal);
            }

            let fixture = CandidatePreserveFixture::new_state_empty_quarantine_prefix(source, usr_outcome);
            let before = fixture.evidence_snapshots();
            let journal = fixture.open_journal();
            let reservation = ActiveStateReservation::acquire().unwrap();
            let authority = apply_authority(&fixture, &journal, &reservation);
            let seal = UsrRollbackCandidatePreserveEffectSeal::new_for_test();
            reset_new_state_candidate_preserve_move_attempt_count();
            let UsrRollbackCandidatePreserveApplyEffectSelection::MoveNewState(lease) =
                authority.into_effect_selection(&seal, &journal).unwrap()
            else {
                panic!("empty private target did not select MoveNewState for {source:?} {usr_outcome:?}");
            };
            drop(lease);
            assert_eq!(new_state_candidate_preserve_move_attempt_count(), 0);
            fixture.assert_evidence_unchanged(&before);
        }
    }
}

#[test]
fn startup_candidate_target_preparation_keeps_archived_unsupported_and_selects_opaque_active_reblit_exchange() {
    for source in CandidateSource::ALL {
        for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
            let archived =
                CandidatePreserveFixture::new(OperationKind::Archived, source, usr_outcome, CandidateLayout::Staged);
            let archived_before = archived.evidence_snapshots();
            let archived_journal = archived.open_journal();
            let archived_reservation = ActiveStateReservation::acquire().unwrap();
            let archived_authority = apply_authority(&archived, &archived_journal, &archived_reservation);
            let seal = UsrRollbackCandidatePreserveEffectSeal::new_for_test();
            reset_new_state_candidate_preserve_move_attempt_count();
            reset_active_reblit_candidate_preserve_exchange_attempt_count();

            assert!(matches!(
                archived_authority
                    .into_effect_selection(&seal, &archived_journal)
                    .unwrap(),
                UsrRollbackCandidatePreserveApplyEffectSelection::Unsupported
            ));
            assert_eq!(new_state_candidate_preserve_move_attempt_count(), 0);
            assert_eq!(active_reblit_candidate_preserve_exchange_attempt_count(), 0);
            archived.assert_evidence_unchanged(&archived_before);
            drop(archived_reservation);
            drop(archived_journal);

            let active = CandidatePreserveFixture::new(
                OperationKind::ActiveReblit,
                source,
                usr_outcome,
                CandidateLayout::Staged,
            );
            let active_before = active.evidence_snapshots();
            let active_journal = active.open_journal();
            let active_reservation = ActiveStateReservation::acquire().unwrap();
            let active_authority = apply_authority(&active, &active_journal, &active_reservation);
            let seal = UsrRollbackCandidatePreserveEffectSeal::new_for_test();
            reset_new_state_candidate_preserve_move_attempt_count();
            reset_active_reblit_candidate_preserve_exchange_attempt_count();

            let UsrRollbackCandidatePreserveApplyEffectSelection::ExchangeActiveReblit(lease) =
                active_authority.into_effect_selection(&seal, &active_journal).unwrap()
            else {
                panic!("exact staged ActiveReblit evidence did not select its opaque exchange lease");
            };
            drop(lease);
            assert_eq!(new_state_candidate_preserve_move_attempt_count(), 0);
            assert_eq!(active_reblit_candidate_preserve_exchange_attempt_count(), 0);
            active.assert_evidence_unchanged(&active_before);
        }
    }
}

#[test]
fn startup_candidate_target_preparation_selection_is_binding_first_for_every_lease() {
    for fixture in [
        CandidatePreserveFixture::new(
            OperationKind::NewState,
            CandidateSource::Exchanged,
            RollbackActionOutcome::Applied,
            CandidateLayout::Staged,
        ),
        CandidatePreserveFixture::new_state_target_residue(
            CandidateSource::Exchanged,
            RollbackActionOutcome::Applied,
            0o500,
        ),
        CandidatePreserveFixture::new_state_empty_quarantine_prefix(
            CandidateSource::Exchanged,
            RollbackActionOutcome::Applied,
        ),
    ] {
        let target = transition_quarantine_path(&fixture.fixture, &fixture.candidate_intent);
        let target_before = target.exists().then(|| target_metadata(&target));
        let first = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        let authority = apply_authority(&fixture, &first, &reservation);
        drop(first);
        let second = fixture.open_journal();
        reset_new_state_candidate_preserve_move_attempt_count();
        let seal = UsrRollbackCandidatePreserveEffectSeal::new_for_test();

        assert!(authority.into_effect_selection(&seal, &second).is_err());
        assert_eq!(new_state_candidate_preserve_move_attempt_count(), 0);
        assert_eq!(target.exists().then(|| target_metadata(&target)), target_before);
        fixture.assert_non_namespace_unchanged();
    }
}

fn apply_authority<'reservation>(
    fixture: &CandidatePreserveFixture,
    journal: &crate::transition_journal::TransitionJournalStore,
    reservation: &'reservation ActiveStateReservation,
) -> crate::client::startup_reconciliation::UsrRollbackCandidatePreserveApplyAuthority<'reservation> {
    let UsrRollbackCandidatePreserveAdmission::Apply(authority) = fixture.capture(journal, reservation) else {
        panic!("exact staged candidate-preservation evidence did not admit Apply authority");
    };
    authority
}

#[derive(Debug, Eq, PartialEq)]
struct TargetMetadata {
    device: u64,
    inode: u64,
    mode: u32,
    links: u64,
    owner: u32,
    group: u32,
    length: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

fn target_metadata(path: &std::path::Path) -> TargetMetadata {
    let metadata = fs::symlink_metadata(path).unwrap();
    TargetMetadata {
        device: metadata.dev(),
        inode: metadata.ino(),
        mode: metadata.mode(),
        links: metadata.nlink(),
        owner: metadata.uid(),
        group: metadata.gid(),
        length: metadata.len(),
        modified_seconds: metadata.mtime(),
        modified_nanoseconds: metadata.mtime_nsec(),
        changed_seconds: metadata.ctime(),
        changed_nanoseconds: metadata.ctime_nsec(),
    }
}
