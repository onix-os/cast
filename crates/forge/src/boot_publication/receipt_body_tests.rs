use super::*;

const ESP_PARTUUID: &str = "11111111-2222-3333-4444-555555555555";
const XBOOTLDR_PARTUUID: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";

fn witness(offset: u64) -> BootPublicationHistoricalRuntimeWitness {
    let minor = u32::try_from(offset).unwrap();
    BootPublicationHistoricalRuntimeWitness::new(
        u64::try_from(nix::libc::makedev(8, minor)).unwrap(),
        100 + offset,
        10 + offset,
        8,
        minor,
        Some(77),
    )
}

fn destination(partuuid: &str, partition_number: u32, offset: u64) -> BootPublicationDestination {
    BootPublicationDestination::new(partuuid, partition_number, witness(offset))
}

fn alias_destinations() -> BootPublicationDestinations {
    BootPublicationDestinations::boot_aliases_esp(destination(ESP_PARTUUID, 1, 1))
}

fn output(
    root: BootPublicationRoot,
    phase: BootPublicationPublicationPhase,
    role: BootPublicationOutputRole,
    path: &str,
    claim: BootPublicationOutputProvenanceClaim,
) -> BootPublicationOutput {
    BootPublicationOutput::new(
        root,
        phase,
        role,
        path,
        0o644,
        BootPublicationXxh3::from_u128(0x00112233445566778899aabbccddeeff),
        12,
        BootPublicationSha256::from_bytes([0x33; 32]),
        claim,
    )
}

fn body_with(
    destinations: BootPublicationDestinations,
    outputs: Vec<BootPublicationOutput>,
) -> Result<BootPublicationReceiptBody, BootPublicationReceiptBodyError> {
    BootPublicationReceiptBody::new(
        TransitionId::parse("00112233445566778899aabbccddeeff").unwrap(),
        None,
        BootPublicationSha256::from_bytes([0x11; 32]),
        BootPublicationSha256::from_bytes([0x22; 32]),
        destinations,
        outputs,
    )
}

#[test]
fn complete_body_retains_every_authority_free_provenance_claim() {
    let claims = [
        BootPublicationOutputProvenanceClaim::BorrowedFirstAdoption,
        BootPublicationOutputProvenanceClaim::UnclaimedAbsent,
        BootPublicationOutputProvenanceClaim::ClaimedPublishedByCast,
    ];
    let body = body_with(
        alias_destinations(),
        vec![
            output(
                BootPublicationRoot::Boot,
                BootPublicationPublicationPhase::Payload,
                BootPublicationOutputRole::Payload,
                "EFI/cast/vmlinuz",
                claims[0],
            ),
            output(
                BootPublicationRoot::Boot,
                BootPublicationPublicationPhase::Entry,
                BootPublicationOutputRole::Entry,
                "loader/entries/cast.conf",
                claims[1],
            ),
            output(
                BootPublicationRoot::Esp,
                BootPublicationPublicationPhase::Bootloader,
                BootPublicationOutputRole::FallbackBootloader,
                "EFI/Boot/BOOTX64.EFI",
                claims[2],
            ),
        ],
    )
    .unwrap();

    assert_eq!(body.outputs().len(), claims.len());
    for (output, claim) in body.outputs().iter().zip(claims) {
        assert_eq!(output.provenance_claim(), claim);
    }
    assert_eq!(
        claims.map(|claim| serde_json::to_string(&claim).unwrap()),
        [
            "\"borrowed-first-adoption\"",
            "\"unclaimed-absent\"",
            "\"claimed-published-by-cast\"",
        ]
    );
}

#[test]
fn destination_shape_is_exact_and_distinct_targets_must_not_alias() {
    let distinct = BootPublicationDestinations::distinct_xbootldr(
        destination(ESP_PARTUUID, 1, 1),
        destination(XBOOTLDR_PARTUUID, 2, 2),
    );
    let body = body_with(
        distinct,
        vec![output(
            BootPublicationRoot::Boot,
            BootPublicationPublicationPhase::Payload,
            BootPublicationOutputRole::Payload,
            "EFI/cast/vmlinuz",
            BootPublicationOutputProvenanceClaim::UnclaimedAbsent,
        )],
    )
    .unwrap();
    assert!(!body.destinations().aliases_esp());
    assert_eq!(body.destinations().esp().partuuid(), ESP_PARTUUID);
    assert_eq!(body.destinations().xbootldr().unwrap().partuuid(), XBOOTLDR_PARTUUID);

    assert_eq!(
        body_with(
            BootPublicationDestinations::distinct_xbootldr(
                destination(ESP_PARTUUID, 1, 1),
                destination(ESP_PARTUUID, 2, 2),
            ),
            body.outputs.clone(),
        ),
        Err(BootPublicationReceiptBodyError::DistinctPartuuidCollision)
    );
}

#[test]
fn historical_destination_device_must_match_its_partition_identity() {
    let mismatched = BootPublicationDestination::new(
        ESP_PARTUUID,
        1,
        BootPublicationHistoricalRuntimeWitness::new(
            u64::try_from(nix::libc::makedev(8, 2)).unwrap(),
            101,
            11,
            8,
            1,
            Some(77),
        ),
    );
    assert_eq!(
        body_with(
            BootPublicationDestinations::boot_aliases_esp(mismatched),
            vec![output(
                BootPublicationRoot::Boot,
                BootPublicationPublicationPhase::Payload,
                BootPublicationOutputRole::Payload,
                "EFI/cast/vmlinuz",
                BootPublicationOutputProvenanceClaim::UnclaimedAbsent,
            )],
        ),
        Err(BootPublicationReceiptBodyError::HistoricalPartitionDeviceMismatch {
            destination: "esp",
        })
    );
}

#[test]
fn distinct_destinations_require_distinct_partition_devices_and_equal_disk_sequences() {
    let output = output(
        BootPublicationRoot::Boot,
        BootPublicationPublicationPhase::Payload,
        BootPublicationOutputRole::Payload,
        "EFI/cast/vmlinuz",
        BootPublicationOutputProvenanceClaim::UnclaimedAbsent,
    );
    let same_partition_device = BootPublicationDestination::new(
        XBOOTLDR_PARTUUID,
        2,
        BootPublicationHistoricalRuntimeWitness::new(
            u64::try_from(nix::libc::makedev(8, 1)).unwrap(),
            202,
            12,
            8,
            1,
            Some(77),
        ),
    );
    assert_eq!(
        body_with(
            BootPublicationDestinations::distinct_xbootldr(
                destination(ESP_PARTUUID, 1, 1),
                same_partition_device,
            ),
            vec![output.clone()],
        ),
        Err(BootPublicationReceiptBodyError::DistinctPartitionIdentityCollision)
    );

    let different_disk_sequence = BootPublicationDestination::new(
        XBOOTLDR_PARTUUID,
        2,
        BootPublicationHistoricalRuntimeWitness::new(
            u64::try_from(nix::libc::makedev(8, 2)).unwrap(),
            202,
            12,
            8,
            2,
            Some(78),
        ),
    );
    assert_eq!(
        body_with(
            BootPublicationDestinations::distinct_xbootldr(
                destination(ESP_PARTUUID, 1, 1),
                different_disk_sequence,
            ),
            vec![output],
        ),
        Err(BootPublicationReceiptBodyError::DistinctDiskSequenceMismatch)
    );
}

#[test]
fn output_mode_is_restricted_to_canonical_active_reblit_mode() {
    let mut invalid = output(
        BootPublicationRoot::Boot,
        BootPublicationPublicationPhase::Payload,
        BootPublicationOutputRole::Payload,
        "EFI/cast/vmlinuz",
        BootPublicationOutputProvenanceClaim::UnclaimedAbsent,
    );
    invalid.mode = 0o600;
    assert_eq!(
        body_with(alias_destinations(), vec![invalid]),
        Err(BootPublicationReceiptBodyError::NonCanonicalOutputMode {
            index: 0,
            actual: 0o600,
        })
    );
}

#[test]
fn empty_oversized_and_unsafely_pathed_inventories_fail_closed() {
    assert_eq!(
        body_with(alias_destinations(), Vec::new()),
        Err(BootPublicationReceiptBodyError::EmptyOutputInventory)
    );

    let valid = output(
        BootPublicationRoot::Boot,
        BootPublicationPublicationPhase::Payload,
        BootPublicationOutputRole::Payload,
        "EFI/cast/vmlinuz",
        BootPublicationOutputProvenanceClaim::UnclaimedAbsent,
    );
    let mut oversized = body_with(alias_destinations(), vec![valid.clone()]).unwrap();
    oversized.outputs = vec![valid.clone(); MAX_BOOT_PUBLICATION_RECEIPT_OUTPUTS + 1];
    assert_eq!(
        oversized.validate(),
        Err(BootPublicationReceiptBodyError::OutputCountLimit {
            actual: MAX_BOOT_PUBLICATION_RECEIPT_OUTPUTS + 1,
        })
    );

    for path in ["/EFI/cast/vmlinuz", "EFI/../vmlinuz", "EFI/CON", "EFI/cast~1/vmlinuz"] {
        assert!(matches!(
            body_with(
                alias_destinations(),
                vec![output(
                    BootPublicationRoot::Boot,
                    BootPublicationPublicationPhase::Payload,
                    BootPublicationOutputRole::Payload,
                    path,
                    BootPublicationOutputProvenanceClaim::UnclaimedAbsent,
                )],
            ),
            Err(BootPublicationReceiptBodyError::UnsafeOutputPath { index: 0, .. })
        ));
    }

    let long = "a".repeat(MAX_BOOT_PUBLICATION_RECEIPT_SINGLE_PATH_BYTES + 1);
    assert!(matches!(
        body_with(
            alias_destinations(),
            vec![output(
                BootPublicationRoot::Boot,
                BootPublicationPublicationPhase::Payload,
                BootPublicationOutputRole::Payload,
                &long,
                BootPublicationOutputProvenanceClaim::UnclaimedAbsent,
            )],
        ),
        Err(BootPublicationReceiptBodyError::UnsafeOutputPath {
            reason: BootPublicationPathError::ByteLimit,
            ..
        })
    ));
}

#[test]
fn output_order_duplicate_and_fat_alias_collisions_are_rejected() {
    let first = output(
        BootPublicationRoot::Boot,
        BootPublicationPublicationPhase::Payload,
        BootPublicationOutputRole::Payload,
        "EFI/A/vmlinuz",
        BootPublicationOutputProvenanceClaim::UnclaimedAbsent,
    );
    let second = output(
        BootPublicationRoot::Boot,
        BootPublicationPublicationPhase::Payload,
        BootPublicationOutputRole::Payload,
        "EFI/B/vmlinuz",
        BootPublicationOutputProvenanceClaim::UnclaimedAbsent,
    );
    assert_eq!(
        body_with(alias_destinations(), vec![second.clone(), first.clone()]),
        Err(BootPublicationReceiptBodyError::NonCanonicalOutputOrder { index: 1 })
    );
    assert_eq!(
        body_with(alias_destinations(), vec![first.clone(), first]),
        Err(BootPublicationReceiptBodyError::OutputPathCollision { first: 0, second: 1 })
    );

    let folded_alias = output(
        BootPublicationRoot::Boot,
        BootPublicationPublicationPhase::Payload,
        BootPublicationOutputRole::Payload,
        "efi/a/vmlinuz",
        BootPublicationOutputProvenanceClaim::UnclaimedAbsent,
    );
    assert_eq!(
        body_with(alias_destinations(), vec![
            output(
                BootPublicationRoot::Boot,
                BootPublicationPublicationPhase::Payload,
                BootPublicationOutputRole::Payload,
                "EFI/A/vmlinuz",
                BootPublicationOutputProvenanceClaim::UnclaimedAbsent,
            ),
            folded_alias,
        ]),
        Err(BootPublicationReceiptBodyError::OutputPathCollision { first: 0, second: 1 })
    );
}
