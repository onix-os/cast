use super::*;
#[test]
fn leaf_drift_between_terminal_checks_fails_before_database_promotion() {
    with_staged_alias_attempt!(
        |fixture, topology_fixture, plan, _inventory, client, staged, _expected_record, fingerprint| {
            let terminal = publish_terminal_alias!(
                staged,
                &client,
                &plan,
                topology_fixture.publication_root()
            );
            let leaf = topology_fixture
                .publication_root()
                .join(plan.outputs().next().unwrap().relative_path());
            arm_before_immediate_pre_promotion_terminal_check(move || {
                fs::remove_file(leaf).unwrap()
            });
            let assessments =
                arm_exact_alias_assessments(topology_fixture.publication_root(), 2);
            let error = terminal.promote_terminal_receipt(&client).unwrap_err();
            assert_eq!(fixture_boot_namespace_assessments_remaining(), 0);
            drop(assessments);
            assert!(matches!(
                error,
                ActiveReblitBootReceiptPromotionError::TerminalEvidence(
                    ActiveReblitBootTerminalEvidenceValidationError::DestinationNotExact {
                        checkpoint: "immediate pre-promotion",
                        ..
                    },
                ),
            ));
            let state = fixture.state_db.boot_publication_receipt_state().unwrap();
            assert_eq!(state.pending().unwrap().fingerprint(), fingerprint);
            assert!(state.head().committed().is_none());
        }
    );
}

#[test]
fn leaf_drift_after_database_success_returns_outcome_but_no_success_authority() {
    with_staged_alias_attempt!(
        |fixture, topology_fixture, plan, _inventory, client, staged, _expected_record, fingerprint| {
            let terminal = publish_terminal_alias!(
                staged,
                &client,
                &plan,
                topology_fixture.publication_root()
            );
            let leaf = topology_fixture
                .publication_root()
                .join(plan.outputs().next().unwrap().relative_path());
            arm_after_database_promotion(move || fs::remove_file(leaf).unwrap());
            let assessments =
                arm_exact_alias_assessments(topology_fixture.publication_root(), 3);
            let error = terminal.promote_terminal_receipt(&client).unwrap_err();
            assert_eq!(fixture_boot_namespace_assessments_remaining(), 0);
            drop(assessments);
            assert_eq!(
                error.durable_promotion_outcome(),
                Some(BootPublicationReceiptPromotionOutcome::Promoted),
            );
            assert!(matches!(
                error,
                ActiveReblitBootReceiptPromotionError::PostPromotion {
                    outcome: BootPublicationReceiptPromotionOutcome::Promoted,
                    source: ActiveReblitBootPostPromotionValidationError::TerminalEvidence {
                        checkpoint: "post-promotion",
                        ..
                    },
                },
            ));
            assert_promoted_state(
                &fixture.state_db.boot_publication_receipt_state().unwrap(),
                fingerprint,
            );
        }
    );
}

#[test]
fn final_validation_catches_late_leaf_drift_after_promoted_revalidation() {
    with_staged_alias_attempt!(
        |fixture, topology_fixture, plan, _inventory, client, staged, _expected_record, fingerprint| {
            let terminal = publish_terminal_alias!(
                staged,
                &client,
                &plan,
                topology_fixture.publication_root()
            );
            let leaf = topology_fixture
                .publication_root()
                .join(plan.outputs().next().unwrap().relative_path());
            arm_before_final_promoted_validation(move || fs::remove_file(leaf).unwrap());
            let assessments =
                arm_exact_alias_assessments(topology_fixture.publication_root(), 4);
            let error = terminal.promote_terminal_receipt(&client).unwrap_err();
            assert_eq!(fixture_boot_namespace_assessments_remaining(), 0);
            drop(assessments);
            assert_eq!(
                error.durable_promotion_outcome(),
                Some(BootPublicationReceiptPromotionOutcome::Promoted),
            );
            assert!(matches!(
                error,
                ActiveReblitBootReceiptPromotionError::PostPromotion {
                    source: ActiveReblitBootPostPromotionValidationError::TerminalEvidence {
                        checkpoint: "final return",
                        ..
                    },
                    ..
                },
            ));
            assert_promoted_state(
                &fixture.state_db.boot_publication_receipt_state().unwrap(),
                fingerprint,
            );
        }
    );
}

#[test]
fn post_promotion_same_bytes_different_journal_inode_fails_stop() {
    with_staged_alias_attempt!(
        |fixture, topology_fixture, plan, _inventory, client, staged, _expected_record, fingerprint| {
            let terminal = publish_terminal_alias!(
                staged,
                &client,
                &plan,
                topology_fixture.publication_root()
            );
            let canonical = fixture
                .installation
                .root
                .join(".cast/journal/state-transition");
            let displaced = canonical
                .parent()
                .unwrap()
                .join("post-promotion-displaced-transition");
            let callback_canonical = canonical.clone();
            let callback_displaced = displaced.clone();
            arm_after_database_promotion(move || {
                let bytes = fs::read(&callback_canonical).unwrap();
                let mode = fs::metadata(&callback_canonical)
                    .unwrap()
                    .permissions()
                    .mode();
                fs::rename(&callback_canonical, &callback_displaced).unwrap();
                fs::write(&callback_canonical, bytes).unwrap();
                fs::set_permissions(
                    &callback_canonical,
                    fs::Permissions::from_mode(mode),
                )
                .unwrap();
            });
            let assessments =
                arm_exact_alias_assessments(topology_fixture.publication_root(), 3);
            let error = terminal.promote_terminal_receipt(&client).unwrap_err();
            assert_eq!(fixture_boot_namespace_assessments_remaining(), 0);
            drop(assessments);
            assert_eq!(
                error.durable_promotion_outcome(),
                Some(BootPublicationReceiptPromotionOutcome::Promoted),
            );
            assert!(matches!(
                error,
                ActiveReblitBootReceiptPromotionError::PostPromotion {
                    source:
                        ActiveReblitBootPostPromotionValidationError::PromotedStagedEvidence {
                            checkpoint: "post-promotion",
                            ..
                        },
                    ..
                },
            ));
            assert_promoted_state(
                &fixture.state_db.boot_publication_receipt_state().unwrap(),
                fingerprint,
            );
            fs::remove_file(&canonical).unwrap();
            fs::rename(&displaced, &canonical).unwrap();
        }
    );
}
