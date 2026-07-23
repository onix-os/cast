//! Durable installed-receipt head and sequential publication contracts.

use std::time::{Duration, Instant};

use diesel::{ExpressionMethods as _, QueryDsl as _, RunQueryDsl as _};

use super::*;
use crate::boot_publication::{
    BootPublicationDestination, BootPublicationDestinations,
    BootPublicationHistoricalRuntimeWitness, BootPublicationOutput,
    BootPublicationOutputProvenanceClaim, BootPublicationOutputRole,
    BootPublicationPublicationPhase, BootPublicationReceiptBody,
    BootPublicationRoot, BootPublicationSha256, BootPublicationXxh3,
    prepare_boot_publication_receipt,
};
use crate::db::state::{
    BootPublicationReceiptPromotionOutcome, BootPublicationReceiptStageOutcome,
    schema::boot_publication_receipts,
};

const ESP_PARTUUID: &str = "11111111-2222-3333-4444-555555555555";

fn transition(digit: char) -> TransitionId {
    TransitionId::parse(digit.to_string().repeat(TransitionId::TEXT_LENGTH)).unwrap()
}

fn receipt(
    digit: char,
    predecessor: Option<BootPublicationReceiptFingerprint>,
    salt: u8,
) -> CanonicalBootPublicationReceipt {
    let body = BootPublicationReceiptBody::new(
        transition(digit),
        predecessor,
        BootPublicationSha256::from_bytes([salt; 32]),
        BootPublicationSha256::from_bytes([salt.wrapping_add(1); 32]),
        BootPublicationDestinations::boot_aliases_esp(BootPublicationDestination::new(
            ESP_PARTUUID,
            1,
            BootPublicationHistoricalRuntimeWitness::new(
                2_049,
                100 + u64::from(salt),
                10 + u64::from(salt),
                8,
                1,
                Some(77 + u64::from(salt)),
            ),
        )),
        vec![BootPublicationOutput::new(
            BootPublicationRoot::Boot,
            BootPublicationPublicationPhase::Payload,
            BootPublicationOutputRole::Payload,
            format!("EFI/cast/vmlinuz-{salt:02x}"),
            0o644,
            BootPublicationXxh3::from_u128(u128::from(salt) + 1),
            u64::from(salt) + 1,
            BootPublicationSha256::from_bytes([salt.wrapping_add(2); 32]),
            BootPublicationOutputProvenanceClaim::UnclaimedAbsent,
        )],
    )
    .unwrap();
    prepare_boot_publication_receipt(body).unwrap()
}

fn pair(receipt: &CanonicalBootPublicationReceipt) -> BootPublicationReceiptPair {
    BootPublicationReceiptPair {
        committed: receipt.body().committed_predecessor(),
        pending: receipt.fingerprint(),
    }
}

fn stage_and_promote(database: &Database, receipt: &CanonicalBootPublicationReceipt) {
    assert_eq!(
        database.stage_boot_publication_receipt(receipt).unwrap(),
        BootPublicationReceiptStageOutcome::Staged,
    );
    assert_eq!(
        database
            .promote_boot_publication_receipt(
                receipt,
                Instant::now() + Duration::from_secs(60),
            )
            .unwrap(),
        BootPublicationReceiptPromotionOutcome::Promoted,
    );
}

fn stored_body(
    database: &Database,
    fingerprint: BootPublicationReceiptFingerprint,
) -> Vec<u8> {
    database.conn.exec(|connection| {
        boot_publication_receipts::table
            .filter(
                boot_publication_receipts::receipt_sha256
                    .eq(fingerprint.as_bytes().as_slice()),
            )
            .select(boot_publication_receipts::canonical_body)
            .first(connection)
            .unwrap()
    })
}

fn receipt_count(database: &Database) -> i64 {
    database.conn.exec(|connection| {
        boot_publication_receipts::table
            .count()
            .get_result(connection)
            .unwrap()
    })
}

#[test]
fn exact_promoted_head_is_the_durable_installed_receipt() {
    let database = Database::new(":memory:").unwrap();
    let installed = receipt('a', None, 0x11);
    stage_and_promote(&database, &installed);
    let expected = pair(&installed);

    let authenticated = database
        .load_exact_promoted_boot_publication_receipt_state(
            installed.body().transition_id(),
            &expected,
        )
        .unwrap();
    assert_eq!(authenticated.head().committed(), Some(installed.fingerprint()));
    assert!(authenticated.head().pending().is_none());
    assert_eq!(authenticated.committed(), Some(&installed));
    assert!(authenticated.pending().is_none());

    assert!(matches!(
        database.load_exact_promoted_boot_publication_receipt_state(
            &transition('b'),
            &expected,
        ),
        Err(ExactPromotedBootPublicationReceiptStateError::TransitionMismatch { .. })
    ));
    let reauthenticated = database
        .load_exact_promoted_boot_publication_receipt_state(
            installed.body().transition_id(),
            &expected,
        )
        .unwrap();
    assert_eq!(reauthenticated, authenticated);
}

#[test]
fn installed_receipt_a_becomes_b_predecessor_and_both_bodies_remain_immutable() {
    let database = Database::new(":memory:").unwrap();
    let installed_a = receipt('c', None, 0x21);
    stage_and_promote(&database, &installed_a);
    let body_a = installed_a.canonical_body().to_vec();

    let installed_b = receipt('d', Some(installed_a.fingerprint()), 0x31);
    let body_b = installed_b.canonical_body().to_vec();
    assert_eq!(
        database.stage_boot_publication_receipt(&installed_b).unwrap(),
        BootPublicationReceiptStageOutcome::Staged,
    );
    let staged_b = database.boot_publication_receipt_state().unwrap();
    assert_eq!(staged_b.head().committed(), Some(installed_a.fingerprint()));
    assert_eq!(staged_b.receipt_pair_for(installed_b.body().transition_id()), Some(pair(&installed_b)));
    assert_eq!(staged_b.pending(), Some(&installed_b));

    assert_eq!(
        database
            .promote_boot_publication_receipt(
                &installed_b,
                Instant::now() + Duration::from_secs(60),
            )
            .unwrap(),
        BootPublicationReceiptPromotionOutcome::Promoted,
    );
    let authenticated_b = database
        .load_exact_promoted_boot_publication_receipt_state(
            installed_b.body().transition_id(),
            &pair(&installed_b),
        )
        .unwrap();
    assert_eq!(authenticated_b.head().committed(), Some(installed_b.fingerprint()));
    assert!(authenticated_b.head().pending().is_none());
    assert_eq!(authenticated_b.committed(), Some(&installed_b));
    assert!(authenticated_b.pending().is_none());
    assert_eq!(receipt_count(&database), 2);
    assert_eq!(stored_body(&database, installed_a.fingerprint()), body_a);
    assert_eq!(stored_body(&database, installed_b.fingerprint()), body_b);
}
