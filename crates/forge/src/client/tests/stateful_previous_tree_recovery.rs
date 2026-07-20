fn exchanged_stateful_identity(fixture: &StatefulTransitionFixture) -> StatefulTreeIdentity {
    let staged_usr = fixture.client.installation.staging_path("usr");
    let identity = fixture
        .client
        .prepare_stateful_tree_identity(&staged_usr, fixture.candidate.id)
        .unwrap();
    identity.exchange_forward(&fixture.client.installation).unwrap();
    identity
}

#[test]
fn retained_previous_moves_reconcile_before_and_after_rename_faults() {
    let fixture = stateful_transition_fixture(false);
    let identity = exchanged_stateful_identity(&fixture);
    let installation = &fixture.client.installation;
    let staged = installation.staging_path("usr");
    let slot = installation.root_path(fixture.previous.id.to_string());
    let archived = slot.join("usr");
    let previous_inode = fs::symlink_metadata(&staged).unwrap().ino();

    crate::transition_identity::arm_retained_previous_move_fault(
        crate::transition_identity::RetainedPreviousMoveFaultPoint::BeforeRename,
    );
    let failure = identity
        .archive_previous(installation, fixture.previous.id)
        .unwrap_err();
    assert_eq!(failure.outcome(), RetainedPreviousMoveOutcome::NotApplied);
    assert_eq!(fs::symlink_metadata(&staged).unwrap().ino(), previous_inode);
    assert!(!archived.exists());
    assert!(!slot.exists());
    assert_eq!(previous_slot_parking_paths(installation, fixture.previous.id).len(), 1);

    crate::transition_identity::arm_retained_previous_move_fault(
        crate::transition_identity::RetainedPreviousMoveFaultPoint::AfterRename,
    );
    identity.archive_previous(installation, fixture.previous.id).unwrap();
    assert!(!staged.exists());
    assert_eq!(fs::symlink_metadata(&archived).unwrap().ino(), previous_inode);
    assert_eq!(
        fs::symlink_metadata(&slot).unwrap().permissions().mode() & 0o7777,
        0o700
    );
    identity.verify_previous_for_recovery(&archived).unwrap();

    crate::transition_identity::arm_retained_previous_move_fault(
        crate::transition_identity::RetainedPreviousMoveFaultPoint::BeforeRename,
    );
    let failure = identity
        .restore_previous(installation, fixture.previous.id)
        .unwrap_err();
    assert_eq!(failure.outcome(), RetainedPreviousMoveOutcome::NotApplied);
    assert_eq!(fs::symlink_metadata(&archived).unwrap().ino(), previous_inode);
    assert!(!staged.exists());

    crate::transition_identity::arm_retained_previous_move_fault(
        crate::transition_identity::RetainedPreviousMoveFaultPoint::AfterRename,
    );
    identity.restore_previous(installation, fixture.previous.id).unwrap();
    assert!(!archived.exists());
    assert_eq!(fs::symlink_metadata(&staged).unwrap().ino(), previous_inode);
    assert!(!slot.exists());
    identity.verify_previous_for_recovery(&staged).unwrap();

    // A compensating restore retires the empty wrapper away from the
    // canonical state name. A fresh attempt therefore succeeds instead
    // of treating its own prior cleanup residue as ambient state.
    identity.archive_previous(installation, fixture.previous.id).unwrap();
    assert_eq!(fs::symlink_metadata(&archived).unwrap().ino(), previous_inode);
    identity.restore_previous(installation, fixture.previous.id).unwrap();
    assert_eq!(fs::symlink_metadata(&staged).unwrap().ino(), previous_inode);
    assert!(!slot.exists());
    let parked = previous_slot_parking_paths(installation, fixture.previous.id);
    assert_eq!(parked.len(), 3);
    assert!(parked.iter().all(|path| fs::read_dir(path).unwrap().next().is_none()));
}

#[test]
fn retained_previous_archive_applied_faults_resume_only_the_sync_suffix() {
    for point in [
        crate::transition_identity::RetainedPreviousMoveFaultPoint::SourceParentSync,
        crate::transition_identity::RetainedPreviousMoveFaultPoint::DestinationParentSync,
        crate::transition_identity::RetainedPreviousMoveFaultPoint::FinalRevalidation,
    ] {
        let fixture = stateful_transition_fixture(false);
        let identity = exchanged_stateful_identity(&fixture);
        let installation = &fixture.client.installation;
        let staged = installation.staging_path("usr");
        let archived = installation.root_path(fixture.previous.id.to_string()).join("usr");
        let previous_inode = fs::symlink_metadata(&staged).unwrap().ino();

        crate::transition_identity::arm_retained_previous_move_fault(point);
        let failure = identity
            .archive_previous(installation, fixture.previous.id)
            .unwrap_err();
        assert_eq!(failure.outcome(), RetainedPreviousMoveOutcome::Applied);
        assert!(!staged.exists(), "archive source returned after {point:?}");
        assert_eq!(fs::symlink_metadata(&archived).unwrap().ino(), previous_inode);

        identity
            .finish_applied_previous_archive(installation, fixture.previous.id)
            .unwrap();
        assert!(!staged.exists(), "sync-only archive resume renamed after {point:?}");
        assert_eq!(fs::symlink_metadata(&archived).unwrap().ino(), previous_inode);
    }
}

#[test]
fn retained_previous_restore_applied_faults_resume_only_the_sync_suffix() {
    for point in [
        crate::transition_identity::RetainedPreviousMoveFaultPoint::SourceParentSync,
        crate::transition_identity::RetainedPreviousMoveFaultPoint::DestinationParentSync,
        crate::transition_identity::RetainedPreviousMoveFaultPoint::FinalRevalidation,
    ] {
        let fixture = stateful_transition_fixture(false);
        let identity = exchanged_stateful_identity(&fixture);
        let installation = &fixture.client.installation;
        let staged = installation.staging_path("usr");
        let archived = installation.root_path(fixture.previous.id.to_string()).join("usr");
        identity.archive_previous(installation, fixture.previous.id).unwrap();
        let previous_inode = fs::symlink_metadata(&archived).unwrap().ino();

        crate::transition_identity::arm_retained_previous_move_fault(point);
        let failure = identity
            .restore_previous(installation, fixture.previous.id)
            .unwrap_err();
        assert_eq!(failure.outcome(), RetainedPreviousMoveOutcome::Applied);
        assert!(!archived.exists(), "restore source returned after {point:?}");
        assert_eq!(fs::symlink_metadata(&staged).unwrap().ino(), previous_inode);

        identity
            .finish_applied_previous_restore(installation, fixture.previous.id)
            .unwrap();
        assert!(!archived.exists(), "sync-only restore resume renamed after {point:?}");
        assert_eq!(fs::symlink_metadata(&staged).unwrap().ino(), previous_inode);
    }
}

#[test]
fn retained_previous_slot_creation_faults_retire_the_state_name_before_retry() {
    for point in [
        crate::transition_identity::RetainedPreviousMoveFaultPoint::BeforeSlotPublish,
        crate::transition_identity::RetainedPreviousMoveFaultPoint::SlotSync,
        crate::transition_identity::RetainedPreviousMoveFaultPoint::RootsParentSync,
    ] {
        let fixture = stateful_transition_fixture(false);
        let identity = exchanged_stateful_identity(&fixture);
        let installation = &fixture.client.installation;
        let staged = installation.staging_path("usr");
        let slot = installation.root_path(fixture.previous.id.to_string());
        let previous_inode = fs::symlink_metadata(&staged).unwrap().ino();

        crate::transition_identity::arm_retained_previous_move_fault(point);
        let failure = identity
            .archive_previous(installation, fixture.previous.id)
            .unwrap_err();
        assert_eq!(failure.outcome(), RetainedPreviousMoveOutcome::NotApplied);
        assert_eq!(fs::symlink_metadata(&staged).unwrap().ino(), previous_inode);
        assert!(!slot.exists(), "canonical slot survived {point:?}");

        identity.archive_previous(installation, fixture.previous.id).unwrap();
        identity.restore_previous(installation, fixture.previous.id).unwrap();
        assert_eq!(fs::symlink_metadata(&staged).unwrap().ino(), previous_inode);
        assert!(!slot.exists(), "retry left canonical slot after {point:?}");
    }
}

#[test]
fn retained_previous_parking_scan_skips_occupied_non_mount_file_types() {
    let fixture = stateful_transition_fixture(false);
    let identity = exchanged_stateful_identity(&fixture);
    let installation = &fixture.client.installation;
    let roots = installation.root_path("");
    let staged = installation.staging_path("usr");
    let token = recovery_tree_token(&staged);
    let parking = |index| roots.join(format!(".previous-slot-{}-{token}-{index}", fixture.previous.id));

    fs::write(parking(0), b"regular occupant").unwrap();
    symlink("/", parking(1)).unwrap();
    fs::create_dir(parking(2)).unwrap();
    fs::set_permissions(parking(2), Permissions::from_mode(0o777)).unwrap();

    identity.archive_previous(installation, fixture.previous.id).unwrap();
    identity.restore_previous(installation, fixture.previous.id).unwrap();

    assert_eq!(fs::read(parking(0)).unwrap(), b"regular occupant");
    assert!(fs::symlink_metadata(parking(1)).unwrap().file_type().is_symlink());
    assert_eq!(
        fs::symlink_metadata(parking(2)).unwrap().permissions().mode() & 0o7777,
        0o777
    );
    assert!(!installation.root_path(fixture.previous.id.to_string()).exists());
    assert!(parking(3).is_dir(), "the first safe free parking name was not used");
    assert!(fs::read_dir(parking(3)).unwrap().next().is_none());
}

#[test]
fn retained_previous_parking_scan_uses_the_final_bounded_candidate() {
    let fixture = stateful_transition_fixture(false);
    let identity = exchanged_stateful_identity(&fixture);
    let installation = &fixture.client.installation;
    let roots = installation.root_path("");
    let staged = installation.staging_path("usr");
    let token = recovery_tree_token(&staged);
    let parking = |index| roots.join(format!(".previous-slot-{}-{token}-{index}", fixture.previous.id));

    for index in 0..255 {
        fs::write(parking(index), b"occupied").unwrap();
    }

    identity.archive_previous(installation, fixture.previous.id).unwrap();
    identity.restore_previous(installation, fixture.previous.id).unwrap();

    assert!(parking(255).is_dir(), "the final bounded parking name was not used");
    assert!(fs::read_dir(parking(255)).unwrap().next().is_none());
    assert!(!installation.root_path(fixture.previous.id.to_string()).exists());
    identity.verify_previous_for_recovery(&staged).unwrap();
}

#[test]
fn retained_previous_parking_exhaustion_preserves_both_namespaces() {
    let fixture = stateful_transition_fixture(false);
    let identity = exchanged_stateful_identity(&fixture);
    let installation = &fixture.client.installation;
    let roots = installation.root_path("");
    let staged = installation.staging_path("usr");
    let previous_inode = fs::symlink_metadata(&staged).unwrap().ino();
    let token = recovery_tree_token(&staged);
    let parking = |index| roots.join(format!(".previous-slot-{}-{token}-{index}", fixture.previous.id));

    for index in 0..256 {
        fs::write(parking(index), b"occupied").unwrap();
    }

    let failure = identity
        .archive_previous(installation, fixture.previous.id)
        .unwrap_err();

    assert_eq!(failure.outcome(), RetainedPreviousMoveOutcome::NotApplied);
    assert!(
        format!("{failure:?}").contains("PreviousArchiveParkingExhausted"),
        "unexpected bounded-scan failure: {failure:?}"
    );
    assert_eq!(fs::symlink_metadata(&staged).unwrap().ino(), previous_inode);
    assert!(!installation.root_path(fixture.previous.id.to_string()).exists());
    identity.verify_previous_for_recovery(&staged).unwrap();
    for index in 0..256 {
        assert_eq!(fs::read(parking(index)).unwrap(), b"occupied");
    }
}

#[test]
fn retained_previous_restore_retirement_faults_resume_without_a_second_rename() {
    for point in [
        crate::transition_identity::RetainedPreviousMoveFaultPoint::BeforeSlotRetire,
        crate::transition_identity::RetainedPreviousMoveFaultPoint::RootsAfterSlotRetireSync,
        crate::transition_identity::RetainedPreviousMoveFaultPoint::FinalSlotRetirementRevalidation,
    ] {
        let fixture = stateful_transition_fixture(false);
        let identity = exchanged_stateful_identity(&fixture);
        let installation = &fixture.client.installation;
        let staged = installation.staging_path("usr");
        let slot = installation.root_path(fixture.previous.id.to_string());
        let archived = slot.join("usr");
        identity.archive_previous(installation, fixture.previous.id).unwrap();
        let previous_inode = fs::symlink_metadata(&archived).unwrap().ino();

        crate::transition_identity::arm_retained_previous_move_fault(point);
        let failure = identity
            .restore_previous(installation, fixture.previous.id)
            .unwrap_err();
        assert_eq!(failure.outcome(), RetainedPreviousMoveOutcome::Applied);
        assert_eq!(fs::symlink_metadata(&staged).unwrap().ino(), previous_inode);

        identity
            .finish_applied_previous_restore(installation, fixture.previous.id)
            .unwrap();
        assert_eq!(fs::symlink_metadata(&staged).unwrap().ino(), previous_inode);
        assert!(!slot.exists(), "retirement resume left state name after {point:?}");
    }

    let fixture = stateful_transition_fixture(false);
    let identity = exchanged_stateful_identity(&fixture);
    let installation = &fixture.client.installation;
    let slot = installation.root_path(fixture.previous.id.to_string());
    identity.archive_previous(installation, fixture.previous.id).unwrap();
    crate::transition_identity::arm_retained_previous_move_fault(
        crate::transition_identity::RetainedPreviousMoveFaultPoint::AfterSlotRetire,
    );
    identity.restore_previous(installation, fixture.previous.id).unwrap();
    assert!(
        !slot.exists(),
        "applied retirement evidence must supersede its syscall error"
    );
}

#[test]
fn retained_previous_moves_adopt_exact_pre_syscall_archive_and_restore_layouts() {
    let fixture = stateful_transition_fixture(false);
    let identity = exchanged_stateful_identity(&fixture);
    let installation = &fixture.client.installation;
    let staged = installation.staging_path("usr");
    let slot = installation.root_path(fixture.previous.id.to_string());
    let archived = slot.join("usr");
    let previous_inode = fs::symlink_metadata(&staged).unwrap().ino();

    let hook_staged = staged.clone();
    let hook_archived = archived.clone();
    crate::transition_identity::arm_before_retained_previous_move_rename(move || {
        fs::rename(&hook_staged, &hook_archived).unwrap();
    });
    identity.archive_previous(installation, fixture.previous.id).unwrap();
    assert_eq!(fs::symlink_metadata(&archived).unwrap().ino(), previous_inode);

    let hook_staged = staged.clone();
    let hook_archived = archived.clone();
    crate::transition_identity::arm_before_retained_previous_move_rename(move || {
        fs::rename(&hook_archived, &hook_staged).unwrap();
    });
    identity.restore_previous(installation, fixture.previous.id).unwrap();
    assert_eq!(fs::symlink_metadata(&staged).unwrap().ino(), previous_inode);
    assert!(!slot.exists());
}

#[test]
fn retained_previous_slot_retirement_preserves_a_racing_replacement() {
    let fixture = stateful_transition_fixture(false);
    let identity = exchanged_stateful_identity(&fixture);
    let installation = &fixture.client.installation;
    let staged = installation.staging_path("usr");
    let slot = installation.root_path(fixture.previous.id.to_string());
    let archived = slot.join("usr");
    let displaced = installation.root_path("displaced-retained-previous-slot");
    identity.archive_previous(installation, fixture.previous.id).unwrap();
    let previous_inode = fs::symlink_metadata(&archived).unwrap().ino();

    let hook_slot = slot.clone();
    let hook_displaced = displaced.clone();
    crate::transition_identity::arm_before_previous_slot_retirement_rename(move || {
        fs::rename(&hook_slot, &hook_displaced).unwrap();
        fs::create_dir(&hook_slot).unwrap();
        fs::write(hook_slot.join("foreign"), b"must survive").unwrap();
    });
    let failure = identity
        .restore_previous(installation, fixture.previous.id)
        .unwrap_err();
    assert_eq!(failure.outcome(), RetainedPreviousMoveOutcome::Applied);
    assert_eq!(fs::symlink_metadata(&staged).unwrap().ino(), previous_inode);
    assert!(displaced.is_dir(), "retained exact slot was destroyed");
    assert!(
        !slot.exists(),
        "racing replacement should have been retired, not deleted"
    );
    let replacements = previous_slot_parking_paths(installation, fixture.previous.id)
        .into_iter()
        .filter(|path| path.join("foreign").exists())
        .collect::<Vec<_>>();
    assert_eq!(replacements.len(), 1);
    assert_eq!(fs::read(replacements[0].join("foreign")).unwrap(), b"must survive");
    assert!(
        identity
            .finish_applied_previous_restore(installation, fixture.previous.id)
            .is_err()
    );
    assert_eq!(fs::read(replacements[0].join("foreign")).unwrap(), b"must survive");
}

#[test]
fn previous_archive_abort_retirement_faults_resume_in_production_recovery() {
    for retirement_point in [
        crate::transition_identity::RetainedPreviousMoveFaultPoint::BeforeSlotRetire,
        crate::transition_identity::RetainedPreviousMoveFaultPoint::AfterSlotRetire,
        crate::transition_identity::RetainedPreviousMoveFaultPoint::RootsAfterSlotRetireSync,
        crate::transition_identity::RetainedPreviousMoveFaultPoint::FinalSlotRetirementRevalidation,
    ] {
        let fixture = stateful_transition_fixture(false);
        let mut armed = false;
        let error = fixture
            .client
            .apply_stateful_blit_with_checkpoint(
                vfs(Vec::new()).unwrap(),
                &fixture.candidate,
                Some(fixture.previous.id),
                generated_system_snapshot("candidate-package"),
                |checkpoint| {
                    if checkpoint == StatefulTransitionCheckpoint::BeforePreviousStateArchive {
                        armed = true;
                        crate::transition_identity::arm_retained_previous_move_faults(&[
                            crate::transition_identity::RetainedPreviousMoveFaultPoint::BeforeRename,
                            retirement_point,
                        ]);
                    }
                    Ok(())
                },
            )
            .unwrap_err();

        assert!(armed, "archive boundary was not reached for {retirement_point:?}");
        assert!(
            matches!(error, Error::StatefulTransitionUsrRestored { .. }),
            "archive-abort retirement did not resume after {retirement_point:?}: {error:#?}"
        );
        assert!(
            !fixture
                .client
                .installation
                .root_path(fixture.previous.id.to_string())
                .exists(),
            "canonical previous-state slot survived {retirement_point:?}"
        );
        assert_fresh_candidate_quarantined_and_invalidated(&fixture);
    }
}

#[test]
fn applied_previous_archive_and_restore_faults_use_full_client_suffix_routing() {
    let fixture = stateful_transition_fixture(false);
    let mut archive_armed = false;
    let mut restore_armed = false;
    let error = fixture
        .client
        .apply_stateful_blit_with_checkpoint(
            vfs(Vec::new()).unwrap(),
            &fixture.candidate,
            Some(fixture.previous.id),
            generated_system_snapshot("candidate-package"),
            |checkpoint| match checkpoint {
                StatefulTransitionCheckpoint::BeforePreviousStateArchive => {
                    archive_armed = true;
                    crate::transition_identity::arm_retained_previous_move_fault(
                        crate::transition_identity::RetainedPreviousMoveFaultPoint::SourceParentSync,
                    );
                    Ok(())
                }
                StatefulTransitionCheckpoint::AfterPreviousStateArchive => {
                    restore_armed = true;
                    crate::transition_identity::arm_retained_previous_move_fault(
                        crate::transition_identity::RetainedPreviousMoveFaultPoint::RootsAfterSlotRetireSync,
                    );
                    Err(injected_state_transition_error("force compensating restore"))
                }
                _ => Ok(()),
            },
        )
        .unwrap_err();

    assert!(archive_armed && restore_armed);
    assert!(
        matches!(error, Error::StatefulTransitionUsrRestored { .. }),
        "{error:#?}"
    );
    assert!(
        !fixture
            .client
            .installation
            .root_path(fixture.previous.id.to_string())
            .exists()
    );
    assert_fresh_candidate_quarantined_and_invalidated(&fixture);
}

#[test]
fn retained_previous_moves_reject_roots_and_restore_staging_substitution() {
    {
        let fixture = stateful_transition_fixture(false);
        let identity = exchanged_stateful_identity(&fixture);
        let installation = &fixture.client.installation;
        let roots = installation.root_path("");
        let displaced_roots = roots.parent().unwrap().join("displaced-root");
        let hook_roots = roots.clone();
        let hook_displaced = displaced_roots.clone();
        crate::transition_identity::arm_before_retained_previous_move_rename(move || {
            fs::rename(&hook_roots, &hook_displaced).unwrap();
            fs::create_dir(&hook_roots).unwrap();
            fs::set_permissions(&hook_roots, Permissions::from_mode(0o700)).unwrap();
        });

        let failure = identity
            .archive_previous(installation, fixture.previous.id)
            .unwrap_err();
        assert_eq!(failure.outcome(), RetainedPreviousMoveOutcome::Ambiguous);
        identity
            .verify_previous_for_recovery(&displaced_roots.join("staging/usr"))
            .unwrap();
        assert!(fs::read_dir(&roots).unwrap().next().is_none());
    }

    {
        let fixture = stateful_transition_fixture(false);
        let identity = exchanged_stateful_identity(&fixture);
        let installation = &fixture.client.installation;
        let staging = installation.staging_dir();
        let displaced_staging = installation.root_path("displaced-staging");
        let archived = installation.root_path(fixture.previous.id.to_string()).join("usr");
        identity.archive_previous(installation, fixture.previous.id).unwrap();
        let previous_inode = fs::symlink_metadata(&archived).unwrap().ino();
        let hook_staging = staging.clone();
        let hook_displaced = displaced_staging.clone();
        crate::transition_identity::arm_before_retained_previous_move_rename(move || {
            fs::rename(&hook_staging, &hook_displaced).unwrap();
            fs::create_dir(&hook_staging).unwrap();
            fs::set_permissions(&hook_staging, Permissions::from_mode(0o700)).unwrap();
        });

        let failure = identity
            .restore_previous(installation, fixture.previous.id)
            .unwrap_err();
        assert_eq!(failure.outcome(), RetainedPreviousMoveOutcome::Ambiguous);
        assert_eq!(fs::symlink_metadata(&archived).unwrap().ino(), previous_inode);
        assert!(fs::read_dir(&staging).unwrap().next().is_none());
        assert!(fs::read_dir(&displaced_staging).unwrap().next().is_none());
    }
}

#[test]
fn fresh_identity_can_archive_after_a_complete_compensating_recovery() {
    let fixture = stateful_transition_fixture(false);
    let first_error = fixture
        .client
        .apply_stateful_blit_with_checkpoint(
            vfs(Vec::new()).unwrap(),
            &fixture.candidate,
            Some(fixture.previous.id),
            generated_system_snapshot("candidate-package"),
            |checkpoint| {
                if checkpoint == StatefulTransitionCheckpoint::AfterPreviousStateArchive {
                    Err(injected_state_transition_error("force first compensating recovery"))
                } else {
                    Ok(())
                }
            },
        )
        .unwrap_err();
    assert!(matches!(first_error, Error::StatefulTransitionUsrRestored { .. }));
    assert_fresh_candidate_quarantined_and_invalidated(&fixture);

    let next = fixture.client.state_db.add(&[], Some("next candidate"), None).unwrap();
    record_state_id(&fixture.client.installation.staging_dir(), next.id).unwrap();
    let staged = fixture.client.installation.staging_path("usr");
    let local_etc = transaction_root::prepare_local_etc(&fixture.client.installation).unwrap();
    let isolation_root = create_root_links(&fixture.client.installation.isolation_dir()).unwrap();
    let mut active_state = active_state_authority::ActiveStateAuthority::acquire(&fixture.client.installation).unwrap();
    let identity = fixture.client.prepare_stateful_tree_identity(&staged, next.id).unwrap();
    let metadata =
        candidate_metadata::decorate_stateful(&identity, &generated_system_snapshot("next-package")).unwrap();
    active_state
        .refresh_after_tree_identity_preparation(&fixture.client.installation)
        .unwrap();
    let live_root_abi = preflight_root_links(&fixture.client.installation.root).unwrap();
    let mut no_fault = |_| Ok(());
    fixture
        .client
        .commit_stateful_staging(
            &vfs(Vec::new()).unwrap(),
            &next,
            Some(&fixture.previous),
            StatefulCandidateOrigin::Fresh,
            true,
            false,
            false,
            &identity,
            Some(&metadata),
            live_root_abi,
            &isolation_root,
            &local_etc,
            &active_state,
            &mut no_fault,
        )
        .unwrap();

    assert_eq!(
        fs::read_to_string(fixture.client.installation.root.join("usr/.stateID")).unwrap(),
        next.id.to_string()
    );
    assert_eq!(
        fs::read_to_string(
            fixture
                .client
                .installation
                .root_path(fixture.previous.id.to_string())
                .join("usr/.stateID")
        )
        .unwrap(),
        fixture.previous.id.to_string()
    );
}

#[test]
fn retained_previous_archive_never_adopts_an_ambient_empty_state_slot() {
    let fixture = stateful_transition_fixture(false);
    let identity = exchanged_stateful_identity(&fixture);
    let installation = &fixture.client.installation;
    let staged = installation.staging_path("usr");
    let slot = installation.root_path(fixture.previous.id.to_string());
    for invalid in [state::Id::from(0), state::Id::from(-1)] {
        let failure = identity.archive_previous(installation, invalid).unwrap_err();
        assert_eq!(failure.outcome(), RetainedPreviousMoveOutcome::NotApplied);
    }
    fs::create_dir(&slot).unwrap();
    fs::set_permissions(&slot, Permissions::from_mode(0o700)).unwrap();
    let ambient_inode = fs::symlink_metadata(&slot).unwrap().ino();

    let failure = identity
        .archive_previous(installation, fixture.previous.id)
        .unwrap_err();
    assert_eq!(failure.outcome(), RetainedPreviousMoveOutcome::NotApplied);
    assert_eq!(fs::symlink_metadata(&slot).unwrap().ino(), ambient_inode);
    assert_eq!(fs::read_dir(&slot).unwrap().count(), 0);
    identity.verify_previous_for_recovery(&staged).unwrap();
}

#[test]
fn retained_previous_archive_rejects_slot_replacement_before_retention() {
    let fixture = stateful_transition_fixture(false);
    let identity = exchanged_stateful_identity(&fixture);
    let installation = &fixture.client.installation;
    let staged = installation.staging_path("usr");
    let roots = installation.root_path("");
    let slot = installation.root_path(fixture.previous.id.to_string());
    let displaced = installation.root_path("displaced-fresh-previous-slot");
    let hook_roots = roots.clone();
    let hook_displaced = displaced.clone();
    crate::transition_identity::arm_before_previous_archive_slot_reopen(move || {
        let parked = fs::read_dir(&hook_roots)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| {
                path.file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with(".previous-slot-"))
            })
            .expect("private previous-state slot must exist before reopen");
        fs::rename(&parked, &hook_displaced).unwrap();
        fs::create_dir(&parked).unwrap();
        fs::set_permissions(&parked, Permissions::from_mode(0o700)).unwrap();
    });

    let failure = identity
        .archive_previous(installation, fixture.previous.id)
        .unwrap_err();
    assert_eq!(failure.outcome(), RetainedPreviousMoveOutcome::NotApplied);
    assert!(displaced.is_dir());
    assert!(!slot.exists());
    identity.verify_previous_for_recovery(&staged).unwrap();

    // The replaced provisional name is inert. A new bounded parking name
    // can still be prepared and published to the canonical state slot.
    identity.archive_previous(installation, fixture.previous.id).unwrap();
    identity.restore_previous(installation, fixture.previous.id).unwrap();
    assert!(!slot.exists());
}

#[test]
fn retained_previous_archive_rejects_state_slot_parent_substitution_before_rename() {
    let fixture = stateful_transition_fixture(false);
    let identity = exchanged_stateful_identity(&fixture);
    let installation = &fixture.client.installation;
    let staged = installation.staging_path("usr");
    let slot = installation.root_path(fixture.previous.id.to_string());
    let displaced = installation.root_path("displaced-previous-slot");
    let hook_slot = slot.clone();
    let hook_displaced = displaced.clone();
    crate::transition_identity::arm_before_retained_previous_move_rename(move || {
        fs::rename(&hook_slot, &hook_displaced).unwrap();
        fs::create_dir(&hook_slot).unwrap();
        fs::set_permissions(&hook_slot, Permissions::from_mode(0o700)).unwrap();
    });

    let failure = identity
        .archive_previous(installation, fixture.previous.id)
        .unwrap_err();
    assert_eq!(failure.outcome(), RetainedPreviousMoveOutcome::Ambiguous);
    assert!(displaced.is_dir());
    assert_eq!(fs::read_dir(&slot).unwrap().count(), 0);
    identity.verify_previous_for_recovery(&staged).unwrap();
}

#[test]
fn retained_previous_archive_rejects_same_token_child_substitution_before_rename() {
    let fixture = stateful_transition_fixture(false);
    let identity = exchanged_stateful_identity(&fixture);
    let installation = &fixture.client.installation;
    let staged = installation.staging_path("usr");
    let displaced = installation.staging_path("displaced-previous-usr");
    let replacement_token = recovery_tree_token(&staged);
    let hook_staged = staged.clone();
    let hook_displaced = displaced.clone();
    crate::transition_identity::arm_before_retained_previous_move_rename(move || {
        fs::rename(&hook_staged, &hook_displaced).unwrap();
        fs::create_dir(&hook_staged).unwrap();
        fs::set_permissions(&hook_staged, Permissions::from_mode(0o755)).unwrap();
        fs::copy(hook_displaced.join(".cast-tree-id"), hook_staged.join(".cast-tree-id")).unwrap();
    });

    let failure = identity
        .archive_previous(installation, fixture.previous.id)
        .unwrap_err();
    assert_eq!(failure.outcome(), RetainedPreviousMoveOutcome::Ambiguous);
    assert_eq!(recovery_tree_token(&staged), replacement_token);
    identity.verify_previous_for_recovery(&displaced).unwrap();
    let slot = installation.root_path(fixture.previous.id.to_string());
    assert!(slot.is_dir());
    assert!(fs::read_dir(slot).unwrap().next().is_none());
}

#[test]
fn retained_exchange_adopts_applied_forward_and_reverse_moves_when_the_syscall_reports_error() {
    let fixture = stateful_transition_fixture(false);
    let live_usr = fixture.client.installation.root.join("usr");
    let staged_usr = fixture.client.installation.staging_path("usr");
    let identity = fixture
        .client
        .prepare_stateful_tree_identity(&staged_usr, fixture.candidate.id)
        .unwrap();
    let candidate_token = recovery_tree_token(&staged_usr);
    let previous_token = recovery_tree_token(&live_usr);

    crate::transition_identity::arm_retained_exchange_fault(
        crate::transition_identity::RetainedExchangeFaultPoint::AfterRename,
    );
    identity.exchange_forward(&fixture.client.installation).unwrap();

    identity.verify_forward_exchange(&live_usr, &staged_usr).unwrap();
    assert_eq!(recovery_tree_token(&live_usr), candidate_token);
    assert_eq!(recovery_tree_token(&staged_usr), previous_token);

    crate::transition_identity::arm_retained_exchange_fault(
        crate::transition_identity::RetainedExchangeFaultPoint::AfterRename,
    );
    identity.exchange_reverse(&fixture.client.installation).unwrap();

    identity.verify_restored(&live_usr, &staged_usr).unwrap();
    assert_eq!(recovery_tree_token(&live_usr), previous_token);
    assert_eq!(recovery_tree_token(&staged_usr), candidate_token);
}

#[test]
fn retained_exchange_error_before_rename_preserves_both_exact_names() {
    let fixture = stateful_transition_fixture(false);
    let live_usr = fixture.client.installation.root.join("usr");
    let staged_usr = fixture.client.installation.staging_path("usr");
    let identity = fixture
        .client
        .prepare_stateful_tree_identity(&staged_usr, fixture.candidate.id)
        .unwrap();
    let candidate_token = recovery_tree_token(&staged_usr);
    let previous_token = recovery_tree_token(&live_usr);

    crate::transition_identity::arm_retained_exchange_fault(
        crate::transition_identity::RetainedExchangeFaultPoint::BeforeRename,
    );
    let failure = identity.exchange_forward(&fixture.client.installation).unwrap_err();

    assert_eq!(failure.outcome(), RetainedExchangeOutcome::NotApplied);
    identity.verify_pre_exchange(&staged_usr, &live_usr).unwrap();
    assert_eq!(recovery_tree_token(&staged_usr), candidate_token);
    assert_eq!(recovery_tree_token(&live_usr), previous_token);
}

#[test]
fn retained_exchange_parent_replacement_is_rejected_before_the_syscall() {
    let fixture = stateful_transition_fixture(false);
    let installation = &fixture.client.installation;
    let live_usr = installation.root.join("usr");
    let staging = installation.staging_dir();
    let staged_usr = installation.staging_path("usr");
    let displaced = installation.root_path("displaced-retained-exchange-staging");
    let identity = fixture
        .client
        .prepare_stateful_tree_identity(&staged_usr, fixture.candidate.id)
        .unwrap();
    let previous_token = recovery_tree_token(&live_usr);
    let candidate_token = recovery_tree_token(&staged_usr);
    let raced_staging = staging.clone();
    let raced_displaced = displaced.clone();

    crate::transition_identity::arm_before_retained_exchange_rename(move || {
        fs::rename(&raced_staging, &raced_displaced).unwrap();
        fs::create_dir(&raced_staging).unwrap();
        fs::create_dir(raced_staging.join("usr")).unwrap();
        fs::write(raced_staging.join("usr/foreign"), b"racing staging tree").unwrap();
    });
    let failure = identity.exchange_forward(installation).unwrap_err();

    assert_eq!(failure.outcome(), RetainedExchangeOutcome::NotApplied);
    assert_eq!(recovery_tree_token(&live_usr), previous_token);
    assert_eq!(recovery_tree_token(&displaced.join("usr")), candidate_token);
    assert_eq!(fs::read(staging.join("usr/foreign")).unwrap(), b"racing staging tree");
}

#[test]
fn retained_exchange_child_substitution_is_rejected_before_the_syscall() {
    let fixture = stateful_transition_fixture(false);
    let installation = &fixture.client.installation;
    let live_usr = installation.root.join("usr");
    let staged_usr = installation.staging_path("usr");
    let displaced = installation.root_path("displaced-retained-exchange-candidate");
    let identity = fixture
        .client
        .prepare_stateful_tree_identity(&staged_usr, fixture.candidate.id)
        .unwrap();
    let previous_token = recovery_tree_token(&live_usr);
    let candidate_token = recovery_tree_token(&staged_usr);
    let raced_staged = staged_usr.clone();
    let raced_displaced = displaced.clone();

    crate::transition_identity::arm_before_retained_exchange_rename(move || {
        fs::rename(&raced_staged, &raced_displaced).unwrap();
        fs::create_dir(&raced_staged).unwrap();
        fs::write(raced_staged.join("foreign"), b"substituted candidate").unwrap();
    });
    let failure = identity.exchange_forward(installation).unwrap_err();

    assert_eq!(failure.outcome(), RetainedExchangeOutcome::NotApplied);
    assert_eq!(recovery_tree_token(&live_usr), previous_token);
    assert_eq!(recovery_tree_token(&displaced), candidate_token);
    assert_eq!(fs::read(staged_usr.join("foreign")).unwrap(), b"substituted candidate");
}

#[test]
fn retained_exchange_post_move_faults_run_the_swapped_recovery_path() {
    for point in [
        crate::transition_identity::RetainedExchangeFaultPoint::StagingParentSync,
        crate::transition_identity::RetainedExchangeFaultPoint::InstallationRootSync,
        crate::transition_identity::RetainedExchangeFaultPoint::FinalRevalidation,
    ] {
        let fixture = stateful_transition_fixture(false);
        crate::transition_identity::arm_retained_exchange_fault(point);

        let error = fixture
            .client
            .apply_stateful_blit_with_checkpoint(
                vfs(Vec::new()).unwrap(),
                &fixture.candidate,
                Some(fixture.previous.id),
                generated_system_snapshot("candidate-package"),
                |_| Ok(()),
            )
            .unwrap_err();

        assert!(
            matches!(error, Error::StatefulTransitionUsrRestored { .. }),
            "unexpected recovery result after {point:?}: {error:#?}"
        );
        assert_fresh_candidate_quarantined_and_invalidated(&fixture);
    }
}

#[test]
fn retained_reverse_exchange_post_move_faults_finish_without_a_second_exchange() {
    for point in [
        crate::transition_identity::RetainedExchangeFaultPoint::StagingParentSync,
        crate::transition_identity::RetainedExchangeFaultPoint::InstallationRootSync,
        crate::transition_identity::RetainedExchangeFaultPoint::FinalRevalidation,
    ] {
        let fixture = stateful_transition_fixture(false);
        let mut primary_injected = false;

        let error = fixture
            .client
            .apply_stateful_blit_with_checkpoint(
                vfs(Vec::new()).unwrap(),
                &fixture.candidate,
                Some(fixture.previous.id),
                generated_system_snapshot("candidate-package"),
                |checkpoint| match checkpoint {
                    StatefulTransitionCheckpoint::AfterUsrExchange if !primary_injected => {
                        primary_injected = true;
                        Err(injected_state_transition_error("force compensating reverse exchange"))
                    }
                    StatefulTransitionCheckpoint::BeforeRecoveryUsrExchange => {
                        crate::transition_identity::arm_retained_exchange_fault(point);
                        Ok(())
                    }
                    _ => Ok(()),
                },
            )
            .unwrap_err();

        assert!(primary_injected, "forward exchange fault was not reached for {point:?}");
        assert!(
            matches!(error, Error::StatefulTransitionUsrRestored { .. }),
            "reverse durability completion failed after {point:?}: {error:#?}"
        );
        assert_fresh_candidate_quarantined_and_invalidated(&fixture);
    }
}
