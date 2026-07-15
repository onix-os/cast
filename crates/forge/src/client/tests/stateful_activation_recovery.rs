#[test]
fn archived_activation_archive_failure_reverses_usr_and_rearchives_the_candidate() {
    let fixture = stateful_transition_fixture(true);
    let error = fixture
        .client
        .activate_state_with_checkpoint(fixture.candidate.id, true, true, |checkpoint| {
            if checkpoint == StatefulTransitionCheckpoint::BeforePreviousStateArchive {
                Err(injected_state_transition_error("previous-state archive"))
            } else {
                Ok(())
            }
        })
        .unwrap_err();

    assert!(matches!(
        error,
        Error::StatefulTransitionUsrRestored {
            candidate,
            previous: Some(previous),
            ..
        } if candidate == fixture.candidate.id && previous == fixture.previous.id
    ));
    assert_recovered_stateful_transition(&fixture);
}

#[test]
fn skipped_boot_is_not_synchronized_during_pre_boot_recovery() {
    let fixture = stateful_transition_fixture(true);
    let mut attempted_boot_repair = false;
    let error = fixture
        .client
        .activate_state_with_checkpoint(fixture.candidate.id, true, true, |checkpoint| match checkpoint {
            StatefulTransitionCheckpoint::AfterUsrExchange => {
                Err(injected_state_transition_error("pre-boot activation failure"))
            }
            StatefulTransitionCheckpoint::BeforeRecoveryBootSynchronization => {
                attempted_boot_repair = true;
                Ok(())
            }
            _ => Ok(()),
        })
        .unwrap_err();

    assert!(matches!(error, Error::StatefulTransitionUsrRestored { .. }));
    assert!(!attempted_boot_repair);
    assert_recovered_stateful_transition(&fixture);
}

#[test]
fn candidate_boot_sees_the_archived_previous_state_and_failure_restores_it() {
    let fixture = stateful_transition_fixture(true);
    let previous_archive = fixture
        .client
        .installation
        .root_path(fixture.previous.id.to_string())
        .join("usr");
    let staged = fixture.client.installation.staging_path("usr");
    let mut observed_boot_boundary = false;

    let error = fixture
        .client
        .activate_state_with_checkpoint(fixture.candidate.id, true, false, |checkpoint| {
            if checkpoint == StatefulTransitionCheckpoint::BeforeCandidateBootSynchronization {
                observed_boot_boundary = true;
                assert_eq!(
                    fs::read_to_string(previous_archive.join(".stateID")).unwrap(),
                    fixture.previous.id.to_string()
                );
                assert!(!staged.exists());
                Err(injected_state_transition_error("candidate boot synchronization"))
            } else {
                Ok(())
            }
        })
        .unwrap_err();

    assert!(observed_boot_boundary);
    assert!(matches!(error, Error::StatefulTransitionUsrRestored { .. }));
    assert_recovered_stateful_transition(&fixture);
}

#[test]
fn new_stateful_post_swap_failure_quarantines_and_invalidates_candidate() {
    let fixture = stateful_transition_fixture(false);
    let candidate_model = generated_system_snapshot("candidate-package");
    let error = fixture
        .client
        .apply_stateful_blit_with_checkpoint(
            vfs(Vec::new()).unwrap(),
            &fixture.candidate,
            Some(fixture.previous.id),
            candidate_model,
            |checkpoint| {
                if checkpoint == StatefulTransitionCheckpoint::AfterPreviousStateArchive {
                    Err(injected_state_transition_error("after previous-state archive"))
                } else {
                    Ok(())
                }
            },
        )
        .unwrap_err();

    assert!(matches!(
        error,
        Error::StatefulTransitionUsrRestored {
            candidate,
            previous: Some(previous),
            ..
        } if candidate == fixture.candidate.id && previous == fixture.previous.id
    ));
    assert_fresh_candidate_quarantined_and_invalidated(&fixture);
}

#[test]
fn previous_archive_never_replaces_a_racing_empty_destination() {
    let fixture = stateful_transition_fixture(false);
    let destination = fixture
        .client
        .installation
        .root_path(fixture.previous.id.to_string())
        .join("usr");
    let mut occupant_inode = None;

    let error = fixture
        .client
        .apply_stateful_blit_with_checkpoint(
            vfs(Vec::new()).unwrap(),
            &fixture.candidate,
            Some(fixture.previous.id),
            generated_system_snapshot("candidate-package"),
            |checkpoint| {
                if checkpoint == StatefulTransitionCheckpoint::BeforePreviousStateArchive {
                    fs::create_dir_all(&destination).unwrap();
                    occupant_inode = Some(fs::symlink_metadata(&destination).unwrap().ino());
                }
                Ok(())
            },
        )
        .unwrap_err();

    assert!(
        matches!(&error, Error::StatefulTransitionUsrRestored { .. }),
        "{error:#?}"
    );
    assert_eq!(
        fs::symlink_metadata(&destination).unwrap().ino(),
        occupant_inode.unwrap()
    );
    assert_eq!(fs::read_dir(&destination).unwrap().count(), 0);
    assert_eq!(
        fs::read_to_string(fixture.client.installation.root.join("usr/.stateID")).unwrap(),
        fixture.previous.id.to_string()
    );
}

#[test]
fn previous_restore_never_replaces_a_racing_empty_staging_destination() {
    let fixture = stateful_transition_fixture(false);
    let staged = fixture.client.installation.staging_path("usr");
    let archived = fixture
        .client
        .installation
        .root_path(fixture.previous.id.to_string())
        .join("usr");
    let mut occupant_inode = None;

    let error = fixture
        .client
        .apply_stateful_blit_with_checkpoint(
            vfs(Vec::new()).unwrap(),
            &fixture.candidate,
            Some(fixture.previous.id),
            generated_system_snapshot("candidate-package"),
            |checkpoint| {
                if checkpoint == StatefulTransitionCheckpoint::AfterPreviousStateArchive {
                    fs::create_dir(&staged).unwrap();
                    occupant_inode = Some(fs::symlink_metadata(&staged).unwrap().ino());
                    Err(injected_state_transition_error("force previous restore"))
                } else {
                    Ok(())
                }
            },
        )
        .unwrap_err();

    assert!(matches!(
        error,
        Error::StatefulTransitionRecoveryFailed {
            restore_previous: Some(_),
            ..
        }
    ));
    assert_eq!(fs::symlink_metadata(&staged).unwrap().ino(), occupant_inode.unwrap());
    assert_eq!(fs::read_dir(&staged).unwrap().count(), 0);
    assert_eq!(
        fs::read_to_string(archived.join(".stateID")).unwrap(),
        fixture.previous.id.to_string()
    );
    assert_eq!(
        fixture.client.state_db.get(fixture.candidate.id).unwrap().id,
        fixture.candidate.id
    );
}

#[test]
fn incomplete_fresh_reverse_retains_live_candidate_record_and_reopens() {
    let fixture = stateful_transition_fixture(false);
    let root = fixture._temporary.path().to_owned();
    let candidate_model = generated_system_snapshot("candidate-package");
    let error = fixture
        .client
        .apply_stateful_blit_with_checkpoint(
            vfs(Vec::new()).unwrap(),
            &fixture.candidate,
            Some(fixture.previous.id),
            candidate_model,
            |checkpoint| match checkpoint {
                StatefulTransitionCheckpoint::AfterUsrExchange => {
                    Err(injected_state_transition_error("fresh transition failure"))
                }
                StatefulTransitionCheckpoint::BeforeRecoveryUsrExchange => {
                    Err(injected_state_transition_error("reverse exchange failure"))
                }
                _ => Ok(()),
            },
        )
        .unwrap_err();

    let Error::StatefulTransitionRecoveryFailed {
        reverse_exchange: Some(_),
        invalidate_candidate,
        ..
    } = error
    else {
        panic!("expected incomplete reverse recovery");
    };
    assert!(invalidate_candidate.is_none());
    assert_eq!(
        fixture.client.state_db.get(fixture.candidate.id).unwrap().id,
        fixture.candidate.id
    );
    assert_eq!(
        fs::read_to_string(fixture.client.installation.root.join("usr/.stateID")).unwrap(),
        fixture.candidate.id.to_string()
    );
    assert_eq!(
        fs::read_to_string(fixture.client.installation.staging_path("usr/.stateID")).unwrap(),
        fixture.previous.id.to_string()
    );

    let candidate = fixture.candidate.id;
    drop(fixture.client);
    let reopened = stateful_test_client(&root);
    assert_eq!(reopened.installation.active_state, Some(candidate));
    assert_eq!(reopened.get_active_state().unwrap().unwrap().id, candidate);
}

#[test]
fn incomplete_previous_restore_retains_live_fresh_candidate_record_and_reopens() {
    let fixture = stateful_transition_fixture(false);
    let root = fixture._temporary.path().to_owned();
    let candidate_model = generated_system_snapshot("candidate-package");
    let error = fixture
        .client
        .apply_stateful_blit_with_checkpoint(
            vfs(Vec::new()).unwrap(),
            &fixture.candidate,
            Some(fixture.previous.id),
            candidate_model,
            |checkpoint| match checkpoint {
                StatefulTransitionCheckpoint::AfterPreviousStateArchive => {
                    Err(injected_state_transition_error("fresh transition failure"))
                }
                StatefulTransitionCheckpoint::BeforeRecoveryPreviousStateRestore => {
                    Err(injected_state_transition_error("previous-state restore failure"))
                }
                _ => Ok(()),
            },
        )
        .unwrap_err();

    let Error::StatefulTransitionRecoveryFailed {
        restore_previous: Some(_),
        invalidate_candidate,
        ..
    } = error
    else {
        panic!("expected incomplete previous-state restore");
    };
    assert!(invalidate_candidate.is_none());
    assert_eq!(
        fixture.client.state_db.get(fixture.candidate.id).unwrap().id,
        fixture.candidate.id
    );
    assert_eq!(
        fs::read_to_string(fixture.client.installation.root.join("usr/.stateID")).unwrap(),
        fixture.candidate.id.to_string()
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

    let candidate = fixture.candidate.id;
    drop(fixture.client);
    let reopened = stateful_test_client(&root);
    assert_eq!(reopened.installation.active_state, Some(candidate));
    assert_eq!(reopened.get_active_state().unwrap().unwrap().id, candidate);
}

#[test]
fn new_stateful_pre_swap_failure_quarantines_and_invalidates_candidate() {
    let fixture = stateful_transition_fixture(false);
    let candidate_model = generated_system_snapshot("candidate-package");
    let error = fixture
        .client
        .apply_stateful_blit_with_checkpoint(
            vfs(Vec::new()).unwrap(),
            &fixture.candidate,
            Some(fixture.previous.id),
            candidate_model,
            |checkpoint| {
                if checkpoint == StatefulTransitionCheckpoint::AfterTransactionTriggers {
                    Err(injected_state_transition_error("pre-swap preparation"))
                } else {
                    Ok(())
                }
            },
        )
        .unwrap_err();

    assert!(matches!(
        error,
        Error::StatefulCandidatePreserved {
            candidate,
            previous: Some(previous),
            ..
        } if candidate == fixture.candidate.id && previous == fixture.previous.id
    ));
    assert_fresh_candidate_quarantined_and_invalidated(&fixture);
}

#[test]
fn incomplete_archived_system_trigger_phase_quarantines_the_mutated_candidate() {
    let fixture = stateful_transition_fixture(true);
    let root = fixture._temporary.path().to_owned();
    let marker = Path::new("usr/partial-system-trigger");
    let error = fixture
        .client
        .activate_state_with_checkpoint(fixture.candidate.id, false, true, |checkpoint| {
            if checkpoint == StatefulTransitionCheckpoint::AfterSystemTriggersStarted {
                fs::write(fixture.client.installation.root.join(marker), b"partial mutation").unwrap();
                Err(injected_state_transition_error("incomplete system trigger phase"))
            } else {
                Ok(())
            }
        })
        .unwrap_err();

    assert!(matches!(
        error,
        Error::StatefulTransitionUsrRestored {
            candidate,
            previous: Some(previous),
            ..
        } if candidate == fixture.candidate.id && previous == fixture.previous.id
    ));
    assert_eq!(
        fixture.client.state_db.get(fixture.candidate.id).unwrap().id,
        fixture.candidate.id
    );
    assert_eq!(
        fs::read_to_string(fixture.client.installation.root.join("usr/.stateID")).unwrap(),
        fixture.previous.id.to_string()
    );
    assert!(
        !fixture
            .client
            .installation
            .root_path(fixture.candidate.id.to_string())
            .exists()
    );
    assert!(!fixture.client.installation.staging_path("usr").exists());

    let quarantines = fs::read_dir(fixture.client.installation.state_quarantine_dir())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    assert_eq!(quarantines.len(), 1);
    assert!(
        quarantines[0]
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with(&format!("failed-archived-state-{}-", fixture.candidate.id))
    );
    assert_eq!(fs::read(quarantines[0].join(marker)).unwrap(), b"partial mutation");

    let previous = fixture.previous.id;
    drop(fixture.client);
    let reopened = stateful_test_client(&root);
    assert_eq!(reopened.installation.active_state, Some(previous));
    assert_eq!(reopened.get_active_state().unwrap().unwrap().id, previous);
}

#[test]
fn completed_archived_system_trigger_phase_can_rearchive_after_later_failure() {
    let fixture = stateful_transition_fixture(true);
    let error = fixture
        .client
        .activate_state_with_checkpoint(fixture.candidate.id, false, false, |checkpoint| {
            if checkpoint == StatefulTransitionCheckpoint::BeforeCandidateBootSynchronization {
                Err(injected_state_transition_error("post-trigger boot preparation"))
            } else {
                Ok(())
            }
        })
        .unwrap_err();

    assert!(matches!(error, Error::StatefulTransitionUsrRestored { .. }));
    assert_recovered_stateful_transition(&fixture);
    assert_eq!(
        fs::read_dir(fixture.client.installation.state_quarantine_dir())
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn two_failed_active_state_reblits_use_unique_non_state_quarantines() {
    let temporary = tempfile::tempdir().unwrap();
    let mut client = stateful_test_client(temporary.path());
    let state = client.state_db.add(&[], Some("active"), None).unwrap();
    client.installation.active_state = Some(state.id);

    let restored_model = generated_system_snapshot("restored-active-package");
    let restored_snapshot = restored_model.encoded().to_owned();
    record_state_id(&client.installation.root, state.id).unwrap();
    record_system_snapshot(&client.installation.root, restored_model).unwrap();

    let mut failed_snapshots = BTreeSet::new();
    for package in ["first-failed-reblit-package", "second-failed-reblit-package"] {
        let failed_model = generated_system_snapshot(package);
        failed_snapshots.insert(failed_model.encoded().to_owned());
        let error = client
            .apply_stateful_blit_with_checkpoint(vfs(Vec::new()).unwrap(), &state, None, failed_model, |checkpoint| {
                match checkpoint {
                    StatefulTransitionCheckpoint::AfterTransactionTriggers => {
                        fs::write(client.installation.staging_path("wrapper-sentinel"), package)?;
                        Ok(())
                    }
                    StatefulTransitionCheckpoint::AfterUsrExchange => {
                        Err(injected_state_transition_error("active-state reblit"))
                    }
                    _ => Ok(()),
                }
            })
            .unwrap_err();

        assert!(
            matches!(
                &error,
                Error::StatefulTransitionUsrRestored {
                    candidate,
                    previous: Some(previous),
                    ..
                } if *candidate == state.id && *previous == state.id
            ),
            "unexpected active reblit recovery result: {error:#?}"
        );
        assert_eq!(
            fs::read_to_string(client.installation.root.join("usr/.stateID")).unwrap(),
            state.id.to_string()
        );
        assert_generated_snapshot(
            &system_model::snapshot_path(&client.installation.root),
            &restored_snapshot,
            "restored-active-package",
        );
        assert!(!client.installation.root_path(state.id.to_string()).join("usr").exists());
        assert_eq!(fs::read_dir(client.installation.staging_dir()).unwrap().count(), 0);
    }

    let quarantine_dir = client.installation.state_quarantine_dir();
    assert!(!quarantine_dir.starts_with(client.installation.root_path("")));
    let quarantines = fs::read_dir(&quarantine_dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    assert_eq!(quarantines.len(), 2);
    assert_eq!(quarantines.iter().collect::<BTreeSet<_>>().len(), 2);

    let mut preserved_snapshots = BTreeSet::new();
    let mut preserved_tokens = BTreeSet::new();
    let mut preserved_sentinels = BTreeSet::new();
    for quarantine in quarantines {
        assert!(
            quarantine
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(&format!("replaced-active-reblit-wrapper-{}-", state.id))
        );
        assert_eq!(
            fs::read_to_string(quarantine.join("usr/.stateID")).unwrap(),
            state.id.to_string()
        );
        let token = recovery_tree_token(&quarantine.join("usr"));
        preserved_tokens.insert(token);
        preserved_sentinels.insert(fs::read_to_string(quarantine.join("wrapper-sentinel")).unwrap());
        preserved_snapshots.insert(fs::read_to_string(system_model::snapshot_path(&quarantine)).unwrap());
    }
    assert_eq!(preserved_tokens.len(), 2);
    assert_eq!(
        preserved_sentinels,
        BTreeSet::from([
            "first-failed-reblit-package".to_owned(),
            "second-failed-reblit-package".to_owned(),
        ])
    );
    assert_eq!(preserved_snapshots, failed_snapshots);
}

#[test]
fn recovery_reports_candidate_preservation_and_boot_repair_failures_without_losing_either_usr() {
    let fixture = stateful_transition_fixture(true);
    let mut attempted_boot_repair = false;
    let error = fixture
        .client
        .activate_state_with_checkpoint(fixture.candidate.id, true, false, |checkpoint| match checkpoint {
            StatefulTransitionCheckpoint::AfterCandidateBootSynchronizationStarted => Err(
                injected_state_transition_error("candidate boot synchronization failure"),
            ),
            StatefulTransitionCheckpoint::BeforeRecoveryCandidatePreservation => {
                Err(injected_state_transition_error("candidate preservation failure"))
            }
            StatefulTransitionCheckpoint::BeforeRecoveryBootSynchronization => {
                attempted_boot_repair = true;
                Err(injected_state_transition_error("restored-state boot repair failure"))
            }
            _ => Ok(()),
        })
        .unwrap_err();

    let Error::StatefulTransitionRecoveryFailed {
        candidate,
        previous: Some(previous),
        restore_previous,
        reverse_exchange,
        preserve_candidate,
        repair_boot,
        ..
    } = error
    else {
        panic!("expected structured state recovery failure");
    };
    assert_eq!(candidate, fixture.candidate.id);
    assert_eq!(previous, fixture.previous.id);
    assert!(restore_previous.is_none());
    assert!(reverse_exchange.is_none());
    assert!(preserve_candidate.is_some());
    assert!(repair_boot.is_some());
    assert!(attempted_boot_repair);

    assert_eq!(
        fs::read_to_string(fixture.client.installation.root.join("usr/.stateID")).unwrap(),
        fixture.previous.id.to_string()
    );
    assert_generated_snapshot(
        &system_model::snapshot_path(&fixture.client.installation.root),
        &fixture.previous_snapshot,
        "previous-package",
    );
    assert_eq!(
        fs::read_to_string(fixture.client.installation.staging_path("usr/.stateID")).unwrap(),
        fixture.candidate.id.to_string()
    );
    assert_generated_snapshot(
        &system_model::snapshot_path(&fixture.client.installation.staging_dir()),
        &fixture.candidate_snapshot,
        "candidate-package",
    );
    assert!(
        !fixture
            .client
            .installation
            .root_path(fixture.candidate.id.to_string())
            .join("usr")
            .exists()
    );
}

#[test]
fn apparent_boot_repair_success_remains_structurally_unverified() {
    let fixture = stateful_transition_fixture(true);
    let error = fixture
        .client
        .activate_state_with_checkpoint(fixture.candidate.id, true, false, |checkpoint| {
            if checkpoint == StatefulTransitionCheckpoint::AfterCandidateBootSynchronizationStarted {
                Err(injected_state_transition_error(
                    "candidate boot synchronization failure",
                ))
            } else {
                Ok(())
            }
        })
        .unwrap_err();

    let Error::StatefulTransitionRecoveryFailed {
        candidate,
        previous: Some(previous),
        repair_boot: Some(repair_boot),
        ..
    } = error
    else {
        panic!("expected unverified boot repair failure");
    };
    assert_eq!(candidate, fixture.candidate.id);
    assert_eq!(previous, fixture.previous.id);
    assert!(matches!(
        *repair_boot,
        Error::StatefulBootRepairUnverified {
            candidate,
            previous: Some(previous),
        } if candidate == fixture.candidate.id && previous == fixture.previous.id
    ));
    assert_recovered_stateful_transition(&fixture);
}

#[test]
fn archived_state_activation_carries_each_generated_snapshot_with_its_usr_tree() {
    let temporary = tempfile::tempdir().unwrap();
    let mut client = stateful_test_client(temporary.path());
    let old = client.state_db.add(&[], Some("old"), None).unwrap();
    let new = client.state_db.add(&[], Some("new"), None).unwrap();
    client.installation.active_state = Some(old.id);

    let old_snapshot = generated_system_snapshot("old-package");
    let old_encoded = old_snapshot.encoded().to_owned();
    record_state_id(&client.installation.root, old.id).unwrap();
    record_system_snapshot(&client.installation.root, old_snapshot).unwrap();

    let archived_new_root = client.installation.root_path(new.id.to_string());
    let new_snapshot = generated_system_snapshot("new-package");
    let new_encoded = new_snapshot.encoded().to_owned();
    record_state_id(&archived_new_root, new.id).unwrap();
    record_system_snapshot(&archived_new_root, new_snapshot).unwrap();

    let archived = client.activate_state(new.id, true, true).unwrap();

    assert_eq!(archived, old.id);
    assert_generated_snapshot(
        &system_model::snapshot_path(&client.installation.root),
        &new_encoded,
        "new-package",
    );
    assert_generated_snapshot(
        &system_model::snapshot_path(&client.installation.root_path(old.id.to_string())),
        &old_encoded,
        "old-package",
    );
    assert_eq!(
        fs::read_to_string(client.installation.root.join("usr/.stateID")).unwrap(),
        new.id.to_string()
    );
    assert_eq!(
        fs::read_to_string(client.installation.root_path(old.id.to_string()).join("usr/.stateID")).unwrap(),
        old.id.to_string()
    );
}
