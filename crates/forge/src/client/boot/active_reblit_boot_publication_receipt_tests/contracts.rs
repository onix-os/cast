use super::*;

#[test]
fn only_the_exact_boot_sync_started_predecessor_is_admitted() {
    with_bound_alias_plan!(|_fixture, plan| {
        let inventory = plan.prepare_desired_publication_inventory().unwrap();
        let claims = inert_claim_bindings(&inventory);
        let error = plan
            .prepare_complete_boot_publication_receipt(&inventory, &preparing_record(), None, &claims)
            .unwrap_err();
        assert!(matches!(
            error,
            ActiveReblitBootPublicationReceiptError::InvalidPredecessor(
                CodecError::IllegalPhaseAdvance { .. }
            )
        ));
    });
}

#[test]
fn provenance_claims_must_cover_the_exact_canonical_inventory() {
    with_bound_alias_plan!(|_fixture, plan| {
        let predecessor = exact_boot_sync_predecessor();
        let inventory = plan.prepare_desired_publication_inventory().unwrap();
        let mut short = inert_claim_bindings(&inventory);
        short.pop();
        assert!(matches!(
            plan.prepare_complete_boot_publication_receipt(&inventory, &predecessor, None, &short),
            Err(
                ActiveReblitBootPublicationReceiptError::ProvenanceClaimCountMismatch {
                    expected,
                    actual,
                }
            ) if expected == inventory.outputs().len() && actual == short.len()
        ));
    });
}

#[test]
fn provenance_claim_bindings_reject_a_same_length_permutation() {
    with_bound_alias_plan!(|_fixture, plan| {
        let predecessor = exact_boot_sync_predecessor();
        let inventory = plan.prepare_desired_publication_inventory().unwrap();
        let mut permuted = inert_claim_bindings(&inventory);
        assert!(permuted.len() > 1);
        permuted.swap(0, 1);

        assert!(matches!(
            plan.prepare_complete_boot_publication_receipt(
                &inventory,
                &predecessor,
                None,
                &permuted,
            ),
            Err(
                ActiveReblitBootPublicationReceiptError::ProvenanceClaimBindingMismatch {
                    index: 0
                }
            )
        ));
    });
}

#[test]
fn mapper_rejects_a_substituted_deadline_and_checks_expiry_at_entry() {
    with_bound_alias_plan!(|_fixture, plan| {
        let predecessor = exact_boot_sync_predecessor();
        let inventory = plan.prepare_desired_publication_inventory().unwrap();
        let claims = inert_claim_bindings(&inventory);
        let mut now = Instant::now;
        assert!(matches!(
            prepare_bound_receipt_with_clock(
                &plan,
                &inventory,
                &predecessor,
                None,
                &claims,
                plan.input_deadline() + Duration::from_nanos(1),
                &mut now,
            ),
            Err(ActiveReblitBootPublicationReceiptError::DeadlineMismatch { .. })
        ));

        let after_deadline = plan.input_deadline() + Duration::from_nanos(1);
        let mut expired = || after_deadline;
        assert!(matches!(
            prepare_bound_receipt_with_clock(
                &plan,
                &inventory,
                &predecessor,
                None,
                &claims,
                plan.input_deadline(),
                &mut expired,
            ),
            Err(ActiveReblitBootPublicationReceiptError::DeadlineExceeded {
                checkpoint: "receipt mapping entry"
            })
        ));
    });
}
