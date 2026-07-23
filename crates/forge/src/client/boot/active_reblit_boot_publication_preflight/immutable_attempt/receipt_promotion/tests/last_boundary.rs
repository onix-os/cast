use super::*;

#[test]
fn leaf_drift_after_last_revalidation_promotes_but_returns_no_authority() {
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
            arm_after_pre_promotion_revalidation(move || fs::remove_file(leaf).unwrap());
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
                &error,
                ActiveReblitBootReceiptPromotionError::PostPromotion {
                    outcome: BootPublicationReceiptPromotionOutcome::Promoted,
                    source: ActiveReblitBootPostPromotionValidationError::TerminalEvidence {
                        checkpoint: "post-promotion",
                        ..
                    },
                },
            ), "{error:?}");
            assert_promoted_state(
                &fixture.state_db.boot_publication_receipt_state().unwrap(),
                fingerprint,
            );
        }
    );
}

#[test]
fn collision_drift_after_last_revalidation_is_detected_after_durable_promotion() {
    with_staged_alias_attempt!(
        |fixture, topology_fixture, plan, _inventory, client, staged, _expected_record, fingerprint| {
            let terminal = publish_terminal_alias!(
                staged,
                &client,
                &plan,
                topology_fixture.publication_root()
            );
            arm_after_pre_promotion_revalidation(arm_bound_plan_collision_drift);
            let assessments =
                arm_exact_alias_assessments(topology_fixture.publication_root(), 3);
            let error = terminal.promote_terminal_receipt(&client).unwrap_err();
            assert_eq!(fixture_boot_namespace_assessments_remaining(), 1);
            drop(assessments);
            assert!(matches!(
                &error,
                ActiveReblitBootReceiptPromotionError::PostPromotion {
                    outcome: BootPublicationReceiptPromotionOutcome::Promoted,
                    source: ActiveReblitBootPostPromotionValidationError::TerminalEvidence {
                        source: ActiveReblitBootTerminalEvidenceValidationError::Preflight {
                            checkpoint: "post-promotion",
                            source: ActiveReblitBootPublicationPreflightError::NamespaceInputs(
                                ActiveReblitBootNamespaceInputError::CollisionDomainDrift,
                            ),
                        },
                        ..
                    },
                },
            ), "{error:?}");
            assert_promoted_state(
                &fixture.state_db.boot_publication_receipt_state().unwrap(),
                fingerprint,
            );
        }
    );
}

#[test]
fn target_attachment_identity_drift_after_last_revalidation_is_detected_after_promotion() {
    with_staged_alias_attempt!(
        |fixture, topology_fixture, plan, _inventory, client, staged, _expected_record, fingerprint| {
            let terminal = publish_terminal_alias!(
                staged,
                &client,
                &plan,
                topology_fixture.publication_root()
            );
            let canonical = topology_fixture.publication_root().to_owned();
            let displaced = canonical
                .parent()
                .unwrap()
                .join("terminal-promotion-displaced-target");
            let mode = fs::metadata(&canonical).unwrap().permissions().mode();
            let callback_canonical = canonical.clone();
            let callback_displaced = displaced.clone();
            arm_after_pre_promotion_revalidation(move || {
                fs::rename(&callback_canonical, &callback_displaced).unwrap();
                fs::create_dir(&callback_canonical).unwrap();
                fs::set_permissions(
                    &callback_canonical,
                    fs::Permissions::from_mode(mode),
                )
                .unwrap();
            });
            let assessments = arm_exact_alias_assessments(&canonical, 3);
            let error = terminal.promote_terminal_receipt(&client).unwrap_err();
            assert_eq!(fixture_boot_namespace_assessments_remaining(), 1);
            drop(assessments);
            assert!(matches!(
                error,
                ActiveReblitBootReceiptPromotionError::PostPromotion {
                    outcome: BootPublicationReceiptPromotionOutcome::Promoted,
                    source: ActiveReblitBootPostPromotionValidationError::TerminalEvidence {
                        checkpoint: "post-promotion",
                        source: ActiveReblitBootTerminalEvidenceValidationError::Preflight {
                            checkpoint: "post-promotion",
                            source: ActiveReblitBootPublicationPreflightError::InitialTargets {
                                source: ActiveReblitBootPublicationTargetsError::OpeningTopology {
                                    source: ActiveReblitMountedBootTopologyCaptureError::Attachment {
                                        role: BootTargetRole::Esp,
                                        ..
                                    },
                                },
                            },
                        },
                    },
                },
            ), "{error:?}");
            assert_promoted_state(
                &fixture.state_db.boot_publication_receipt_state().unwrap(),
                fingerprint,
            );
            fs::remove_dir(&canonical).unwrap();
            fs::rename(displaced, canonical).unwrap();
        }
    );
}

#[test]
fn journal_inode_substitution_after_last_revalidation_fails_stop_after_promotion() {
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
                .join("pre-promotion-displaced-transition");
            let callback_canonical = canonical.clone();
            let callback_displaced = displaced.clone();
            arm_after_pre_promotion_revalidation(move || {
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
            assert!(matches!(
                error,
                ActiveReblitBootReceiptPromotionError::PostPromotion {
                    outcome: BootPublicationReceiptPromotionOutcome::Promoted,
                    source:
                        ActiveReblitBootPostPromotionValidationError::PromotedStagedEvidence {
                            checkpoint: "post-promotion",
                            ..
                        },
                },
            ));
            assert_promoted_state(
                &fixture.state_db.boot_publication_receipt_state().unwrap(),
                fingerprint,
            );
            fs::remove_file(&canonical).unwrap();
            fs::rename(displaced, canonical).unwrap();
        }
    );
}
