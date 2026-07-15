#[test]
fn canonical_round_trip_covers_every_phase() {
    let phases = [
        Phase::Preparing,
        Phase::FreshStateAllocating,
        Phase::FreshStateAllocated,
        Phase::CandidatePrepareStarted,
        Phase::CandidatePrepared,
        Phase::TransactionTriggersStarted,
        Phase::TransactionTriggersComplete,
        Phase::UsrExchangeIntent,
        Phase::UsrExchanged,
        Phase::RootLinksComplete,
        Phase::SystemTriggersStarted,
        Phase::SystemTriggersComplete,
        Phase::PreviousArchiveIntent,
        Phase::PreviousArchived,
        Phase::BootSyncStarted,
        Phase::BootSyncComplete,
        Phase::CommitDecided,
        Phase::CommitCleanupComplete,
        Phase::Complete,
        Phase::RollbackDecided,
        Phase::PreviousRestoreIntent,
        Phase::PreviousRestoredToStaging,
        Phase::ReverseExchangeIntent,
        Phase::UsrRestored,
        Phase::CandidatePreserveIntent,
        Phase::CandidatePreserved,
        Phase::FreshDbInvalidationIntent,
        Phase::FreshDbInvalidated,
        Phase::BootRepairRequired,
        Phase::BootRepairStarted,
        Phase::BootRepairUnverified,
        Phase::RollbackComplete,
    ];
    for phase in phases {
        let value = record(phase);
        assert_eq!(decode(&encode(&value).unwrap()).unwrap(), value);
    }
}

#[test]
fn canonical_v1_full_frame_and_json_order_are_locked_by_golden_bytes() {
    const GOLDEN_JSON_WITH_NEWLINE: &[u8] =
        include_bytes!("../../../../../tests/fixtures/transition-journal-v1-rollback-decided.json");
    const GOLDEN_HEX_WITH_NEWLINE: &[u8] =
        include_bytes!("../../../../../tests/fixtures/transition-journal-v1-rollback-decided.hex");
    assert_eq!(GOLDEN_JSON_WITH_NEWLINE.last(), Some(&b'\n'));
    assert_eq!(GOLDEN_HEX_WITH_NEWLINE.last(), Some(&b'\n'));
    let golden_json = &GOLDEN_JSON_WITH_NEWLINE[..GOLDEN_JSON_WITH_NEWLINE.len() - 1];
    let mut golden_frame = Vec::with_capacity((GOLDEN_HEX_WITH_NEWLINE.len() - 1) / 2);
    let mut pairs = GOLDEN_HEX_WITH_NEWLINE[..GOLDEN_HEX_WITH_NEWLINE.len() - 1].chunks_exact(2);
    for pair in &mut pairs {
        let nibble = |byte: u8| match byte {
            b'0'..=b'9' => byte - b'0',
            b'a'..=b'f' => byte - b'a' + 10,
            _ => panic!("golden frame is not lowercase hexadecimal"),
        };
        golden_frame.push((nibble(pair[0]) << 4) | nibble(pair[1]));
    }
    assert!(pairs.remainder().is_empty());

    let value = rollback_decided(&new_state_record(Phase::BootSyncStarted));
    assert_eq!(encode(&value).unwrap(), golden_frame);
    assert_eq!(&golden_frame[HEADER_SIZE..], golden_json);
    assert_eq!(decode(&golden_frame).unwrap(), value);
}

#[test]
fn exact_record_limit_and_n_plus_one_are_distinguished() {
    assert!(enforce_record_size(MAX_CANONICAL_RECORD_BYTES).is_ok());
    assert!(matches!(
        enforce_record_size(MAX_CANONICAL_RECORD_BYTES + 1),
        Err(CodecError::RecordTooLarge(size)) if size == MAX_CANONICAL_RECORD_BYTES + 1
    ));
    assert!(matches!(
        decode(&vec![0; MAX_CANONICAL_RECORD_BYTES]),
        Err(CodecError::InvalidMagic)
    ));
    assert!(matches!(
        decode(&vec![0; MAX_CANONICAL_RECORD_BYTES + 1]),
        Err(CodecError::RecordTooLarge(_))
    ));
}

#[test]
fn checksum_covers_header_fields_and_payload() {
    let valid = encode(&record(Phase::Preparing)).unwrap();
    for offset in [MAGIC_END, VERSION_END, CHECKSUM_END, valid.len() - 1] {
        let mut corrupt = valid.clone();
        corrupt[offset] ^= 1;
        assert!(decode(&corrupt).is_err(), "offset {offset} unexpectedly decoded");
    }
}

#[test]
fn unknown_frame_and_payload_versions_are_rejected() {
    let mut frame = encode(&record(Phase::Preparing)).unwrap();
    frame[MAGIC_END..VERSION_END].copy_from_slice(&2_u16.to_be_bytes());
    assert!(matches!(decode(&frame), Err(CodecError::UnsupportedFrameVersion(2))));

    let valid = encode(&record(Phase::Preparing)).unwrap();
    let unknown = replace_payload(&valid, |payload| payload.replacen("\"version\":1", "\"version\":2", 1));
    assert!(matches!(
        decode(&unknown),
        Err(CodecError::UnsupportedPayloadVersion(2))
    ));
}

#[test]
fn unknown_phase_field_and_duplicate_field_are_rejected() {
    let valid = encode(&record(Phase::Preparing)).unwrap();
    let unknown_phase = replace_payload(&valid, |payload| {
        payload.replacen("\"phase\":\"preparing\"", "\"phase\":\"future-phase\"", 1)
    });
    assert!(matches!(decode(&unknown_phase), Err(CodecError::Json(_))));

    let unknown_field = replace_payload(&valid, |payload| payload.replacen('{', "{\"surprise\":true,", 1));
    assert!(matches!(decode(&unknown_field), Err(CodecError::Json(_))));

    let duplicate = replace_payload(&valid, |payload| {
        payload.replacen("\"generation\":7", "\"generation\":7,\"generation\":8", 1)
    });
    assert!(matches!(decode(&duplicate), Err(CodecError::Json(_))));

    let nested_unknown = replace_payload(&valid, |payload| {
        payload.replacen(
            "\"archive_previous\":true",
            "\"archive_previous\":true,\"future_option\":false",
            1,
        )
    });
    assert!(matches!(decode(&nested_unknown), Err(CodecError::Json(_))));

    let nested_duplicate = replace_payload(&valid, |payload| {
        payload.replacen(
            "\"run_boot_sync\":true",
            "\"run_boot_sync\":true,\"run_boot_sync\":false",
            1,
        )
    });
    assert!(matches!(decode(&nested_duplicate), Err(CodecError::Json(_))));
}

#[test]
fn reboot_identity_schema_is_required_strict_and_has_no_v1_aliases() {
    let valid = encode(&record(Phase::Preparing)).unwrap();
    let rejects = |frame: Vec<u8>| assert!(matches!(decode(&frame), Err(CodecError::Json(_))));
    const BOOT_FIELD: &str = "\"boot_id\":\"01234567-89ab-4cde-8f01-23456789abcd\",";
    const NAMESPACE_FIELD: &str = "\"mount_namespace\":{\"st_dev\":30,\"inode\":31}";
    const EPOCH_FIELD: &str = concat!(
        "\"creation_epoch\":{",
        "\"boot_id\":\"01234567-89ab-4cde-8f01-23456789abcd\",",
        "\"mount_namespace\":{\"st_dev\":30,\"inode\":31}},"
    );
    const CANDIDATE_TOKEN_FIELD: &str = "\"tree_token\":\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\",";
    const PREVIOUS_TOKEN_FIELD: &str = "\"tree_token\":\"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\",";
    const CANDIDATE_RUNTIME_FIELD: &str = "\"usr_runtime_identity\":{\"st_dev\":10,\"inode\":10,\"mount_id\":12}";
    const PREVIOUS_RUNTIME_FIELD: &str = "\"usr_runtime_identity\":{\"st_dev\":10,\"inode\":20,\"mount_id\":12}";

    rejects(replace_payload(&valid, |payload| payload.replacen(EPOCH_FIELD, "", 1)));
    rejects(replace_payload(&valid, |payload| {
        payload.replacen(EPOCH_FIELD, &format!("{EPOCH_FIELD}{EPOCH_FIELD}"), 1)
    }));
    rejects(replace_payload(&valid, |payload| payload.replacen(BOOT_FIELD, "", 1)));
    rejects(replace_payload(&valid, |payload| {
        payload.replacen(BOOT_FIELD, &format!("{BOOT_FIELD}{BOOT_FIELD}"), 1)
    }));
    rejects(replace_payload(&valid, |payload| {
        payload.replacen(&format!(",{NAMESPACE_FIELD}"), "", 1)
    }));
    rejects(replace_payload(&valid, |payload| {
        payload.replacen("\"inode\":31", "\"inode\":31,\"future_namespace_field\":1", 1)
    }));
    rejects(replace_payload(&valid, |payload| {
        payload.replacen(
            NAMESPACE_FIELD,
            &format!("{NAMESPACE_FIELD},\"future_epoch_field\":true"),
            1,
        )
    }));
    for token_field in [CANDIDATE_TOKEN_FIELD, PREVIOUS_TOKEN_FIELD] {
        rejects(replace_payload(&valid, |payload| payload.replacen(token_field, "", 1)));
        rejects(replace_payload(&valid, |payload| {
            payload.replacen(token_field, &format!("{token_field}{token_field}"), 1)
        }));
    }
    for runtime_field in [CANDIDATE_RUNTIME_FIELD, PREVIOUS_RUNTIME_FIELD] {
        rejects(replace_payload(&valid, |payload| {
            payload.replacen(&format!(",{runtime_field}"), "", 1)
        }));
        rejects(replace_payload(&valid, |payload| {
            payload.replacen(runtime_field, &format!("{runtime_field},{runtime_field}"), 1)
        }));
    }
    rejects(replace_payload(&valid, |payload| {
        payload.replacen("\"usr_runtime_identity\"", "\"usr_identity\"", 1)
    }));
    rejects(replace_payload(&valid, |payload| {
        payload.replacen("\"mount_id\":12", "\"statx_mount_id\":12", 1)
    }));
    rejects(replace_payload(&valid, |payload| {
        payload.replacen(",\"mount_id\":12", "", 1)
    }));
}

#[test]
fn record_trailing_bytes_and_noncanonical_json_are_rejected() {
    let mut trailing = encode(&record(Phase::Preparing)).unwrap();
    trailing.push(b' ');
    assert!(matches!(decode(&trailing), Err(CodecError::LengthMismatch { .. })));

    let valid = encode(&record(Phase::Preparing)).unwrap();
    let payload = std::str::from_utf8(&valid[HEADER_SIZE..]).unwrap();
    let noncanonical = frame_payload(format!(" {payload}").as_bytes());
    assert!(matches!(decode(&noncanonical), Err(CodecError::NonCanonicalPayload)));
}

#[test]
fn bounded_identifiers_and_obvious_semantic_mismatches_fail_closed() {
    for invalid in [
        "ABCDEF0123456789abcdef0123456789",
        "0123456789abcdef0123456789abcde",
        "g123456789abcdef0123456789abcdef",
    ] {
        assert!(TransitionId::parse(invalid).is_err());
    }
    assert_eq!(boot_id().as_str(), "01234567-89ab-4cde-8f01-23456789abcd");
    for invalid in [
        "",
        "01234567-89ab-4cde-8f01-23456789abc",
        "01234567-89ab-4cde-8f01-23456789abcdd",
        "01234567-89AB-4cde-8f01-23456789abcd",
        "0123456789ab-4cde-8f01-23456789abcd",
        "01234567-89ab-4cde-8f01-23456789abcg",
        "00000000-0000-0000-0000-000000000000",
        "01234567-89ab-4cde-8f01-23456789abc\n",
    ] {
        assert!(matches!(BootId::parse(invalid), Err(CodecError::InvalidBootId)));
    }
    assert_eq!(tree_token('a').as_str(), "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    for invalid in [
        "",
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        "gaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "00000000000000000000000000000000",
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n",
    ] {
        assert!(matches!(TreeToken::parse(invalid), Err(CodecError::InvalidTreeToken)));
    }

    let valid = encode(&record(Phase::Preparing)).unwrap();
    let invalid_boot = replace_payload(&valid, |payload| {
        payload.replacen(
            "01234567-89ab-4cde-8f01-23456789abcd",
            "01234567-89AB-4cde-8f01-23456789abcd",
            1,
        )
    });
    assert!(matches!(decode(&invalid_boot), Err(CodecError::Json(_))));
    let invalid_token = replace_payload(&valid, |payload| {
        payload.replacen(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "00000000000000000000000000000000",
            1,
        )
    });
    assert!(matches!(decode(&invalid_token), Err(CodecError::Json(_))));

    for invalid in ["", ".", "..", "../escape", "Upper", "has space"] {
        assert!(matches!(
            QuarantineName::parse(invalid),
            Err(CodecError::InvalidQuarantineName)
        ));
    }
    assert!(matches!(
        QuarantineName::parse("a".repeat(MAX_QUARANTINE_NAME_BYTES + 1)),
        Err(CodecError::InvalidQuarantineName)
    ));

    let mut mismatch = record(Phase::Preparing);
    mismatch.candidate.origin = CandidateOrigin::Archived;
    assert!(matches!(
        encode(&mismatch),
        Err(CodecError::OperationOriginMismatch { .. })
    ));

    let mut archived = archived_record(Phase::Preparing);
    archived.previous.origin = PreviousOrigin::Unmanaged;
    assert!(matches!(
        encode(&archived),
        Err(CodecError::ArchiveOptionMismatch { .. })
    ));

    let mut reblit = reblit_record(Phase::Preparing);
    reblit.candidate.id = Some(99);
    assert!(matches!(encode(&reblit), Err(CodecError::ActiveReblitStateMismatch)));
}

#[test]
fn generated_tree_tokens_are_canonical_and_distinct() {
    let mut generated = std::collections::BTreeSet::new();
    for _ in 0..64 {
        let token = TreeToken::generate().unwrap();
        assert_eq!(token.as_str().len(), TreeToken::TEXT_LENGTH);
        assert_eq!(TreeToken::parse(token.as_str()).unwrap(), token);
        assert!(generated.insert(token), "kernel CSPRNG repeated a 128-bit tree token");
    }
}

#[test]
fn preparing_constructor_derives_wire_fields_and_rejects_invalid_operation_layouts() {
    let quarantine = QuarantineName::parse("constructor-proof").unwrap();
    assert_eq!(quarantine.as_str(), "constructor-proof");
    let previous = Previous {
        id: Some(41),
        tree_token: tree_token('b'),
        usr_runtime_identity: identity(20),
        origin: PreviousOrigin::ActiveState,
    };
    let record = TransitionRecord::preparing(
        id(),
        runtime_epoch(),
        Operation::ActivateArchived,
        Some(42),
        tree_token('a'),
        identity(10),
        previous.clone(),
        false,
        true,
        quarantine.clone(),
    )
    .unwrap();
    assert_eq!(record.format, PAYLOAD_FORMAT);
    assert_eq!(record.version, PAYLOAD_VERSION);
    assert_eq!(record.generation, 1);
    assert_eq!(record.phase, Phase::Preparing);
    assert_eq!(record.rollback, None);
    assert_eq!(record.creation_epoch, runtime_epoch());
    assert_eq!(record.candidate.origin, CandidateOrigin::Archived);
    assert_eq!(record.candidate.tree_token, tree_token('a'));
    assert!(record.options.archive_previous);
    assert!(!record.options.run_system_triggers);
    assert!(record.options.run_boot_sync);

    assert!(matches!(
        TransitionRecord::preparing(
            id(),
            runtime_epoch(),
            Operation::NewState,
            Some(42),
            tree_token('a'),
            identity(10),
            previous.clone(),
            true,
            true,
            quarantine.clone(),
        ),
        Err(CodecError::CandidateStateLayout)
    ));
    assert!(matches!(
        TransitionRecord::preparing(
            id(),
            runtime_epoch(),
            Operation::ActivateArchived,
            None,
            tree_token('a'),
            identity(10),
            previous,
            true,
            true,
            quarantine,
        ),
        Err(CodecError::ExistingCandidateStateMissing)
    ));
}

#[test]
fn preparing_pins_epoch_tokens_runtime_witnesses_and_operation_relationships_fail_closed() {
    for mount_namespace in [
        MountNamespaceIdentity { st_dev: 0, inode: 31 },
        MountNamespaceIdentity { st_dev: 30, inode: 0 },
    ] {
        let mut invalid = record(Phase::Preparing);
        invalid.creation_epoch.mount_namespace = mount_namespace;
        assert!(matches!(encode(&invalid), Err(CodecError::ZeroMountNamespaceIdentity)));
    }

    let mut invalid = record(Phase::Preparing);
    invalid.creation_epoch.boot_id = BootId("00000000-0000-0000-0000-000000000000".to_owned());
    assert!(matches!(encode(&invalid), Err(CodecError::InvalidBootId)));

    for runtime_identity in [
        RuntimeTreeIdentity {
            st_dev: 0,
            inode: 10,
            mount_id: 12,
        },
        RuntimeTreeIdentity {
            st_dev: 10,
            inode: 0,
            mount_id: 12,
        },
        RuntimeTreeIdentity {
            st_dev: 10,
            inode: 10,
            mount_id: 0,
        },
    ] {
        let mut invalid_candidate = record(Phase::Preparing);
        invalid_candidate.candidate.usr_runtime_identity = runtime_identity;
        assert!(matches!(
            encode(&invalid_candidate),
            Err(CodecError::ZeroRuntimeTreeIdentity)
        ));

        let mut invalid_previous = record(Phase::Preparing);
        invalid_previous.previous.usr_runtime_identity = runtime_identity;
        assert!(matches!(
            encode(&invalid_previous),
            Err(CodecError::ZeroRuntimeTreeIdentity)
        ));
    }

    let mut invalid = record(Phase::Preparing);
    invalid.candidate.tree_token = TreeToken("00000000000000000000000000000000".to_owned());
    assert!(matches!(encode(&invalid), Err(CodecError::InvalidTreeToken)));

    let mut invalid = record(Phase::Preparing);
    invalid.previous.tree_token = invalid.candidate.tree_token.clone();
    assert!(matches!(
        encode(&invalid),
        Err(CodecError::CandidatePreviousTreeTokenCollision)
    ));

    let mut invalid = record(Phase::Preparing);
    invalid.candidate.usr_runtime_identity = invalid.previous.usr_runtime_identity;
    invalid.candidate.usr_runtime_identity.mount_id += 1;
    assert!(matches!(
        encode(&invalid),
        Err(CodecError::CandidatePreviousObjectCollision)
    ));

    let mut invalid = record(Phase::Preparing);
    invalid.candidate.usr_runtime_identity.st_dev += 1;
    assert!(matches!(
        encode(&invalid),
        Err(CodecError::CandidatePreviousFilesystemMismatch { .. })
    ));

    let mut invalid = record(Phase::Preparing);
    invalid.candidate.usr_runtime_identity.mount_id += 1;
    assert!(matches!(
        encode(&invalid),
        Err(CodecError::CandidatePreviousMountMismatch { .. })
    ));

    let valid = record(Phase::Preparing);
    assert_eq!(
        valid.candidate.usr_runtime_identity.st_dev,
        valid.previous.usr_runtime_identity.st_dev
    );
    assert_eq!(
        valid.candidate.usr_runtime_identity.mount_id,
        valid.previous.usr_runtime_identity.mount_id
    );
    assert_ne!(
        valid.candidate.usr_runtime_identity.inode,
        valid.previous.usr_runtime_identity.inode
    );
    encode(&valid).unwrap();

    let mut invalid = record(Phase::Preparing);
    invalid.candidate.id = Some(42);
    assert!(matches!(encode(&invalid), Err(CodecError::CandidateStateLayout)));

    let mut invalid = record(Phase::CandidatePrepared);
    invalid.candidate.id = invalid.previous.id;
    assert!(matches!(
        encode(&invalid),
        Err(CodecError::CandidatePreviousStateCollision)
    ));

    let mut invalid = record(Phase::CandidatePrepared);
    invalid.candidate.usr_runtime_identity = invalid.previous.usr_runtime_identity;
    assert!(matches!(
        encode(&invalid),
        Err(CodecError::CandidatePreviousObjectCollision)
    ));

    let mut invalid = new_state_record(Phase::Preparing);
    invalid.previous.id = None;
    assert!(matches!(
        encode(&invalid),
        Err(CodecError::PreviousOriginStateMismatch { .. })
    ));

    let mut invalid =
        without_previous_archive(new_state_record(Phase::Preparing), PreviousOrigin::SynthesizedEmpty);
    invalid.previous.id = Some(41);
    assert!(matches!(
        encode(&invalid),
        Err(CodecError::PreviousOriginStateMismatch { .. })
    ));

    let invalid = archived_record(Phase::Preparing);
    assert_eq!(invalid.commit_disposition(), CommitDisposition::Archive);
    let invalid = reblit_record(Phase::Preparing);
    assert_eq!(invalid.commit_disposition(), CommitDisposition::Discard);
    let invalid = without_previous_archive(new_state_record(Phase::Preparing), PreviousOrigin::SynthesizedEmpty);
    assert_eq!(invalid.commit_disposition(), CommitDisposition::Discard);
    let invalid = without_previous_archive(new_state_record(Phase::Preparing), PreviousOrigin::Unmanaged);
    assert_eq!(invalid.commit_disposition(), CommitDisposition::Quarantine);
}
