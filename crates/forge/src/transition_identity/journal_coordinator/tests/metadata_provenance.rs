fn expected_metadata_provenance() -> db::state::MetadataProvenance {
    db::state::MetadataProvenance::from_outputs(COORDINATOR_OS_RELEASE, COORDINATOR_SYSTEM_SNAPSHOT)
}

fn assert_exact_metadata_provenance(fixture: &CoordinatorFixture) {
    assert_eq!(
        fixture
            .database
            .required_metadata_provenance(fixture.candidate_state)
            .unwrap(),
        expected_metadata_provenance()
    );
}

fn assert_generated_metadata_name_absent(fixture: &CoordinatorFixture, name: &str) {
    assert_state_metadata_name_absent(&fixture.candidate_path.join("lib").join(name));
}

#[test]
fn journal_coordinator_new_state_provenance_commit_faults_precede_every_canonical_output() {
    for point in [
        db::state::MetadataProvenanceFaultPoint::BeforeCommit,
        db::state::MetadataProvenanceFaultPoint::AfterCommit,
    ] {
        let (fixture, coordinator) = coordinator_at_candidate_prepare_started(CandidateKind::NewState);
        let started = coordinator.record().clone();
        db::state::arm_metadata_provenance_fault(point);

        let failure = finish_candidate_prepare(coordinator).unwrap_err();
        db::state::assert_metadata_provenance_fault_consumed();
        let expected_outcome = match point {
            db::state::MetadataProvenanceFaultPoint::BeforeCommit => {
                db::state::MetadataProvenancePersistenceOutcome::DefinitelyNotApplied
            }
            db::state::MetadataProvenanceFaultPoint::AfterCommit => {
                db::state::MetadataProvenancePersistenceOutcome::AppliedButReportedError
            }
        };
        assert!(matches!(
            &failure,
            StatefulTransitionCoordinatorError::MetadataProvenance(
                db::state::MetadataProvenanceError::FaultInjected {
                    point: actual,
                    outcome,
                }
            ) if *actual == point
                && *outcome == expected_outcome
        ));
        assert_eq!(reopen_record(&fixture.installation.root), started);
        assert_eq!(
            fixture
                .database
                .transition_ownership(fixture.candidate_state, &started.transition_id)
                .unwrap(),
            TransitionOwnership::Matching
        );
        assert_candidate_state_id_absent(&fixture);
        assert_generated_metadata_name_absent(&fixture, "os-release");
        assert_generated_metadata_name_absent(&fixture, "system-model.glu");
        match point {
            db::state::MetadataProvenanceFaultPoint::BeforeCommit => {
                assert_eq!(
                    fixture
                        .database
                        .metadata_provenance(fixture.candidate_state)
                        .unwrap(),
                    None
                );
            }
            db::state::MetadataProvenanceFaultPoint::AfterCommit => {
                assert_exact_metadata_provenance(&fixture);
            }
        }
    }
}

#[test]
fn journal_coordinator_first_and_second_metadata_publication_faults_retain_provenance() {
    for (name, foreign) in [
        ("os-release", b"foreign first output\n".as_slice()),
        ("system-model.glu", b"foreign second output\n".as_slice()),
    ] {
        let (fixture, coordinator) = coordinator_at_candidate_prepare_started(CandidateKind::NewState);
        let started = coordinator.record().clone();
        let collision = fixture.candidate_path.join("lib").join(name);
        crate::transition_identity::arm_before_candidate_metadata_publication(name, move || {
            write_canonical_file(&collision, foreign);
        });

        let failure = finish_candidate_prepare(coordinator).unwrap_err();
        assert!(matches!(
            failure,
            StatefulTransitionCoordinatorError::CandidateMetadata(_)
        ));
        assert_eq!(reopen_record(&fixture.installation.root), started);
        assert_exact_metadata_provenance(&fixture);
        assert_candidate_state_id_absent(&fixture);
        assert_eq!(fs::read(fixture.candidate_path.join("lib").join(name)).unwrap(), foreign);
        if name == "os-release" {
            assert_generated_metadata_name_absent(&fixture, "system-model.glu");
        } else {
            assert_eq!(
                fs::read(fixture.candidate_path.join("lib/os-release")).unwrap(),
                COORDINATOR_OS_RELEASE
            );
        }
    }
}

#[test]
fn journal_coordinator_candidate_prepared_journal_faults_retain_complete_provenance_evidence() {
    for successor_applied in [false, true] {
        let (fixture, coordinator) = coordinator_at_candidate_prepare_started(CandidateKind::NewState);
        let started = coordinator.record().clone();
        if successor_applied {
            crate::transition_journal::arm_next_update_first_directory_sync_fault();
        } else {
            crate::transition_journal::arm_next_temporary_sync_fault();
        }

        assert!(matches!(
            finish_candidate_prepare(coordinator),
            Err(StatefulTransitionCoordinatorError::Journal(_))
        ));
        if successor_applied {
            crate::transition_journal::assert_update_first_directory_sync_fault_consumed();
        } else {
            crate::transition_journal::assert_temporary_sync_fault_consumed();
        }

        let reopened = reopen_record(&fixture.installation.root);
        if successor_applied {
            assert_record_prefix(&reopened, Operation::NewState, Phase::CandidatePrepared, 5);
        } else {
            assert_eq!(reopened, started);
        }
        assert_exact_metadata_provenance(&fixture);
        assert_candidate_metadata(&fixture);
        assert_candidate_state_id(&fixture, fixture.candidate_state);
        assert_eq!(
            fixture
                .database
                .transition_ownership(fixture.candidate_state, &started.transition_id)
                .unwrap(),
            TransitionOwnership::Matching
        );
    }
}

#[test]
fn journal_coordinator_existing_candidates_require_exact_nonlegacy_provenance_before_publication() {
    for candidate_kind in [CandidateKind::Archived, CandidateKind::ActiveReblit] {
        let (fixture, coordinator) = coordinator_at_candidate_prepare_started(candidate_kind);
        let started = coordinator.record().clone();
        let archived_before =
            (candidate_kind == CandidateKind::Archived).then(|| candidate_metadata_evidence(&fixture));
        fixture
            .database
            .delete_metadata_provenance_for_test(fixture.candidate_state)
            .unwrap();

        let missing = finish_candidate_prepare(coordinator).unwrap_err();
        assert!(matches!(
            missing,
            StatefulTransitionCoordinatorError::MetadataProvenance(
                db::state::MetadataProvenanceError::Missing { state_id }
            ) if state_id == i32::from(fixture.candidate_state)
        ));
        assert_eq!(reopen_record(&fixture.installation.root), started);
        if let Some(before) = archived_before {
            assert_eq!(candidate_metadata_evidence(&fixture), before);
        } else {
            assert!(!fixture.candidate_path.join("lib").exists());
            assert_generated_metadata_name_absent(&fixture, "os-release");
            assert_generated_metadata_name_absent(&fixture, "system-model.glu");
            assert_candidate_state_id_absent(&fixture);
        }

        let (fixture, coordinator) = coordinator_at_candidate_prepare_started(candidate_kind);
        let started = coordinator.record().clone();
        let mismatch = coordinator
            .finish_candidate_prepare(|_| {
                crate::transition_identity::CandidateMetadataOutputs::from_policy(
                    COORDINATOR_OS_RELEASE,
                    b"different policy-derived system model\n".as_slice(),
                )
            })
            .unwrap_err();
        assert!(matches!(
            mismatch,
            StatefulTransitionCoordinatorError::MetadataProvenance(
                db::state::MetadataProvenanceError::Mismatch { state_id }
            ) if state_id == i32::from(fixture.candidate_state)
        ));
        assert_eq!(reopen_record(&fixture.installation.root), started);
        if candidate_kind == CandidateKind::ActiveReblit {
            assert_generated_metadata_name_absent(&fixture, "os-release");
            assert_generated_metadata_name_absent(&fixture, "system-model.glu");
            assert_candidate_state_id_absent(&fixture);
        }
    }
}

#[test]
fn journal_coordinator_archived_verification_sandwich_detects_provenance_removal_without_mutation() {
    let (fixture, coordinator) = coordinator_at_candidate_prepare_started(CandidateKind::Archived);
    let started = coordinator.record().clone();
    let before = candidate_metadata_evidence(&fixture);
    let database = fixture.database.clone();
    let candidate = fixture.candidate_state;
    crate::transition_identity::arm_after_existing_candidate_metadata_release_retained(move || {
        database.delete_metadata_provenance_for_test(candidate).unwrap();
    });

    let failure = finish_candidate_prepare(coordinator).unwrap_err();
    assert!(matches!(
        failure,
        StatefulTransitionCoordinatorError::MetadataProvenance(
            db::state::MetadataProvenanceError::Missing { state_id }
        ) if state_id == i32::from(candidate)
    ));
    assert_eq!(reopen_record(&fixture.installation.root), started);
    assert_eq!(candidate_metadata_evidence(&fixture), before);
    assert_candidate_state_id(&fixture, candidate);
}

#[test]
fn journal_coordinator_provenance_is_revalidated_before_trigger_and_exchange_intents() {
    let (fixture, prepared) = coordinator_at_candidate_prepared(CandidateKind::NewState);
    let record = prepared.record().clone();
    fixture
        .database
        .delete_metadata_provenance_for_test(fixture.candidate_state)
        .unwrap();
    let calls = std::cell::Cell::new(0usize);
    let failure = prepared
        .run_transaction_triggers(|_| {
            calls.set(calls.get() + 1);
            Ok::<(), TriggerEffectError>(())
        })
        .unwrap_err();
    assert!(matches!(
        failure,
        StatefulTransactionTriggerFailure::Preflight {
            source: StatefulTransitionCoordinatorError::MetadataProvenance(
                db::state::MetadataProvenanceError::Missing { .. }
            ),
            ..
        }
    ));
    assert_eq!(calls.get(), 0);
    assert_eq!(reopen_record(&fixture.installation.root), record);

    let (fixture, prepared) = coordinator_at_candidate_prepared(CandidateKind::NewState);
    let transition = prepared.record().transition_id.clone();
    let database = fixture.database.clone();
    let candidate = fixture.candidate_state;
    let calls = std::cell::Cell::new(0usize);
    let failure = prepared
        .run_transaction_triggers(|_| {
            calls.set(calls.get() + 1);
            database.delete_metadata_provenance_for_test(candidate).unwrap();
            Ok::<(), TriggerEffectError>(())
        })
        .unwrap_err();
    assert!(matches!(
        failure,
        StatefulTransactionTriggerFailure::PostEffectEvidence {
            transition_id,
            source: StatefulTransitionCoordinatorError::MetadataProvenance(
                db::state::MetadataProvenanceError::Missing { .. }
            ),
        } if transition_id == transition
    ));
    assert_eq!(calls.get(), 1);
    assert_record_prefix(
        &reopen_record(&fixture.installation.root),
        Operation::NewState,
        Phase::TransactionTriggersStarted,
        6,
    );

    let (fixture, coordinator) = coordinator_at_candidate_prepare_started(CandidateKind::Archived);
    let prepared = match finish_candidate_prepare(coordinator).unwrap() {
        PreparedStatefulTransitionCoordinator::Archived(prepared) => prepared,
        _ => panic!("archived activation received transaction-trigger authority"),
    };
    let record = prepared.record().clone();
    fixture
        .database
        .delete_metadata_provenance_for_test(fixture.candidate_state)
        .unwrap();
    let failure = prepared.begin_usr_exchange_intent().unwrap_err();
    assert!(matches!(
        failure,
        UsrExchangeIntentFailure::Preflight {
            source: StatefulTransitionCoordinatorError::MetadataProvenance(
                db::state::MetadataProvenanceError::Missing { .. }
            ),
            ..
        }
    ));
    assert_eq!(reopen_record(&fixture.installation.root), record);
}
