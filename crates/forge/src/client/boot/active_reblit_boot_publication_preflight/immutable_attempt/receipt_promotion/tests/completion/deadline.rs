use super::*;

pub(super) const SCENARIO_COUNT: usize = 1;

#[test]
fn inherited_completion_deadline_expires_without_journal_advance_or_token() {
    with_staged_alias_attempt!(
        |fixture, topology_fixture, plan, _inventory, client, staged, expected_record, _fingerprint| {
            let promoted = promote_alias_for_completion!(
                staged,
                &client,
                &plan,
                topology_fixture.publication_root()
            );
            let canonical = canonical_journal(&fixture.installation);
            let journal_inode = fs::metadata(&canonical).unwrap().ino();
            let journal_bytes = fs::read(&canonical).unwrap();
            let database_before = fixture
                .state_db
                .boot_publication_receipt_state()
                .unwrap();
            let outputs_before = publication_snapshot!(
                &plan,
                topology_fixture.publication_root()
            );
            reset_and_assert_no_legacy_boot_effect();
            arm_before_completion_deadline(arm_expired_deadline);
            let assessments =
                arm_exact_alias_assessments(topology_fixture.publication_root(), 2);

            let error = promoted.persist_boot_sync_complete(&client).unwrap_err();

            assert_before_completion_deadline_hook_consumed();
            assert_eq!(fixture_boot_namespace_assessments_remaining(), 0);
            drop(assessments);
            assert!(matches!(
                error,
                ActiveReblitBootSyncCompletionError::Deadline(
                    ActiveReblitBootTerminalEvidenceValidationError::DeadlineExceeded {
                        checkpoint: "immediate completion persistence",
                        ..
                    },
                ),
            ));
            assert_eq!(fs::metadata(&canonical).unwrap().ino(), journal_inode);
            assert_eq!(fs::read(&canonical).unwrap(), journal_bytes);
            assert_eq!(
                fixture.state_db.boot_publication_receipt_state().unwrap(),
                database_before,
            );
            assert_eq!(
                publication_snapshot!(&plan, topology_fixture.publication_root()),
                outputs_before,
            );
            assert_no_legacy_boot_effect();
            assert_eq!(load_journal_record(&fixture.installation), expected_record);
            assert_clean_journal_inventory(&fixture.installation);
        }
    );
}
