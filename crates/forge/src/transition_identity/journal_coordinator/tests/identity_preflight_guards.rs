// Ports of the identity-preparation guards from the legacy client route.
//
// These fire inside the shared `prepare_candidate`, before any journal record
// exists, so they are driven through `StatefulTreeIdentity` directly rather
// than through a coordinator. The legacy tests reached the same code through
// `apply_stateful_blit_with_checkpoint`; the disposition the client wrapped
// around the refusal is legacy-route detail, but the guard itself is shared.

/// Build an installation whose live and candidate trees carry one identical
/// marker frame, and return it alongside the paths both claims are read from.
fn installation_with_a_token_shared_by_both_trees() -> (
    tempfile::TempDir,
    Installation,
    db::state::Database,
    state::Id,
    state::Id,
    PathBuf,
    Vec<u8>,
) {
    let temporary = private_installation_tempdir();
    let mut installation = Installation::open(temporary.path(), None).unwrap();
    let database = db::state::Database::new(":memory:").unwrap();
    let previous_state = database.add(&[], Some("duplicate token previous"), None).unwrap().id;
    let candidate_state = add_cleared_state_with_provenance(&database, "duplicate token candidate", 'e');
    prepare_previous_tree(&installation, PreviousKind::Active, previous_state);
    installation.active_state = Some(previous_state);
    install_root_abi_subset(&installation.root, 0);

    let candidate_path = installation.staging_path("usr");
    create_canonical_directory(&candidate_path);
    write_canonical_file(&candidate_path.join(".stateID"), candidate_state.to_string().as_bytes());

    // Publish the candidate's own marker first, then plant a byte-identical
    // frame on the live tree. Copying an already-published frame is what makes
    // the two tokens equal without either marker looking forged.
    let candidate_store = TreeMarkerStore::open_path(&candidate_path).unwrap();
    let marker = candidate_store.adopt_or_create_before_journal().unwrap();
    marker.revalidate(&candidate_store).unwrap();
    let frame = fs::read(candidate_path.join(".cast-tree-id")).unwrap();
    drop(marker);
    drop(candidate_store);

    let live_marker = installation.root.join("usr/.cast-tree-id");
    fs::write(&live_marker, &frame).unwrap();
    fs::set_permissions(&live_marker, fs::Permissions::from_mode(0o444)).unwrap();

    (
        temporary,
        installation,
        database,
        previous_state,
        candidate_state,
        candidate_path,
        frame,
    )
}

#[test]
fn duplicate_permanent_tree_tokens_block_the_exchange_and_retain_both_trees() {
    let (_temporary, installation, database, previous_state, candidate_state, candidate_path, frame) =
        installation_with_a_token_shared_by_both_trees();

    let error = StatefulTreeIdentity::prepare(&installation, &database, &candidate_path, candidate_state)
        .err()
        .expect("a token claimed by both trees must refuse the transition");
    assert!(
        matches!(error, crate::transition_identity::Error::DuplicateTreeToken { .. }),
        "expected a duplicate-token refusal, got {error:?}"
    );

    // The refusal is the cheap half of the claim. The substance is that both
    // trees survive it intact: an ambiguous token identifies neither tree, so
    // it cannot license repairing, renaming, or re-marking either side.
    assert_eq!(fs::read(candidate_path.join(".cast-tree-id")).unwrap(), frame);
    assert_eq!(fs::read(installation.root.join("usr/.cast-tree-id")).unwrap(), frame);
    assert_eq!(
        fs::read_to_string(candidate_path.join(".stateID")).unwrap(),
        candidate_state.to_string()
    );
    assert_eq!(
        fs::read_to_string(installation.root.join("usr/.stateID")).unwrap(),
        previous_state.to_string()
    );
    assert_eq!(database.get(candidate_state).unwrap().id, candidate_state);
    assert_canonical_journal_absent(&installation.root);
}
