use std::os::unix::ffi::OsStrExt as _;

use sha2::{Digest as _, Sha256};

use super::*;
use crate::{
    boot_publication::BootPublicationSha256,
    transition_journal::encode as encode_transition_record,
};

#[test]
fn real_bound_alias_plan_maps_to_one_complete_authority_free_receipt() {
    with_bound_alias_plan!(|fixture, plan| {
        let original = support::TreeSnapshot::capture(&fixture.installation.root);
        let predecessor = exact_boot_sync_predecessor();
        let committed = Some(receipt_fingerprint(0x44));
        let inventory = plan.prepare_desired_publication_inventory().unwrap();
        let claims = inert_claim_bindings(&inventory);
        let expected_predecessor_sha256 = BootPublicationSha256::from_bytes(
            Sha256::digest(encode_transition_record(&predecessor).unwrap()).into(),
        );

        let receipt = plan
            .prepare_complete_boot_publication_receipt(&inventory, &predecessor, committed, &claims)
            .unwrap();
        let body = receipt.body();

        assert_eq!(body.transition_id(), &predecessor.transition_id);
        assert_eq!(body.committed_predecessor(), committed);
        assert_eq!(body.predecessor_journal_sha256(), expected_predecessor_sha256);
        assert_eq!(
            body.desired_inventory_sha256().as_bytes(),
            inventory.fingerprint().as_bytes()
        );
        assert!(body.destinations().aliases_esp());
        assert!(body.destinations().xbootldr().is_none());
        assert_eq!(body.outputs().len(), inventory.outputs().len());
        assert_eq!(
            body.outputs()
                .iter()
                .map(|output| output.provenance_claim())
                .collect::<Vec<_>>(),
            claims.iter().map(|claim| claim.claim()).collect::<Vec<_>>()
        );
        for (output, desired) in body.outputs().iter().zip(inventory.outputs()) {
            assert_eq!(output.relative_path().as_bytes(), desired.relative_path().as_os_str().as_bytes());
            assert_eq!(output.mode(), desired.mode());
            assert_eq!(output.xxh3().as_u128(), desired.checksum());
            assert_eq!(output.length(), desired.length());
            assert_eq!(output.content_sha256().as_bytes(), desired.content_identity().as_bytes());
        }

        let decoded = decode_boot_publication_receipt(receipt.canonical_body()).unwrap();
        assert_eq!(decoded.fingerprint(), receipt.fingerprint());
        assert_eq!(decoded.body(), receipt.body());
        assert_eq!(original, support::TreeSnapshot::capture(&fixture.installation.root));
    });
}

#[test]
fn committed_predecessor_and_claim_data_are_fingerprint_significant() {
    with_bound_alias_plan!(|_fixture, plan| {
        let predecessor = exact_boot_sync_predecessor();
        let inventory = plan.prepare_desired_publication_inventory().unwrap();
        let absent = claim_bindings(&inventory, |_| {
            BootPublicationOutputProvenanceClaim::UnclaimedAbsent
        });
        let claimed = claim_bindings(&inventory, |_| {
            BootPublicationOutputProvenanceClaim::ClaimedPublishedByCast
        });

        let without_committed = plan
            .prepare_complete_boot_publication_receipt(&inventory, &predecessor, None, &absent)
            .unwrap();
        let with_committed = plan
            .prepare_complete_boot_publication_receipt(
                &inventory,
                &predecessor,
                Some(receipt_fingerprint(0x77)),
                &absent,
            )
            .unwrap();
        let different_claims = plan
            .prepare_complete_boot_publication_receipt(&inventory, &predecessor, None, &claimed)
            .unwrap();

        assert_ne!(without_committed.fingerprint(), with_committed.fingerprint());
        assert_ne!(without_committed.fingerprint(), different_claims.fingerprint());
    });
}
