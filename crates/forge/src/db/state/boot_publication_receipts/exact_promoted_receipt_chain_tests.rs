use std::time::{Duration, Instant};

use diesel::{
    Connection as _, ExpressionMethods as _, QueryDsl as _, RunQueryDsl as _,
    SqliteConnection, connection::SimpleConnection as _,
};

use super::*;
use crate::boot_publication::{
    BootPublicationDestination, BootPublicationDestinations,
    BootPublicationHistoricalRuntimeWitness, BootPublicationOutput,
    BootPublicationOutputProvenanceClaim, BootPublicationOutputRole,
    BootPublicationPublicationPhase, BootPublicationReceiptBody,
    BootPublicationRoot, BootPublicationSha256, BootPublicationXxh3,
    prepare_boot_publication_receipt,
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

fn receipt_pair(receipt: &CanonicalBootPublicationReceipt) -> BootPublicationReceiptPair {
    BootPublicationReceiptPair {
        committed: receipt.body().committed_predecessor(),
        pending: receipt.fingerprint(),
    }
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

#[test]
fn current_chain_admits_only_a_strictly_empty_database() {
    let empty = Database::new(":memory:").unwrap();
    assert_eq!(
        empty
            .load_current_exact_promoted_boot_publication_receipt_chain()
            .unwrap(),
        CurrentExactPromotedBootPublicationReceiptChain::Empty,
    );

    let orphaned = Database::new(":memory:").unwrap();
    let orphan = receipt('1', None, 0x11);
    orphaned.conn.exec(|connection| {
        insert_receipt(connection, &orphan).unwrap();
    });
    assert!(matches!(
        orphaned.load_current_exact_promoted_boot_publication_receipt_chain(),
        Err(
            CurrentExactPromotedBootPublicationReceiptChainError::ReceiptBodiesWithoutCommittedHead {
                count: 1,
            }
        )
    ));
}

#[test]
fn current_chain_derives_the_installed_identity_and_predecessor_from_storage() {
    let database = Database::new(":memory:").unwrap();
    let predecessor = receipt('2', None, 0x21);
    stage_and_promote(&database, &predecessor);
    let installed = receipt('3', Some(predecessor.fingerprint()), 0x31);
    stage_and_promote(&database, &installed);

    let current = database
        .load_current_exact_promoted_boot_publication_receipt_chain()
        .unwrap();
    let CurrentExactPromotedBootPublicationReceiptChain::Installed(chain) = current else {
        panic!("a committed receipt must load as installed");
    };
    assert_eq!(chain.installed_receipt(), &installed);
    assert_eq!(
        chain.committed_predecessor_receipt(),
        Some(&predecessor),
    );
}

#[test]
fn current_chain_rejects_pending_state_without_caller_correlation() {
    let database = Database::new(":memory:").unwrap();
    let pending = receipt('4', None, 0x41);
    stage(&database, &pending);

    assert!(matches!(
        database.load_current_exact_promoted_boot_publication_receipt_chain(),
        Err(CurrentExactPromotedBootPublicationReceiptChainError::ExactPromoted(
            ExactPromotedBootPublicationReceiptStateError::PendingHeadPresent {
                transition_id,
                fingerprint,
            }
        )) if transition_id == *pending.body().transition_id()
            && fingerprint == pending.fingerprint()
    ));
}

#[test]
fn current_chain_rejects_dangling_and_mismatched_immutable_bodies() {
    let dangling = Database::new(":memory:").unwrap();
    let predecessor = receipt('5', None, 0x51);
    stage_and_promote(&dangling, &predecessor);
    let installed = receipt('6', Some(predecessor.fingerprint()), 0x61);
    stage_and_promote(&dangling, &installed);
    dangling.delete_boot_publication_receipt_body_for_test(predecessor.fingerprint());
    assert!(matches!(
        dangling.load_current_exact_promoted_boot_publication_receipt_chain(),
        Err(CurrentExactPromotedBootPublicationReceiptChainError::ExactPromoted(
            ExactPromotedBootPublicationReceiptStateError::State(
                BootPublicationReceiptStateError::DanglingReference {
                    reference: ReceiptReference::CommittedPredecessor,
                    fingerprint,
                }
            )
        )) if fingerprint == predecessor.fingerprint()
    ));

    let mismatched = Database::new(":memory:").unwrap();
    let installed = receipt('7', None, 0x71);
    stage_and_promote(&mismatched, &installed);
    let foreign_transition = transition('8');
    mismatched.conn.exec(|connection| {
        let changed = diesel::update(
            boot_publication_receipts::table.filter(
                boot_publication_receipts::receipt_sha256
                    .eq(installed.fingerprint().as_bytes().as_slice()),
            ),
        )
        .set(
            boot_publication_receipts::transition_id.eq(foreign_transition.as_str()),
        )
        .execute(connection)
        .unwrap();
        assert_eq!(changed, 1);
    });
    assert!(matches!(
        mismatched.load_current_exact_promoted_boot_publication_receipt_chain(),
        Err(CurrentExactPromotedBootPublicationReceiptChainError::ExactPromoted(
            ExactPromotedBootPublicationReceiptStateError::State(
                BootPublicationReceiptStateError::BodyTransitionMismatch { stored, body }
            )
        )) if stored == foreign_transition && body == *installed.body().transition_id()
    ));
}

#[test]
fn exact_chain_loads_a_promoted_installed_receipt_without_a_predecessor() {
    let database = Database::new(":memory:").unwrap();
    let installed = receipt('3', None, 0x31);
    stage_and_promote(&database, &installed);

    let chain = database
        .load_exact_promoted_boot_publication_receipt_chain(
            installed.body().transition_id(),
            &receipt_pair(&installed),
        )
        .unwrap();

    assert_eq!(chain.installed_receipt(), &installed);
    assert!(chain.committed_predecessor_receipt().is_none());
}

#[test]
fn exact_chain_returns_both_immutable_canonical_receipts() {
    let database = Database::new(":memory:").unwrap();
    let predecessor = receipt('4', None, 0x41);
    stage_and_promote(&database, &predecessor);
    let installed = receipt('5', Some(predecessor.fingerprint()), 0x51);
    stage_and_promote(&database, &installed);

    let chain = database
        .load_exact_promoted_boot_publication_receipt_chain(
            installed.body().transition_id(),
            &receipt_pair(&installed),
        )
        .unwrap();

    assert_eq!(chain.installed_receipt(), &installed);
    assert_eq!(
        chain.committed_predecessor_receipt(),
        Some(&predecessor),
    );
    assert_eq!(
        chain.installed_receipt().canonical_body(),
        stored_body(&database, installed.fingerprint()),
    );
    assert_eq!(
        chain
            .committed_predecessor_receipt()
            .unwrap()
            .canonical_body(),
        stored_body(&database, predecessor.fingerprint()),
    );
}

#[test]
fn exact_chain_rejects_pending_and_mismatched_compact_correlations() {
    let pending_database = Database::new(":memory:").unwrap();
    let pending = receipt('6', None, 0x61);
    stage(&pending_database, &pending);
    assert!(matches!(
        pending_database.load_exact_promoted_boot_publication_receipt_chain(
            pending.body().transition_id(),
            &receipt_pair(&pending),
        ),
        Err(ExactPromotedBootPublicationReceiptStateError::PendingHeadPresent { .. })
    ));

    let database = Database::new(":memory:").unwrap();
    let predecessor = receipt('7', None, 0x71);
    stage_and_promote(&database, &predecessor);
    let installed = receipt('8', Some(predecessor.fingerprint()), 0x81);
    stage_and_promote(&database, &installed);
    let exact_pair = receipt_pair(&installed);

    assert!(matches!(
        database.load_exact_promoted_boot_publication_receipt_chain(
            &transition('9'),
            &exact_pair,
        ),
        Err(ExactPromotedBootPublicationReceiptStateError::TransitionMismatch { .. })
    ));
    let wrong_predecessor = BootPublicationReceiptPair {
        committed: None,
        pending: installed.fingerprint(),
    };
    assert!(matches!(
        database.load_exact_promoted_boot_publication_receipt_chain(
            installed.body().transition_id(),
            &wrong_predecessor,
        ),
        Err(
            ExactPromotedBootPublicationReceiptStateError::CommittedPredecessorMismatch {
                ..
            }
        )
    ));
}

#[test]
fn exact_chain_requires_the_named_predecessor_body_without_mutation() {
    let database = Database::new(":memory:").unwrap();
    let predecessor = receipt('a', None, 0xa2);
    stage_and_promote(&database, &predecessor);
    let installed = receipt('b', Some(predecessor.fingerprint()), 0xb2);
    stage_and_promote(&database, &installed);
    let head_before = database.boot_publication_receipt_head().unwrap();
    let rows_before = receipt_row_count(&database);

    database.delete_boot_publication_receipt_body_for_test(predecessor.fingerprint());

    assert!(matches!(
        database.load_exact_promoted_boot_publication_receipt_chain(
            installed.body().transition_id(),
            &receipt_pair(&installed),
        ),
        Err(ExactPromotedBootPublicationReceiptStateError::State(
            BootPublicationReceiptStateError::DanglingReference {
                reference: ReceiptReference::CommittedPredecessor,
                fingerprint,
            }
        )) if fingerprint == predecessor.fingerprint()
    ));
    assert_eq!(database.boot_publication_receipt_head().unwrap(), head_before);
    assert_eq!(receipt_row_count(&database), rows_before - 1);
}

#[test]
fn current_chain_loader_is_read_only_and_uses_no_exclusive_lock() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.db");
    let url = path.to_str().unwrap();
    let database = Database::new(url).unwrap();
    let predecessor = receipt('c', None, 0xc2);
    stage_and_promote(&database, &predecessor);
    let installed = receipt('d', Some(predecessor.fingerprint()), 0xd2);
    stage_and_promote(&database, &installed);
    let state_before = database.boot_publication_receipt_state().unwrap();
    let predecessor_body = stored_body(&database, predecessor.fingerprint());
    let installed_body = stored_body(&database, installed.fingerprint());
    let rows_before = receipt_row_count(&database);

    database.conn.exec(|connection| {
        connection
            .batch_execute(
                "CREATE TRIGGER reject_chain_head_insert \
                 BEFORE INSERT ON boot_publication_receipt_head \
                 BEGIN SELECT RAISE(ABORT, 'chain loader inserted head'); END; \
                 CREATE TRIGGER reject_chain_head_update \
                 BEFORE UPDATE ON boot_publication_receipt_head \
                 BEGIN SELECT RAISE(ABORT, 'chain loader updated head'); END; \
                 CREATE TRIGGER reject_chain_head_delete \
                 BEFORE DELETE ON boot_publication_receipt_head \
                 BEGIN SELECT RAISE(ABORT, 'chain loader deleted head'); END; \
                 CREATE TRIGGER reject_chain_body_insert \
                 BEFORE INSERT ON boot_publication_receipts \
                 BEGIN SELECT RAISE(ABORT, 'chain loader inserted body'); END; \
                 CREATE TRIGGER reject_chain_body_update \
                 BEFORE UPDATE ON boot_publication_receipts \
                 BEGIN SELECT RAISE(ABORT, 'chain loader updated body'); END; \
                 CREATE TRIGGER reject_chain_body_delete \
                 BEFORE DELETE ON boot_publication_receipts \
                 BEGIN SELECT RAISE(ABORT, 'chain loader deleted body'); END;",
            )
            .unwrap();
    });

    let mut independent_reader = SqliteConnection::establish(url).unwrap();
    independent_reader.batch_execute("BEGIN DEFERRED").unwrap();
    let independent_rows: i64 = boot_publication_receipts::table
        .count()
        .get_result(&mut independent_reader)
        .unwrap();
    assert_eq!(independent_rows, rows_before);

    let current = database
        .load_current_exact_promoted_boot_publication_receipt_chain()
        .unwrap();
    let CurrentExactPromotedBootPublicationReceiptChain::Installed(chain) = current else {
        panic!("a committed receipt must load as installed");
    };
    assert_eq!(chain.installed_receipt(), &installed);
    assert_eq!(
        chain.committed_predecessor_receipt(),
        Some(&predecessor),
    );

    independent_reader.batch_execute("ROLLBACK").unwrap();
    assert_eq!(database.boot_publication_receipt_state().unwrap(), state_before);
    assert_eq!(receipt_row_count(&database), rows_before);
    assert_eq!(stored_body(&database, predecessor.fingerprint()), predecessor_body);
    assert_eq!(stored_body(&database, installed.fingerprint()), installed_body);
}
