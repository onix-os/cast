use super::*;

pub(super) const SCENARIO_COUNT: usize = 3;

#[test]
fn cleaned_promoted_typestate_preserves_exact_authority() {
    with_staged_alias_attempt!(
        |fixture, topology_fixture, plan, _inventory, client, staged, _expected_record, fingerprint| {
            let terminal = publish_terminal_alias!(
                staged,
                &client,
                &plan,
                topology_fixture.publication_root()
            );
            let assessments =
                arm_exact_alias_assessments(topology_fixture.publication_root(), 4);
            let promoted = terminal.promote_terminal_receipt(&client).unwrap();
            assert_eq!(fixture_boot_namespace_assessments_remaining(), 0);
            drop(assessments);

            let database_outcome = promoted.database_outcome();
            let publication_count = promoted.publication_count();
            let published_count = promoted.published_count();
            let already_exact_count = promoted.already_exact_count();
            let replaced_count = promoted.replaced_count();
            let evidence = evidence_snapshot(promoted.evidence());
            assert!(!promoted.promoted_cleanup_required());

            let cleaned = promoted
                .try_into_cleaned()
                .expect("exact alias fixture requires no cleanup");
            assert_eq!(cleaned.receipt_fingerprint(), fingerprint);
            assert_eq!(cleaned.database_outcome(), database_outcome);
            assert_eq!(cleaned.publication_count(), publication_count);
            assert_eq!(cleaned.published_count(), published_count);
            assert_eq!(cleaned.already_exact_count(), already_exact_count);
            assert_eq!(cleaned.replaced_count(), replaced_count);
            assert_eq!(evidence_snapshot(cleaned.evidence()), evidence);
            assert_eq!(
                fixture
                    .state_db
                    .boot_publication_receipt_state()
                    .unwrap()
                    .head()
                    .committed(),
                Some(fingerprint),
            );
        }
    );
}

#[test]
fn first_adoption_completion_persists_only_exact_boot_sync_complete() {
    with_staged_alias_attempt!(
        |fixture, topology_fixture, plan, _inventory, client, staged, expected_record, fingerprint| {
            let promoted = promote_alias_for_completion!(
                staged,
                &client,
                &plan,
                topology_fixture.publication_root()
            );
            assert_eq!(
                promoted.database_outcome(),
                BootPublicationReceiptPromotionOutcome::Promoted,
            );
            let pair = expected_record
                .boot_publication_receipt_correlation()
                .unwrap()
                .unwrap();
            assert!(pair.committed.is_none());
            let successor = expected_record
                .boot_sync_complete_successor(pair)
                .unwrap();
            let database_before = fixture
                .state_db
                .boot_publication_receipt_state()
                .unwrap();
            let outputs_before = publication_snapshot!(
                &plan,
                topology_fixture.publication_root()
            );
            let evidence = evidence_snapshot(promoted.evidence());
            let publication_count = promoted.publication_count();
            let published_count = promoted.published_count();
            let already_exact_count = promoted.already_exact_count();
            let canonical = canonical_journal(&fixture.installation);
            let predecessor_inode = fs::metadata(&canonical).unwrap().ino();
            reset_and_assert_no_legacy_boot_effect();

            let assessments =
                arm_exact_alias_assessments(topology_fixture.publication_root(), 4);
            let completed = promoted.persist_boot_sync_complete(&client).unwrap();
            assert_eq!(fixture_boot_namespace_assessments_remaining(), 0);
            drop(assessments);

            assert_eq!(completed.record(), &successor);
            assert_eq!(completed.record().phase, Phase::BootSyncComplete);
            assert_eq!(completed.record().generation, expected_record.generation + 1);
            assert_eq!(
                completed
                    .record()
                    .boot_publication_receipt_correlation()
                    .unwrap(),
                Some(pair),
            );
            assert_eq!(completed.receipt_fingerprint(), fingerprint);
            assert_eq!(
                completed.database_outcome(),
                BootPublicationReceiptPromotionOutcome::Promoted,
            );
            assert_eq!(completed.publication_count(), publication_count);
            assert_eq!(completed.published_count(), published_count);
            assert_eq!(completed.already_exact_count(), already_exact_count);
            assert_eq!(evidence_snapshot(completed.evidence()), evidence);
            let completed_inode = fs::metadata(&canonical).unwrap().ino();
            assert_ne!(
                completed_inode, predecessor_inode,
                "BootSyncComplete must be published as a successor journal inode",
            );
            assert_eq!(
                fixture.state_db.boot_publication_receipt_state().unwrap(),
                database_before,
            );
            assert_eq!(
                publication_snapshot!(&plan, topology_fixture.publication_root()),
                outputs_before,
            );
            assert_no_legacy_boot_effect();

            drop(completed);
            assert_eq!(load_journal_record(&fixture.installation), successor);
            assert_eq!(
                fs::metadata(&canonical).unwrap().ino(),
                completed_inode,
                "reopening must retain the exact completed successor inode",
            );
            assert_clean_journal_inventory(&fixture.installation);
        }
    );
}

#[test]
fn chained_already_promoted_completion_preserves_pair_bodies_and_outputs() {
    with_staged_alias_attempt!(
        before_stage |client, plan, inventory, claims, journal_predecessor, deadline| {
            let mut prior_record = journal_predecessor.clone();
            prior_record.transition_id =
                TransitionId::parse("fedcba9876543210fedcba9876543210").unwrap();
            let prior_receipt = plan
                .prepare_complete_boot_publication_receipt(
                    inventory,
                    &prior_record,
                    None,
                    claims,
                )
                .unwrap();
            client
                .state_db
                .stage_boot_publication_receipt(&prior_receipt)
                .unwrap();
            assert_eq!(
                client
                    .state_db
                    .promote_boot_publication_receipt(&prior_receipt, deadline)
                    .unwrap(),
                BootPublicationReceiptPromotionOutcome::Promoted,
            );
        },
        |fixture, topology_fixture, plan, _inventory, client, staged, expected_record, fingerprint| {
            let terminal = publish_terminal_alias!(
                staged,
                &client,
                &plan,
                topology_fixture.publication_root()
            );
            assert_eq!(
                fixture
                    .state_db
                    .promote_boot_publication_receipt(
                        terminal.staged.receipt(),
                        plan.input_deadline(),
                    )
                    .unwrap(),
                BootPublicationReceiptPromotionOutcome::Promoted,
            );
            let promotion_assessments =
                arm_exact_alias_assessments(topology_fixture.publication_root(), 4);
            let promoted = terminal.promote_terminal_receipt(&client).unwrap();
            assert_eq!(fixture_boot_namespace_assessments_remaining(), 0);
            drop(promotion_assessments);
            assert_eq!(
                promoted.database_outcome(),
                BootPublicationReceiptPromotionOutcome::AlreadyPromoted,
            );
            let promoted = promoted
                .try_into_cleaned()
                .expect("already-promoted alias fixture requires no cleanup");

            let pair = expected_record
                .boot_publication_receipt_correlation()
                .unwrap()
                .unwrap();
            assert!(pair.committed.is_some());
            let successor = expected_record
                .boot_sync_complete_successor(pair)
                .unwrap();
            let database_before = fixture
                .state_db
                .boot_publication_receipt_state()
                .unwrap();
            let outputs_before = publication_snapshot!(
                &plan,
                topology_fixture.publication_root()
            );
            reset_and_assert_no_legacy_boot_effect();

            let completion_assessments =
                arm_exact_alias_assessments(topology_fixture.publication_root(), 4);
            let completed = promoted.persist_boot_sync_complete(&client).unwrap();
            assert_eq!(fixture_boot_namespace_assessments_remaining(), 0);
            drop(completion_assessments);

            assert_eq!(completed.record(), &successor);
            assert_eq!(
                completed
                    .record()
                    .boot_publication_receipt_correlation()
                    .unwrap(),
                Some(pair),
            );
            assert_eq!(completed.receipt_fingerprint(), fingerprint);
            assert_eq!(
                completed.database_outcome(),
                BootPublicationReceiptPromotionOutcome::AlreadyPromoted,
            );
            let database_after = fixture
                .state_db
                .boot_publication_receipt_state()
                .unwrap();
            assert_eq!(database_after, database_before);
            fixture
                .state_db
                .require_promoted_boot_publication_receipt(
                    database_before.committed().unwrap(),
                )
                .unwrap();
            assert_eq!(
                publication_snapshot!(&plan, topology_fixture.publication_root()),
                outputs_before,
            );
            assert_no_legacy_boot_effect();

            drop(completed);
            assert_eq!(load_journal_record(&fixture.installation), successor);
            assert_clean_journal_inventory(&fixture.installation);
        }
    );
}
