#[test]
fn generic_forward_successor_cannot_enter_boot_sync_complete() {
    let started = reblit_record(Phase::BootSyncStarted);

    for candidate in [None, Some(91)] {
        assert!(matches!(
            started.forward_successor(candidate),
            Err(CodecError::ExplicitBootSyncCompleteSuccessorRequired)
        ));
    }
    assert_eq!(started.phase, Phase::BootSyncStarted);
    assert_eq!(started.generation, 7);
}

#[test]
fn typed_boot_sync_complete_successor_preserves_exact_v3_receipt_pair() {
    let chained = boot_publication_receipts();
    for expected_pair in [
        chained,
        BootPublicationReceiptPair {
            committed: None,
            pending: receipt_fingerprint(0x66),
        },
    ] {
        let mut started = reblit_record(Phase::BootSyncStarted);
        started.boot_publication_receipts = Some(expected_pair);
        started.validate().unwrap();

        let complete = started.boot_sync_complete_successor(expected_pair).unwrap();
        let mut expected = started.clone();
        expected.generation += 1;
        expected.phase = Phase::BootSyncComplete;

        assert_eq!(complete, expected);
        assert_receipts(&complete, expected_pair);
        validate_advance(&started, &complete).unwrap();
        assert_eq!(decode(&encode(&complete).unwrap()).unwrap(), complete);
    }
}

#[test]
fn typed_boot_sync_complete_successor_rejects_legacy_payload_versions() {
    let expected_pair = boot_publication_receipts();

    for version in [PAYLOAD_VERSION_V1, PAYLOAD_VERSION_V2] {
        let mut legacy = reblit_record(Phase::BootSyncStarted);
        legacy.version = version;
        legacy.boot_publication_receipts = None;
        encode(&legacy).unwrap();

        assert!(matches!(
            legacy.boot_sync_complete_successor(expected_pair),
            Err(CodecError::PayloadVersionBootPublicationReceiptsMismatch(actual)) if actual == version
        ));
    }
}

#[test]
fn typed_boot_sync_complete_successor_rejects_each_receipt_pair_mismatch() {
    let started = reblit_record(Phase::BootSyncStarted);
    let exact = started.boot_publication_receipt_correlation().unwrap().unwrap();
    let wrong_pairs = [
        BootPublicationReceiptPair {
            committed: exact.committed,
            pending: receipt_fingerprint(0x55),
        },
        BootPublicationReceiptPair {
            committed: None,
            pending: exact.pending,
        },
    ];

    for wrong_pair in wrong_pairs {
        assert_ne!(wrong_pair, exact);
        assert!(matches!(
            started.boot_sync_complete_successor(wrong_pair),
            Err(CodecError::BootPublicationReceiptsChangedIllegally)
        ));
    }
    assert_receipts(&started, exact);
}

#[test]
fn typed_boot_sync_complete_successor_rejects_valid_adjacent_wrong_phases() {
    let expected_pair = boot_publication_receipts();

    for source in [
        reblit_record(Phase::SystemTriggersComplete),
        reblit_record(Phase::BootSyncComplete),
    ] {
        let current = source.phase;
        assert!(matches!(
            source.boot_sync_complete_successor(expected_pair),
            Err(CodecError::IllegalPhaseAdvance {
                current: actual,
                next: Phase::BootSyncComplete,
            }) if actual == current
        ));
    }
}

#[test]
fn typed_boot_sync_complete_successor_rejects_generation_exhaustion() {
    let mut started = reblit_record(Phase::BootSyncStarted);
    started.generation = u64::MAX;
    let expected_pair = started.boot_publication_receipt_correlation().unwrap().unwrap();

    assert!(matches!(
        started.boot_sync_complete_successor(expected_pair),
        Err(CodecError::GenerationExhausted)
    ));
}

#[test]
fn typed_boot_sync_complete_successor_keeps_operation_policy_outside_the_journal() {
    for started in [
        new_state_record(Phase::BootSyncStarted),
        archived_record(Phase::BootSyncStarted),
        reblit_record(Phase::BootSyncStarted),
    ] {
        let operation = started.operation;
        let expected_pair = started.boot_publication_receipt_correlation().unwrap().unwrap();
        let complete = started.boot_sync_complete_successor(expected_pair).unwrap();

        assert_eq!(complete.operation, operation);
        assert_eq!(complete.phase, Phase::BootSyncComplete);
        assert_receipts(&complete, expected_pair);
    }
}
