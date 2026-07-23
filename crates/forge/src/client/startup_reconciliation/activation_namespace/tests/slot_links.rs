use super::*;

pub(super) fn run() {
    exact_transition_slots_are_bound();
    archived_candidate_parking_is_role_and_token_exact();
    active_reblit_slot_location_tracks_the_reservation_phase();
    active_reblit_never_accepts_previous_parking();
    active_reblit_parking_wrapper_inventory_is_exact();
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
    let mut reblit = Fixture::active_reblit();
    reblit.record.phase = Phase::CandidatePrepared;
    let parking = archived_candidate_parking(&reblit, 42, reblit.record.previous.tree_token.as_str());
    link_slot(
        &reblit.installation.root.join("usr/.cast-tree-id"),
        &parking,
        42,
        reblit.record.previous.tree_token.as_str(),
    );
    install_active_reblit_reservation(&reblit);
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

fn active_reblit_slot_location_tracks_the_reservation_phase() {
    let mut early_single_link = Fixture::active_reblit();
    early_single_link.record.phase = Phase::CandidatePrepareStarted;
    assert_eq!(early_single_link.assess(), Ok(()));

    let mut early_canonical = Fixture::active_reblit();
    early_canonical.record.phase = Phase::CandidatePrepareStarted;
    link_active_reblit_canonical_slot(&early_canonical);
    assert_eq!(early_canonical.assess(), Ok(()));

    let mut early_parked = Fixture::active_reblit();
    early_parked.record.phase = Phase::CandidatePrepareStarted;
    link_active_reblit_parked_slot(&early_parked, 0);
    assert_eq!(
        early_parked.assess(),
        Err(NamespacePolicyConflict::ActiveReblitPreviousSlot)
    );

    let mut prepared_canonical = Fixture::active_reblit();
    prepared_canonical.record.phase = Phase::CandidatePrepared;
    link_active_reblit_canonical_slot(&prepared_canonical);
    assert_eq!(prepared_canonical.assess(), Ok(()));

    let mut prepared_canonical_with_replacement = Fixture::active_reblit();
    prepared_canonical_with_replacement.record.phase = Phase::CandidatePrepared;
    link_active_reblit_canonical_slot(&prepared_canonical_with_replacement);
    install_active_reblit_reservation(&prepared_canonical_with_replacement);
    assert_eq!(prepared_canonical_with_replacement.assess(), Ok(()));

    let mut prepared_parked_without_replacement = Fixture::active_reblit();
    prepared_parked_without_replacement.record.phase = Phase::CandidatePrepared;
    link_active_reblit_parked_slot(&prepared_parked_without_replacement, 0);
    assert_eq!(
        prepared_parked_without_replacement.assess(),
        Err(NamespacePolicyConflict::ActiveReblitPreviousSlot)
    );

    let mut prepared_parked = Fixture::active_reblit();
    prepared_parked.record.phase = Phase::CandidatePrepared;
    link_active_reblit_parked_slot(&prepared_parked, 0);
    install_active_reblit_reservation(&prepared_parked);
    assert_eq!(prepared_parked.assess(), Ok(()));

    let mut prepared_single_link = Fixture::active_reblit();
    prepared_single_link.record.phase = Phase::CandidatePrepared;
    assert_eq!(prepared_single_link.assess(), Ok(()));

    let mut started_canonical = Fixture::active_reblit();
    started_canonical.record.phase = Phase::TransactionTriggersStarted;
    link_active_reblit_canonical_slot(&started_canonical);
    install_active_reblit_reservation(&started_canonical);
    install_root_abi(&started_canonical);
    assert_eq!(
        started_canonical.assess(),
        Err(NamespacePolicyConflict::ActiveReblitPreviousSlot)
    );

    let mut started_parked = Fixture::active_reblit();
    started_parked.record.phase = Phase::TransactionTriggersStarted;
    link_active_reblit_parked_slot(&started_parked, 0);
    install_active_reblit_reservation(&started_parked);
    install_root_abi(&started_parked);
    assert_eq!(started_parked.assess(), Ok(()));

    let mut started_single_link = Fixture::active_reblit();
    started_single_link.record.phase = Phase::TransactionTriggersStarted;
    install_active_reblit_reservation(&started_single_link);
    install_root_abi(&started_single_link);
    assert_eq!(started_single_link.assess(), Ok(()));

    let mut rollback_canonical = Fixture::active_reblit();
    rollback_canonical.record.phase = Phase::RollbackDecided;
    rollback_canonical.record.rollback = Some(rollback_plan(
        ForwardPhase::TransactionTriggersStarted,
        RollbackAction::NotRequired,
        RollbackAction::NotRequired,
        RollbackAction::Pending,
    ));
    link_active_reblit_canonical_slot(&rollback_canonical);
    install_active_reblit_reservation(&rollback_canonical);
    install_root_abi(&rollback_canonical);
    assert_eq!(
        rollback_canonical.assess(),
        Err(NamespacePolicyConflict::ActiveReblitPreviousSlot)
    );

    let mut rollback_parked = Fixture::active_reblit();
    rollback_parked.record.phase = Phase::RollbackDecided;
    rollback_parked.record.rollback = Some(rollback_plan(
        ForwardPhase::TransactionTriggersStarted,
        RollbackAction::NotRequired,
        RollbackAction::NotRequired,
        RollbackAction::Pending,
    ));
    link_active_reblit_parked_slot(&rollback_parked, 0);
    install_active_reblit_reservation(&rollback_parked);
    install_root_abi(&rollback_parked);
    assert_eq!(rollback_parked.assess(), Ok(()));

    let mut prepared_rollback_without_replacement = Fixture::active_reblit();
    prepared_rollback_without_replacement.record.phase = Phase::RollbackDecided;
    prepared_rollback_without_replacement.record.rollback = Some(rollback_plan(
        ForwardPhase::CandidatePrepared,
        RollbackAction::NotRequired,
        RollbackAction::NotRequired,
        RollbackAction::Pending,
    ));
    link_active_reblit_parked_slot(&prepared_rollback_without_replacement, 0);
    assert_eq!(
        prepared_rollback_without_replacement.assess(),
        Err(NamespacePolicyConflict::ActiveReblitPreviousSlot)
    );

    let mut prepared_rollback_with_replacement = Fixture::active_reblit();
    prepared_rollback_with_replacement.record.phase = Phase::RollbackDecided;
    prepared_rollback_with_replacement.record.rollback = Some(rollback_plan(
        ForwardPhase::CandidatePrepared,
        RollbackAction::NotRequired,
        RollbackAction::NotRequired,
        RollbackAction::Pending,
    ));
    link_active_reblit_parked_slot(&prepared_rollback_with_replacement, 0);
    install_active_reblit_reservation(&prepared_rollback_with_replacement);
    assert_eq!(prepared_rollback_with_replacement.assess(), Ok(()));
}

fn active_reblit_never_accepts_previous_parking() {
    let mut fixture = Fixture::active_reblit();
    fixture.record.phase = Phase::CandidatePrepared;
    let state = fixture.record.previous.id.unwrap();
    let token = fixture.record.previous.tree_token.as_str();
    let parking = fixture
        .installation
        .root_path(format!(".previous-slot-{state}-{token}-0"));
    create_private_directory(&parking);
    link_slot(
        &fixture.installation.root.join("usr/.cast-tree-id"),
        &parking,
        state,
        token,
    );
    assert!(matches!(
        fixture.snapshot(),
        Err(CaptureError::SlotWrongTransitionState { .. })
    ));
}

fn active_reblit_parking_wrapper_inventory_is_exact() {
    let mut empty = Fixture::active_reblit();
    empty.record.phase = Phase::CandidatePrepared;
    archived_candidate_parking_at(&empty, 42, empty.record.previous.tree_token.as_str(), 0);
    assert_eq!(empty.assess(), Err(NamespacePolicyConflict::ActiveReblitPreviousSlot));

    let mut extra = Fixture::active_reblit();
    extra.record.phase = Phase::CandidatePrepared;
    link_active_reblit_parked_slot(&extra, 0);
    archived_candidate_parking_at(&extra, 42, extra.record.previous.tree_token.as_str(), 1);
    assert_eq!(extra.assess(), Err(NamespacePolicyConflict::ActiveReblitPreviousSlot));

    let mut exhausted = Fixture::active_reblit();
    exhausted.record.phase = Phase::CandidatePrepared;
    link_active_reblit_canonical_slot(&exhausted);
    for index in 0..256 {
        archived_candidate_parking_at(&exhausted, 42, exhausted.record.previous.tree_token.as_str(), index);
    }
    assert_eq!(
        exhausted.assess(),
        Err(NamespacePolicyConflict::ActiveReblitPreviousSlot)
    );

    let mut previous_empty = Fixture::active_reblit();
    previous_empty.record.phase = Phase::CandidatePrepared;
    let state = previous_empty.record.previous.id.unwrap();
    let token = previous_empty.record.previous.tree_token.as_str();
    create_private_directory(
        &previous_empty
            .installation
            .root_path(format!(".previous-slot-{state}-{token}-0")),
    );
    assert_eq!(
        previous_empty.assess(),
        Err(NamespacePolicyConflict::ActiveReblitPreviousSlot)
    );
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
    archived_candidate_parking_at(fixture, state, token, 0)
}

fn archived_candidate_parking_at(fixture: &Fixture, state: i32, token: &str, index: usize) -> PathBuf {
    let parking = fixture
        .installation
        .root_path(format!(".archived-candidate-slot-{state}-{token}-{index}"));
    create_private_directory(&parking);
    parking
}

fn link_active_reblit_canonical_slot(fixture: &Fixture) {
    let state = fixture.record.previous.id.unwrap();
    let token = fixture.record.previous.tree_token.as_str();
    let wrapper = fixture.installation.root_path(state.to_string());
    create_private_directory(&wrapper);
    link_slot(
        &fixture.installation.root.join("usr/.cast-tree-id"),
        &wrapper,
        state,
        token,
    );
}

fn link_active_reblit_parked_slot(fixture: &Fixture, index: usize) {
    let state = fixture.record.previous.id.unwrap();
    let token = fixture.record.previous.tree_token.as_str();
    let wrapper = archived_candidate_parking_at(fixture, state, token, index);
    link_slot(
        &fixture.installation.root.join("usr/.cast-tree-id"),
        &wrapper,
        state,
        token,
    );
}

fn install_active_reblit_reservation(fixture: &Fixture) {
    create_private_directory(&active_reblit_wrapper_path(&fixture.installation, &fixture.record));
}

fn link_slot(marker: &Path, wrapper: &Path, state: i32, token: &str) {
    fs::hard_link(marker, wrapper.join(format!(".cast-state-slot-{state}-{token}"))).unwrap();
}
