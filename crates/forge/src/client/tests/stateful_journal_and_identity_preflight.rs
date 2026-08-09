#[test]
fn candidate_pre_journal_namespace_substitution_preserves_detached_cast_and_replacement_before_markers() {
    let fixture = stateful_transition_fixture(false);
    let cast = fixture.client.installation.root.join(".cast");
    let detached = fixture
        .client
        .installation
        .root
        .join("detached-candidate-pre-journal-cast");
    let cast_identity = root_abi_inode(&cast);
    let candidate_usr = fixture.client.installation.staging_path("usr");
    let candidate_usr_identity = root_abi_inode(&candidate_usr);
    let detached_candidate_usr = detached.join("root/staging/usr");
    let live_usr = fixture.client.installation.root.join("usr");
    let live_usr_identity = root_abi_inode(&live_usr);
    let global_lock = cast.join(".cast-lockfile");
    let journal_lock = cast.join("journal/state-transition.lock");
    let global_lock_identity = root_abi_inode(&global_lock);
    let journal_lock_identity = root_abi_inode(&journal_lock);
    let database_identities = ["install", "state", "layout"].map(|name| {
        let path = cast.join("db").join(name);
        (name, root_abi_inode(&path))
    });

    assert!(!cast.join("journal/state-transition").exists());
    assert!(!candidate_usr.join(".cast-tree-id").exists());
    assert!(!live_usr.join(".cast-tree-id").exists());

    let replacement_marker = cast.join("foreign-replacement");
    let hook_cast = cast.clone();
    let hook_detached = detached.clone();
    let hook_marker = replacement_marker.clone();
    crate::transition_identity::arm_after_candidate_mutable_namespace_preflight(move || {
        fs::rename(&hook_cast, &hook_detached).unwrap();
        fs::create_dir(&hook_cast).unwrap();
        fs::set_permissions(&hook_cast, Permissions::from_mode(0o700)).unwrap();
        fs::write(&hook_marker, b"replacement must remain untouched").unwrap();
    });

    let error = fixture
        .client
        .prepare_stateful_tree_identity(&candidate_usr, fixture.candidate.id)
        .unwrap_err();
    assert!(matches!(
        error,
        crate::transition_identity::Error::Installation(installation::Error::PrepareDirectory {
            path,
            ..
        }) if path == cast
    ));

    let mut replacement_names = fs::read_dir(&cast)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    replacement_names.sort();
    assert_eq!(replacement_names, [OsString::from("foreign-replacement")]);
    assert_ne!(root_abi_inode(&cast), cast_identity);
    assert_eq!(root_abi_inode(&detached), cast_identity);
    assert_eq!(fs::read(&replacement_marker).unwrap(), b"replacement must remain untouched");
    assert_eq!(fs::metadata(&cast).unwrap().permissions().mode() & 0o7777, 0o700);
    assert!(!cast.join(".cast-lockfile").exists());
    assert!(!cast.join("db").exists());
    assert!(!cast.join("journal").exists());

    assert_eq!(root_abi_inode(&detached.join(".cast-lockfile")), global_lock_identity);
    assert_eq!(
        root_abi_inode(&detached.join("journal/state-transition.lock")),
        journal_lock_identity
    );
    assert!(!detached.join("journal/state-transition").exists());
    for (name, identity) in database_identities {
        assert_eq!(root_abi_inode(&detached.join("db").join(name)), identity);
    }

    assert!(!candidate_usr.exists());
    assert_eq!(root_abi_inode(&detached_candidate_usr), candidate_usr_identity);
    assert!(!detached_candidate_usr.join(".cast-tree-id").exists());
    assert_eq!(root_abi_inode(&live_usr), live_usr_identity);
    assert!(!live_usr.join(".cast-tree-id").exists());
    assert_eq!(
        fs::read_to_string(detached_candidate_usr.join(".stateID")).unwrap(),
        fixture.candidate.id.to_string()
    );
    assert_eq!(
        fs::read_to_string(live_usr.join(".stateID")).unwrap(),
        fixture.previous.id.to_string()
    );
    assert_eq!(fixture.client.state_db.get(fixture.candidate.id).unwrap().id, fixture.candidate.id);
    assert_eq!(fixture.client.state_db.get(fixture.previous.id).unwrap().id, fixture.previous.id);
}

#[test]
fn candidate_pre_journal_legacy_hardlinked_archived_payload_fails_before_marker_or_exchange() {
    let fixture = stateful_transition_fixture(true);
    let installation = &fixture.client.installation;
    let archived_usr = installation.root_path(fixture.candidate.id.to_string()).join("usr");
    let live_usr = installation.root.join("usr");
    let payload = archived_usr.join("lib/system-model.glu");
    let external = installation.root.join("legacy-archived-payload-hardlink");
    let archived_usr_identity = root_abi_inode(&archived_usr);
    let live_usr_identity = root_abi_inode(&live_usr);
    let payload_identity = root_abi_inode(&payload);
    let payload_bytes = fs::read(&payload).unwrap();
    let candidate_row = fixture.client.state_db.get(fixture.candidate.id).unwrap();
    let previous_row = fixture.client.state_db.get(fixture.previous.id).unwrap();
    assert_eq!(fs::metadata(&payload).unwrap().nlink(), 1);

    fs::hard_link(&payload, &external).unwrap();
    assert_eq!(root_abi_inode(&external), payload_identity);
    assert_eq!(fs::metadata(&payload).unwrap().nlink(), 2);

    let error = fixture
        .client
        .activate_state(fixture.candidate.id, true, true)
        .unwrap_err();
    // The coordinated route reports preparation failures through its own
    // wrapper; the property under test is unchanged — the hardlinked payload is
    // refused at identity preparation, before any marker or exchange.
    let Error::CoordinatedNewState(boxed) = error else {
        panic!("expected coordinated archived activation failure: {error:?}");
    };
    let failure = boxed
        .downcast_ref::<crate::client::new_state_boot_transition::LiveNewStateBootError>()
        .unwrap_or_else(|| panic!("expected live new-state boot error, got {boxed:#?}"));
    assert_eq!(
        failure.stage(),
        "archived candidate identity",
        "hardlink refusal must happen at identity preparation, not later",
    );
    let authority_source = failure
        .source_ref()
        .downcast_ref::<crate::client::JournalUsrExchangeAuthorityError>()
        .unwrap_or_else(|| panic!("expected exchange-authority source, got {failure:#?}"));
    let crate::client::JournalUsrExchangeAuthorityError::Identity(identity_source) = authority_source else {
        panic!("expected identity source, got {authority_source:#?}");
    };
    let crate::transition_identity::Error::CandidateInventory(inventory_source) = identity_source else {
        panic!("expected candidate inventory source, got {identity_source:#?}");
    };
    let crate::transition_identity::CandidateInventoryError::UnexpectedHardlink { path, links } = inventory_source
    else {
        panic!("expected unexpected-hardlink source, got {inventory_source:#?}");
    };
    assert_eq!(path, &payload);
    assert_eq!(*links, 2);

    assert_eq!(root_abi_inode(&archived_usr), archived_usr_identity);
    assert_eq!(root_abi_inode(&live_usr), live_usr_identity);
    assert!(!installation.staging_path("usr").exists());
    assert!(!archived_usr.join(".cast-tree-id").exists());
    assert!(!live_usr.join(".cast-tree-id").exists());
    let journal = crate::transition_journal::TransitionJournalStore::open(&installation.root).unwrap();
    assert!(journal.load().unwrap().is_none());
    assert!(!installation.root.join(".cast/journal/state-transition").exists());

    assert_eq!(root_abi_inode(&payload), payload_identity);
    assert_eq!(root_abi_inode(&external), payload_identity);
    assert_eq!(fs::metadata(&payload).unwrap().nlink(), 2);
    assert_eq!(fs::metadata(&external).unwrap().nlink(), 2);
    assert_eq!(fs::read(&payload).unwrap(), payload_bytes);
    assert_eq!(fs::read(&external).unwrap(), payload_bytes);
    assert_eq!(
        fs::read_to_string(archived_usr.join(".stateID")).unwrap(),
        fixture.candidate.id.to_string()
    );
    assert_eq!(
        fs::read_to_string(live_usr.join(".stateID")).unwrap(),
        fixture.previous.id.to_string()
    );
    assert_eq!(fixture.client.state_db.get(fixture.candidate.id).unwrap(), candidate_row);
    assert_eq!(fixture.client.state_db.get(fixture.previous.id).unwrap(), previous_row);
    assert_eq!(fixture.client.installation.active_state, Some(fixture.previous.id));
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
    assert!(take_observed_trigger_scopes().is_empty());

    let error = fixture
        .client
        .activate_state_inner(fixture.candidate.id, false, true)
        .unwrap_err();
    assert!(matches!(
        error,
        Error::RootAbiLinkTypeConflict { path, .. } if path == foreign
    ));
    // The "no checkpoint fired" assertion that used to sit here was vacuous:
    // the callback was never invoked and no `StatefulTransitionCheckpoint`
    // variant was ever constructed. The trigger-scope assertion below carries
    // the real claim — the conflict is detected before any trigger runs.
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
