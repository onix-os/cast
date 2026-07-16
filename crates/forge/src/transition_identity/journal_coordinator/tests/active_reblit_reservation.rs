use crate::transition_identity::{
    RetainedActivePreviousSlotParkingFaultPoint as SlotFaultPoint,
    RetainedStagingWrapperRotationFaultPoint as WrapperFaultPoint, RetainedStagingWrapperRotationOutcome,
    arm_active_previous_slot_parking_faults, arm_before_staging_wrapper_final_preparation_revalidation,
    arm_before_staging_wrapper_journal_validation, arm_staging_wrapper_rotation_faults,
};
use crate::transition_identity::active_previous_slot_parking::RetainedActivePreviousSlotParkingOutcome;
use crate::transition_identity::staging_wrapper_rotation::{
    ActiveReblitReservationError, StagingWrapperPreparationEvidenceStage,
    StagingWrapperPreparationFailure,
};

fn active_reblit_reservation_candidate(
    retain_previous_slot: bool,
) -> (CoordinatorFixture, PreparedActiveReblitReservationCoordinator) {
    let (fixture, identity, authority) = fixture_parts(
        CandidateKind::ActiveReblit,
        PreviousKind::Active,
        false,
        retain_previous_slot,
    );
    assert!(authority.is_none());
    let coordinator = identity
        .begin_transition(request(CandidateKind::ActiveReblit, &fixture, false, false))
        .unwrap()
        .begin_candidate_prepare()
        .unwrap();
    let prepared = finish_candidate_prepare(coordinator).unwrap();
    let PreparedStatefulTransitionCoordinator::ActiveReblitReservation(prepared) = prepared else {
        panic!("ActiveReblit candidate did not enter the reservation typestate")
    };
    (fixture, prepared)
}

fn active_reblit_replacement_path(
    fixture: &CoordinatorFixture,
    record: &TransitionRecord,
    index: usize,
) -> PathBuf {
    fixture.installation.state_quarantine_dir().join(format!(
        "replaced-active-reblit-wrapper-{}-{}-{index}",
        fixture.candidate_state,
        record.previous.tree_token.as_str()
    ))
}

fn active_reblit_parked_slot_path(
    fixture: &CoordinatorFixture,
    record: &TransitionRecord,
    index: usize,
) -> PathBuf {
    fixture.installation.root_path(format!(
        ".archived-candidate-slot-{}-{}-{index}",
        fixture.previous_state,
        record.previous.tree_token.as_str()
    ))
}

fn active_reblit_slot_marker_path(wrapper: &Path, state: state::Id, token: &str) -> PathBuf {
    wrapper.join(format!(".cast-state-slot-{state}-{token}"))
}

fn assert_empty_private_reservation(path: &Path) {
    let metadata = fs::symlink_metadata(path).unwrap();
    assert!(metadata.file_type().is_dir());
    assert_eq!(metadata.uid(), nix::unistd::Uid::effective().as_raw());
    assert_eq!(metadata.permissions().mode() & 0o7777, 0o700);
    assert_eq!(fs::read_dir(path).unwrap().count(), 0);
}

fn assert_active_reblit_candidate_prepared(fixture: &CoordinatorFixture) {
    assert_record_prefix(
        &read_canonical(&fixture.installation.root),
        Operation::ActiveReblit,
        Phase::CandidatePrepared,
        3,
    );
}

#[test]
fn journal_coordinator_active_reblit_reservation_keeps_wrong_wrapper_mode_untouched() {
    let (fixture, prepared) = active_reblit_reservation_candidate(false);
    let staging = fixture.installation.staging_dir();
    fs::set_permissions(&staging, fs::Permissions::from_mode(0o755)).unwrap();
    let before = fs::symlink_metadata(&staging).unwrap();
    let record = prepared.record().clone();

    let failure = prepared
        .reserve_for_transaction_triggers(&fixture.installation)
        .unwrap_err();

    assert!(matches!(
        failure,
        ActiveReblitReservationFailure::Reservation {
            source: ActiveReblitReservationError::Replacement(ref source),
            ..
        } if source.outcome() == RetainedStagingWrapperRotationOutcome::NotApplied
    ));
    let after = fs::symlink_metadata(&staging).unwrap();
    assert_eq!((after.dev(), after.ino()), (before.dev(), before.ino()));
    assert_eq!(after.permissions().mode() & 0o7777, 0o755);
    assert_eq!(fs::read_dir(&staging).unwrap().count(), 1);
    assert!(!active_reblit_replacement_path(&fixture, &record, 0).exists());
    assert_active_reblit_candidate_prepared(&fixture);
}

#[test]
fn journal_coordinator_active_reblit_reservation_preserves_typed_coordinator_evidence() {
    let (fixture, prepared) = active_reblit_reservation_candidate(false);
    let candidate = fixture.candidate_path.clone();
    arm_before_staging_wrapper_journal_validation(move || {
        fs::set_permissions(candidate, fs::Permissions::from_mode(0o700)).unwrap();
    });

    let failure = prepared
        .reserve_for_transaction_triggers(&fixture.installation)
        .unwrap_err();

    let ActiveReblitReservationFailure::Reservation {
        source: ActiveReblitReservationError::Replacement(source),
        ..
    } = &failure
    else {
        panic!("journal callback failure lost its replacement stage: {failure:#?}")
    };
    assert_eq!(source.outcome(), RetainedStagingWrapperRotationOutcome::NotApplied);
    assert!(matches!(
        source.coordinator_evidence_for_test(),
        Some(StatefulTransitionCoordinatorError::Identity(_))
    ));
    assert_active_reblit_candidate_prepared(&fixture);
}

#[test]
fn journal_coordinator_active_reblit_reservation_handles_one_link_and_parks_two_link_previous() {
    for retain_previous_slot in [false, true] {
        let (fixture, prepared) = active_reblit_reservation_candidate(retain_previous_slot);
        let record = prepared.record().clone();
        let token = record.previous.tree_token.as_str();
        let canonical = fixture.installation.root_path(fixture.previous_state.to_string());
        let parked = active_reblit_parked_slot_path(&fixture, &record, 0);
        let live_marker = fixture.installation.root.join("usr/.cast-tree-id");
        let marker_before = fs::symlink_metadata(&live_marker).unwrap();
        let canonical_before = retain_previous_slot.then(|| fs::symlink_metadata(&canonical).unwrap());

        let ready = prepared
            .reserve_for_transaction_triggers(&fixture.installation)
            .unwrap();

        assert_eq!(ready.record().phase, Phase::CandidatePrepared);
        assert_empty_private_reservation(&active_reblit_replacement_path(&fixture, &record, 0));
        if retain_previous_slot {
            assert!(!canonical.exists());
            let parked_metadata = fs::symlink_metadata(&parked).unwrap();
            let canonical_before = canonical_before.unwrap();
            assert_eq!(
                (parked_metadata.dev(), parked_metadata.ino()),
                (canonical_before.dev(), canonical_before.ino())
            );
            let parked_marker = fs::symlink_metadata(active_reblit_slot_marker_path(
                &parked,
                fixture.previous_state,
                token,
            ))
            .unwrap();
            assert_eq!((parked_marker.dev(), parked_marker.ino()), (marker_before.dev(), marker_before.ino()));
            assert_eq!(parked_marker.nlink(), 2);
        } else {
            assert_eq!(marker_before.nlink(), 1);
            assert!(!canonical.exists());
            assert!(!parked.exists());
        }
        assert_active_reblit_candidate_prepared(&fixture);
    }
}

#[test]
fn journal_coordinator_active_reblit_reservation_reports_ambiguous_replacement_stage() {
    let (fixture, prepared) = active_reblit_reservation_candidate(false);
    let record = prepared.record().clone();
    let replacement = active_reblit_replacement_path(&fixture, &record, 0);
    let displaced = replacement.with_extension("retained-displaced");
    let hook_replacement = replacement.clone();
    arm_before_staging_wrapper_final_preparation_revalidation(move || {
        fs::rename(hook_replacement, displaced).unwrap();
    });

    let failure = prepared
        .reserve_for_transaction_triggers(&fixture.installation)
        .unwrap_err();

    assert!(matches!(
        failure,
        ActiveReblitReservationFailure::Reservation {
            source: ActiveReblitReservationError::ReplacementPreparation(
                StagingWrapperPreparationFailure::DurableReservationEvidenceFailed {
                    stage: StagingWrapperPreparationEvidenceStage::EvidenceSandwich,
                    ..
                }
            ),
            ..
        }
    ));
    assert!(!replacement.exists());
    assert_active_reblit_candidate_prepared(&fixture);
}

#[test]
fn journal_coordinator_active_reblit_reservation_reports_durable_final_checkpoint_failure() {
    let (fixture, prepared) = active_reblit_reservation_candidate(false);
    let record = prepared.record().clone();
    arm_staging_wrapper_rotation_faults([WrapperFaultPoint::FinalPreparationRevalidation]);

    let failure = prepared
        .reserve_for_transaction_triggers(&fixture.installation)
        .unwrap_err();
    arm_staging_wrapper_rotation_faults([]);

    assert!(matches!(
        failure,
        ActiveReblitReservationFailure::Reservation {
            source: ActiveReblitReservationError::ReplacementPreparation(
                StagingWrapperPreparationFailure::DurableReservationEvidenceFailed {
                    stage: StagingWrapperPreparationEvidenceStage::FinalCheckpoint,
                    ..
                }
            ),
            ..
        }
    ));
    assert_empty_private_reservation(&active_reblit_replacement_path(&fixture, &record, 0));
    assert_active_reblit_candidate_prepared(&fixture);
}

#[test]
fn journal_coordinator_active_reblit_reservation_retries_one_durability_unproven_fault() {
    let (fixture, prepared) = active_reblit_reservation_candidate(false);
    let record = prepared.record().clone();
    arm_staging_wrapper_rotation_faults([WrapperFaultPoint::ReplacementPreparationSync]);

    let ready = prepared
        .reserve_for_transaction_triggers(&fixture.installation)
        .unwrap();
    arm_staging_wrapper_rotation_faults([]);

    assert_eq!(ready.record().phase, Phase::CandidatePrepared);
    assert_empty_private_reservation(&active_reblit_replacement_path(&fixture, &record, 0));
    assert_active_reblit_candidate_prepared(&fixture);
}

#[test]
fn journal_coordinator_active_reblit_reservation_reports_applied_slot_after_durable_replacement() {
    let (fixture, prepared) = active_reblit_reservation_candidate(true);
    let record = prepared.record().clone();
    let canonical = fixture.installation.root_path(fixture.previous_state.to_string());
    let parked = active_reblit_parked_slot_path(&fixture, &record, 0);
    arm_active_previous_slot_parking_faults([SlotFaultPoint::RootsPostSync, SlotFaultPoint::RootsPostSync]);

    let failure = prepared
        .reserve_for_transaction_triggers(&fixture.installation)
        .unwrap_err();
    arm_active_previous_slot_parking_faults([]);

    assert!(matches!(
        failure,
        ActiveReblitReservationFailure::Reservation {
            source: ActiveReblitReservationError::PreviousSlotAfterDurableReplacement(ref source),
            ..
        } if source.outcome() == RetainedActivePreviousSlotParkingOutcome::Applied
    ));
    assert_empty_private_reservation(&active_reblit_replacement_path(&fixture, &record, 0));
    assert!(!canonical.exists());
    assert!(parked.is_dir());
    assert_active_reblit_candidate_prepared(&fixture);
}

#[test]
fn journal_coordinator_active_reblit_reservation_preserves_foreign_name_exhaustion() {
    {
        let (fixture, prepared) = active_reblit_reservation_candidate(false);
        let record = prepared.record().clone();
        for index in 0..256 {
            fs::write(active_reblit_replacement_path(&fixture, &record, index), index.to_string()).unwrap();
        }

        let failure = prepared
            .reserve_for_transaction_triggers(&fixture.installation)
            .unwrap_err();

        assert!(matches!(
            failure,
            ActiveReblitReservationFailure::Reservation {
                source: ActiveReblitReservationError::Replacement(ref source),
                ..
            } if source.outcome() == RetainedStagingWrapperRotationOutcome::NotApplied
        ));
        for index in 0..256 {
            assert_eq!(
                fs::read_to_string(active_reblit_replacement_path(&fixture, &record, index)).unwrap(),
                index.to_string()
            );
        }
        assert_active_reblit_candidate_prepared(&fixture);
    }

    {
        let (fixture, prepared) = active_reblit_reservation_candidate(true);
        let record = prepared.record().clone();
        let canonical = fixture.installation.root_path(fixture.previous_state.to_string());
        for index in 0..256 {
            fs::write(active_reblit_parked_slot_path(&fixture, &record, index), index.to_string()).unwrap();
        }

        let failure = prepared
            .reserve_for_transaction_triggers(&fixture.installation)
            .unwrap_err();

        assert!(matches!(
            failure,
            ActiveReblitReservationFailure::Reservation {
                source: ActiveReblitReservationError::PreviousSlotAfterDurableReplacement(ref source),
                ..
            } if source.outcome() == RetainedActivePreviousSlotParkingOutcome::NotApplied
        ));
        assert_empty_private_reservation(&active_reblit_replacement_path(&fixture, &record, 0));
        assert!(canonical.is_dir());
        for index in 0..256 {
            assert_eq!(
                fs::read_to_string(active_reblit_parked_slot_path(&fixture, &record, index)).unwrap(),
                index.to_string()
            );
        }
        assert_active_reblit_candidate_prepared(&fixture);
    }
}
