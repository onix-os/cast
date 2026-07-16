use super::*;

pub(super) fn run() {
    exact_transition_slots_are_bound();
    archived_candidate_parking_is_role_and_token_exact();
    ambient_state_slots_are_read_only_and_exact();
    exact_inventory_supersedes_the_legacy_two_link_blocker();
}

fn exact_transition_slots_are_bound() {
    let exact = Fixture::new_state(Some(41), PreviousOrigin::ActiveState);
    let wrapper = exact.installation.root_path("41");
    create_private_directory(&wrapper);
    fs::hard_link(
        exact.installation.root.join("usr/.cast-tree-id"),
        wrapper.join(format!(
            ".cast-state-slot-41-{}",
            exact.record.previous.tree_token.as_str()
        )),
    )
    .unwrap();
    assert_eq!(exact.assess(), Ok(()));

    let wrong_state = Fixture::new_state(Some(41), PreviousOrigin::ActiveState);
    let wrapper = wrong_state.installation.root_path("42");
    create_private_directory(&wrapper);
    fs::hard_link(
        wrong_state.installation.root.join("usr/.cast-tree-id"),
        wrapper.join(format!(
            ".cast-state-slot-42-{}",
            wrong_state.record.previous.tree_token.as_str()
        )),
    )
    .unwrap();
    assert!(matches!(
        wrong_state.snapshot(),
        Err(CaptureError::SlotWrongTransitionState {
            actual: 42,
            expected: Some(41),
            ..
        })
    ));
}

fn archived_candidate_parking_is_role_and_token_exact() {
    let reblit = Fixture::active_reblit();
    let parking = archived_candidate_parking(&reblit, 42, reblit.record.previous.tree_token.as_str());
    link_slot(
        &reblit.installation.root.join("usr/.cast-tree-id"),
        &parking,
        42,
        reblit.record.previous.tree_token.as_str(),
    );
    assert_eq!(reblit.assess(), Ok(()));

    let previous = Fixture::new_state(Some(41), PreviousOrigin::ActiveState);
    let parking = archived_candidate_parking(&previous, 41, previous.record.previous.tree_token.as_str());
    link_slot(
        &previous.installation.root.join("usr/.cast-tree-id"),
        &parking,
        41,
        previous.record.previous.tree_token.as_str(),
    );
    assert_eq!(previous.assess(), Ok(()));

    let wrong_state = Fixture::new_state(Some(41), PreviousOrigin::ActiveState);
    let parking = archived_candidate_parking(&wrong_state, 42, wrong_state.record.previous.tree_token.as_str());
    link_slot(
        &wrong_state.installation.root.join("usr/.cast-tree-id"),
        &parking,
        42,
        wrong_state.record.previous.tree_token.as_str(),
    );
    assert!(matches!(
        wrong_state.snapshot(),
        Err(CaptureError::SlotWrongTransitionState {
            actual: 42,
            expected: Some(41),
            ..
        })
    ));

    let wrong_token = Fixture::new_state(Some(41), PreviousOrigin::ActiveState);
    let parking = archived_candidate_parking(&wrong_token, 41, wrong_token.record.candidate.tree_token.as_str());
    link_slot(
        &wrong_token.installation.root.join("usr/.cast-tree-id"),
        &parking,
        41,
        wrong_token.record.previous.tree_token.as_str(),
    );
    assert!(matches!(
        wrong_token.snapshot(),
        Err(CaptureError::SlotWrongTransitionState {
            actual: 41,
            expected: Some(41),
            ..
        })
    ));

    let wrong_candidate_operation = Fixture::new_state(Some(41), PreviousOrigin::ActiveState);
    let parking = archived_candidate_parking(
        &wrong_candidate_operation,
        42,
        wrong_candidate_operation.record.candidate.tree_token.as_str(),
    );
    link_slot(
        &wrong_candidate_operation.installation.staging_path("usr/.cast-tree-id"),
        &parking,
        42,
        wrong_candidate_operation.record.candidate.tree_token.as_str(),
    );
    let mut wrong_candidate_operation = wrong_candidate_operation;
    wrong_candidate_operation.record.candidate.id = Some(42);
    assert!(matches!(
        wrong_candidate_operation.snapshot(),
        Err(CaptureError::SlotWrongTransitionState {
            actual: 42,
            expected: Some(42),
            ..
        })
    ));
}

fn ambient_state_slots_are_read_only_and_exact() {
    let ambient = Fixture::new_state(Some(41), PreviousOrigin::ActiveState);
    let ambient_wrapper = ambient.installation.root_path("43");
    create_private_directory(&ambient_wrapper);
    let (ambient_token, _) = create_marked_tree(&ambient_wrapper.join("usr"));
    write_state_id(&ambient_wrapper.join("usr"), b"43");
    link_slot(
        &ambient_wrapper.join("usr/.cast-tree-id"),
        &ambient_wrapper,
        43,
        ambient_token.as_str(),
    );
    assert_eq!(ambient.assess(), Ok(()));

    let wrong_state = Fixture::new_state(Some(41), PreviousOrigin::ActiveState);
    let wrapper = wrong_state.installation.root_path("43");
    create_private_directory(&wrapper);
    let (token, _) = create_marked_tree(&wrapper.join("usr"));
    write_state_id(&wrapper.join("usr"), b"43");
    link_slot(&wrapper.join("usr/.cast-tree-id"), &wrapper, 44, token.as_str());
    assert!(matches!(
        wrong_state.snapshot(),
        Err(CaptureError::SlotWrongWrapper { .. })
    ));

    let wrong_token = Fixture::new_state(Some(41), PreviousOrigin::ActiveState);
    let wrapper = wrong_token.installation.root_path("43");
    create_private_directory(&wrapper);
    create_marked_tree(&wrapper.join("usr"));
    write_state_id(&wrapper.join("usr"), b"43");
    let foreign_token = wrong_token.record.candidate.tree_token.as_str();
    link_slot(&wrapper.join("usr/.cast-tree-id"), &wrapper, 43, foreign_token);
    assert!(matches!(
        wrong_token.snapshot(),
        Err(CaptureError::SlotTokenMismatch { .. })
    ));

    let wrong_location = Fixture::new_state(Some(41), PreviousOrigin::ActiveState);
    let wrapper = wrong_location.installation.root_path("43");
    create_private_directory(&wrapper);
    let (token, _) = create_marked_tree(&wrapper.join("usr"));
    write_state_id(&wrapper.join("usr"), b"43");
    let parking = archived_candidate_parking(&wrong_location, 43, wrong_location.record.candidate.tree_token.as_str());
    link_slot(&wrapper.join("usr/.cast-tree-id"), &parking, 43, token.as_str());
    assert!(matches!(
        wrong_location.snapshot(),
        Err(CaptureError::SlotWrongTransitionState {
            actual: 43,
            expected: Some(43),
            ..
        })
    ));
}

fn exact_inventory_supersedes_the_legacy_two_link_blocker() {
    // The legacy fixed-name reader deliberately leaves nlink=2 unresolved,
    // but an exact bounded inventory supersedes that limitation. A rejected
    // inventory must retain the legacy blocker.
    for exact_inventory in [true, false] {
        let database = db::state::Database::new(":memory:").unwrap();
        let previous = database.add(&[], Some("slot previous"), None).unwrap();
        let state = i32::from(previous.id);
        let fixture = Fixture::new_state(Some(state), PreviousOrigin::ActiveState);
        let slot_state = if exact_inventory { state } else { state + 1 };
        let wrapper = fixture.installation.root_path(slot_state.to_string());
        create_private_directory(&wrapper);
        link_slot(
            &fixture.installation.root.join("usr/.cast-tree-id"),
            &wrapper,
            slot_state,
            fixture.record.previous.tree_token.as_str(),
        );
        let journal =
            TransitionJournalStore::open_retained(fixture.installation.root_directory(), &fixture.installation.root)
                .unwrap();
        journal.create(&fixture.record).unwrap();
        let pending =
            PendingSystemTransition::inspect(&fixture.installation, &database, journal, fixture.record.clone(), None)
                .unwrap();
        assert_eq!(
            pending.blockers().contains(&RecoveryBlocker::UnresolvedStateSlotLink),
            !exact_inventory
        );
        assert_eq!(
            pending
                .blockers()
                .contains(&RecoveryBlocker::ActivationNamespaceRejected),
            !exact_inventory
        );
    }
}

fn archived_candidate_parking(fixture: &Fixture, state: i32, token: &str) -> PathBuf {
    let parking = fixture
        .installation
        .root_path(format!(".archived-candidate-slot-{state}-{token}-0"));
    create_private_directory(&parking);
    parking
}

fn link_slot(marker: &Path, wrapper: &Path, state: i32, token: &str) {
    fs::hard_link(marker, wrapper.join(format!(".cast-state-slot-{state}-{token}"))).unwrap();
}
