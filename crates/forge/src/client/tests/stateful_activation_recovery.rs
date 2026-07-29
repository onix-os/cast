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
fn archived_state_activation_carries_each_generated_snapshot_with_its_usr_tree() {
    let temporary = tempfile::tempdir().unwrap();
    let mut client = stateful_test_client(temporary.path());
    let old_snapshot = generated_system_snapshot("old-package");
    let new_snapshot = generated_system_snapshot("new-package");
    // Both states carry provenance: the coordinated activation route verifies
    // the candidate's stored metadata rather than trusting its absence.
    let old = add_state_with_metadata(&client, "old", &old_snapshot);
    let new = add_state_with_metadata(&client, "new", &new_snapshot);
    client.installation.active_state = Some(old.id);

    let old_encoded = old_snapshot.encoded().to_owned();
    record_state_id(&client.installation.root, old.id).unwrap();
    record_candidate_metadata(&client.installation.root, old_snapshot);

    let archived_new_root = client.installation.root_path(new.id.to_string());
    let new_encoded = new_snapshot.encoded().to_owned();
    record_state_id(&archived_new_root, new.id).unwrap();
    record_candidate_metadata(&archived_new_root, new_snapshot);

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
