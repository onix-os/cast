use super::*;

#[test]
fn wrong_client_is_rejected_before_fresh_namespace_admission() {
    with_staged_alias_attempt!(
        |fixture, topology_fixture, plan, _inventory, client, staged, _expected_record, fingerprint| {
            let terminal = publish_terminal_alias!(
                staged,
                &client,
                &plan,
                topology_fixture.publication_root()
            );
            let repositories = repository::Manager::with_explicit(
                "terminal-promotion-wrong-client",
                repository::Map::default(),
                fixture.installation.clone(),
            )
            .unwrap();
            let wrong_client = Client {
                registry: crate::client::build_repository_registry(&repositories),
                install_db: db::meta::Database::new(":memory:").unwrap(),
                state_db: db::state::Database::new(":memory:").unwrap(),
                layout_db: fixture.layout_db.clone(),
                config: None,
                repositories,
                scope: crate::client::Scope::Stateful,
                installation: fixture.installation.clone(),
            };

            let error = terminal
                .promote_terminal_receipt(&wrong_client)
                .unwrap_err();
            assert!(matches!(
                error,
                ActiveReblitBootReceiptPromotionError::InitialAdmission { .. },
            ));
            let state = fixture.state_db.boot_publication_receipt_state().unwrap();
            assert_eq!(state.pending().unwrap().fingerprint(), fingerprint);
            assert!(state.head().committed().is_none());
        }
    );
}

#[test]
fn inherited_deadline_expiry_fails_without_receipt_promotion() {
    with_staged_alias_attempt!(
        |fixture, topology_fixture, plan, _inventory, client, staged, _expected_record, fingerprint| {
            let terminal = publish_terminal_alias!(
                staged,
                &client,
                &plan,
                topology_fixture.publication_root()
            );
            arm_expired_deadline();
            let error = terminal.promote_terminal_receipt(&client).unwrap_err();
            assert!(matches!(
                error,
                ActiveReblitBootReceiptPromotionError::TerminalEvidence(
                    ActiveReblitBootTerminalEvidenceValidationError::DeadlineExceeded {
                        checkpoint: "initial terminal admission",
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
fn deadline_expiry_after_staged_revalidation_fails_without_receipt_promotion() {
    with_staged_alias_attempt!(
        |fixture, topology_fixture, plan, _inventory, client, staged, _expected_record, fingerprint| {
            let terminal = publish_terminal_alias!(
                staged,
                &client,
                &plan,
                topology_fixture.publication_root()
            );
            arm_after_pre_promotion_revalidation(arm_expired_deadline);
            let assessments =
                arm_exact_alias_assessments(topology_fixture.publication_root(), 2);
            let error = terminal.promote_terminal_receipt(&client).unwrap_err();
            assert_eq!(fixture_boot_namespace_assessments_remaining(), 0);
            drop(assessments);
            assert!(matches!(
                error,
                ActiveReblitBootReceiptPromotionError::TerminalEvidence(
                    ActiveReblitBootTerminalEvidenceValidationError::DeadlineExceeded {
                        checkpoint: "immediate database promotion",
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
fn missing_terminal_leaf_fails_closed_before_receipt_promotion() {
    with_staged_alias_attempt!(
        |fixture, topology_fixture, plan, _inventory, client, staged, _expected_record, fingerprint| {
            let terminal = publish_terminal_alias!(
                staged,
                &client,
                &plan,
                topology_fixture.publication_root()
            );
            let missing = topology_fixture
                .publication_root()
                .join(plan.outputs().next().unwrap().relative_path());
            arm_before_fresh_admission(move || fs::remove_file(missing).unwrap());
            let assessments =
                arm_exact_alias_assessments(topology_fixture.publication_root(), 1);
            let error = terminal.promote_terminal_receipt(&client).unwrap_err();
            assert_eq!(fixture_boot_namespace_assessments_remaining(), 0);
            drop(assessments);
            assert!(matches!(
                error,
                ActiveReblitBootReceiptPromotionError::TerminalEvidence(
                    ActiveReblitBootTerminalEvidenceValidationError::DestinationNotExact {
                        checkpoint: "initial terminal admission",
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
