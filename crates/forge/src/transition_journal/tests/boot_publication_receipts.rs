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
fn canonical_v2_full_frame_and_json_order_remain_legacy_stable() {
    const GOLDEN_V1_WITH_NEWLINE: &[u8] =
        include_bytes!("../../../../../tests/fixtures/transition-journal-v1-rollback-decided.json");
    let golden_v1 = std::str::from_utf8(&GOLDEN_V1_WITH_NEWLINE[..GOLDEN_V1_WITH_NEWLINE.len() - 1]).unwrap();
    let expected_v2 = golden_v1.replacen("\"version\":1", "\"version\":2", 1);

    let mut source = new_state_record(Phase::BootSyncStarted);
    source.version = PAYLOAD_VERSION_V2;
    source.boot_publication_receipts = None;
    let value = rollback_decided(&source);
    let frame = encode(&value).unwrap();
    assert_eq!(&frame[HEADER_SIZE..], expected_v2.as_bytes());
    assert_eq!(frame, frame_payload(expected_v2.as_bytes()));
    assert_eq!(decode(&frame).unwrap(), value);
    assert_eq!(encode(&decode(&frame).unwrap()).unwrap(), frame);
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

    for version in [PAYLOAD_VERSION_V1, PAYLOAD_VERSION_V2] {
        let mut legacy = started.clone();
        legacy.version = version;
        legacy.boot_publication_receipts = None;
        let legacy_frame = encode(&legacy).unwrap();
        assert_eq!(decode(&legacy_frame).unwrap(), legacy);
        assert_eq!(encode(&decode(&legacy_frame).unwrap()).unwrap(), legacy_frame);
        let explicit_legacy_field = replace_payload(&legacy_frame, |payload| {
            payload.replacen(
                "\"rollback\":null,",
                "\"rollback\":null,\"boot_publication_receipts\":null,",
                1,
            )
        });
        assert!(matches!(
            decode(&explicit_legacy_field),
            Err(CodecError::NonCanonicalPayload)
        ));

        legacy.boot_publication_receipts = Some(receipts);
        assert!(matches!(
            encode(&legacy),
            Err(CodecError::PayloadVersionBootPublicationReceiptsMismatch(actual)) if actual == version
        ));
    }

    let v3_preparing = encode(&preparing).unwrap();
    let v2_preparing = replace_payload(&v3_preparing, |payload| {
        payload.replacen(
            &format!("\"version\":{PAYLOAD_VERSION}"),
            &format!("\"version\":{PAYLOAD_VERSION_V2}"),
            1,
        )
    });
    let decoded_v2 = decode(&v2_preparing).unwrap();
    assert_eq!(decoded_v2.version, PAYLOAD_VERSION_V2);
    assert_eq!(encode(&decoded_v2).unwrap(), v2_preparing);
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

    for version in [PAYLOAD_VERSION_V1, PAYLOAD_VERSION_V2] {
        let mut legacy = source.clone();
        legacy.version = version;
        assert!(matches!(
            legacy.forward_successor(None),
            Err(CodecError::ExplicitBootSyncStartedSuccessorRequired)
        ));
        assert!(matches!(
            legacy.boot_sync_started_successor(receipts),
            Err(CodecError::PayloadVersionBootPublicationReceiptsMismatch(actual)) if actual == version
        ));
    }
}

#[test]
fn legacy_payloads_freeze_before_boot_entry_and_retain_conservative_recovery() {
    for version in [PAYLOAD_VERSION_V1, PAYLOAD_VERSION_V2] {
        let mut pre_boot = new_state_record(Phase::PreviousArchived);
        pre_boot.version = version;
        assert_eq!(pre_boot.boot_publication_receipt_correlation().unwrap(), None);
        assert!(matches!(
            pre_boot.forward_successor(None),
            Err(CodecError::ExplicitBootSyncStartedSuccessorRequired)
        ));

        let safe_rollback = pre_boot
            .rollback_decision(boot_observations(InitialRollbackAction::AlreadySatisfied))
            .unwrap();
        assert_eq!(safe_rollback.version, version);
        assert_eq!(safe_rollback.boot_publication_receipt_correlation().unwrap(), None);
        assert_eq!(safe_rollback.rollback.as_ref().unwrap().boot, BootRollback::NotRequired);

        let mut existing_started = new_state_record(Phase::BootSyncStarted);
        existing_started.version = version;
        existing_started.boot_publication_receipts = None;
        let framed = encode(&existing_started).unwrap();
        let existing_started = decode(&framed).unwrap();
        assert_eq!(existing_started.boot_publication_receipt_correlation().unwrap(), None);

        let decided = existing_started
            .rollback_decision(boot_observations(InitialRollbackAction::AlreadySatisfied))
            .unwrap();
        let required = decided.rollback_successor(None).unwrap();
        assert_eq!(required.phase, Phase::BootRepairRequired);
        assert_eq!(required.boot_publication_receipt_correlation().unwrap(), None);
        let started = required.boot_repair_started_successor().unwrap();
        let unverified = started.boot_repair_unverified_successor().unwrap();
        assert_eq!(unverified.phase, Phase::BootRepairUnverified);
        assert_eq!(unverified.version, version);
        assert_eq!(unverified.boot_publication_receipt_correlation().unwrap(), None);
    }
}

#[test]
fn receipt_pair_is_preserved_by_every_forward_rollback_and_boot_repair_successor() {
    let receipts = boot_publication_receipts();
    let source = new_state_record(Phase::PreviousArchived);
    let started = source.boot_sync_started_successor(receipts).unwrap();
    let mut forward = started.clone();
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

    let mut forward = started.forward_successor(None).unwrap();
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
