fn boot_observations(action: InitialRollbackAction) -> RollbackObservations {
    RollbackObservations {
        allocated_candidate_id: None,
        previous_archive: Some(action),
        usr_exchange: Some(action),
        candidate: action,
        fresh_db: Some(action),
    }
}

fn assert_receipts(record: &TransitionRecord, expected: BootPublicationReceiptPair) {
    assert_eq!(record.boot_publication_receipt_correlation().unwrap(), Some(expected));
}

#[test]
fn payload_v3_boot_publication_receipts_are_canonical_and_version_gated() {
    let source = new_state_record(Phase::PreviousArchived);
    let receipts = boot_publication_receipts();
    let started = source.boot_sync_started_successor(receipts).unwrap();
    let framed = encode(&started).unwrap();
    let payload = std::str::from_utf8(&framed[HEADER_SIZE..]).unwrap();
    let receipts_json = serde_json::to_string(&receipts).unwrap();
    assert!(payload.contains(&format!(
        "\"rollback\":null,\"boot_publication_receipts\":{receipts_json},\"candidate\":"
    )));
    assert_eq!(decode(&framed).unwrap(), started);
    assert_eq!(encode(&decode(&framed).unwrap()).unwrap(), framed);

    let invalid_case = replace_payload(&framed, |payload| {
        payload.replacen("\"pending\":\"1", "\"pending\":\"A", 1)
    });
    assert!(matches!(decode(&invalid_case), Err(CodecError::Json(_))));
    let pending = "11".repeat(32);
    let invalid_length = replace_payload(&framed, |payload| payload.replacen(&pending, &pending[..63], 1));
    assert!(matches!(decode(&invalid_length), Err(CodecError::Json(_))));
    let nested_extra = replace_payload(&framed, |payload| {
        payload.replacen("\"pending\":", "\"unexpected\":true,\"pending\":", 1)
    });
    assert!(matches!(decode(&nested_extra), Err(CodecError::Json(_))));

    let preparing = new_state_record(Phase::Preparing);
    let explicit_null = replace_payload(&encode(&preparing).unwrap(), |payload| {
        payload.replacen("\"rollback\":null,", "\"rollback\":null,\"boot_publication_receipts\":null,", 1)
    });
    assert!(matches!(decode(&explicit_null), Err(CodecError::NonCanonicalPayload)));

    // Any version other than the single supported one is rejected outright.
    let foreign = replace_payload(&encode(&preparing).unwrap(), |payload| {
        payload.replacen(
            &format!("\"version\":{PAYLOAD_VERSION}"),
            &format!("\"version\":{}", PAYLOAD_VERSION - 1),
            1,
        )
    });
    assert!(matches!(
        decode(&foreign),
        Err(CodecError::UnsupportedPayloadVersion(version)) if version == PAYLOAD_VERSION - 1
    ));
}

#[test]
fn payload_v3_receipt_presence_tracks_exact_boot_sync_reachability() {
    for phase in [
        Phase::Preparing,
        Phase::PreviousArchiveIntent,
        Phase::PreviousArchived,
    ] {
        let valid = new_state_record(phase);
        assert_eq!(valid.boot_publication_receipt_correlation().unwrap(), None);

        let mut invalid = valid;
        invalid.boot_publication_receipts = Some(boot_publication_receipts());
        assert!(matches!(
            encode(&invalid),
            Err(CodecError::BootPublicationReceiptPresenceMismatch {
                phase: actual,
                required: false,
            }) if actual == phase
        ));
    }

    for phase in [
        Phase::BootSyncStarted,
        Phase::BootSyncComplete,
        Phase::CommitDecided,
        Phase::CommitCleanupComplete,
        Phase::Complete,
    ] {
        let valid = new_state_record(phase);
        assert_receipts(&valid, boot_publication_receipts());

        let mut invalid = valid;
        invalid.boot_publication_receipts = None;
        assert!(matches!(
            encode(&invalid),
            Err(CodecError::BootPublicationReceiptPresenceMismatch {
                phase: actual,
                required: true,
            }) if actual == phase
        ));
    }

    for phase in [Phase::CommitDecided, Phase::CommitCleanupComplete, Phase::Complete] {
        let mut no_boot = new_state_record(phase);
        no_boot.options.run_boot_sync = false;
        no_boot.boot_publication_receipts = None;
        encode(&no_boot).unwrap();
    }

    let boot_rollback = rollback_decided(&new_state_record(Phase::BootSyncStarted));
    assert_receipts(&boot_rollback, boot_publication_receipts());
    let mut missing = boot_rollback;
    missing.boot_publication_receipts = None;
    assert!(matches!(
        encode(&missing),
        Err(CodecError::BootPublicationReceiptPresenceMismatch { required: true, .. })
    ));

    let pre_boot_rollback = rollback_decided(&new_state_record(Phase::PreviousArchived));
    assert_eq!(pre_boot_rollback.boot_publication_receipt_correlation().unwrap(), None);
    let mut invented = pre_boot_rollback;
    invented.boot_publication_receipts = Some(boot_publication_receipts());
    assert!(matches!(
        encode(&invented),
        Err(CodecError::BootPublicationReceiptPresenceMismatch { required: false, .. })
    ));
}

#[test]
fn production_boot_sync_entry_requires_the_typed_receipt_successor() {
    let source = new_state_record(Phase::PreviousArchived);
    assert!(matches!(
        source.forward_successor(None),
        Err(CodecError::ExplicitBootSyncStartedSuccessorRequired)
    ));

    let receipts = boot_publication_receipts();
    let started = source.boot_sync_started_successor(receipts).unwrap();
    assert_eq!(started.phase, Phase::BootSyncStarted);
    assert_eq!(started.generation, source.generation + 1);
    assert_receipts(&started, receipts);
    validate_advance(&source, &started).unwrap();

    let first_publication = BootPublicationReceiptPair {
        committed: None,
        pending: receipt_fingerprint(0x44),
    };
    assert_receipts(
        &source.boot_sync_started_successor(first_publication).unwrap(),
        first_publication,
    );

    let wrong_phase = new_state_record(Phase::Preparing);
    assert!(matches!(
        wrong_phase.boot_sync_started_successor(receipts),
        Err(CodecError::IllegalPhaseAdvance {
            current: Phase::Preparing,
            next: Phase::BootSyncStarted,
        })
    ));

    // Generic forward advancement can never enter the receipt-bearing phase; it
    // must go through the typed successor regardless of anything else.
    assert!(matches!(
        source.forward_successor(None),
        Err(CodecError::ExplicitBootSyncStartedSuccessorRequired)
    ));
}

#[test]
fn receipt_pair_is_preserved_by_every_forward_rollback_and_boot_repair_successor() {
    let receipts = boot_publication_receipts();
    let source = new_state_record(Phase::PreviousArchived);
    let started = source.boot_sync_started_successor(receipts).unwrap();
    let mut forward = started.boot_sync_complete_successor(receipts).unwrap();
    assert_receipts(&forward, receipts);
    while forward.phase != Phase::Complete {
        forward = forward.forward_successor(None).unwrap();
        assert_receipts(&forward, receipts);
    }

    let mut rollback = started
        .rollback_decision(boot_observations(InitialRollbackAction::Pending))
        .unwrap();
    assert_receipts(&rollback, receipts);
    for outcome in [
        None,
        Some(RollbackActionOutcome::Applied),
        None,
        Some(RollbackActionOutcome::AlreadySatisfied),
        None,
        Some(RollbackActionOutcome::Applied),
        None,
        Some(RollbackActionOutcome::AlreadySatisfied),
        None,
    ] {
        rollback = rollback.rollback_successor(outcome).unwrap();
        assert_receipts(&rollback, receipts);
    }
    assert_eq!(rollback.phase, Phase::BootRepairRequired);

    let repair_started = rollback.boot_repair_started_successor().unwrap();
    assert_receipts(&repair_started, receipts);
    let unverified = repair_started.boot_repair_unverified_successor().unwrap();
    assert_receipts(&unverified, receipts);
    for outcome in [BootRepairOutcome::Applied, BootRepairOutcome::AlreadySatisfied] {
        let complete = repair_started.boot_repair_complete_successor(outcome).unwrap();
        assert_receipts(&complete, receipts);
        let rollback_complete = complete.boot_repair_rollback_complete_successor().unwrap();
        assert_receipts(&rollback_complete, receipts);
    }
}

#[test]
fn receipt_pair_replacement_is_rejected_across_every_successor_family() {
    let receipts = boot_publication_receipts();
    let replacement = BootPublicationReceiptPair {
        committed: None,
        pending: receipt_fingerprint(0x33),
    };
    let started = new_state_record(Phase::PreviousArchived)
        .boot_sync_started_successor(receipts)
        .unwrap();

    let mut forward = started.boot_sync_complete_successor(receipts).unwrap();
    forward.boot_publication_receipts = Some(replacement);
    assert!(matches!(
        validate_advance(&started, &forward),
        Err(CodecError::BootPublicationReceiptsChangedIllegally)
    ));

    let decided = started
        .rollback_decision(boot_observations(InitialRollbackAction::AlreadySatisfied))
        .unwrap();
    let mut required = decided.rollback_successor(None).unwrap();
    required.boot_publication_receipts = Some(replacement);
    assert!(matches!(
        validate_advance(&decided, &required),
        Err(CodecError::BootPublicationReceiptsChangedIllegally)
    ));

    let required = decided.rollback_successor(None).unwrap();
    let mut repair_started = required.boot_repair_started_successor().unwrap();
    repair_started.boot_publication_receipts = Some(replacement);
    assert!(matches!(
        validate_advance(&required, &repair_started),
        Err(CodecError::BootPublicationReceiptsChangedIllegally)
    ));

    let mut malformed = started;
    malformed.boot_publication_receipts = None;
    assert!(matches!(
        malformed.boot_publication_receipt_correlation(),
        Err(CodecError::BootPublicationReceiptPresenceMismatch { required: true, .. })
    ));
}
