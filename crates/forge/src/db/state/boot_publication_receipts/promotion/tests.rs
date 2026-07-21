use diesel::{ExpressionMethods as _, QueryDsl as _, RunQueryDsl as _};

use super::*;
use crate::boot_publication::{
    BootPublicationDestination, BootPublicationDestinations,
    BootPublicationHistoricalRuntimeWitness, BootPublicationOutput,
    BootPublicationOutputProvenanceClaim, BootPublicationOutputRole,
    BootPublicationPublicationPhase, BootPublicationReceiptBody,
    BootPublicationReceiptFingerprint, BootPublicationRoot,
    BootPublicationSha256, BootPublicationXxh3,
    prepare_boot_publication_receipt,
};
use crate::db::state::{
    BootPublicationReceiptStageOutcome,
    schema::{boot_publication_receipt_head, boot_publication_receipts},
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

fn stage(database: &Database, receipt: &CanonicalBootPublicationReceipt) {
    assert_eq!(
        database.stage_boot_publication_receipt(receipt).unwrap(),
        BootPublicationReceiptStageOutcome::Staged,
    );
}

fn stage_and_promote(database: &Database, receipt: &CanonicalBootPublicationReceipt) {
    stage(database, receipt);
    assert_eq!(
        database.promote_boot_publication_receipt(receipt).unwrap(),
        BootPublicationReceiptPromotionOutcome::Promoted,
    );
}

fn receipt_row_count(database: &Database) -> i64 {
    database.conn.exec(|connection| {
        boot_publication_receipts::table
            .count()
            .get_result(connection)
            .unwrap()
    })
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

fn replace_pending_fingerprint(
    connection: &mut SqliteConnection,
    fingerprint: BootPublicationReceiptFingerprint,
) {
    let changed = diesel::update(boot_publication_receipt_head::table)
        .set(
            boot_publication_receipt_head::pending_receipt_sha256
                .eq(Some(fingerprint.as_bytes().as_slice())),
        )
        .execute(connection)
        .unwrap();
    assert_eq!(changed, 1);
}

fn replace_committed_fingerprint(
    connection: &mut SqliteConnection,
    fingerprint: BootPublicationReceiptFingerprint,
) {
    let changed = diesel::update(boot_publication_receipt_head::table)
        .set(
            boot_publication_receipt_head::committed_receipt_sha256
                .eq(Some(fingerprint.as_bytes().as_slice())),
        )
        .execute(connection)
        .unwrap();
    assert_eq!(changed, 1);
}

fn restore_pending_head(
    connection: &mut SqliteConnection,
    receipt: &CanonicalBootPublicationReceipt,
) {
    let committed = receipt.body().committed_predecessor();
    let pending = receipt.fingerprint();
    let changed = diesel::update(boot_publication_receipt_head::table)
        .set((
            boot_publication_receipt_head::committed_receipt_sha256.eq(
                committed
                    .as_ref()
                    .map(|fingerprint| fingerprint.as_bytes().as_slice()),
            ),
            boot_publication_receipt_head::pending_transition_id
                .eq(Some(receipt.body().transition_id().as_str())),
            boot_publication_receipt_head::pending_receipt_sha256
                .eq(Some(pending.as_bytes().as_slice())),
        ))
        .execute(connection)
        .unwrap();
    assert_eq!(changed, 1);
}

#[path = "tests/api.rs"]
mod api;
#[path = "tests/failures.rs"]
mod failures;
#[path = "tests/corruption.rs"]
mod corruption;
