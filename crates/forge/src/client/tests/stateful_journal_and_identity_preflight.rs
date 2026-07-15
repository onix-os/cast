#[test]
fn unresolved_journal_evidence_blocks_marker_publication_before_activation() {
    let fixture = stateful_transition_fixture(false);
    let journal = crate::transition_journal::TransitionJournalStore::open(&fixture.client.installation.root).unwrap();
    drop(journal);
    let canonical = fixture.client.installation.root.join(".cast/journal/state-transition");
    fs::write(&canonical, b"not-a-canonical-transition-record").unwrap();
    fs::set_permissions(&canonical, Permissions::from_mode(0o600)).unwrap();

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

    assert!(matches!(
        error,
        Error::StatefulTreeIdentityPreparationFailed {
            candidate,
            previous: Some(previous),
            ..
        } if candidate == fixture.candidate.id && previous == fixture.previous.id
    ));
    assert!(!fixture.client.installation.root.join("usr/.cast-tree-id").exists());
    assert!(!fixture.client.installation.staging_path("usr/.cast-tree-id").exists());
    assert_eq!(fs::read(&canonical).unwrap(), b"not-a-canonical-transition-record");
}

#[test]
fn orphan_transition_row_blocks_marker_publication_before_activation() {
    let fixture = stateful_transition_fixture(false);
    let transition = state::TransitionId::generate().unwrap();
    fixture
        .client
        .state_db
        .add_with_transition(&transition, &[], Some("orphan"), None)
        .unwrap();

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

    assert!(matches!(
        error,
        Error::StatefulTreeIdentityPreparationFailed {
            candidate,
            previous: Some(previous),
            ..
        } if candidate == fixture.candidate.id && previous == fixture.previous.id
    ));
    assert!(!fixture.client.installation.root.join("usr/.cast-tree-id").exists());
    assert!(!fixture.client.installation.staging_path("usr/.cast-tree-id").exists());
    assert!(
        !fixture
            .client
            .installation
            .root
            .join(".cast/journal/state-transition")
            .exists()
    );
}

#[test]
fn first_install_synthesizes_syncs_marks_and_exchanges_an_empty_previous_usr() {
    let temporary = tempfile::tempdir().unwrap();
    let client = stateful_test_client(temporary.path());
    let candidate = client.state_db.add(&[], Some("first state"), None).unwrap();
    record_state_id(&client.installation.staging_dir(), candidate.id).unwrap();
    let candidate_usr = client.installation.staging_path("usr");
    let live_usr = client.installation.root.join("usr");
    assert!(!live_usr.exists());

    let mut active_state = active_state_authority::ActiveStateAuthority::acquire(&client.installation).unwrap();
    let tree_identity = client
        .prepare_stateful_tree_identity(&candidate_usr, candidate.id)
        .unwrap();
    let metadata_proof =
        candidate_metadata::decorate_stateful(&tree_identity, &generated_system_snapshot("first-state-package"))
            .unwrap();
    active_state
        .refresh_after_tree_identity_preparation(&client.installation)
        .unwrap();
    let live_root_abi = preflight_root_links(&client.installation.root).unwrap();
    tree_identity.verify_pre_exchange(&candidate_usr, &live_usr).unwrap();
    let synthesized_token = recovery_tree_token(&live_usr);
    let candidate_token = recovery_tree_token(&candidate_usr);
    assert_ne!(synthesized_token, candidate_token);
    let metadata = fs::symlink_metadata(&live_usr).unwrap();
    assert!(metadata.file_type().is_dir());
    assert_eq!(metadata.uid(), unsafe { nix::libc::geteuid() });
    assert_eq!(metadata.permissions().mode() & 0o7777, 0o755);
    let entries = fs::read_dir(&live_usr)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    assert_eq!(entries, [OsString::from(".cast-tree-id")]);

    client
        .commit_stateful_staging(
            &vfs(Vec::new()).unwrap(),
            &candidate,
            None,
            StatefulCandidateOrigin::Fresh,
            false,
            false,
            false,
            &tree_identity,
            Some(&metadata_proof),
            live_root_abi,
            &active_state,
            &mut |_| Ok(()),
        )
        .unwrap();

    assert_eq!(recovery_tree_token(&live_usr), candidate_token);
    assert_eq!(recovery_tree_token(&candidate_usr), synthesized_token);
    assert_eq!(
        fs::read_to_string(live_usr.join(".stateID")).unwrap(),
        candidate.id.to_string()
    );
    assert_eq!(client.state_db.get(candidate.id).unwrap().id, candidate.id);
    assert!(!client.installation.root.join(".cast/journal/state-transition").exists());
}

#[test]
fn failed_first_install_can_retry_the_exact_marker_only_previous_baseline() {
    let temporary = tempfile::tempdir().unwrap();
    let client = stateful_test_client(temporary.path());
    let live_usr = client.installation.root.join("usr");
    let mut previous_token = None;

    for summary in ["first failed attempt", "retry attempt"] {
        let candidate = client.state_db.add(&[], Some(summary), None).unwrap();
        let mut reached_exchange = false;
        let error = client
            .apply_stateful_blit_with_checkpoint(
                vfs(Vec::new()).unwrap(),
                &candidate,
                None,
                generated_system_snapshot(summary),
                |checkpoint| {
                    if checkpoint == StatefulTransitionCheckpoint::BeforeUsrExchange {
                        reached_exchange = true;
                        Err(injected_state_transition_error("fail before first-install exchange"))
                    } else {
                        Ok(())
                    }
                },
            )
            .unwrap_err();

        assert!(
            reached_exchange,
            "retry stopped before the exchange boundary: {error:#?}"
        );
        assert!(matches!(error, Error::StatefulCandidatePreserved { .. }));
        let token = recovery_tree_token(&live_usr);
        if let Some(previous_token) = &previous_token {
            assert_eq!(
                &token, previous_token,
                "retry must adopt the exact durable baseline token"
            );
        } else {
            previous_token = Some(token);
        }
        let entries = fs::read_dir(&live_usr)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        assert_eq!(entries, [OsString::from(".cast-tree-id")]);
    }
}

#[test]
fn first_install_marker_retry_rejects_marker_plus_foreign_content_unchanged() {
    let temporary = tempfile::tempdir().unwrap();
    let client = stateful_test_client(temporary.path());
    let first = client.state_db.add(&[], Some("first attempt"), None).unwrap();
    record_state_id(&client.installation.staging_dir(), first.id).unwrap();
    let identity = client
        .prepare_stateful_tree_identity(&client.installation.staging_path("usr"), first.id)
        .unwrap();
    drop(identity);

    let live_usr = client.installation.root.join("usr");
    let token = recovery_tree_token(&live_usr);
    let foreign = live_usr.join("foreign");
    fs::write(&foreign, b"do not remove").unwrap();

    let error = client
        .prepare_stateful_tree_identity(&client.installation.staging_path("usr"), first.id)
        .unwrap_err();
    assert!(matches!(
        error,
        crate::transition_identity::Error::LiveUsrNotEmpty { .. }
    ));
    assert_eq!(recovery_tree_token(&live_usr), token);
    assert_eq!(fs::read(&foreign).unwrap(), b"do not remove");
}

#[test]
fn first_install_rejects_a_hostile_live_usr_symlink_unchanged() {
    let temporary = tempfile::tempdir().unwrap();
    let client = stateful_test_client(temporary.path());
    let candidate = client.state_db.add(&[], Some("first state"), None).unwrap();
    record_state_id(&client.installation.staging_dir(), candidate.id).unwrap();
    let candidate_usr = client.installation.staging_path("usr");
    let foreign = client.installation.root.join("foreign-usr");
    fs::create_dir(&foreign).unwrap();
    fs::write(foreign.join("foreign"), b"untouched").unwrap();
    symlink("foreign-usr", client.installation.root.join("usr")).unwrap();

    let error = client
        .prepare_stateful_tree_identity(&candidate_usr, candidate.id)
        .unwrap_err();
    assert!(matches!(error, crate::transition_identity::Error::LiveUsr { .. }));
    assert!(
        fs::symlink_metadata(client.installation.root.join("usr"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        fs::read_link(client.installation.root.join("usr")).unwrap(),
        Path::new("foreign-usr")
    );
    assert_eq!(fs::read(foreign.join("foreign")).unwrap(), b"untouched");
    assert!(!candidate_usr.join(".cast-tree-id").exists());
}

#[test]
fn first_install_rejects_a_preexisting_nonempty_unmanaged_usr_unchanged() {
    let temporary = tempfile::tempdir().unwrap();
    let client = stateful_test_client(temporary.path());
    let candidate = client.state_db.add(&[], Some("first state"), None).unwrap();
    record_state_id(&client.installation.staging_dir(), candidate.id).unwrap();
    let candidate_usr = client.installation.staging_path("usr");
    let live_usr = client.installation.root.join("usr");
    fs::create_dir(&live_usr).unwrap();
    fs::set_permissions(&live_usr, Permissions::from_mode(0o755)).unwrap();
    fs::write(live_usr.join("foreign"), b"untouched").unwrap();

    let error = client
        .prepare_stateful_tree_identity(&candidate_usr, candidate.id)
        .unwrap_err();
    assert!(
        matches!(&error, crate::transition_identity::Error::LiveUsrNotEmpty { .. }),
        "unexpected nonempty live /usr result: {error:#?}"
    );
    assert_eq!(fs::read(live_usr.join("foreign")).unwrap(), b"untouched");
    assert!(!live_usr.join(".cast-tree-id").exists());
    assert!(!candidate_usr.join(".cast-tree-id").exists());
}

#[test]
fn first_install_rejects_a_racing_nonempty_usr_occupant_unchanged() {
    let temporary = tempfile::tempdir().unwrap();
    let client = stateful_test_client(temporary.path());
    let candidate = client.state_db.add(&[], Some("first state"), None).unwrap();
    record_state_id(&client.installation.staging_dir(), candidate.id).unwrap();
    let candidate_usr = client.installation.staging_path("usr");
    let live_usr = client.installation.root.join("usr");
    let raced = live_usr.clone();
    crate::transition_identity::arm_before_live_usr_mkdir(move || {
        fs::create_dir(&raced).unwrap();
        fs::write(raced.join("foreign"), b"racing occupant").unwrap();
    });

    let error = client
        .prepare_stateful_tree_identity(&candidate_usr, candidate.id)
        .unwrap_err();
    assert!(matches!(
        error,
        crate::transition_identity::Error::LiveUsrAppeared { .. }
    ));
    assert_eq!(fs::read(live_usr.join("foreign")).unwrap(), b"racing occupant");
    assert!(!live_usr.join(".cast-tree-id").exists());
    assert!(!candidate_usr.join(".cast-tree-id").exists());
}

#[test]
fn duplicate_permanent_tree_tokens_block_exchange_and_retain_both_trees() {
    let fixture = stateful_transition_fixture(false);
    let candidate_usr = fixture.client.installation.staging_path("usr");
    let live_usr = fixture.client.installation.root.join("usr");
    let journal = crate::transition_journal::TransitionJournalStore::open(&fixture.client.installation.root).unwrap();
    assert!(journal.load().unwrap().is_none());
    let candidate_store = crate::tree_marker::TreeMarkerStore::open_path(&candidate_usr).unwrap();
    let candidate_marker = candidate_store.adopt_or_create_before_journal().unwrap();
    candidate_marker.revalidate(&candidate_store).unwrap();
    let frame = fs::read(candidate_usr.join(".cast-tree-id")).unwrap();
    fs::write(live_usr.join(".cast-tree-id"), &frame).unwrap();
    fs::set_permissions(live_usr.join(".cast-tree-id"), Permissions::from_mode(0o444)).unwrap();
    drop(candidate_marker);
    drop(candidate_store);
    drop(journal);

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

    let Error::StatefulTreeIdentityPreparationFailed { source, .. } = error else {
        panic!("expected durable identity preparation failure");
    };
    let Error::StatefulTreeIdentity { source } = *source else {
        panic!("expected tree identity source");
    };
    assert!(matches!(
        source.downcast_ref::<crate::transition_identity::Error>(),
        Some(crate::transition_identity::Error::DuplicateTreeToken { .. })
    ));
    assert_eq!(fs::read(candidate_usr.join(".cast-tree-id")).unwrap(), frame);
    assert_eq!(fs::read(live_usr.join(".cast-tree-id")).unwrap(), frame);
    assert_eq!(
        fs::read_to_string(candidate_usr.join(".stateID")).unwrap(),
        fixture.candidate.id.to_string()
    );
    assert_eq!(
        fs::read_to_string(live_usr.join(".stateID")).unwrap(),
        fixture.previous.id.to_string()
    );
    assert_eq!(
        fixture.client.state_db.get(fixture.candidate.id).unwrap().id,
        fixture.candidate.id
    );
}

#[test]
fn recovery_rejects_same_content_marker_name_substitution_without_repair() {
    let fixture = stateful_transition_fixture(false);
    let candidate_model = generated_system_snapshot("candidate-package");
    let live_usr = fixture.client.installation.root.join("usr");
    let marker_path = live_usr.join(".cast-tree-id");
    let mut replacement = None;

    let error = fixture
        .client
        .apply_stateful_blit_with_checkpoint(
            vfs(Vec::new()).unwrap(),
            &fixture.candidate,
            Some(fixture.previous.id),
            candidate_model,
            |checkpoint| {
                if checkpoint == StatefulTransitionCheckpoint::AfterUsrExchange {
                    let frame = fs::read(&marker_path).unwrap();
                    let original = fs::symlink_metadata(&marker_path).unwrap().ino();
                    fs::remove_file(&marker_path).unwrap();
                    fs::write(&marker_path, &frame).unwrap();
                    fs::set_permissions(&marker_path, Permissions::from_mode(0o444)).unwrap();
                    let substituted = fs::symlink_metadata(&marker_path).unwrap().ino();
                    assert_ne!(original, substituted);
                    replacement = Some((frame, substituted));
                    Err(injected_state_transition_error("same-content marker substitution"))
                } else {
                    Ok(())
                }
            },
        )
        .unwrap_err();

    assert!(matches!(
        error,
        Error::StatefulTransitionRecoveryFailed {
            reverse_exchange: Some(_),
            ..
        }
    ));
    let (frame, inode) = replacement.unwrap();
    assert_eq!(fs::read(&marker_path).unwrap(), frame);
    assert_eq!(fs::symlink_metadata(&marker_path).unwrap().ino(), inode);
    assert_eq!(
        fs::read_to_string(live_usr.join(".stateID")).unwrap(),
        fixture.candidate.id.to_string()
    );
    assert_eq!(
        fixture.client.state_db.get(fixture.candidate.id).unwrap().id,
        fixture.candidate.id
    );
}

#[test]
fn recovery_rejects_whole_directory_same_token_substitution_without_exchange() {
    let fixture = stateful_transition_fixture(false);
    let live_usr = fixture.client.installation.root.join("usr");
    let displaced = fixture.client.installation.root.join("displaced-candidate-usr");
    let mut replacement_identity = None;

    let error = fixture
        .client
        .apply_stateful_blit_with_checkpoint(
            vfs(Vec::new()).unwrap(),
            &fixture.candidate,
            Some(fixture.previous.id),
            generated_system_snapshot("candidate-package"),
            |checkpoint| {
                if checkpoint == StatefulTransitionCheckpoint::AfterUsrExchange {
                    let marker = fs::read(live_usr.join(".cast-tree-id")).unwrap();
                    let state_id = fs::read(live_usr.join(".stateID")).unwrap();
                    fs::rename(&live_usr, &displaced).unwrap();
                    fs::create_dir(&live_usr).unwrap();
                    fs::set_permissions(&live_usr, Permissions::from_mode(0o755)).unwrap();
                    fs::write(live_usr.join(".cast-tree-id"), marker).unwrap();
                    fs::set_permissions(live_usr.join(".cast-tree-id"), Permissions::from_mode(0o444)).unwrap();
                    fs::write(live_usr.join(".stateID"), state_id).unwrap();
                    replacement_identity = Some(fs::symlink_metadata(&live_usr).unwrap().ino());
                    Err(injected_state_transition_error(
                        "whole-directory same-token substitution",
                    ))
                } else {
                    Ok(())
                }
            },
        )
        .unwrap_err();

    assert!(matches!(
        error,
        Error::StatefulTransitionRecoveryFailed {
            reverse_exchange: Some(_),
            ..
        }
    ));
    assert_eq!(
        fs::symlink_metadata(&live_usr).unwrap().ino(),
        replacement_identity.unwrap(),
        "recovery must not exchange the substituted directory"
    );
    assert_eq!(recovery_tree_token(&live_usr), recovery_tree_token(&displaced));
    assert_eq!(
        fs::read_to_string(displaced.join(".stateID")).unwrap(),
        fixture.candidate.id.to_string()
    );
    assert_eq!(
        fs::read_to_string(fixture.client.installation.staging_path("usr/.stateID")).unwrap(),
        fixture.previous.id.to_string()
    );
}

#[test]
fn missing_live_usr_between_identity_check_and_exchange_is_never_recreated() {
    let fixture = stateful_transition_fixture(false);
    let live_usr = fixture.client.installation.root.join("usr");
    let displaced = fixture.client.installation.root.join("displaced-previous-usr");
    let staged_usr = fixture.client.installation.staging_path("usr");
    let mut expected_tokens = None;

    let error = fixture
        .client
        .apply_stateful_blit_with_checkpoint(
            vfs(Vec::new()).unwrap(),
            &fixture.candidate,
            Some(fixture.previous.id),
            generated_system_snapshot("candidate-package"),
            |checkpoint| {
                if checkpoint == StatefulTransitionCheckpoint::BeforeUsrExchange {
                    expected_tokens = Some((recovery_tree_token(&staged_usr), recovery_tree_token(&live_usr)));
                    fs::rename(&live_usr, &displaced).unwrap();
                }
                Ok(())
            },
        )
        .unwrap_err();

    assert!(matches!(&error, Error::StatefulCandidatePreserved { .. }), "{error:#?}");
    let (candidate_token, previous_token) = expected_tokens.unwrap();
    assert!(
        !live_usr.exists(),
        "promotion must not synthesize an unmarked exchange target"
    );
    assert_eq!(recovery_tree_token(&displaced), previous_token);
    let quarantines = fs::read_dir(fixture.client.installation.state_quarantine_dir())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    assert_eq!(quarantines.len(), 1);
    assert_eq!(recovery_tree_token(&quarantines[0].join("usr")), candidate_token);
}

#[test]
fn archived_live_root_abi_conflict_precedes_staging_triggers_and_usr_exchange() {
    let fixture = stateful_transition_fixture(true);
    let installation = &fixture.client.installation;
    let foreign = installation.root.join("bin");
    fs::write(&foreign, b"foreign live root entry").unwrap();
    let identity = root_abi_inode(&foreign);
    let live_usr = installation.root.join("usr");
    let archived_root = installation.root_path(fixture.candidate.id.to_string());
    let archived_usr = archived_root.join("usr");
    let staging = installation.staging_dir();
    let live_identity = root_abi_inode(&live_usr);
    let archive_identity = root_abi_inode(&archived_root);
    let archived_usr_identity = root_abi_inode(&archived_usr);
    let staging_identity = root_abi_inode(&staging);
    let states = fixture.client.state_db.all().unwrap();
    let mut checkpoints = Vec::new();
    assert!(take_observed_trigger_scopes().is_empty());

    let error = fixture
        .client
        .activate_state_with_checkpoint(fixture.candidate.id, false, true, |checkpoint| {
            checkpoints.push(checkpoint);
            Ok(())
        })
        .unwrap_err();
    assert!(matches!(
        error,
        Error::RootAbiLinkTypeConflict { path, .. } if path == foreign
    ));
    assert!(checkpoints.is_empty());
    assert!(take_observed_trigger_scopes().is_empty());
    assert_eq!(root_abi_inode(&foreign), identity);
    assert_eq!(fs::read(&foreign).unwrap(), b"foreign live root entry");
    assert_root_abi_absent(&installation.root.join("sbin"));
    assert_eq!(root_abi_inode(&live_usr), live_identity);
    assert_eq!(root_abi_inode(&archived_root), archive_identity);
    assert_eq!(root_abi_inode(&archived_usr), archived_usr_identity);
    assert_eq!(root_abi_inode(&staging), staging_identity);
    assert!(!installation.staging_path("usr").exists());
    assert!(!live_usr.join(".cast-tree-id").exists());
    assert!(!archived_usr.join(".cast-tree-id").exists());
    assert_eq!(fixture.client.state_db.all().unwrap(), states);
    assert_eq!(
        fs::read_to_string(live_usr.join(".stateID")).unwrap(),
        fixture.previous.id.to_string()
    );
    assert_eq!(
        fs::read_to_string(archived_usr.join(".stateID")).unwrap(),
        fixture.candidate.id.to_string()
    );
}

#[test]
fn isolation_root_abi_conflict_fails_before_usr_exchange_and_preserves_foreign_entry() {
    let fixture = stateful_transition_fixture(false);
    let foreign = fixture.client.installation.isolation_dir().join("bin");
    fs::write(&foreign, b"foreign isolation entry").unwrap();
    let identity = root_abi_inode(&foreign);
    let candidate_model = generated_system_snapshot("candidate-package");

    let error = fixture
        .client
        .apply_stateful_blit_with_checkpoint(
            vfs(Vec::new()).unwrap(),
            &fixture.candidate,
            Some(fixture.previous.id),
            candidate_model,
            |_| Ok(()),
        )
        .unwrap_err();
    assert!(matches!(
        error,
        Error::StatefulCandidatePreserved {
            primary,
            candidate,
            previous: Some(previous),
        } if candidate == fixture.candidate.id
            && previous == fixture.previous.id
            && matches!(
                primary.as_ref(),
                Error::RootAbiLinkTypeConflict { path, .. } if path == &foreign
            )
    ));
    assert_eq!(root_abi_inode(&foreign), identity);
    assert_eq!(fs::read(&foreign).unwrap(), b"foreign isolation entry");
    assert_root_abi_absent(&fixture.client.installation.isolation_dir().join("sbin"));
    assert_fresh_candidate_quarantined_and_invalidated(&fixture);
}

#[test]
fn ephemeral_root_and_isolation_root_abi_conflicts_are_both_non_destructive() {
    let root_temporary = tempfile::tempdir().unwrap();
    prepare_private_installation_root(root_temporary.path());
    let installation_root = root_temporary.path().join("installation");
    let blit_root = root_temporary.path().join("ephemeral");
    fs::create_dir(&installation_root).unwrap();
    let installation = test_installation(&installation_root);
    let client = Client::builder("root-abi-ephemeral-root-test", installation)
        .repositories(repository::Map::default())
        .ephemeral(&blit_root)
        .build()
        .unwrap();
    let candidate = client
        .materialize_ephemeral_candidate(std::iter::empty::<&package::Id>())
        .unwrap();

    let foreign = blit_root.join("bin");
    fs::write(&foreign, b"foreign ephemeral entry").unwrap();
    let identity = root_abi_inode(&foreign);
    let error = client
        .apply_ephemeral_candidate(candidate, generated_system_snapshot("ephemeral-package"))
        .unwrap_err();
    assert!(matches!(error, Error::RootAbiLinkTypeConflict { path, .. } if path == foreign));
    assert_eq!(root_abi_inode(&foreign), identity);
    assert_eq!(fs::read(&foreign).unwrap(), b"foreign ephemeral entry");
    assert!(!blit_root.join("usr/lib/os-release").exists());
    assert!(!blit_root.join("usr/lib/system-model.glu").exists());

    let isolation_temporary = tempfile::tempdir().unwrap();
    prepare_private_installation_root(isolation_temporary.path());
    let installation_root = isolation_temporary.path().join("installation");
    let blit_root = isolation_temporary.path().join("ephemeral");
    fs::create_dir(&installation_root).unwrap();
    let installation = test_installation(&installation_root);
    let client = Client::builder("root-abi-ephemeral-isolation-test", installation)
        .repositories(repository::Map::default())
        .ephemeral(&blit_root)
        .build()
        .unwrap();
    let candidate = client
        .materialize_ephemeral_candidate(std::iter::empty::<&package::Id>())
        .unwrap();
    let isolation_foreign = client.installation.isolation_dir().join("bin");
    fs::write(&isolation_foreign, b"foreign isolation entry").unwrap();
    let isolation_identity = root_abi_inode(&isolation_foreign);
    let error = client
        .apply_ephemeral_candidate(candidate, generated_system_snapshot("ephemeral-package"))
        .unwrap_err();
    assert!(matches!(
        error,
        Error::RootAbiLinkTypeConflict { path, .. } if path == isolation_foreign
    ));
    assert_eq!(root_abi_inode(&isolation_foreign), isolation_identity);
    assert_eq!(fs::read(&isolation_foreign).unwrap(), b"foreign isolation entry");
    assert_root_abi_links(&blit_root);
    assert!(!blit_root.join("usr/lib/os-release").exists());
    assert!(!blit_root.join("usr/lib/system-model.glu").exists());
}
