use super::*;

#[test]
fn fresh_database_has_one_empty_exact_receipt_head() {
    let database = Database::new(":memory:").unwrap();

    let head = database.boot_publication_receipt_head().unwrap();
    assert_eq!(head.committed(), None);
    assert_eq!(head.pending(), None);
    assert_eq!(head.receipt_pair(), None);
    assert_eq!(head.receipt_pair_for(&transition('0')), None);
}

#[test]
fn first_stage_writes_the_exact_transition_and_pair_once() {
    let database = Database::new(":memory:").unwrap();
    let transition_id = transition('1');
    let pair = BootPublicationReceiptPair {
        committed: None,
        pending: fingerprint(0x11),
    };

    assert_eq!(
        database
            .stage_boot_publication_receipt_pair(&transition_id, &pair)
            .unwrap(),
        BootPublicationReceiptStageOutcome::Staged
    );

    let head = database.boot_publication_receipt_head().unwrap();
    assert_eq!(head.committed(), None);
    assert_eq!(head.receipt_pair(), Some(pair));
    assert_eq!(head.receipt_pair_for(&transition_id), Some(pair));
    assert_eq!(head.receipt_pair_for(&transition('2')), None);
    let pending = head.pending().unwrap();
    assert_eq!(pending.transition_id(), &transition_id);
    assert_eq!(pending.fingerprint(), pair.pending);
}

#[test]
fn exact_stage_retry_is_distinguished_and_does_not_change_the_head() {
    let database = Database::new(":memory:").unwrap();
    let transition_id = transition('3');
    let committed = fingerprint(0x33);
    let pair = BootPublicationReceiptPair {
        committed: Some(committed),
        pending: fingerprint(0x34),
    };
    database
        .replace_boot_publication_receipt_head_for_test(Some(committed), None)
        .unwrap();

    assert_eq!(
        database
            .stage_boot_publication_receipt_pair(&transition_id, &pair)
            .unwrap(),
        BootPublicationReceiptStageOutcome::Staged
    );
    let first = database.boot_publication_receipt_head().unwrap();
    assert_eq!(
        database
            .stage_boot_publication_receipt_pair(&transition_id, &pair)
            .unwrap(),
        BootPublicationReceiptStageOutcome::AlreadyStaged
    );
    assert_eq!(database.boot_publication_receipt_head().unwrap(), first);
}

#[test]
fn committed_mismatch_fails_before_pending_conflict_or_mutation() {
    let database = Database::new(":memory:").unwrap();
    let transition_id = transition('4');
    let committed = fingerprint(0x40);
    let pending = fingerprint(0x41);
    database
        .replace_boot_publication_receipt_head_for_test(Some(committed), Some((&transition_id, pending)))
        .unwrap();
    let before = database.boot_publication_receipt_head().unwrap();
    let requested = BootPublicationReceiptPair {
        committed: Some(fingerprint(0x42)),
        pending,
    };

    assert!(matches!(
        database.stage_boot_publication_receipt_pair(&transition('5'), &requested),
        Err(BootPublicationReceiptHeadError::CommittedMismatch {
            expected: Some(expected),
            actual: Some(actual),
        }) if expected == requested.committed.unwrap() && actual == committed
    ));
    assert_eq!(database.boot_publication_receipt_head().unwrap(), before);
}

#[test]
fn every_nonexact_existing_pending_pair_is_a_hard_conflict() {
    let database = Database::new(":memory:").unwrap();
    let owner = transition('6');
    let pair = BootPublicationReceiptPair {
        committed: None,
        pending: fingerprint(0x60),
    };
    database
        .stage_boot_publication_receipt_pair(&owner, &pair)
        .unwrap();
    let before = database.boot_publication_receipt_head().unwrap();

    for (requested_transition, requested_pair) in [
        (transition('7'), pair),
        (
            owner.clone(),
            BootPublicationReceiptPair {
                committed: None,
                pending: fingerprint(0x61),
            },
        ),
        (
            transition('8'),
            BootPublicationReceiptPair {
                committed: None,
                pending: fingerprint(0x62),
            },
        ),
    ] {
        assert!(matches!(
            database.stage_boot_publication_receipt_pair(&requested_transition, &requested_pair),
            Err(BootPublicationReceiptHeadError::PendingConflict { .. })
        ));
        assert_eq!(database.boot_publication_receipt_head().unwrap(), before);
    }
}

#[test]
fn typed_test_replacement_and_clear_preserve_schema_invariants() {
    let database = Database::new(":memory:").unwrap();
    let transition_id = transition('9');
    let committed = fingerprint(0x90);
    let pending = fingerprint(0x91);

    database
        .replace_boot_publication_receipt_head_for_test(Some(committed), Some((&transition_id, pending)))
        .unwrap();
    let head = database.boot_publication_receipt_head().unwrap();
    assert_eq!(head.committed(), Some(committed));
    assert_eq!(
        head.receipt_pair_for(&transition_id),
        Some(BootPublicationReceiptPair {
            committed: Some(committed),
            pending,
        })
    );

    database.clear_boot_publication_receipt_head_for_test().unwrap();
    let cleared = database.boot_publication_receipt_head().unwrap();
    assert_eq!(cleared.committed(), None);
    assert_eq!(cleared.pending(), None);
}

#[test]
fn staged_receipt_head_survives_database_reopen_exactly() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.db");
    let url = path.to_str().unwrap();
    let transition_id = transition('a');
    let committed = fingerprint(0xa0);
    let pair = BootPublicationReceiptPair {
        committed: Some(committed),
        pending: fingerprint(0xa1),
    };

    let database = Database::new(url).unwrap();
    database
        .replace_boot_publication_receipt_head_for_test(Some(committed), None)
        .unwrap();
    database
        .stage_boot_publication_receipt_pair(&transition_id, &pair)
        .unwrap();
    let expected = database.boot_publication_receipt_head().unwrap();
    drop(database);

    let reopened = Database::new(url).unwrap();
    assert_eq!(reopened.boot_publication_receipt_head().unwrap(), expected);
    assert_eq!(expected.receipt_pair_for(&transition_id), Some(pair));
}
