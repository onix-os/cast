use super::*;

fn system_triggers_complete(mut record: TransitionRecord) -> TransitionRecord {
    loop {
        match record.forward_successor(None) {
            Ok(successor) => record = successor,
            Err(CodecError::ExplicitBootSyncStartedSuccessorRequired) => return record,
            Err(error) => panic!("construct exact pre-boot record: {error}"),
        }
    }
}

fn second_boot_sync_journal(
    installation: &Installation,
) -> (
    TransitionJournalStore,
    TransitionRecord,
    TransitionJournalRecordBinding,
) {
    let cast = installation.retained_mutable_cast_directory().unwrap();
    let journal = TransitionJournalStore::open_in_retained_cast(cast, &installation.root).unwrap();
    let mut preparing = preparing_record();
    preparing.transition_id =
        TransitionId::parse("fedcba9876543210fedcba9876543210").unwrap();
    journal.create(&preparing).unwrap();
    let predecessor = system_triggers_complete(preparing);
    let mut current = journal.load_revalidated_retained_cast(cast).unwrap().unwrap();
    while current != predecessor {
        let successor = current.forward_successor(None).unwrap();
        journal.advance(&current, &successor).unwrap();
        current = successor;
    }
    let binding = journal.record_binding(cast, &predecessor).unwrap();
    (journal, predecessor, binding)
}

#[test]
fn promoted_a_is_authenticated_as_b_predecessor_and_retained_delta_input() {
    with_bound_staging_plan!(|fixture, plan, inventory, historical_claims| {
        let historical_predecessor = system_triggers_complete(preparing_record());
        let receipt_a = plan
            .prepare_complete_boot_publication_receipt(
                &inventory,
                &historical_predecessor,
                None,
                &historical_claims,
            )
            .unwrap();
        fixture
            .state_db
            .stage_boot_publication_receipt(&receipt_a)
            .unwrap();
        assert_eq!(
            fixture
                .state_db
                .promote_boot_publication_receipt(&receipt_a, plan.input_deadline())
                .unwrap(),
            BootPublicationReceiptPromotionOutcome::Promoted,
        );

        let client = staging_client(&fixture, fixture.state_db.clone());
        let (journal, predecessor, binding) =
            second_boot_sync_journal(&fixture.installation);
        let staged = stage_with_retained_stores(
            &fixture.installation,
            &fixture.state_db,
            &plan,
            &inventory,
            journal,
            predecessor,
            binding,
        )
        .unwrap();

        assert_eq!(
            staged.receipt().body().committed_predecessor(),
            Some(receipt_a.fingerprint()),
        );
        let fresh = staged.revalidate_against(&client).unwrap();
        assert!(fresh.prepared_delta().requests().iter().all(|request| {
            request.desired_expected().is_some()
                && request.installed_expected().is_some()
                && request.installed_is_owned()
        }));
        assert!(fresh.classified_delta().entries().iter().all(|entry| {
            entry.installed_expected().is_some()
                && entry.action() == ActiveReblitBootPublicationDeltaAction::PublishDesired
        }));
        let derived_claims = fresh
            .classified_delta()
            .derive_receipt_provenance_claims(&inventory)
            .unwrap();
        assert!(derived_claims.iter().copied().all(|claim| {
            claim.claim() == BootPublicationOutputProvenanceClaim::UnclaimedAbsent
        }));
    });
}
