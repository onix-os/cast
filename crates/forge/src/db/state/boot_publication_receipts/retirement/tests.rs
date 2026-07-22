use std::time::{Duration, Instant};

use diesel::{
    ExpressionMethods as _, QueryDsl as _, RunQueryDsl as _,
    connection::SimpleConnection as _,
};

use super::*;
use crate::boot_publication::{
    BootPublicationDestination, BootPublicationDestinations,
    BootPublicationHistoricalRuntimeWitness, BootPublicationOutput,
    BootPublicationOutputProvenanceClaim, BootPublicationOutputRole,
    BootPublicationPublicationPhase, BootPublicationReceiptBody,
    BootPublicationRoot, BootPublicationSha256, BootPublicationXxh3,
    CanonicalBootPublicationReceipt, prepare_boot_publication_receipt,
};
use crate::db::state::{
    BootPublicationReceiptPromotionOutcome, BootPublicationReceiptStageOutcome,
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

fn delete_body(
    database: &Database,
    fingerprint: BootPublicationReceiptFingerprint,
) {
    let deleted = database.conn.exec(|connection| {
        diesel::delete(
            boot_publication_receipts::table.filter(
                boot_publication_receipts::receipt_sha256
                    .eq(fingerprint.as_bytes().as_slice()),
            ),
        )
        .execute(connection)
        .unwrap()
    });
    assert_eq!(deleted, 1);
}

fn replace_committed(
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

#[test]
fn exact_promoted_head_retires_once_and_preserves_immutable_chain() {
    let database = Database::new(":memory:").unwrap();
    let predecessor = receipt('1', None, 0x11);
    stage_and_promote(&database, &predecessor);
    let current = receipt('2', Some(predecessor.fingerprint()), 0x21);
    stage_and_promote(&database, &current);
    let exact_pair = pair(&current);
    let predecessor_body = stored_body(&database, predecessor.fingerprint());
    let current_body = stored_body(&database, current.fingerprint());

    assert_eq!(
        database
            .retire_promoted_boot_publication_receipt_head(
                current.body().transition_id(),
                &exact_pair,
            )
            .unwrap(),
        BootPublicationReceiptRetirementOutcome::Retired,
    );

    let retired = database.boot_publication_receipt_state().unwrap();
    assert!(retired.head().committed().is_none());
    assert!(retired.head().pending().is_none());
    assert!(retired.committed().is_none());
    assert!(retired.pending().is_none());
    assert_eq!(receipt_row_count(&database), 2);
    assert_eq!(stored_body(&database, predecessor.fingerprint()), predecessor_body);
    assert_eq!(stored_body(&database, current.fingerprint()), current_body);
}

#[test]
fn exact_retired_retry_is_read_only_and_reauthenticates_retained_chain() {
    let database = Database::new(":memory:").unwrap();
    let predecessor = receipt('3', None, 0x31);
    stage_and_promote(&database, &predecessor);
    let current = receipt('4', Some(predecessor.fingerprint()), 0x41);
    stage_and_promote(&database, &current);
    let exact_pair = pair(&current);
    database
        .retire_promoted_boot_publication_receipt_head(
            current.body().transition_id(),
            &exact_pair,
        )
        .unwrap();
    database.conn.exec(|connection| {
        connection
            .batch_execute(
                "CREATE TRIGGER reject_retired_head_write BEFORE UPDATE ON boot_publication_receipt_head BEGIN SELECT RAISE(ABORT, 'retired retry wrote head'); END; \
                 CREATE TRIGGER reject_retired_body_update BEFORE UPDATE ON boot_publication_receipts BEGIN SELECT RAISE(ABORT, 'retired retry updated body'); END; \
                 CREATE TRIGGER reject_retired_body_delete BEFORE DELETE ON boot_publication_receipts BEGIN SELECT RAISE(ABORT, 'retired retry deleted body'); END;",
            )
            .unwrap();
    });
    assert_eq!(
        database
            .retire_promoted_boot_publication_receipt_head(
                current.body().transition_id(),
                &exact_pair,
            )
            .unwrap(),
        BootPublicationReceiptRetirementOutcome::AlreadyRetired,
    );
    assert_eq!(receipt_row_count(&database), 2);

    let missing_current = Database::new(":memory:").unwrap();
    let current = receipt('5', None, 0x51);
    stage_and_promote(&missing_current, &current);
    let exact_pair = pair(&current);
    missing_current
        .retire_promoted_boot_publication_receipt_head(
            current.body().transition_id(),
            &exact_pair,
        )
        .unwrap();
    delete_body(&missing_current, current.fingerprint());
    assert!(matches!(
        missing_current.retire_promoted_boot_publication_receipt_head(
            current.body().transition_id(),
            &exact_pair,
        ),
        Err(BootPublicationReceiptRetirementError::State(
            BootPublicationReceiptStateError::DanglingReference {
                reference: ReceiptReference::Retired,
                ..
            }
        ))
    ));

    let missing_predecessor = Database::new(":memory:").unwrap();
    let predecessor = receipt('6', None, 0x61);
    stage_and_promote(&missing_predecessor, &predecessor);
    let current = receipt('7', Some(predecessor.fingerprint()), 0x71);
    stage_and_promote(&missing_predecessor, &current);
    let exact_pair = pair(&current);
    missing_predecessor
        .retire_promoted_boot_publication_receipt_head(
            current.body().transition_id(),
            &exact_pair,
        )
        .unwrap();
    delete_body(&missing_predecessor, predecessor.fingerprint());
    assert!(matches!(
        missing_predecessor.retire_promoted_boot_publication_receipt_head(
            current.body().transition_id(),
            &exact_pair,
        ),
        Err(BootPublicationReceiptRetirementError::State(
            BootPublicationReceiptStateError::DanglingReference {
                reference: ReceiptReference::CommittedPredecessor,
                ..
            }
        ))
    ));
}

#[test]
fn pending_foreign_identity_and_corrupt_body_fail_without_retirement() {
    let pending_database = Database::new(":memory:").unwrap();
    let pending = receipt('8', None, 0x81);
    pending_database.stage_boot_publication_receipt(&pending).unwrap();
    let pending_before = pending_database.boot_publication_receipt_state().unwrap();
    assert!(matches!(
        pending_database.retire_promoted_boot_publication_receipt_head(
            pending.body().transition_id(),
            &pair(&pending),
        ),
        Err(BootPublicationReceiptRetirementError::StateMismatch { .. })
    ));
    assert_eq!(
        pending_database.boot_publication_receipt_state().unwrap(),
        pending_before,
    );

    let foreign_database = Database::new(":memory:").unwrap();
    let current = receipt('9', None, 0x91);
    stage_and_promote(&foreign_database, &current);
    let before = foreign_database.boot_publication_receipt_state().unwrap();
    assert!(matches!(
        foreign_database.retire_promoted_boot_publication_receipt_head(
            &transition('a'),
            &pair(&current),
        ),
        Err(BootPublicationReceiptRetirementError::StateMismatch { .. })
    ));
    let foreign_pair = BootPublicationReceiptPair {
        committed: None,
        pending: BootPublicationReceiptFingerprint::from_bytes([0xa1; 32]),
    };
    assert!(matches!(
        foreign_database.retire_promoted_boot_publication_receipt_head(
            current.body().transition_id(),
            &foreign_pair,
        ),
        Err(BootPublicationReceiptRetirementError::StateMismatch { .. })
    ));
    assert_eq!(foreign_database.boot_publication_receipt_state().unwrap(), before);

    let corrupt_database = Database::new(":memory:").unwrap();
    let current = receipt('b', None, 0xb1);
    let foreign = receipt('c', None, 0xc1);
    stage_and_promote(&corrupt_database, &current);
    let head = corrupt_database.boot_publication_receipt_head().unwrap();
    corrupt_database.conn.exec(|connection| {
        let changed = diesel::update(
            boot_publication_receipts::table.filter(
                boot_publication_receipts::receipt_sha256
                    .eq(current.fingerprint().as_bytes().as_slice()),
            ),
        )
        .set(boot_publication_receipts::canonical_body.eq(foreign.canonical_body()))
        .execute(connection)
        .unwrap();
        assert_eq!(changed, 1);
    });
    assert!(matches!(
        corrupt_database.retire_promoted_boot_publication_receipt_head(
            current.body().transition_id(),
            &pair(&current),
        ),
        Err(BootPublicationReceiptRetirementError::State(
            BootPublicationReceiptStateError::BodyFingerprintMismatch { .. }
        ))
    ));
    assert_eq!(corrupt_database.boot_publication_receipt_head().unwrap(), head);
}

#[test]
fn transaction_and_commit_report_seams_never_claim_false_success() {
    let before_update = Database::new(":memory:").unwrap();
    let current = receipt('d', None, 0xd1);
    stage_and_promote(&before_update, &current);
    let before = before_update.boot_publication_receipt_state().unwrap();
    arm_before_head_update(|connection| {
        replace_committed(
            connection,
            BootPublicationReceiptFingerprint::from_bytes([0xd2; 32]),
        );
    });
    assert!(matches!(
        before_update.retire_promoted_boot_publication_receipt_head(
            current.body().transition_id(),
            &pair(&current),
        ),
        Err(BootPublicationReceiptRetirementError::HeadUpdateRowMismatch { changed: 0 })
    ));
    assert_eq!(before_update.boot_publication_receipt_state().unwrap(), before);

    let after_update = Database::new(":memory:").unwrap();
    let current = receipt('e', None, 0xe1);
    stage_and_promote(&after_update, &current);
    let before = after_update.boot_publication_receipt_state().unwrap();
    let fingerprint = current.fingerprint();
    arm_after_head_update_before_commit(move |connection| {
        replace_committed(connection, fingerprint);
    });
    assert!(matches!(
        after_update.retire_promoted_boot_publication_receipt_head(
            current.body().transition_id(),
            &pair(&current),
        ),
        Err(BootPublicationReceiptRetirementError::TerminalRevalidationMismatch)
    ));
    assert_eq!(after_update.boot_publication_receipt_state().unwrap(), before);

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.db");
    let commit_failure = Database::new(path.to_str().unwrap()).unwrap();
    commit_failure.conn.exec(|connection| {
        connection
            .batch_execute(
                "PRAGMA foreign_keys = ON; \
                 CREATE TABLE retirement_commit_parent (id INTEGER PRIMARY KEY); \
                 CREATE TABLE retirement_commit_child (parent_id INTEGER NOT NULL, FOREIGN KEY (parent_id) REFERENCES retirement_commit_parent(id) DEFERRABLE INITIALLY DEFERRED);",
            )
            .unwrap();
    });
    let current = receipt('f', None, 0xf1);
    stage_and_promote(&commit_failure, &current);
    let before = commit_failure.boot_publication_receipt_state().unwrap();
    arm_after_head_update_before_commit(|connection| {
        connection
            .batch_execute("INSERT INTO retirement_commit_child (parent_id) VALUES (1)")
            .unwrap();
    });
    let result = commit_failure.retire_promoted_boot_publication_receipt_head(
        current.body().transition_id(),
        &pair(&current),
    );
    assert!(matches!(
        result,
        Err(BootPublicationReceiptRetirementError::CommitReport {
            durable: BootPublicationReceiptRetirementDurableState::Promoted,
            ..
        })
    ));
    assert_eq!(commit_failure.boot_publication_receipt_state().unwrap(), before);

    let uncertain_success = Database::new(":memory:").unwrap();
    let current = receipt('0', None, 0x01);
    stage_and_promote(&uncertain_success, &current);
    arm_boot_publication_receipt_retirement_after_commit_error(DatabaseError::RowNotFound);
    assert!(matches!(
        uncertain_success.retire_promoted_boot_publication_receipt_head(
            current.body().transition_id(),
            &pair(&current),
        ),
        Err(BootPublicationReceiptRetirementError::CommitReport {
            durable: BootPublicationReceiptRetirementDurableState::Retired,
            ..
        })
    ));
    assert_eq!(
        uncertain_success
            .retire_promoted_boot_publication_receipt_head(
                current.body().transition_id(),
                &pair(&current),
            )
            .unwrap(),
        BootPublicationReceiptRetirementOutcome::AlreadyRetired,
    );
}

#[test]
fn body_and_head_drift_at_mutation_boundaries_roll_back_exactly() {
    let body_drift = Database::new(":memory:").unwrap();
    let current = receipt('a', None, 0xa2);
    let foreign = receipt('b', None, 0xb2);
    stage_and_promote(&body_drift, &current);
    let before = body_drift.boot_publication_receipt_state().unwrap();
    let fingerprint = current.fingerprint();
    let foreign_body = foreign.canonical_body().to_vec();
    arm_before_head_update(move |connection| {
        let changed = diesel::update(
            boot_publication_receipts::table.filter(
                boot_publication_receipts::receipt_sha256
                    .eq(fingerprint.as_bytes().as_slice()),
            ),
        )
        .set(boot_publication_receipts::canonical_body.eq(foreign_body))
        .execute(connection)
        .unwrap();
        assert_eq!(changed, 1);
    });
    assert!(matches!(
        body_drift.retire_promoted_boot_publication_receipt_head(
            current.body().transition_id(),
            &pair(&current),
        ),
        Err(BootPublicationReceiptRetirementError::State(
            BootPublicationReceiptStateError::BodyFingerprintMismatch { .. }
        ))
    ));
    assert_eq!(body_drift.boot_publication_receipt_state().unwrap(), before);

    let head_drift = Database::new(":memory:").unwrap();
    let current = receipt('c', None, 0xc2);
    stage_and_promote(&head_drift, &current);
    let before = head_drift.boot_publication_receipt_state().unwrap();
    arm_after_head_update_before_commit(|connection| {
        replace_committed(
            connection,
            BootPublicationReceiptFingerprint::from_bytes([0xc3; 32]),
        );
    });
    assert!(matches!(
        head_drift.retire_promoted_boot_publication_receipt_head(
            current.body().transition_id(),
            &pair(&current),
        ),
        Err(BootPublicationReceiptRetirementError::State(
            BootPublicationReceiptStateError::DanglingReference { .. }
        ))
    ));
    assert_eq!(head_drift.boot_publication_receipt_state().unwrap(), before);
}
