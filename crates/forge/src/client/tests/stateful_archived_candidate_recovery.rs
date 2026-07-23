#[test]
fn retained_archived_candidate_move_classifies_and_resumes_exact_layouts() {
    use crate::transition_identity::{
        RetainedArchivedCandidateMoveFaultPoint as FaultPoint, arm_retained_archived_candidate_move_fault,
    };

    let fixture = stateful_transition_fixture(true);
    let state_root = fixture.client.installation.root_path(fixture.candidate.id.to_string());
    let staging_root = fixture.client.installation.staging_dir();
    let state_inode = fs::symlink_metadata(&state_root).unwrap().ino();
    let staging_inode = fs::symlink_metadata(&staging_root).unwrap().ino();
    let identity = fixture
        .client
        .prepare_stateful_tree_identity(&state_root.join("usr"), fixture.candidate.id)
        .unwrap();

    arm_retained_archived_candidate_move_fault(FaultPoint::BeforeExchange);
    let failure = identity
        .stage_archived_candidate(&fixture.client.installation, fixture.candidate.id)
        .unwrap_err();
    assert_eq!(failure.outcome(), RetainedArchivedCandidateMoveOutcome::NotApplied);
    assert_eq!(fs::symlink_metadata(&state_root).unwrap().ino(), state_inode);
    assert_eq!(fs::symlink_metadata(&staging_root).unwrap().ino(), staging_inode);

    arm_retained_archived_candidate_move_fault(FaultPoint::AfterExchange);
    identity
        .stage_archived_candidate(&fixture.client.installation, fixture.candidate.id)
        .unwrap();
    assert_eq!(fs::symlink_metadata(&state_root).unwrap().ino(), staging_inode);
    assert_eq!(fs::symlink_metadata(&staging_root).unwrap().ino(), state_inode);
    arm_retained_archived_candidate_move_fault(FaultPoint::AfterExchange);
    identity
        .rearchive_archived_candidate(&fixture.client.installation, fixture.candidate.id)
        .unwrap();
    assert_eq!(fs::symlink_metadata(&state_root).unwrap().ino(), state_inode);
    assert_eq!(fs::symlink_metadata(&staging_root).unwrap().ino(), staging_inode);

    arm_retained_archived_candidate_move_fault(FaultPoint::CandidatePostSync);
    let failure = identity
        .stage_archived_candidate(&fixture.client.installation, fixture.candidate.id)
        .unwrap_err();
    assert_eq!(failure.outcome(), RetainedArchivedCandidateMoveOutcome::Applied);
    assert_eq!(fs::symlink_metadata(&state_root).unwrap().ino(), staging_inode);
    assert_eq!(fs::symlink_metadata(&staging_root).unwrap().ino(), state_inode);
    identity
        .finish_applied_archived_candidate_stage(&fixture.client.installation, fixture.candidate.id)
        .unwrap();
    assert_eq!(fs::symlink_metadata(&staging_root).unwrap().ino(), state_inode);

    arm_retained_archived_candidate_move_fault(FaultPoint::RootsParentSync);
    let failure = identity
        .rearchive_archived_candidate(&fixture.client.installation, fixture.candidate.id)
        .unwrap_err();
    assert_eq!(failure.outcome(), RetainedArchivedCandidateMoveOutcome::Applied);
    assert_eq!(fs::symlink_metadata(&state_root).unwrap().ino(), state_inode);
    assert_eq!(fs::symlink_metadata(&staging_root).unwrap().ino(), staging_inode);
    identity
        .finish_applied_archived_candidate_rearchive(&fixture.client.installation, fixture.candidate.id)
        .unwrap();
    assert_eq!(fs::symlink_metadata(&state_root).unwrap().ino(), state_inode);
    assert_eq!(fs::symlink_metadata(&staging_root).unwrap().ino(), staging_inode);
}

#[test]
fn retained_archived_candidate_move_adopts_only_the_exact_exchanged_wrappers() {
    let fixture = stateful_transition_fixture(true);
    let state_root = fixture.client.installation.root_path(fixture.candidate.id.to_string());
    let staging_root = fixture.client.installation.staging_dir();
    let identity = fixture
        .client
        .prepare_stateful_tree_identity(&state_root.join("usr"), fixture.candidate.id)
        .unwrap();

    let staged_state = state_root.clone();
    let staged_staging = staging_root.clone();
    crate::transition_identity::arm_before_retained_archived_candidate_exchange(move || {
        externally_exchange_directory_names(&staged_state, &staged_staging);
    });
    identity
        .stage_archived_candidate(&fixture.client.installation, fixture.candidate.id)
        .unwrap();
    assert_eq!(
        fs::read_to_string(staging_root.join("usr/.stateID")).unwrap(),
        fixture.candidate.id.to_string()
    );

    let archived_state = state_root.clone();
    let archived_staging = staging_root.clone();
    crate::transition_identity::arm_before_retained_archived_candidate_exchange(move || {
        externally_exchange_directory_names(&archived_state, &archived_staging);
    });
    identity
        .rearchive_archived_candidate(&fixture.client.installation, fixture.candidate.id)
        .unwrap();
    assert_eq!(
        fs::read_to_string(state_root.join("usr/.stateID")).unwrap(),
        fixture.candidate.id.to_string()
    );
    assert!(!staging_root.join("usr").exists());
}

#[test]
fn displaced_archived_candidate_slot_retirement_preserves_racing_occupants() {
    let fixture = stateful_transition_fixture(true);
    let state_root = fixture.client.installation.root_path(fixture.candidate.id.to_string());
    let staging_root = fixture.client.installation.staging_dir();
    let identity = fixture
        .client
        .prepare_stateful_tree_identity(&state_root.join("usr"), fixture.candidate.id)
        .unwrap();
    identity
        .stage_archived_candidate(&fixture.client.installation, fixture.candidate.id)
        .unwrap();

    let held_candidate = fixture.client.installation.root_path("held-archived-candidate-usr");
    fs::rename(staging_root.join("usr"), &held_candidate).unwrap();
    let displaced_inode = fs::symlink_metadata(&state_root).unwrap().ino();
    let displaced = fixture.client.installation.root_path("displaced-staging-wrapper");
    let raced_state = state_root.clone();
    let raced_displaced = displaced.clone();
    crate::transition_identity::arm_before_retired_archived_candidate_slot_move(move || {
        fs::rename(&raced_state, &raced_displaced).unwrap();
        fs::create_dir(&raced_state).unwrap();
        fs::set_permissions(&raced_state, Permissions::from_mode(0o700)).unwrap();
        fs::write(raced_state.join("foreign"), b"racing occupant").unwrap();
    });

    identity
        .retire_displaced_archived_candidate_slot(&fixture.client.installation, fixture.candidate.id)
        .unwrap_err();

    assert_eq!(fs::symlink_metadata(&displaced).unwrap().ino(), displaced_inode);
    assert!(held_candidate.join(".stateID").is_file());
    assert!(!state_root.exists());
    let parking = archived_candidate_slot_parking_paths(&fixture.client.installation, fixture.candidate.id);
    assert_eq!(parking.len(), 1);
    assert_eq!(fs::read(parking[0].join("foreign")).unwrap(), b"racing occupant");
}

#[test]
fn archived_activation_resumes_applied_staging_suffix_before_full_recovery() {
    let fixture = stateful_transition_fixture(true);
    crate::transition_identity::arm_retained_archived_candidate_move_fault(
        crate::transition_identity::RetainedArchivedCandidateMoveFaultPoint::CandidatePostSync,
    );

    let error = fixture
        .client
        .activate_state_with_checkpoint(fixture.candidate.id, true, true, |checkpoint| {
            if checkpoint == StatefulTransitionCheckpoint::BeforePreviousStateArchive {
                Err(injected_state_transition_error("recover after staged suffix resume"))
            } else {
                Ok(())
            }
        })
        .unwrap_err();

    assert!(matches!(error, Error::StatefulTransitionUsrRestored { .. }));
    assert_recovered_stateful_transition(&fixture);
}

#[test]
fn archived_activation_resumes_applied_rearchive_suffix_during_full_recovery() {
    let fixture = stateful_transition_fixture(true);

    let error = fixture
        .client
        .activate_state_with_checkpoint(fixture.candidate.id, true, true, |checkpoint| {
            if checkpoint == StatefulTransitionCheckpoint::BeforePreviousStateArchive {
                crate::transition_identity::arm_retained_archived_candidate_move_fault(
                    crate::transition_identity::RetainedArchivedCandidateMoveFaultPoint::FinalRevalidation,
                );
                Err(injected_state_transition_error("resume rearchive durability suffix"))
            } else {
                Ok(())
            }
        })
        .unwrap_err();

    assert!(matches!(error, Error::StatefulTransitionUsrRestored { .. }));
    assert_recovered_stateful_transition(&fixture);
}

#[test]
fn archived_activation_keeps_rearchive_preparation_sticky_through_presync_faults() {
    use crate::transition_identity::{
        RetainedArchivedCandidateMoveFaultPoint as FaultPoint, arm_retained_archived_candidate_move_fault,
    };

    for fault in [
        FaultPoint::CandidatePreSync,
        FaultPoint::CandidateWrapperSync,
        FaultPoint::DisplacedWrapperSync,
    ] {
        let fixture = stateful_transition_fixture(true);
        let error = fixture
            .client
            .activate_state_with_checkpoint(fixture.candidate.id, true, true, |checkpoint| {
                if checkpoint == StatefulTransitionCheckpoint::BeforePreviousStateArchive {
                    arm_retained_archived_candidate_move_fault(fault);
                    Err(injected_state_transition_error("retry sticky rearchive preparation"))
                } else {
                    Ok(())
                }
            })
            .unwrap_err();

        assert!(
            matches!(error, Error::StatefulTransitionUsrRestored { .. }),
            "recovery did not complete for {fault:?}: {error:#?}"
        );
        assert_recovered_stateful_transition(&fixture);
    }
}

#[test]
fn forged_exact_tree_marker_hardlink_is_not_adopted_in_process() {
    let fixture = stateful_transition_fixture(true);
    let state_root = fixture.client.installation.root_path(fixture.candidate.id.to_string());
    let identity = fixture
        .client
        .prepare_stateful_tree_identity(&state_root.join("usr"), fixture.candidate.id)
        .unwrap();
    let token = recovery_tree_token(&state_root.join("usr"));
    let forged = state_root.join(format!(".cast-state-slot-{}-{token}", fixture.candidate.id));
    fs::hard_link(state_root.join("usr/.cast-tree-id"), &forged).unwrap();

    let failure = identity
        .stage_archived_candidate(&fixture.client.installation, fixture.candidate.id)
        .unwrap_err();
    assert_eq!(failure.outcome(), RetainedArchivedCandidateMoveOutcome::NotApplied);
    assert_eq!(
        fs::symlink_metadata(&forged).unwrap().ino(),
        fs::symlink_metadata(state_root.join("usr/.cast-tree-id"))
            .unwrap()
            .ino()
    );
}

#[test]
fn exact_parked_tree_marker_hardlink_is_reauthorized_after_reopen() {
    let fixture = stateful_transition_fixture(true);
    let state_root = fixture.client.installation.root_path(fixture.candidate.id.to_string());
    let staging_root = fixture.client.installation.staging_dir();
    let held_candidate = fixture.client.installation.root_path("held-candidate-for-slot-reopen");
    let identity = fixture
        .client
        .prepare_stateful_tree_identity(&state_root.join("usr"), fixture.candidate.id)
        .unwrap();
    identity
        .stage_archived_candidate(&fixture.client.installation, fixture.candidate.id)
        .unwrap();
    fs::rename(staging_root.join("usr"), &held_candidate).unwrap();
    identity
        .retire_displaced_archived_candidate_slot(&fixture.client.installation, fixture.candidate.id)
        .unwrap();
    drop(identity);

    let marker = held_candidate.join(".cast-tree-id");
    let marker_metadata = fs::symlink_metadata(&marker).unwrap();
    assert_eq!(marker_metadata.nlink(), 2);
    let parked = archived_candidate_slot_parking_paths(&fixture.client.installation, fixture.candidate.id);
    assert_eq!(parked.len(), 1);
    let slot_link = fs::read_dir(&parked[0])
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".cast-state-slot-")
        })
        .unwrap();
    assert_eq!(fs::symlink_metadata(&slot_link).unwrap().ino(), marker_metadata.ino());

    let reopened = fixture
        .client
        .prepare_stateful_tree_identity(&held_candidate, fixture.candidate.id)
        .unwrap();
    reopened.verify_candidate_for_recovery(&held_candidate).unwrap();
    drop(reopened);

    let marker_name = slot_link.file_name().unwrap().to_owned();
    let token = marker_name.to_string_lossy().rsplit('-').next().unwrap().to_owned();
    let copied_wrapper = fixture
        .client
        .installation
        .root_path(format!(".archived-candidate-slot-{}-{token}-1", fixture.candidate.id));
    fs::create_dir(&copied_wrapper).unwrap();
    fs::set_permissions(&copied_wrapper, Permissions::from_mode(0o700)).unwrap();
    let copied_marker = copied_wrapper.join(&marker_name);
    fs::copy(&marker, &copied_marker).unwrap();
    fs::set_permissions(&copied_marker, Permissions::from_mode(0o444)).unwrap();
    assert_eq!(
        archived_candidate_slot_parking_paths(&fixture.client.installation, fixture.candidate.id).len(),
        2
    );
    fixture
        .client
        .prepare_stateful_tree_identity(&held_candidate, fixture.candidate.id)
        .unwrap_err();
    assert_ne!(
        fs::symlink_metadata(&copied_marker).unwrap().ino(),
        marker_metadata.ino()
    );

    fs::remove_file(&copied_marker).unwrap();
    fs::remove_dir(&copied_wrapper).unwrap();
    let extra_link = fixture.client.installation.root_path("extra-tree-marker-hardlink");
    fs::hard_link(&marker, &extra_link).unwrap();
    fixture
        .client
        .prepare_stateful_tree_identity(&held_candidate, fixture.candidate.id)
        .unwrap_err();
    assert_eq!(fs::symlink_metadata(&marker).unwrap().nlink(), 3);
}

#[test]
fn retained_archived_candidate_move_rejects_substituted_roots_as_ambiguous() {
    let fixture = stateful_transition_fixture(true);
    let roots = fixture.client.installation.root_path("");
    let displaced_roots = fixture.client.installation.root.join("displaced-state-roots");
    let candidate = fixture.candidate.id;
    let identity = fixture
        .client
        .prepare_stateful_tree_identity(
            &fixture.client.installation.root_path(candidate.to_string()).join("usr"),
            candidate,
        )
        .unwrap();
    let raced_roots = roots.clone();
    let raced_displaced = displaced_roots.clone();
    crate::transition_identity::arm_before_retained_archived_candidate_exchange(move || {
        fs::rename(&raced_roots, &raced_displaced).unwrap();
        fs::create_dir(&raced_roots).unwrap();
        fs::set_permissions(&raced_roots, Permissions::from_mode(0o700)).unwrap();
    });

    let failure = identity
        .stage_archived_candidate(&fixture.client.installation, candidate)
        .unwrap_err();

    assert_eq!(failure.outcome(), RetainedArchivedCandidateMoveOutcome::Ambiguous);
    assert!(roots.is_dir());
    assert!(
        displaced_roots
            .join(candidate.to_string())
            .join("usr/.stateID")
            .is_file()
    );
    assert!(displaced_roots.join("staging").is_dir());
}

#[test]
fn retained_archived_candidate_move_rejects_a_substituted_source_wrapper() {
    let fixture = stateful_transition_fixture(true);
    let state_root = fixture.client.installation.root_path(fixture.candidate.id.to_string());
    let displaced = fixture
        .client
        .installation
        .root_path("displaced-archived-candidate-wrapper");
    let identity = fixture
        .client
        .prepare_stateful_tree_identity(&state_root.join("usr"), fixture.candidate.id)
        .unwrap();
    let raced_state = state_root.clone();
    let raced_displaced = displaced.clone();
    crate::transition_identity::arm_before_retained_archived_candidate_exchange(move || {
        fs::rename(&raced_state, &raced_displaced).unwrap();
        fs::create_dir(&raced_state).unwrap();
        fs::set_permissions(&raced_state, Permissions::from_mode(0o700)).unwrap();
        fs::write(raced_state.join("foreign"), b"replacement wrapper").unwrap();
    });

    let failure = identity
        .stage_archived_candidate(&fixture.client.installation, fixture.candidate.id)
        .unwrap_err();

    assert_eq!(failure.outcome(), RetainedArchivedCandidateMoveOutcome::Ambiguous);
    assert_eq!(fs::read(state_root.join("foreign")).unwrap(), b"replacement wrapper");
    assert_eq!(
        fs::read_to_string(displaced.join("usr/.stateID")).unwrap(),
        fixture.candidate.id.to_string()
    );
    assert!(!fixture.client.installation.staging_path("usr").exists());
}

#[test]
fn retained_archived_candidate_move_rejects_a_substituted_fixed_staging_wrapper() {
    let fixture = stateful_transition_fixture(true);
    let state_root = fixture.client.installation.root_path(fixture.candidate.id.to_string());
    let staging_root = fixture.client.installation.staging_dir();
    let displaced = fixture.client.installation.root_path("displaced-fixed-staging-wrapper");
    let identity = fixture
        .client
        .prepare_stateful_tree_identity(&state_root.join("usr"), fixture.candidate.id)
        .unwrap();
    let raced_staging = staging_root.clone();
    let raced_displaced = displaced.clone();
    crate::transition_identity::arm_before_retained_archived_candidate_exchange(move || {
        fs::rename(&raced_staging, &raced_displaced).unwrap();
        fs::create_dir(&raced_staging).unwrap();
        fs::set_permissions(&raced_staging, Permissions::from_mode(0o700)).unwrap();
        fs::write(raced_staging.join("foreign"), b"replacement staging wrapper").unwrap();
    });

    let failure = identity
        .stage_archived_candidate(&fixture.client.installation, fixture.candidate.id)
        .unwrap_err();

    assert_eq!(failure.outcome(), RetainedArchivedCandidateMoveOutcome::Ambiguous);
    assert_eq!(
        fs::read(staging_root.join("foreign")).unwrap(),
        b"replacement staging wrapper"
    );
    assert!(displaced.is_dir());
    assert_eq!(
        fs::read_to_string(state_root.join("usr/.stateID")).unwrap(),
        fixture.candidate.id.to_string()
    );
}

#[test]
fn displaced_archived_candidate_restore_faults_are_exactly_classified_and_resumable() {
    use crate::transition_identity::{
        RetainedArchivedCandidateMoveFaultPoint as FaultPoint, arm_retained_archived_candidate_move_fault,
    };

    for (fault, expected) in [
        (
            FaultPoint::BeforeDisplacedSlotRestore,
            Some(RetainedArchivedCandidateMoveOutcome::NotApplied),
        ),
        (FaultPoint::AfterDisplacedSlotRestore, None),
        (
            FaultPoint::RootsAfterDisplacedSlotRestoreSync,
            Some(RetainedArchivedCandidateMoveOutcome::RearchivePreparationApplied),
        ),
    ] {
        let fixture = stateful_transition_fixture(true);
        let state_root = fixture.client.installation.root_path(fixture.candidate.id.to_string());
        let staging_root = fixture.client.installation.staging_dir();
        let held_candidate = fixture
            .client
            .installation
            .root_path(format!("held-candidate-for-{fault:?}"));
        let identity = fixture
            .client
            .prepare_stateful_tree_identity(&state_root.join("usr"), fixture.candidate.id)
            .unwrap();
        identity
            .stage_archived_candidate(&fixture.client.installation, fixture.candidate.id)
            .unwrap();
        fs::rename(staging_root.join("usr"), &held_candidate).unwrap();
        arm_retained_archived_candidate_move_fault(FaultPoint::FinalDisplacedSlotRetirementRevalidation);
        identity
            .retire_displaced_archived_candidate_slot(&fixture.client.installation, fixture.candidate.id)
            .unwrap_err();
        assert_eq!(
            archived_candidate_slot_parking_paths(&fixture.client.installation, fixture.candidate.id).len(),
            1
        );
        fs::rename(&held_candidate, staging_root.join("usr")).unwrap();

        arm_retained_archived_candidate_move_fault(fault);
        let first = identity.rearchive_archived_candidate(&fixture.client.installation, fixture.candidate.id);
        match expected {
            Some(expected) => assert_eq!(first.unwrap_err().outcome(), expected, "fault {fault:?}"),
            None => first.unwrap(),
        }
        if expected.is_some() {
            identity
                .rearchive_archived_candidate(&fixture.client.installation, fixture.candidate.id)
                .unwrap();
        }

        assert!(archived_candidate_slot_parking_paths(&fixture.client.installation, fixture.candidate.id).is_empty());
        assert_eq!(
            fs::read_to_string(state_root.join("usr/.stateID")).unwrap(),
            fixture.candidate.id.to_string()
        );
        assert!(!staging_root.join("usr").exists());
    }
}

#[test]
fn archived_candidate_marker_transfer_faults_resume_without_a_second_wrapper_exchange() {
    use crate::transition_identity::{
        RetainedArchivedCandidateMoveFaultPoint as FaultPoint, arm_retained_archived_candidate_move_fault,
    };

    for fault in [
        FaultPoint::BeforeSlotMarkerTransfer,
        FaultPoint::SlotMarkerParentSync,
        FaultPoint::FinalSlotMarkerRevalidation,
    ] {
        let fixture = stateful_transition_fixture(true);
        let state_root = fixture.client.installation.root_path(fixture.candidate.id.to_string());
        let staging_root = fixture.client.installation.staging_dir();
        let state_inode = fs::symlink_metadata(&state_root).unwrap().ino();
        let staging_inode = fs::symlink_metadata(&staging_root).unwrap().ino();
        let identity = fixture
            .client
            .prepare_stateful_tree_identity(&state_root.join("usr"), fixture.candidate.id)
            .unwrap();

        arm_retained_archived_candidate_move_fault(fault);
        let failure = identity
            .stage_archived_candidate(&fixture.client.installation, fixture.candidate.id)
            .unwrap_err();
        assert_eq!(failure.outcome(), RetainedArchivedCandidateMoveOutcome::Applied);
        assert_eq!(fs::symlink_metadata(&state_root).unwrap().ino(), staging_inode);
        assert_eq!(fs::symlink_metadata(&staging_root).unwrap().ino(), state_inode);

        identity
            .finish_applied_archived_candidate_stage(&fixture.client.installation, fixture.candidate.id)
            .unwrap();
        assert_eq!(fs::symlink_metadata(&state_root).unwrap().ino(), staging_inode);
        assert_eq!(fs::symlink_metadata(&staging_root).unwrap().ino(), state_inode);
    }
}

#[test]
fn externally_premoved_slot_marker_fast_path_still_finishes_durability() {
    use crate::transition_identity::{
        RetainedArchivedCandidateMoveFaultPoint as FaultPoint, arm_before_archived_candidate_slot_marker_location,
        arm_retained_archived_candidate_move_fault,
    };

    let fixture = stateful_transition_fixture(true);
    let state_root = fixture.client.installation.root_path(fixture.candidate.id.to_string());
    let staging_root = fixture.client.installation.staging_dir();
    let state_inode = fs::symlink_metadata(&state_root).unwrap().ino();
    let staging_inode = fs::symlink_metadata(&staging_root).unwrap().ino();
    let identity = fixture
        .client
        .prepare_stateful_tree_identity(&state_root.join("usr"), fixture.candidate.id)
        .unwrap();
    let token = recovery_tree_token(&state_root.join("usr"));
    identity
        .stage_archived_candidate(&fixture.client.installation, fixture.candidate.id)
        .unwrap();
    let marker_name = format!(".cast-state-slot-{}-{token}", fixture.candidate.id);
    let source = state_root.join(&marker_name);
    let destination = staging_root.join(&marker_name);
    arm_before_archived_candidate_slot_marker_location(move || {
        fs::rename(&source, &destination).unwrap();
    });
    arm_retained_archived_candidate_move_fault(FaultPoint::SlotMarkerParentSync);

    let failure = identity
        .rearchive_archived_candidate(&fixture.client.installation, fixture.candidate.id)
        .unwrap_err();
    assert_eq!(
        failure.outcome(),
        RetainedArchivedCandidateMoveOutcome::RearchivePreparationApplied
    );
    fixture
        .client
        .rearchive_archived_candidate(&identity, fixture.candidate.id)
        .unwrap();
    assert_eq!(fs::symlink_metadata(&state_root).unwrap().ino(), state_inode);
    assert_eq!(fs::symlink_metadata(&staging_root).unwrap().ino(), staging_inode);
}

#[test]
fn archived_candidate_rearchive_marker_preparation_faults_are_resumable() {
    use crate::transition_identity::{
        RetainedArchivedCandidateMoveFaultPoint as FaultPoint, arm_retained_archived_candidate_move_fault,
    };

    for (fault, expected) in [
        (
            FaultPoint::BeforeSlotMarkerTransfer,
            Some(RetainedArchivedCandidateMoveOutcome::NotApplied),
        ),
        (FaultPoint::AfterSlotMarkerTransfer, None),
        (
            FaultPoint::SlotMarkerParentSync,
            Some(RetainedArchivedCandidateMoveOutcome::RearchivePreparationApplied),
        ),
        (
            FaultPoint::FinalSlotMarkerRevalidation,
            Some(RetainedArchivedCandidateMoveOutcome::RearchivePreparationApplied),
        ),
    ] {
        let fixture = stateful_transition_fixture(true);
        let state_root = fixture.client.installation.root_path(fixture.candidate.id.to_string());
        let staging_root = fixture.client.installation.staging_dir();
        let state_inode = fs::symlink_metadata(&state_root).unwrap().ino();
        let staging_inode = fs::symlink_metadata(&staging_root).unwrap().ino();
        let identity = fixture
            .client
            .prepare_stateful_tree_identity(&state_root.join("usr"), fixture.candidate.id)
            .unwrap();
        identity
            .stage_archived_candidate(&fixture.client.installation, fixture.candidate.id)
            .unwrap();

        arm_retained_archived_candidate_move_fault(fault);
        let first = identity.rearchive_archived_candidate(&fixture.client.installation, fixture.candidate.id);
        match expected {
            Some(expected) => assert_eq!(first.unwrap_err().outcome(), expected, "fault {fault:?}"),
            None => first.unwrap(),
        }
        if expected.is_some() {
            fixture
                .client
                .rearchive_archived_candidate(&identity, fixture.candidate.id)
                .unwrap();
        }

        assert_eq!(fs::symlink_metadata(&state_root).unwrap().ino(), state_inode);
        assert_eq!(fs::symlink_metadata(&staging_root).unwrap().ino(), staging_inode);
        assert_eq!(
            fs::read_to_string(state_root.join("usr/.stateID")).unwrap(),
            fixture.candidate.id.to_string()
        );
    }
}

#[test]
fn archived_candidate_parking_scan_skips_every_foreign_occupant_kind() {
    let fixture = stateful_transition_fixture(true);
    let state_root = fixture.client.installation.root_path(fixture.candidate.id.to_string());
    let staging_root = fixture.client.installation.staging_dir();
    let roots = fixture.client.installation.root_path("");
    let identity = fixture
        .client
        .prepare_stateful_tree_identity(&state_root.join("usr"), fixture.candidate.id)
        .unwrap();
    let token = recovery_tree_token(&state_root.join("usr"));
    let parking = |index| {
        roots.join(format!(
            ".archived-candidate-slot-{}-{token}-{index}",
            fixture.candidate.id
        ))
    };
    fs::write(parking(0), b"regular occupant").unwrap();
    symlink("/", parking(1)).unwrap();
    nix::unistd::mkfifo(&parking(2), Mode::from_bits_truncate(0o600)).unwrap();
    fs::create_dir(parking(3)).unwrap();
    fs::set_permissions(parking(3), Permissions::from_mode(0o777)).unwrap();

    identity
        .stage_archived_candidate(&fixture.client.installation, fixture.candidate.id)
        .unwrap();
    let held_candidate = fixture.client.installation.root_path("held-candidate-for-parking-scan");
    fs::rename(staging_root.join("usr"), &held_candidate).unwrap();
    identity
        .retire_displaced_archived_candidate_slot(&fixture.client.installation, fixture.candidate.id)
        .unwrap();

    assert_eq!(fs::read(parking(0)).unwrap(), b"regular occupant");
    assert!(fs::symlink_metadata(parking(1)).unwrap().file_type().is_symlink());
    assert!(fs::symlink_metadata(parking(2)).unwrap().file_type().is_fifo());
    assert_eq!(
        fs::symlink_metadata(parking(3)).unwrap().permissions().mode() & 0o7777,
        0o777
    );
    let retained = parking(4);
    assert!(retained.is_dir());
    let retained_entries = fs::read_dir(&retained)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    assert_eq!(retained_entries.len(), 1);
    assert!(retained_entries[0].starts_with(".cast-state-slot-"));
    assert!(held_candidate.join(".stateID").is_file());
}

#[test]
fn archived_candidate_restore_preparation_uses_one_bounded_client_retry() {
    use crate::transition_identity::{
        RetainedArchivedCandidateMoveFaultPoint as FaultPoint, arm_retained_archived_candidate_move_fault,
    };

    let fixture = stateful_transition_fixture(true);
    let state_root = fixture.client.installation.root_path(fixture.candidate.id.to_string());
    let staging_root = fixture.client.installation.staging_dir();
    let held_candidate = fixture
        .client
        .installation
        .root_path("held-candidate-for-client-restore-retry");
    let identity = fixture
        .client
        .prepare_stateful_tree_identity(&state_root.join("usr"), fixture.candidate.id)
        .unwrap();
    identity
        .stage_archived_candidate(&fixture.client.installation, fixture.candidate.id)
        .unwrap();
    fs::rename(staging_root.join("usr"), &held_candidate).unwrap();
    arm_retained_archived_candidate_move_fault(FaultPoint::FinalDisplacedSlotRetirementRevalidation);
    identity
        .retire_displaced_archived_candidate_slot(&fixture.client.installation, fixture.candidate.id)
        .unwrap_err();
    fs::rename(&held_candidate, staging_root.join("usr")).unwrap();

    arm_retained_archived_candidate_move_fault(FaultPoint::RootsAfterDisplacedSlotRestoreSync);
    fixture
        .client
        .rearchive_archived_candidate(&identity, fixture.candidate.id)
        .unwrap();

    assert_eq!(
        fs::read_to_string(state_root.join("usr/.stateID")).unwrap(),
        fixture.candidate.id.to_string()
    );
    assert!(!staging_root.join("usr").exists());
}

#[test]
fn archived_candidate_marker_preparation_after_restore_uses_one_bounded_client_retry() {
    use crate::transition_identity::{
        RetainedArchivedCandidateMoveFaultPoint as FaultPoint, arm_retained_archived_candidate_move_fault,
    };

    let fixture = stateful_transition_fixture(true);
    let state_root = fixture.client.installation.root_path(fixture.candidate.id.to_string());
    let staging_root = fixture.client.installation.staging_dir();
    let held_candidate = fixture
        .client
        .installation
        .root_path("held-candidate-for-client-marker-retry");
    let identity = fixture
        .client
        .prepare_stateful_tree_identity(&state_root.join("usr"), fixture.candidate.id)
        .unwrap();
    identity
        .stage_archived_candidate(&fixture.client.installation, fixture.candidate.id)
        .unwrap();
    fs::rename(staging_root.join("usr"), &held_candidate).unwrap();
    arm_retained_archived_candidate_move_fault(FaultPoint::FinalDisplacedSlotRetirementRevalidation);
    identity
        .retire_displaced_archived_candidate_slot(&fixture.client.installation, fixture.candidate.id)
        .unwrap_err();
    fs::rename(&held_candidate, staging_root.join("usr")).unwrap();

    arm_retained_archived_candidate_move_fault(FaultPoint::BeforeSlotMarkerTransfer);
    fixture
        .client
        .rearchive_archived_candidate(&identity, fixture.candidate.id)
        .unwrap();

    assert_eq!(
        fs::read_to_string(state_root.join("usr/.stateID")).unwrap(),
        fixture.candidate.id.to_string()
    );
    assert!(!staging_root.join("usr").exists());
}

#[test]
fn multiple_structural_reusable_state_slot_links_fail_closed() {
    let fixture = stateful_transition_fixture(false);
    let identity = exchanged_stateful_identity(&fixture);
    let installation = &fixture.client.installation;
    let staged = installation.staging_path("usr");
    let token = recovery_tree_token(&staged);
    let marker_name = format!(".cast-state-slot-{}-{token}", fixture.previous.id);

    for index in 0..2 {
        let slot = installation.root_path(format!(
            ".archived-candidate-slot-{}-{token}-{index}",
            fixture.previous.id
        ));
        fs::create_dir(&slot).unwrap();
        fs::set_permissions(&slot, Permissions::from_mode(0o700)).unwrap();
        let marker = slot.join(&marker_name);
        fs::hard_link(staged.join(".cast-tree-id"), marker).unwrap();
    }

    let failure = identity
        .archive_previous(installation, fixture.previous.id)
        .unwrap_err();

    assert_eq!(failure.outcome(), RetainedPreviousMoveOutcome::NotApplied);
    assert!(
        format!("{failure:?}").contains("links: 3"),
        "unexpected extra-link failure: {failure:?}"
    );
    assert!(staged.join(".stateID").is_file());
    assert!(!installation.root_path(fixture.previous.id.to_string()).exists());
}

#[test]
fn repeated_archived_activations_reuse_wrapper_slots_beyond_the_scan_bound() {
    let mut fixture = stateful_transition_fixture(true);
    fixture
        .client
        .activate_state_with_checkpoint(fixture.candidate.id, true, true, |_| Ok(()))
        .unwrap();
    fixture.client.installation.active_state = Some(fixture.candidate.id);
    let mut active = fixture.candidate.id;
    let mut next = fixture.previous.id;

    // The bounded parking namespace has 256 names. Crossing it proves
    // successful activation reuses the authenticated wrapper instead of
    // consuming one name per transaction.
    for _ in 0..257 {
        let replaced = fixture
            .client
            .activate_state_with_checkpoint(next, true, true, |_| Ok(()))
            .unwrap();
        assert_eq!(replaced, active);
        fixture.client.installation.active_state = Some(next);
        std::mem::swap(&mut active, &mut next);

        let retained_count = archived_candidate_slot_parking_paths(&fixture.client.installation, fixture.previous.id)
            .len()
            + archived_candidate_slot_parking_paths(&fixture.client.installation, fixture.candidate.id).len();
        assert_eq!(retained_count, 1);
    }

    assert_eq!(
        fs::read_to_string(fixture.client.installation.root.join("usr/.stateID")).unwrap(),
        active.to_string()
    );
    assert!(
        fixture
            .client
            .installation
            .root_path(next.to_string())
            .join("usr")
            .is_dir()
    );
}

#[test]
fn displaced_archived_candidate_retirement_without_an_attempt_fails_closed() {
    let fixture = stateful_transition_fixture(true);
    let state_root = fixture.client.installation.root_path(fixture.candidate.id.to_string());
    let identity = fixture
        .client
        .prepare_stateful_tree_identity(&state_root.join("usr"), fixture.candidate.id)
        .unwrap();

    let error = identity
        .retire_displaced_archived_candidate_slot(&fixture.client.installation, fixture.candidate.id)
        .unwrap_err();

    assert!(format!("{error:?}").contains("AttemptMissing"));
    assert!(state_root.join("usr/.stateID").is_file());
}

#[test]
fn displaced_archived_candidate_retirement_resumes_without_a_second_move() {
    let fixture = stateful_transition_fixture(true);
    let state_root = fixture.client.installation.root_path(fixture.candidate.id.to_string());
    let staging_root = fixture.client.installation.staging_dir();
    let identity = fixture
        .client
        .prepare_stateful_tree_identity(&state_root.join("usr"), fixture.candidate.id)
        .unwrap();
    identity
        .stage_archived_candidate(&fixture.client.installation, fixture.candidate.id)
        .unwrap();
    let held_candidate = fixture
        .client
        .installation
        .root_path("held-candidate-for-retirement-resume");
    fs::rename(staging_root.join("usr"), &held_candidate).unwrap();
    crate::transition_identity::arm_retained_archived_candidate_move_fault(
        crate::transition_identity::RetainedArchivedCandidateMoveFaultPoint::RootsAfterDisplacedSlotRetireSync,
    );

    identity
        .retire_displaced_archived_candidate_slot(&fixture.client.installation, fixture.candidate.id)
        .unwrap_err();
    let parked = archived_candidate_slot_parking_paths(&fixture.client.installation, fixture.candidate.id);
    assert_eq!(parked.len(), 1);
    let parked_inode = fs::symlink_metadata(&parked[0]).unwrap().ino();
    assert!(!state_root.exists());

    identity
        .retire_displaced_archived_candidate_slot(&fixture.client.installation, fixture.candidate.id)
        .unwrap();
    assert_eq!(fs::symlink_metadata(&parked[0]).unwrap().ino(), parked_inode);
    assert!(!state_root.exists());
    assert!(held_candidate.join(".stateID").is_file());
}

#[test]
fn archived_retirement_suffix_failure_restores_the_slot_during_full_recovery() {
    let fixture = stateful_transition_fixture(true);
    crate::transition_identity::arm_retained_archived_candidate_move_fault(
        crate::transition_identity::RetainedArchivedCandidateMoveFaultPoint::FinalDisplacedSlotRetirementRevalidation,
    );

    let error = fixture
        .client
        .activate_state_with_checkpoint(fixture.candidate.id, true, true, |_| Ok(()))
        .unwrap_err();

    assert!(matches!(error, Error::StatefulTransitionUsrRestored { .. }));
    assert_recovered_stateful_transition(&fixture);
    assert!(archived_candidate_slot_parking_paths(&fixture.client.installation, fixture.candidate.id).is_empty());
}

#[test]
fn quarantined_archived_candidate_retries_only_retirement_durability() {
    let fixture = stateful_transition_fixture(true);
    crate::transition_identity::arm_retained_archived_candidate_move_fault(
        crate::transition_identity::RetainedArchivedCandidateMoveFaultPoint::RootsAfterDisplacedSlotRetireSync,
    );

    let error = fixture
        .client
        .activate_state_with_checkpoint(fixture.candidate.id, false, true, |checkpoint| {
            if checkpoint == StatefulTransitionCheckpoint::AfterSystemTriggersStarted {
                Err(injected_state_transition_error(
                    "quarantine with retirement suffix retry",
                ))
            } else {
                Ok(())
            }
        })
        .unwrap_err();

    assert!(matches!(error, Error::StatefulTransitionUsrRestored { .. }));
    assert_eq!(
        fs::read_dir(fixture.client.installation.state_quarantine_dir())
            .unwrap()
            .count(),
        1
    );
    assert_eq!(
        archived_candidate_slot_parking_paths(&fixture.client.installation, fixture.candidate.id).len(),
        1
    );
    assert!(!fixture.client.installation.staging_path("usr").exists());
}
