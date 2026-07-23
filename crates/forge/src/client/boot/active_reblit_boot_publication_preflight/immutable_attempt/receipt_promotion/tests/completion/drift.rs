use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Drift {
    Namespace,
    Database,
    Journal,
    Plan,
}

impl Drift {
    const ALL: [Self; 4] = [
        Self::Namespace,
        Self::Database,
        Self::Journal,
        Self::Plan,
    ];
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PreAdvanceScenario {
    WrongClient,
    Drift(Drift),
}

impl PreAdvanceScenario {
    const ALL: [Self; 5] = [
        Self::WrongClient,
        Self::Drift(Drift::Namespace),
        Self::Drift(Drift::Database),
        Self::Drift(Drift::Journal),
        Self::Drift(Drift::Plan),
    ];
}

const PRE_ADVANCE_SCENARIO_COUNT: usize = PreAdvanceScenario::ALL.len();
const POST_ADVANCE_SCENARIO_COUNT: usize = Drift::ALL.len();
const FINAL_RETURN_SCENARIO_COUNT: usize = 1;
pub(super) const SCENARIO_COUNT: usize = PRE_ADVANCE_SCENARIO_COUNT
    + POST_ADVANCE_SCENARIO_COUNT
    + FINAL_RETURN_SCENARIO_COUNT;

#[test]
fn pre_advance_wrong_client_and_four_drift_axes_never_reach_boot_sync_complete() {
    let mut exercised = 0usize;
    for scenario in PreAdvanceScenario::ALL {
        with_staged_alias_attempt!(
            |fixture, topology_fixture, plan, _inventory, client, staged, expected_record, _fingerprint| {
                let promoted = promote_alias_for_completion!(
                    staged,
                    &client,
                    &plan,
                    topology_fixture.publication_root()
                );
                let outputs_before = publication_snapshot!(
                    &plan,
                    topology_fixture.publication_root()
                );
                let database_before = fixture
                    .state_db
                    .boot_publication_receipt_state()
                    .unwrap();
                let canonical = canonical_journal(&fixture.installation);
                let journal_inode_before = fs::metadata(&canonical).unwrap().ino();
                let journal_bytes_before = fs::read(&canonical).unwrap();
                let wrong_client = match scenario {
                    PreAdvanceScenario::WrongClient => {
                        let repositories = repository::Manager::with_explicit(
                            "completion-wrong-client",
                            repository::Map::default(),
                            fixture.installation.clone(),
                        )
                        .unwrap();
                        Some(Client {
                            registry: crate::client::build_repository_registry(
                                &repositories,
                            ),
                            install_db: db::meta::Database::new(":memory:").unwrap(),
                            state_db: db::state::Database::new(":memory:").unwrap(),
                            layout_db: fixture.layout_db.clone(),
                            config: None,
                            repositories,
                            scope: crate::client::Scope::Stateful,
                            installation: fixture.installation.clone(),
                        })
                    }
                    PreAdvanceScenario::Drift(_) => None,
                };
                let wrong_database_before = wrong_client.as_ref().map(|client| {
                    client.state_db.boot_publication_receipt_state().unwrap()
                });
                match scenario {
                    PreAdvanceScenario::WrongClient => {}
                    PreAdvanceScenario::Drift(Drift::Namespace) => {
                        let leaf = first_output_path(
                            &plan,
                            topology_fixture.publication_root(),
                        );
                        arm_after_initial_completion_handoff(move || {
                            fs::remove_file(leaf).unwrap()
                        });
                    }
                    PreAdvanceScenario::Drift(Drift::Database) => {
                        let database = fixture.state_db.clone();
                        arm_after_initial_completion_handoff(move || {
                            database
                                .clear_boot_publication_receipt_head_for_test()
                                .unwrap()
                        });
                    }
                    PreAdvanceScenario::Drift(Drift::Journal) => {
                        let displaced = canonical
                            .parent()
                            .unwrap()
                            .join("completion-pre-drift-displaced");
                        let callback_canonical = canonical.clone();
                        arm_after_initial_completion_handoff(move || {
                            replace_file_identity(&callback_canonical, &displaced)
                        });
                    }
                    PreAdvanceScenario::Drift(Drift::Plan) => {
                        arm_after_initial_completion_handoff(
                            arm_bound_plan_collision_drift,
                        );
                    }
                }
                let expected_assessments = match scenario {
                    PreAdvanceScenario::WrongClient => 0,
                    PreAdvanceScenario::Drift(Drift::Namespace) => 2,
                    PreAdvanceScenario::Drift(
                        Drift::Database | Drift::Journal | Drift::Plan,
                    ) => 1,
                };
                reset_and_assert_no_legacy_boot_effect();
                let assessments = (expected_assessments != 0).then(|| {
                    arm_exact_alias_assessments(
                        topology_fixture.publication_root(),
                        expected_assessments,
                    )
                });

                let completion_client = wrong_client.as_ref().unwrap_or(&client);
                let error = promoted
                    .persist_boot_sync_complete(completion_client)
                    .unwrap_err();

                if matches!(scenario, PreAdvanceScenario::Drift(_)) {
                    assert_after_initial_completion_handoff_hook_consumed();
                }
                assert_eq!(fixture_boot_namespace_assessments_remaining(), 0);
                drop(assessments);
                match scenario {
                    PreAdvanceScenario::WrongClient => assert!(matches!(
                        &error,
                        ActiveReblitBootSyncCompletionError::InitialHandoff(
                            ActiveReblitBootPostPromotionValidationError::PromotedStagedEvidence {
                                checkpoint: "initial completion admission",
                                source:
                                    ActiveReblitBootSyncPromotedValidationError::ClientCapabilityMismatch,
                            },
                        ),
                    ), "{error:?}"),
                    PreAdvanceScenario::Drift(Drift::Namespace) => assert!(matches!(
                        &error,
                        ActiveReblitBootSyncCompletionError::ImmediateHandoff(
                            ActiveReblitBootPostPromotionValidationError::TerminalEvidence {
                                checkpoint: "immediate pre-persistence",
                                source:
                                    ActiveReblitBootTerminalEvidenceValidationError::DestinationNotExact {
                                        checkpoint: "immediate pre-persistence",
                                        plan_index: 0,
                                        state: BootNamespaceDestinationState::Absent,
                                    },
                            },
                        ),
                    ), "{error:?}"),
                    PreAdvanceScenario::Drift(Drift::Database) => assert!(matches!(
                        &error,
                        ActiveReblitBootSyncCompletionError::ImmediateHandoff(
                            ActiveReblitBootPostPromotionValidationError::PromotedStagedEvidence {
                                checkpoint: "immediate pre-persistence",
                                source:
                                    ActiveReblitBootSyncPromotedValidationError::ReceiptState(
                                        BootPublicationReceiptPromotionError::MissingPending,
                                    ),
                            },
                        ),
                    ), "{error:?}"),
                    PreAdvanceScenario::Drift(Drift::Journal) => assert!(matches!(
                        &error,
                        ActiveReblitBootSyncCompletionError::ImmediateHandoff(
                            ActiveReblitBootPostPromotionValidationError::PromotedStagedEvidence {
                                checkpoint: "immediate pre-persistence",
                                source:
                                    ActiveReblitBootSyncPromotedValidationError::BootSyncStartedBindingChanged,
                            },
                        ),
                    ), "{error:?}"),
                    PreAdvanceScenario::Drift(Drift::Plan) => assert!(matches!(
                        &error,
                        ActiveReblitBootSyncCompletionError::ImmediateHandoff(
                            ActiveReblitBootPostPromotionValidationError::TerminalEvidence {
                                checkpoint: "immediate pre-persistence",
                                source:
                                    ActiveReblitBootTerminalEvidenceValidationError::Preflight {
                                        checkpoint: "immediate pre-persistence",
                                        source:
                                            ActiveReblitBootPublicationPreflightError::NamespaceInputs(
                                                ActiveReblitBootNamespaceInputError::CollisionDomainDrift,
                                            ),
                                    },
                            },
                        ),
                    ), "{error:?}"),
                }
                assert_eq!(load_journal_record(&fixture.installation), expected_record);
                assert_clean_journal_inventory(&fixture.installation);
                match scenario {
                    PreAdvanceScenario::Drift(Drift::Database) => {
                        let state = fixture
                            .state_db
                            .boot_publication_receipt_state()
                            .unwrap();
                        assert!(state.head().committed().is_none());
                        assert!(state.head().pending().is_none());
                    }
                    _ => assert_eq!(
                        fixture.state_db.boot_publication_receipt_state().unwrap(),
                        database_before,
                    ),
                }
                if scenario != PreAdvanceScenario::Drift(Drift::Namespace) {
                    assert_eq!(
                        publication_snapshot!(
                            &plan,
                            topology_fixture.publication_root()
                        ),
                        outputs_before,
                    );
                }
                if scenario == PreAdvanceScenario::WrongClient {
                    assert_eq!(
                        fs::metadata(&canonical).unwrap().ino(),
                        journal_inode_before,
                    );
                    assert_eq!(fs::read(&canonical).unwrap(), journal_bytes_before);
                    assert_eq!(
                        wrong_client
                            .as_ref()
                            .unwrap()
                            .state_db
                            .boot_publication_receipt_state()
                            .unwrap(),
                        wrong_database_before.unwrap(),
                    );
                }
                assert_no_legacy_boot_effect();
            }
        );
        exercised += 1;
    }
    assert_eq!(exercised, PRE_ADVANCE_SCENARIO_COUNT);
}

#[test]
fn post_advance_namespace_database_journal_and_plan_drift_returns_no_completion_token() {
    let mut exercised = 0usize;
    for drift in Drift::ALL {
        with_staged_alias_attempt!(
            |fixture, topology_fixture, plan, _inventory, client, staged, expected_record, _fingerprint| {
                let promoted = promote_alias_for_completion!(
                    staged,
                    &client,
                    &plan,
                    topology_fixture.publication_root()
                );
                let pair = expected_record
                    .boot_publication_receipt_correlation()
                    .unwrap()
                    .unwrap();
                let successor = expected_record
                    .boot_sync_complete_successor(pair)
                    .unwrap();
                let outputs_before = publication_snapshot!(
                    &plan,
                    topology_fixture.publication_root()
                );
                let database_before = fixture
                    .state_db
                    .boot_publication_receipt_state()
                    .unwrap();
                let canonical = canonical_journal(&fixture.installation);
                match drift {
                    Drift::Namespace => {
                        let leaf = first_output_path(
                            &plan,
                            topology_fixture.publication_root(),
                        );
                        arm_after_boot_sync_complete_persistence(move || {
                            fs::remove_file(leaf).unwrap()
                        });
                    }
                    Drift::Database => {
                        let database = fixture.state_db.clone();
                        arm_after_boot_sync_complete_persistence(move || {
                            database
                                .clear_boot_publication_receipt_head_for_test()
                                .unwrap()
                        });
                    }
                    Drift::Journal => {
                        let displaced = canonical
                            .parent()
                            .unwrap()
                            .join("completion-post-drift-displaced");
                        arm_after_boot_sync_complete_persistence(move || {
                            replace_file_identity(&canonical, &displaced)
                        });
                    }
                    Drift::Plan => {
                        arm_after_boot_sync_complete_persistence(
                            arm_bound_plan_collision_drift,
                        );
                    }
                }
                let expected_assessments = match drift {
                    Drift::Namespace => 3,
                    Drift::Database | Drift::Journal | Drift::Plan => 2,
                };
                reset_and_assert_no_legacy_boot_effect();
                let assessments = arm_exact_alias_assessments(
                    topology_fixture.publication_root(),
                    expected_assessments,
                );

                let error = promoted.persist_boot_sync_complete(&client).unwrap_err();

                assert_after_boot_sync_complete_persistence_hook_consumed();
                assert_eq!(fixture_boot_namespace_assessments_remaining(), 0);
                drop(assessments);
                match drift {
                    Drift::Namespace => assert!(matches!(
                        &error,
                        ActiveReblitBootSyncCompletionError::PostCompletion {
                            durable:
                                DurableActiveReblitBootSyncCompletionRecord::BootSyncComplete,
                            source:
                                ActiveReblitBootPostCompletionValidationError::TerminalEvidence {
                                    checkpoint: "post-persistence",
                                    source:
                                        ActiveReblitBootTerminalEvidenceValidationError::DestinationNotExact {
                                            checkpoint: "post-persistence",
                                            plan_index: 0,
                                            state: BootNamespaceDestinationState::Absent,
                                        },
                                },
                        },
                    ), "{error:?}"),
                    Drift::Database => assert!(matches!(
                        &error,
                        ActiveReblitBootSyncCompletionError::PostCompletionAndReconciliation {
                            validation:
                                ActiveReblitBootPostCompletionValidationError::CompletedStagedEvidence {
                                    checkpoint: "post-persistence",
                                    source:
                                        ActiveReblitBootSyncCompleteValidationError::ReceiptState(
                                            BootPublicationReceiptPromotionError::MissingPending,
                                        ),
                                },
                            reconciliation:
                                ActiveReblitBootSyncCompletionReconciliationError::ReceiptState(
                                    BootPublicationReceiptPromotionError::MissingPending,
                                ),
                        },
                    ), "{error:?}"),
                    Drift::Journal => assert!(matches!(
                        &error,
                        ActiveReblitBootSyncCompletionError::PostCompletion {
                            durable:
                                DurableActiveReblitBootSyncCompletionRecord::BootSyncComplete,
                            source:
                                ActiveReblitBootPostCompletionValidationError::CompletedStagedEvidence {
                                    checkpoint: "post-persistence",
                                    source:
                                        ActiveReblitBootSyncCompleteValidationError::BindingChanged,
                                },
                        },
                    ), "{error:?}"),
                    Drift::Plan => assert!(matches!(
                        &error,
                        ActiveReblitBootSyncCompletionError::PostCompletion {
                            durable:
                                DurableActiveReblitBootSyncCompletionRecord::BootSyncComplete,
                            source:
                                ActiveReblitBootPostCompletionValidationError::TerminalEvidence {
                                    checkpoint: "post-persistence",
                                    source:
                                        ActiveReblitBootTerminalEvidenceValidationError::Preflight {
                                            checkpoint: "post-persistence",
                                            source:
                                                ActiveReblitBootPublicationPreflightError::NamespaceInputs(
                                                    ActiveReblitBootNamespaceInputError::CollisionDomainDrift,
                                                ),
                                        },
                                },
                        },
                    ), "{error:?}"),
                }
                assert_eq!(load_journal_record(&fixture.installation), successor);
                assert_clean_journal_inventory(&fixture.installation);
                match drift {
                    Drift::Database => {
                        let state = fixture
                            .state_db
                            .boot_publication_receipt_state()
                            .unwrap();
                        assert!(state.head().committed().is_none());
                        assert!(state.head().pending().is_none());
                    }
                    _ => assert_eq!(
                        fixture.state_db.boot_publication_receipt_state().unwrap(),
                        database_before,
                    ),
                }
                if drift != Drift::Namespace {
                    assert_eq!(
                        publication_snapshot!(
                            &plan,
                            topology_fixture.publication_root()
                        ),
                        outputs_before,
                    );
                }
                assert_no_legacy_boot_effect();
            }
        );
        exercised += 1;
    }
    assert_eq!(exercised, POST_ADVANCE_SCENARIO_COUNT);
}

#[test]
fn final_return_revalidation_catches_late_drift_after_durable_completion() {
    let mut exercised = 0usize;
    with_staged_alias_attempt!(
        |fixture, topology_fixture, plan, _inventory, client, staged, expected_record, _fingerprint| {
            let promoted = promote_alias_for_completion!(
                staged,
                &client,
                &plan,
                topology_fixture.publication_root()
            );
            let pair = expected_record
                .boot_publication_receipt_correlation()
                .unwrap()
                .unwrap();
            let successor = expected_record
                .boot_sync_complete_successor(pair)
                .unwrap();
            let database_before = fixture
                .state_db
                .boot_publication_receipt_state()
                .unwrap();
            let leaf = first_output_path(
                &plan,
                topology_fixture.publication_root(),
            );
            arm_before_final_completion_validation(move || {
                fs::remove_file(leaf).unwrap()
            });
            reset_and_assert_no_legacy_boot_effect();
            let assessments =
                arm_exact_alias_assessments(topology_fixture.publication_root(), 4);

            let error = promoted.persist_boot_sync_complete(&client).unwrap_err();

            assert_before_final_completion_validation_hook_consumed();
            assert_eq!(fixture_boot_namespace_assessments_remaining(), 0);
            drop(assessments);
            assert!(matches!(
                &error,
                ActiveReblitBootSyncCompletionError::PostCompletion {
                    durable:
                        DurableActiveReblitBootSyncCompletionRecord::BootSyncComplete,
                    source:
                        ActiveReblitBootPostCompletionValidationError::TerminalEvidence {
                            checkpoint: "final return",
                            source:
                                ActiveReblitBootTerminalEvidenceValidationError::DestinationNotExact {
                                    checkpoint: "final return",
                                    plan_index: 0,
                                    state: BootNamespaceDestinationState::Absent,
                                },
                        },
                },
            ), "{error:?}");
            assert_eq!(
                fixture.state_db.boot_publication_receipt_state().unwrap(),
                database_before,
            );
            assert_no_legacy_boot_effect();
            assert_eq!(load_journal_record(&fixture.installation), successor);
            assert_clean_journal_inventory(&fixture.installation);
        }
    );
    exercised += 1;
    assert_eq!(exercised, FINAL_RETURN_SCENARIO_COUNT);
}
