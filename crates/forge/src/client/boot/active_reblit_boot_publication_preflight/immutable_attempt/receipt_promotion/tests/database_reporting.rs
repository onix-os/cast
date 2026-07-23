use super::*;
use crate::db::state::{
    BootPublicationReceiptPromotionDurableState,
    BootPublicationReceiptPromotionError,
};

#[test]
fn ambiguous_commit_report_preserves_promoted_classification_without_success_authority() {
    with_staged_alias_attempt!(
        |fixture, topology_fixture, plan, _inventory, client, staged, _expected_record, fingerprint| {
            let terminal = publish_terminal_alias!(
                staged,
                &client,
                &plan,
                topology_fixture.publication_root()
            );
            db::state::arm_boot_publication_receipt_promotion_after_commit_error(
                db::Error::RowNotFound,
            );
            let assessments =
                arm_exact_alias_assessments(topology_fixture.publication_root(), 2);
            let error = terminal.promote_terminal_receipt(&client).unwrap_err();
            assert_eq!(fixture_boot_namespace_assessments_remaining(), 0);
            drop(assessments);
            assert_eq!(error.durable_promotion_outcome(), None);
            assert_eq!(
                error.durable_receipt_state(),
                Some(BootPublicationReceiptPromotionDurableState::Promoted),
            );
            assert!(matches!(
                error,
                ActiveReblitBootReceiptPromotionError::DatabasePromotion(
                    BootPublicationReceiptPromotionError::CommitReport {
                        durable: BootPublicationReceiptPromotionDurableState::Promoted,
                        ..
                    },
                ),
            ));
            assert_promoted_state(
                &fixture.state_db.boot_publication_receipt_state().unwrap(),
                fingerprint,
            );
        }
    );
}
