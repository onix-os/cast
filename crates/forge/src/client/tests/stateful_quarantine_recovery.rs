#[test]
fn quarantine_durability_faults_never_invalidate_the_fresh_candidate() {
    use crate::transition_identity::QuarantineFaultPoint;

    for fault in [
        QuarantineFaultPoint::CandidatePreSync,
        QuarantineFaultPoint::SlotSync,
        QuarantineFaultPoint::QuarantineBaseSync,
        QuarantineFaultPoint::Rename,
        QuarantineFaultPoint::MovedCandidateSync,
        QuarantineFaultPoint::SourceParentSync,
        QuarantineFaultPoint::DestinationParentSync,
        QuarantineFaultPoint::FinalRevalidation,
    ] {
        let fixture = stateful_transition_fixture(false);
        let mut candidate_token = None;
        crate::transition_identity::arm_quarantine_faults(fault, 2);

        let error = fixture
            .client
            .apply_stateful_blit_with_checkpoint(
                vfs(Vec::new()).unwrap(),
                &fixture.candidate,
                Some(fixture.previous.id),
                generated_system_snapshot("candidate-package"),
                |checkpoint| {
                    if checkpoint == StatefulTransitionCheckpoint::AfterUsrExchange {
                        candidate_token = Some(recovery_tree_token(&fixture.client.installation.root.join("usr")));
                        Err(injected_state_transition_error("force failed-candidate quarantine"))
                    } else {
                        Ok(())
                    }
                },
            )
            .unwrap_err();

        assert!(
            matches!(
                &error,
                Error::StatefulTransitionRecoveryFailed {
                    preserve_candidate: Some(_),
                    ..
                }
            ),
            "fault {fault:?} unexpectedly completed preservation: {error:#?}"
        );
        assert_eq!(
            fixture.client.state_db.get(fixture.candidate.id).unwrap().id,
            fixture.candidate.id,
            "fault {fault:?} deleted the only candidate correlation"
        );
        assert_eq!(
            fs::read_to_string(fixture.client.installation.root.join("usr/.stateID")).unwrap(),
            fixture.previous.id.to_string()
        );

        let expected = candidate_token.unwrap();
        let mut retained_tokens = Vec::new();
        let staged = fixture.client.installation.staging_path("usr");
        if staged.exists() {
            retained_tokens.push(recovery_tree_token(&staged));
        }
        for entry in fs::read_dir(fixture.client.installation.state_quarantine_dir()).unwrap() {
            let usr = entry.unwrap().path().join("usr");
            if usr.exists() {
                retained_tokens.push(recovery_tree_token(&usr));
            }
        }
        assert_eq!(
            retained_tokens,
            [expected],
            "fault {fault:?} lost or duplicated the candidate tree"
        );
    }
}

#[test]
fn single_quarantine_durability_fault_is_resumed_before_invalidation() {
    use crate::transition_identity::QuarantineFaultPoint;

    for fault in [
        QuarantineFaultPoint::CandidatePreSync,
        QuarantineFaultPoint::SlotSync,
        QuarantineFaultPoint::QuarantineBaseSync,
        QuarantineFaultPoint::Rename,
        QuarantineFaultPoint::MovedCandidateSync,
        QuarantineFaultPoint::SourceParentSync,
        QuarantineFaultPoint::DestinationParentSync,
        QuarantineFaultPoint::FinalRevalidation,
    ] {
        let fixture = stateful_transition_fixture(false);
        let mut token = None;

        crate::transition_identity::arm_quarantine_fault(fault);
        let error = fixture
            .client
            .apply_stateful_blit_with_checkpoint(
                vfs(Vec::new()).unwrap(),
                &fixture.candidate,
                Some(fixture.previous.id),
                generated_system_snapshot("candidate-package"),
                |checkpoint| {
                    if checkpoint == StatefulTransitionCheckpoint::AfterUsrExchange {
                        token = Some(recovery_tree_token(&fixture.client.installation.root.join("usr")));
                        Err(injected_state_transition_error("force resumable quarantine fault"))
                    } else {
                        Ok(())
                    }
                },
            )
            .unwrap_err();

        assert!(
            matches!(error, Error::StatefulTransitionUsrRestored { .. }),
            "single fault {fault:?} did not resume through production recovery: {error:#?}"
        );
        assert!(fixture.client.state_db.get(fixture.candidate.id).is_err());
        assert!(!fixture.client.installation.staging_path("usr").exists());
        let quarantines = fs::read_dir(fixture.client.installation.state_quarantine_dir())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        assert_eq!(quarantines.len(), 1);
        assert_eq!(recovery_tree_token(&quarantines[0].join("usr")), token.unwrap());
    }
}

#[test]
fn quarantine_is_revalidated_after_the_invalidation_checkpoint() {
    let fixture = stateful_transition_fixture(false);
    let mut displaced = None;

    let error = fixture
        .client
        .apply_stateful_blit_with_checkpoint(
            vfs(Vec::new()).unwrap(),
            &fixture.candidate,
            Some(fixture.previous.id),
            generated_system_snapshot("candidate-package"),
            |checkpoint| match checkpoint {
                StatefulTransitionCheckpoint::AfterUsrExchange => {
                    Err(injected_state_transition_error("force quarantine"))
                }
                StatefulTransitionCheckpoint::BeforeRecoveryCandidateInvalidation => {
                    let quarantine = fs::read_dir(fixture.client.installation.state_quarantine_dir())
                        .unwrap()
                        .next()
                        .unwrap()
                        .unwrap()
                        .path();
                    let moved = quarantine.with_extension("displaced");
                    fs::rename(&quarantine, &moved).unwrap();
                    fs::create_dir(&quarantine).unwrap();
                    fs::write(quarantine.join("sentinel"), b"substituted slot").unwrap();
                    displaced = Some((quarantine, moved));
                    Ok(())
                }
                _ => Ok(()),
            },
        )
        .unwrap_err();

    assert!(matches!(
        error,
        Error::StatefulTransitionRecoveryFailed {
            invalidate_candidate: Some(_),
            ..
        }
    ));
    assert_eq!(
        fixture.client.state_db.get(fixture.candidate.id).unwrap().id,
        fixture.candidate.id
    );
    let (replacement, moved) = displaced.unwrap();
    assert_eq!(fs::read(replacement.join("sentinel")).unwrap(), b"substituted slot");
    assert_eq!(
        fs::read_to_string(moved.join("usr/.stateID")).unwrap(),
        fixture.candidate.id.to_string()
    );
}

#[test]
fn deterministic_quarantine_name_collision_preserves_foreign_entry_and_database_row() {
    let fixture = stateful_transition_fixture(false);
    let mut collision = None;

    let error = fixture
        .client
        .apply_stateful_blit_with_checkpoint(
            vfs(Vec::new()).unwrap(),
            &fixture.candidate,
            Some(fixture.previous.id),
            generated_system_snapshot("candidate-package"),
            |checkpoint| {
                if checkpoint == StatefulTransitionCheckpoint::AfterUsrExchange {
                    let token = recovery_tree_token(&fixture.client.installation.root.join("usr"));
                    let path = fixture
                        .client
                        .installation
                        .state_quarantine_dir()
                        .join(format!("failed-new-state-{}-{token}", fixture.candidate.id));
                    fs::create_dir(&path).unwrap();
                    fs::write(path.join("sentinel"), b"foreign quarantine occupant").unwrap();
                    collision = Some(path);
                    Err(injected_state_transition_error("quarantine collision"))
                } else {
                    Ok(())
                }
            },
        )
        .unwrap_err();

    assert!(matches!(
        &error,
        Error::StatefulTransitionRecoveryFailed {
            preserve_candidate: Some(_),
            ..
        }
    ));
    let collision = collision.unwrap();
    assert_eq!(
        fs::read(collision.join("sentinel")).unwrap(),
        b"foreign quarantine occupant"
    );
    assert_eq!(
        fixture.client.state_db.get(fixture.candidate.id).unwrap().id,
        fixture.candidate.id
    );
    assert_eq!(
        fs::read_to_string(fixture.client.installation.staging_path("usr/.stateID")).unwrap(),
        fixture.candidate.id.to_string()
    );
}

#[test]
fn empty_deterministic_quarantine_collision_is_never_adopted() {
    let fixture = stateful_transition_fixture(false);
    let mut collision = None;

    let error = fixture
        .client
        .apply_stateful_blit_with_checkpoint(
            vfs(Vec::new()).unwrap(),
            &fixture.candidate,
            Some(fixture.previous.id),
            generated_system_snapshot("candidate-package"),
            |checkpoint| {
                if checkpoint == StatefulTransitionCheckpoint::AfterUsrExchange {
                    let token = recovery_tree_token(&fixture.client.installation.root.join("usr"));
                    let path = fixture
                        .client
                        .installation
                        .state_quarantine_dir()
                        .join(format!("failed-new-state-{}-{token}", fixture.candidate.id));
                    fs::create_dir(&path).unwrap();
                    fs::set_permissions(&path, Permissions::from_mode(0o700)).unwrap();
                    collision = Some(path);
                    Err(injected_state_transition_error("empty quarantine collision"))
                } else {
                    Ok(())
                }
            },
        )
        .unwrap_err();

    assert!(matches!(
        &error,
        Error::StatefulTransitionRecoveryFailed {
            preserve_candidate: Some(_),
            ..
        }
    ));
    assert_eq!(
        fixture.client.state_db.get(fixture.candidate.id).unwrap().id,
        fixture.candidate.id
    );
    assert!(fixture.client.installation.staging_path("usr").is_dir());
    assert_eq!(fs::read_dir(collision.unwrap()).unwrap().count(), 0);
}

#[test]
fn quarantine_slot_creation_rejects_replacement_before_retention() {
    let fixture = stateful_transition_fixture(false);
    let quarantine_root = fixture.client.installation.state_quarantine_dir();
    let observed: std::rc::Rc<std::cell::RefCell<Option<(PathBuf, PathBuf)>>> =
        std::rc::Rc::new(std::cell::RefCell::new(None));
    let hook_observed = observed.clone();
    crate::transition_identity::arm_before_quarantine_slot_reopen(move || {
        let created = fs::read_dir(&quarantine_root)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .next()
            .expect("quarantine slot must have been created before reopen");
        let displaced = created.with_extension("created");
        fs::rename(&created, &displaced).unwrap();
        fs::create_dir(&created).unwrap();
        fs::set_permissions(&created, Permissions::from_mode(0o700)).unwrap();
        hook_observed.replace(Some((created, displaced)));
    });

    let error = fixture
        .client
        .apply_stateful_blit_with_checkpoint(
            vfs(Vec::new()).unwrap(),
            &fixture.candidate,
            Some(fixture.previous.id),
            generated_system_snapshot("candidate-package"),
            |checkpoint| {
                if checkpoint == StatefulTransitionCheckpoint::AfterUsrExchange {
                    Err(injected_state_transition_error("replace fresh quarantine slot"))
                } else {
                    Ok(())
                }
            },
        )
        .unwrap_err();

    assert!(matches!(
        &error,
        Error::StatefulTransitionRecoveryFailed {
            preserve_candidate: Some(_),
            ..
        }
    ));
    assert_eq!(
        fixture.client.state_db.get(fixture.candidate.id).unwrap().id,
        fixture.candidate.id
    );
    assert_eq!(
        fs::read_to_string(fixture.client.installation.staging_path("usr/.stateID")).unwrap(),
        fixture.candidate.id.to_string()
    );
    let (replacement, displaced) = observed.as_ref().borrow().clone().unwrap();
    assert_eq!(fs::read_dir(replacement).unwrap().count(), 0);
    assert_eq!(fs::read_dir(displaced).unwrap().count(), 0);
}

#[test]
fn stateful_tree_tokens_follow_their_logical_trees_through_exchange_and_archive() {
    let fixture = stateful_transition_fixture(true);
    let live_usr = fixture.client.installation.root.join("usr");
    let staged_usr = fixture.client.installation.staging_path("usr");
    let previous_archive = fixture
        .client
        .installation
        .root_path(fixture.previous.id.to_string())
        .join("usr");
    let mut exchanged_tokens = None;

    fixture
        .client
        .activate_state_with_checkpoint(fixture.candidate.id, true, true, |checkpoint| {
            if checkpoint == StatefulTransitionCheckpoint::AfterUsrExchange {
                let candidate_wrapper = fixture.client.installation.root_path(fixture.candidate.id.to_string());
                let candidate_token = fs::read_dir(candidate_wrapper)
                    .unwrap()
                    .map(|entry| entry.unwrap().file_name())
                    .find_map(|name| {
                        name.to_string_lossy()
                            .strip_prefix(&format!(".cast-state-slot-{}-", fixture.candidate.id))
                            .map(str::to_owned)
                    })
                    .expect("candidate slot hardlink was present at the exchange boundary");
                exchanged_tokens = Some((candidate_token, recovery_tree_token(&staged_usr)));
            }
            Ok(())
        })
        .unwrap();

    let (candidate_token, previous_token) = exchanged_tokens.expect("exchange boundary was observed");
    assert_ne!(candidate_token, previous_token);
    let parked = archived_candidate_slot_parking_paths(&fixture.client.installation, fixture.candidate.id);
    assert_eq!(parked.len(), 1);
    let slot_link = fs::read_dir(&parked[0])
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.file_name().unwrap().to_string_lossy().ends_with(&candidate_token))
        .unwrap();
    assert_eq!(
        fs::symlink_metadata(live_usr.join(".cast-tree-id")).unwrap().ino(),
        fs::symlink_metadata(slot_link).unwrap().ino()
    );
    assert_eq!(recovery_tree_token(&previous_archive), previous_token);
    assert!(!staged_usr.exists());
}

#[test]
fn recovery_never_recreates_a_missing_candidate_tree_marker() {
    let fixture = stateful_transition_fixture(false);
    let candidate_model = generated_system_snapshot("candidate-package");
    let live_usr = fixture.client.installation.root.join("usr");
    let staged_usr = fixture.client.installation.staging_path("usr");

    let error = fixture
        .client
        .apply_stateful_blit_with_checkpoint(
            vfs(Vec::new()).unwrap(),
            &fixture.candidate,
            Some(fixture.previous.id),
            candidate_model,
            |checkpoint| {
                if checkpoint == StatefulTransitionCheckpoint::AfterUsrExchange {
                    fs::remove_file(live_usr.join(".cast-tree-id")).unwrap();
                    Err(injected_state_transition_error("marker removed after exchange"))
                } else {
                    Ok(())
                }
            },
        )
        .unwrap_err();

    assert!(
        matches!(
            &error,
            Error::StatefulTransitionRecoveryFailed {
                candidate,
                previous: Some(previous),
                reverse_exchange: Some(_),
                ..
            } if *candidate == fixture.candidate.id && *previous == fixture.previous.id
        ),
        "unexpected recovery result: {error:#?}"
    );
    assert!(!live_usr.join(".cast-tree-id").exists());
    assert!(staged_usr.join(".cast-tree-id").is_file());
    assert_eq!(
        fs::read_to_string(live_usr.join(".stateID")).unwrap(),
        fixture.candidate.id.to_string()
    );
    assert_eq!(
        fs::read_to_string(staged_usr.join(".stateID")).unwrap(),
        fixture.previous.id.to_string()
    );
    assert_eq!(
        fixture.client.state_db.get(fixture.candidate.id).unwrap().id,
        fixture.candidate.id,
        "an unauthenticated candidate must retain its database row"
    );
}
