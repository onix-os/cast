use diesel::{RunQueryDsl as _, sql_types::{Binary, Text}};

use super::*;
use crate::boot_publication::{
    BootPublicationDestination, BootPublicationDestinations, BootPublicationHistoricalRuntimeWitness,
    BootPublicationOutput, BootPublicationOutputProvenanceClaim, BootPublicationOutputRole,
    BootPublicationPublicationPhase, BootPublicationReceiptBody, BootPublicationRoot,
    BootPublicationSha256, BootPublicationXxh3, prepare_boot_publication_receipt,
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

fn insert_raw_receipt(database: &Database, receipt: &CanonicalBootPublicationReceipt) {
    database.conn.exec(|connection| {
        diesel::sql_query(
            "INSERT INTO boot_publication_receipts (receipt_sha256, transition_id, canonical_body) \
             VALUES (?, ?, ?)",
        )
        .bind::<Binary, _>(receipt.fingerprint().as_bytes().as_slice())
        .bind::<Text, _>(receipt.body().transition_id().as_str())
        .bind::<Binary, _>(receipt.canonical_body())
        .execute(connection)
        .unwrap();
    });
}

fn receipt_row_count(database: &Database) -> i64 {
    database.conn.exec(|connection| {
        boot_publication_receipts::table
            .count()
            .get_result(connection)
            .unwrap()
    })
}

#[path = "tests/api.rs"]
mod api;
#[path = "tests/corruption.rs"]
mod corruption;
#[path = "tests/migration.rs"]
mod migration;
