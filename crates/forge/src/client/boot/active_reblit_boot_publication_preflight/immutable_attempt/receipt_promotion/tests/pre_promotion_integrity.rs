use super::*;

#[test]
fn cleared_pending_head_after_initial_admission_fails_before_promotion() {
    with_staged_alias_attempt!(
        |fixture, topology_fixture, plan, _inventory, client, staged, _expected_record, _fingerprint| {
            let terminal = publish_terminal_alias!(
                staged,
                &client,
                &plan,
                topology_fixture.publication_root()
            );
            let database = fixture.state_db.clone();
            arm_before_immediate_pre_promotion_terminal_check(move || {
                database
                    .clear_boot_publication_receipt_head_for_test()
                    .unwrap()
            });
            let assessments =
                arm_exact_alias_assessments(topology_fixture.publication_root(), 2);
            let error = terminal.promote_terminal_receipt(&client).unwrap_err();
            assert_eq!(fixture_boot_namespace_assessments_remaining(), 0);
            drop(assessments);
            assert!(matches!(
                error,
                ActiveReblitBootReceiptPromotionError::PrePromotionPending(_),
            ));
            let state = fixture.state_db.boot_publication_receipt_state().unwrap();
            assert!(state.head().committed().is_none());
            assert!(state.head().pending().is_none());
        }
    );
}

#[test]
fn missing_pending_body_after_initial_admission_fails_before_promotion() {
    with_staged_alias_attempt!(
        |fixture, topology_fixture, plan, _inventory, client, staged, _expected_record, fingerprint| {
            let terminal = publish_terminal_alias!(
                staged,
                &client,
                &plan,
                topology_fixture.publication_root()
            );
            let database = fixture.state_db.clone();
            arm_before_immediate_pre_promotion_terminal_check(move || {
                database.delete_boot_publication_receipt_body_for_test(fingerprint)
            });
            let assessments =
                arm_exact_alias_assessments(topology_fixture.publication_root(), 2);
            let error = terminal.promote_terminal_receipt(&client).unwrap_err();
            assert_eq!(fixture_boot_namespace_assessments_remaining(), 0);
            drop(assessments);
            assert!(matches!(
                error,
                ActiveReblitBootReceiptPromotionError::PrePromotionPending(_),
            ));
            assert!(fixture.state_db.boot_publication_receipt_state().is_err());
        }
    );
}

#[test]
fn same_bytes_different_journal_inode_before_staged_revalidation_fails_before_promotion() {
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
                .join("pre-validation-displaced-transition");
            let callback_canonical = canonical.clone();
            let callback_displaced = displaced.clone();
            arm_before_immediate_pre_promotion_terminal_check(move || {
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
                arm_exact_alias_assessments(topology_fixture.publication_root(), 2);
            let error = terminal.promote_terminal_receipt(&client).unwrap_err();
            assert_eq!(fixture_boot_namespace_assessments_remaining(), 0);
            drop(assessments);
            assert!(matches!(
                error,
                ActiveReblitBootReceiptPromotionError::PrePromotionPending(_),
            ));
            let state = fixture.state_db.boot_publication_receipt_state().unwrap();
            assert_eq!(state.pending().unwrap().fingerprint(), fingerprint);
            assert!(state.head().committed().is_none());
            fs::remove_file(&canonical).unwrap();
            fs::rename(displaced, canonical).unwrap();
        }
    );
}
