use diesel::connection::SimpleConnection as _;

use super::*;
use crate::db::Error as DatabaseError;

#[test]
fn missing_foreign_and_conflicting_preimages_fail_without_mutation() {
    let empty = Database::new(":memory:").unwrap();
    let requested = receipt('6', None, 0x61);
    assert!(matches!(
        empty.promote_boot_publication_receipt(&requested),
        Err(BootPublicationReceiptPromotionError::MissingPending)
    ));

    let database = Database::new(":memory:").unwrap();
    let staged = receipt('7', None, 0x71);
    stage(&database, &staged);
    let before = database.boot_publication_receipt_state().unwrap();
    let foreign_transition = receipt('8', None, 0x81);
    assert!(matches!(
        database.promote_boot_publication_receipt(&foreign_transition),
        Err(BootPublicationReceiptPromotionError::PendingTransitionMismatch { .. })
    ));
    assert_eq!(database.boot_publication_receipt_state().unwrap(), before);

    let same_transition_other_body = receipt('7', None, 0x72);
    assert!(matches!(
        database.promote_boot_publication_receipt(&same_transition_other_body),
        Err(BootPublicationReceiptPromotionError::PendingFingerprintMismatch { .. })
    ));
    assert_eq!(database.boot_publication_receipt_state().unwrap(), before);

    let wrong_predecessor = receipt(
        '7',
        Some(BootPublicationReceiptFingerprint::from_bytes([0x73; 32])),
        0x73,
    );
    assert!(matches!(
        database.promote_boot_publication_receipt(&wrong_predecessor),
        Err(BootPublicationReceiptPromotionError::CommittedPredecessorMismatch { .. })
    ));
    assert_eq!(database.boot_publication_receipt_state().unwrap(), before);
}

#[test]
fn conditional_and_terminal_races_roll_back_to_the_exact_pending_state() {
    let before_race = Database::new(":memory:").unwrap();
    let first = receipt('9', None, 0x91);
    stage(&before_race, &first);
    let before = before_race.boot_publication_receipt_state().unwrap();
    arm_before_head_update(|connection| {
        replace_pending_fingerprint(
            connection,
            BootPublicationReceiptFingerprint::from_bytes([0x92; 32]),
        );
    });
    assert!(matches!(
        before_race.promote_boot_publication_receipt(&first),
        Err(BootPublicationReceiptPromotionError::HeadUpdateRowMismatch { changed: 0 })
    ));
    assert_eq!(before_race.boot_publication_receipt_state().unwrap(), before);

    let after_race = Database::new(":memory:").unwrap();
    let second = receipt('a', None, 0xa1);
    stage(&after_race, &second);
    let before = after_race.boot_publication_receipt_state().unwrap();
    arm_after_head_update_before_commit(|connection| {
        replace_committed_fingerprint(
            connection,
            BootPublicationReceiptFingerprint::from_bytes([0xa2; 32]),
        );
    });
    assert!(matches!(
        after_race.promote_boot_publication_receipt(&second),
        Err(BootPublicationReceiptPromotionError::State(
            BootPublicationReceiptStateError::DanglingReference { .. }
        ))
    ));
    assert_eq!(after_race.boot_publication_receipt_state().unwrap(), before);
}

#[test]
fn well_formed_nonterminal_revalidation_rolls_back_instead_of_committing() {
    let database = Database::new(":memory:").unwrap();
    let pending = receipt('b', None, 0xb1);
    stage(&database, &pending);
    let before = database.boot_publication_receipt_state().unwrap();
    let callback_receipt = receipt('b', None, 0xb1);
    arm_after_head_update_before_commit(move |connection| {
        restore_pending_head(connection, &callback_receipt);
    });

    assert!(matches!(
        database.promote_boot_publication_receipt(&pending),
        Err(BootPublicationReceiptPromotionError::TerminalRevalidationMismatch)
    ));
    assert_eq!(database.boot_publication_receipt_state().unwrap(), before);
}

#[test]
fn genuine_commit_failure_reconciles_the_rolled_back_pending_state() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.db");
    let url = path.to_str().unwrap();
    let database = Database::new(url).unwrap();
    database.conn.exec(|connection| {
        connection
            .batch_execute(
                "PRAGMA foreign_keys = ON; \
                 CREATE TABLE promotion_commit_parent (id INTEGER PRIMARY KEY); \
                 CREATE TABLE promotion_commit_child ( \
                     parent_id INTEGER NOT NULL, \
                     FOREIGN KEY (parent_id) REFERENCES promotion_commit_parent(id) \
                         DEFERRABLE INITIALLY DEFERRED \
                 );",
            )
            .unwrap();
    });
    let pending = receipt('c', None, 0xc1);
    stage(&database, &pending);
    let before = database.boot_publication_receipt_state().unwrap();
    arm_after_head_update_before_commit(|connection| {
        connection
            .batch_execute("INSERT INTO promotion_commit_child (parent_id) VALUES (1)")
            .unwrap();
    });

    let result = database.promote_boot_publication_receipt(&pending);
    assert!(matches!(
        &result,
        Err(BootPublicationReceiptPromotionError::CommitReport {
            durable: BootPublicationReceiptPromotionDurableState::Pending,
            ..
        })
    ), "{result:?}");
    assert_eq!(database.boot_publication_receipt_state().unwrap(), before);
    assert_eq!(
        database.promote_boot_publication_receipt(&pending).unwrap(),
        BootPublicationReceiptPromotionOutcome::Promoted,
    );
    let promoted = database.boot_publication_receipt_state().unwrap();
    drop(database);

    let reopened = Database::new(url).unwrap();
    assert_eq!(reopened.boot_publication_receipt_state().unwrap(), promoted);
}

#[test]
fn storage_failure_rolls_back_and_ambiguous_success_is_exactly_classified() {
    let rejected = Database::new(":memory:").unwrap();
    let pending = receipt('d', None, 0xd1);
    stage(&rejected, &pending);
    let before = rejected.boot_publication_receipt_state().unwrap();
    rejected.conn.exec(|connection| {
        connection
            .batch_execute(
                "CREATE TRIGGER reject_receipt_promotion \
                 BEFORE UPDATE OF committed_receipt_sha256 ON boot_publication_receipt_head \
                 BEGIN SELECT RAISE(ABORT, 'synthetic promotion failure'); END;",
            )
            .unwrap();
    });
    assert!(matches!(
        rejected.promote_boot_publication_receipt(&pending),
        Err(BootPublicationReceiptPromotionError::Database(_))
    ));
    assert_eq!(rejected.boot_publication_receipt_state().unwrap(), before);

    let ambiguous = Database::new(":memory:").unwrap();
    let pending = receipt('e', None, 0xe1);
    stage(&ambiguous, &pending);
    arm_after_commit_before_return(|| Err(DatabaseError::RowNotFound));
    assert!(matches!(
        ambiguous.promote_boot_publication_receipt(&pending),
        Err(BootPublicationReceiptPromotionError::CommitReport {
            durable: BootPublicationReceiptPromotionDurableState::Promoted,
            ..
        })
    ));
    let state = ambiguous.boot_publication_receipt_state().unwrap();
    assert_eq!(state.committed(), Some(&pending));
    assert!(state.pending().is_none());
    assert_eq!(
        ambiguous.promote_boot_publication_receipt(&pending).unwrap(),
        BootPublicationReceiptPromotionOutcome::AlreadyPromoted,
    );
}
