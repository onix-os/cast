use std::time::{Duration, Instant};

use super::*;
use crate::{
    boot_publication::{
        BootPublicationDestination, BootPublicationDestinations,
        BootPublicationHistoricalRuntimeWitness, BootPublicationOutput,
        BootPublicationOutputProvenanceClaim, BootPublicationOutputRole,
        BootPublicationPublicationPhase, BootPublicationReceiptBody,
        BootPublicationRoot, BootPublicationSha256, BootPublicationXxh3,
        CanonicalBootPublicationReceipt, prepare_boot_publication_receipt,
    },
    db::state::{
        BootPublicationReceiptPromotionOutcome, BootPublicationReceiptStageOutcome,
        Database, ExactPromotedBootPublicationReceiptChain,
    },
    state::TransitionId,
};

const ESP_PARTUUID: &str = "11111111-2222-3333-4444-555555555555";
const OTHER_ESP_PARTUUID: &str = "22222222-3333-4444-5555-666666666666";
const XBOOTLDR_PARTUUID: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";

fn transition(digit: char) -> TransitionId {
    TransitionId::parse(digit.to_string().repeat(TransitionId::TEXT_LENGTH)).unwrap()
}

fn witness(seed: u64, partition_minor: u32) -> BootPublicationHistoricalRuntimeWitness {
    BootPublicationHistoricalRuntimeWitness::new(
        2_048 + u64::from(partition_minor),
        100 + seed,
        10 + seed,
        8,
        partition_minor,
        Some(70 + seed),
    )
}

fn alias_destinations(
    partuuid: &str,
    partition_number: u32,
    runtime_seed: u64,
) -> BootPublicationDestinations {
    BootPublicationDestinations::boot_aliases_esp(BootPublicationDestination::new(
        partuuid,
        partition_number,
        witness(runtime_seed, 1),
    ))
}

fn distinct_destinations(runtime_seed: u64) -> BootPublicationDestinations {
    let disk_sequence = 70 + runtime_seed;
    BootPublicationDestinations::distinct_xbootldr(
        BootPublicationDestination::new(
            ESP_PARTUUID,
            1,
            BootPublicationHistoricalRuntimeWitness::new(
                2_049,
                100 + runtime_seed,
                10 + runtime_seed,
                8,
                1,
                Some(disk_sequence),
            ),
        ),
        BootPublicationDestination::new(
            XBOOTLDR_PARTUUID,
            2,
            BootPublicationHistoricalRuntimeWitness::new(
                2_050,
                200 + runtime_seed,
                20 + runtime_seed,
                8,
                2,
                Some(disk_sequence),
            ),
        ),
    )
}

fn payload(
    path: &str,
    content: u8,
    claim: BootPublicationOutputProvenanceClaim,
) -> BootPublicationOutput {
    output(
        BootPublicationRoot::Boot,
        BootPublicationPublicationPhase::Payload,
        BootPublicationOutputRole::Payload,
        path,
        content,
        claim,
    )
}

fn fallback_bootloader(
    path: &str,
    content: u8,
    claim: BootPublicationOutputProvenanceClaim,
) -> BootPublicationOutput {
    output(
        BootPublicationRoot::Esp,
        BootPublicationPublicationPhase::Bootloader,
        BootPublicationOutputRole::FallbackBootloader,
        path,
        content,
        claim,
    )
}

fn output(
    root: BootPublicationRoot,
    phase: BootPublicationPublicationPhase,
    role: BootPublicationOutputRole,
    path: &str,
    content: u8,
    claim: BootPublicationOutputProvenanceClaim,
) -> BootPublicationOutput {
    BootPublicationOutput::new(
        root,
        phase,
        role,
        path,
        0o644,
        BootPublicationXxh3::from_u128(u128::from(content) + 1),
        u64::from(content) + 10,
        BootPublicationSha256::from_bytes([content; 32]),
        claim,
    )
}

fn receipt(
    digit: char,
    predecessor: Option<BootPublicationReceiptFingerprint>,
    salt: u8,
    destinations: BootPublicationDestinations,
    outputs: Vec<BootPublicationOutput>,
) -> CanonicalBootPublicationReceipt {
    prepare_boot_publication_receipt(
        BootPublicationReceiptBody::new(
            transition(digit),
            predecessor,
            BootPublicationSha256::from_bytes([salt; 32]),
            BootPublicationSha256::from_bytes([salt.wrapping_add(1); 32]),
            destinations,
            outputs,
        )
        .unwrap(),
    )
    .unwrap()
}

fn promote(database: &Database, receipt: &CanonicalBootPublicationReceipt) {
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

fn promoted_chain(
    predecessor: Option<&CanonicalBootPublicationReceipt>,
    installed: &CanonicalBootPublicationReceipt,
) -> ExactPromotedBootPublicationReceiptChain {
    let database = Database::new(":memory:").unwrap();
    if let Some(predecessor) = predecessor {
        promote(&database, predecessor);
    }
    promote(&database, installed);
    database
        .load_current_exact_promoted_boot_publication_receipt_chain()
        .unwrap()
        .into_installed_for_test()
}

trait InstalledChainForTest {
    fn into_installed_for_test(self) -> ExactPromotedBootPublicationReceiptChain;
}

impl InstalledChainForTest for crate::db::state::CurrentExactPromotedBootPublicationReceiptChain {
    fn into_installed_for_test(self) -> ExactPromotedBootPublicationReceiptChain {
        let Self::Installed(chain) = self else {
            panic!("the fixture promoted an installed receipt");
        };
        chain
    }
}

#[test]
fn classifies_noop_replacement_owned_stale_and_unowned_preserve() {
    let predecessor = receipt(
        '1',
        None,
        0x11,
        alias_destinations(ESP_PARTUUID, 1, 1),
        vec![
            payload(
                "EFI/Linux/keep.efi",
                1,
                BootPublicationOutputProvenanceClaim::UnclaimedAbsent,
            ),
            payload(
                "EFI/Linux/replace.efi",
                2,
                BootPublicationOutputProvenanceClaim::ClaimedPublishedByCast,
            ),
            payload(
                "EFI/Linux/stale-borrowed.efi",
                3,
                BootPublicationOutputProvenanceClaim::BorrowedFirstAdoption,
            ),
            payload(
                "EFI/Linux/stale-owned.efi",
                4,
                BootPublicationOutputProvenanceClaim::ClaimedPublishedByCast,
            ),
        ],
    );
    let installed = receipt(
        '2',
        Some(predecessor.fingerprint()),
        0x22,
        alias_destinations(ESP_PARTUUID, 1, 2),
        vec![
            payload(
                "EFI/Linux/keep.efi",
                1,
                BootPublicationOutputProvenanceClaim::ClaimedPublishedByCast,
            ),
            payload(
                "EFI/Linux/new-borrowed.efi",
                5,
                BootPublicationOutputProvenanceClaim::BorrowedFirstAdoption,
            ),
            payload(
                "EFI/Linux/replace.efi",
                8,
                BootPublicationOutputProvenanceClaim::ClaimedPublishedByCast,
            ),
        ],
    );
    let chain = promoted_chain(Some(&predecessor), &installed);

    let plan = chain
        .prepare_active_reblit_promoted_boot_cleanup_plan()
        .unwrap();
    assert_eq!(plan.promoted_receipt(), installed.fingerprint());
    assert_eq!(plan.entries().len(), 4);
    assert!(matches!(
        plan.entries()[0].disposition(),
        ActiveReblitPromotedBootCleanupDisposition::NoOp
    ));
    assert!(matches!(
        plan.entries()[1].disposition(),
        ActiveReblitPromotedBootCleanupDisposition::ReplaceOwned
    ));
    assert!(matches!(
        plan.entries()[2].disposition(),
        ActiveReblitPromotedBootCleanupDisposition::PreserveUnownedStale
    ));
    assert!(matches!(
        plan.entries()[3].disposition(),
        ActiveReblitPromotedBootCleanupDisposition::DeleteOwnedStale
    ));
    assert_eq!(
        plan.entries()[0].predecessor_output().relative_path(),
        "EFI/Linux/keep.efi"
    );
    assert_eq!(
        plan.entries()[1]
            .installed_output()
            .unwrap()
            .relative_path(),
        "EFI/Linux/replace.efi"
    );
    assert!(plan.entries()[2].installed_output().is_none());
    assert!(plan.entries()[3].installed_output().is_none());
}

#[test]
fn first_receipt_has_no_cleanup_and_rejects_false_prior_ownership() {
    let first = receipt(
        '3',
        None,
        0x31,
        alias_destinations(ESP_PARTUUID, 1, 3),
        vec![
            payload(
                "EFI/Linux/adopted.efi",
                1,
                BootPublicationOutputProvenanceClaim::BorrowedFirstAdoption,
            ),
            payload(
                "EFI/Linux/published.efi",
                2,
                BootPublicationOutputProvenanceClaim::UnclaimedAbsent,
            ),
        ],
    );
    let chain = promoted_chain(None, &first);
    assert!(
        chain
            .prepare_active_reblit_promoted_boot_cleanup_plan()
            .unwrap()
            .entries()
            .is_empty()
    );

    let false_owned = receipt(
        '4',
        None,
        0x41,
        alias_destinations(ESP_PARTUUID, 1, 4),
        vec![payload(
            "EFI/Linux/false-owned.efi",
            3,
            BootPublicationOutputProvenanceClaim::ClaimedPublishedByCast,
        )],
    );
    let chain = promoted_chain(None, &false_owned);
    assert!(matches!(
        chain.prepare_active_reblit_promoted_boot_cleanup_plan(),
        Err(ActiveReblitPromotedBootCleanupPlanError::CurrentOnlyOwnershipClaim {
            installed_index: 0
        })
    ));
}

#[test]
fn historical_runtime_drift_does_not_change_stable_destination_identity() {
    let predecessor = receipt(
        '5',
        None,
        0x51,
        alias_destinations(ESP_PARTUUID, 1, 5),
        vec![payload(
            "EFI/Linux/stable.efi",
            1,
            BootPublicationOutputProvenanceClaim::UnclaimedAbsent,
        )],
    );
    let installed = receipt(
        '6',
        Some(predecessor.fingerprint()),
        0x61,
        alias_destinations(ESP_PARTUUID, 1, 600),
        vec![payload(
            "EFI/Linux/stable.efi",
            1,
            BootPublicationOutputProvenanceClaim::ClaimedPublishedByCast,
        )],
    );
    let chain = promoted_chain(Some(&predecessor), &installed);

    let plan = chain
        .prepare_active_reblit_promoted_boot_cleanup_plan()
        .unwrap();
    assert!(matches!(
        plan.entries()[0].disposition(),
        ActiveReblitPromotedBootCleanupDisposition::NoOp
    ));
}

#[test]
fn destination_layout_partuuid_and_partition_number_mismatches_fail_closed() {
    let predecessor = receipt(
        '7',
        None,
        0x71,
        alias_destinations(ESP_PARTUUID, 1, 7),
        vec![payload(
            "EFI/Linux/stable.efi",
            1,
            BootPublicationOutputProvenanceClaim::UnclaimedAbsent,
        )],
    );
    for (digit, destinations, expected_layout_mismatch) in [
        ('8', distinct_destinations(8), true),
        (
            '9',
            alias_destinations(OTHER_ESP_PARTUUID, 1, 9),
            false,
        ),
        ('a', alias_destinations(ESP_PARTUUID, 2, 10), false),
    ] {
        let installed = receipt(
            digit,
            Some(predecessor.fingerprint()),
            digit as u8,
            destinations,
            vec![payload(
                "EFI/Linux/stable.efi",
                1,
                BootPublicationOutputProvenanceClaim::ClaimedPublishedByCast,
            )],
        );
        let chain = promoted_chain(Some(&predecessor), &installed);
        let result = chain.prepare_active_reblit_promoted_boot_cleanup_plan();
        if expected_layout_mismatch {
            assert!(matches!(
                result,
                Err(ActiveReblitPromotedBootCleanupPlanError::DestinationLayoutMismatch)
            ));
        } else {
            assert!(matches!(
                result,
                Err(ActiveReblitPromotedBootCleanupPlanError::StableDestinationMismatch {
                    destination: "esp"
                })
            ));
        }
    }
}

#[test]
fn fat_casefold_collision_with_different_path_semantics_fails_closed() {
    let predecessor = receipt(
        'b',
        None,
        0xb1,
        alias_destinations(ESP_PARTUUID, 1, 11),
        vec![payload(
            "EFI/Linux/Cast.efi",
            1,
            BootPublicationOutputProvenanceClaim::UnclaimedAbsent,
        )],
    );
    let installed = receipt(
        'c',
        Some(predecessor.fingerprint()),
        0xc1,
        alias_destinations(ESP_PARTUUID, 1, 12),
        vec![payload(
            "efi/linux/cast.efi",
            1,
            BootPublicationOutputProvenanceClaim::ClaimedPublishedByCast,
        )],
    );
    let chain = promoted_chain(Some(&predecessor), &installed);

    assert!(matches!(
        chain.prepare_active_reblit_promoted_boot_cleanup_plan(),
        Err(
            ActiveReblitPromotedBootCleanupPlanError::CrossReceiptPhysicalKeyMismatch {
                predecessor_index: 0,
                installed_index: 0,
            }
        )
    ));
}

#[test]
fn cross_receipt_ancestor_descendant_collision_fails_closed() {
    let predecessor = receipt(
        'd',
        None,
        0xd1,
        alias_destinations(ESP_PARTUUID, 1, 13),
        vec![payload(
            "EFI/Linux/cast",
            1,
            BootPublicationOutputProvenanceClaim::UnclaimedAbsent,
        )],
    );
    let installed = receipt(
        'e',
        Some(predecessor.fingerprint()),
        0xe1,
        alias_destinations(ESP_PARTUUID, 1, 14),
        vec![payload(
            "EFI/Linux/cast/kernel.efi",
            2,
            BootPublicationOutputProvenanceClaim::UnclaimedAbsent,
        )],
    );
    let chain = promoted_chain(Some(&predecessor), &installed);

    assert!(matches!(
        chain.prepare_active_reblit_promoted_boot_cleanup_plan(),
        Err(ActiveReblitPromotedBootCleanupPlanError::CrossReceiptHierarchyConflict)
    ));
}

#[test]
fn aliases_share_cross_root_keys_while_distinct_destinations_do_not() {
    let alias_predecessor = receipt(
        'f',
        None,
        0xf1,
        alias_destinations(ESP_PARTUUID, 1, 15),
        vec![payload(
            "EFI/BOOT/BOOTX64.EFI",
            1,
            BootPublicationOutputProvenanceClaim::UnclaimedAbsent,
        )],
    );
    let alias_installed = receipt(
        '0',
        Some(alias_predecessor.fingerprint()),
        0x01,
        alias_destinations(ESP_PARTUUID, 1, 16),
        vec![fallback_bootloader(
            "EFI/BOOT/BOOTX64.EFI",
            2,
            BootPublicationOutputProvenanceClaim::UnclaimedAbsent,
        )],
    );
    let chain = promoted_chain(Some(&alias_predecessor), &alias_installed);
    assert!(matches!(
        chain.prepare_active_reblit_promoted_boot_cleanup_plan(),
        Err(ActiveReblitPromotedBootCleanupPlanError::CrossReceiptPhysicalKeyMismatch {
            ..
        })
    ));

    let distinct_predecessor = receipt(
        '1',
        None,
        0x12,
        distinct_destinations(17),
        vec![payload(
            "EFI/BOOT/BOOTX64.EFI",
            1,
            BootPublicationOutputProvenanceClaim::UnclaimedAbsent,
        )],
    );
    let distinct_installed = receipt(
        '2',
        Some(distinct_predecessor.fingerprint()),
        0x23,
        distinct_destinations(18),
        vec![fallback_bootloader(
            "EFI/BOOT/BOOTX64.EFI",
            2,
            BootPublicationOutputProvenanceClaim::UnclaimedAbsent,
        )],
    );
    let chain = promoted_chain(Some(&distinct_predecessor), &distinct_installed);
    let plan = chain
        .prepare_active_reblit_promoted_boot_cleanup_plan()
        .unwrap();
    assert!(matches!(
        plan.entries()[0].disposition(),
        ActiveReblitPromotedBootCleanupDisposition::DeleteOwnedStale
    ));
}

#[test]
fn borrowed_replacement_and_invalid_retained_provenance_fail_closed() {
    let borrowed = receipt(
        '3',
        None,
        0x34,
        alias_destinations(ESP_PARTUUID, 1, 19),
        vec![payload(
            "EFI/Linux/borrowed.efi",
            1,
            BootPublicationOutputProvenanceClaim::BorrowedFirstAdoption,
        )],
    );
    let replacement = receipt(
        '4',
        Some(borrowed.fingerprint()),
        0x45,
        alias_destinations(ESP_PARTUUID, 1, 20),
        vec![payload(
            "EFI/Linux/borrowed.efi",
            2,
            BootPublicationOutputProvenanceClaim::ClaimedPublishedByCast,
        )],
    );
    let chain = promoted_chain(Some(&borrowed), &replacement);
    assert!(matches!(
        chain.prepare_active_reblit_promoted_boot_cleanup_plan(),
        Err(ActiveReblitPromotedBootCleanupPlanError::BorrowedReplacement {
            predecessor_index: 0,
            installed_index: 0,
        })
    ));

    let owned = receipt(
        '5',
        None,
        0x56,
        alias_destinations(ESP_PARTUUID, 1, 21),
        vec![payload(
            "EFI/Linux/owned.efi",
            3,
            BootPublicationOutputProvenanceClaim::UnclaimedAbsent,
        )],
    );
    let invalid_retain = receipt(
        '6',
        Some(owned.fingerprint()),
        0x67,
        alias_destinations(ESP_PARTUUID, 1, 22),
        vec![payload(
            "EFI/Linux/owned.efi",
            3,
            BootPublicationOutputProvenanceClaim::UnclaimedAbsent,
        )],
    );
    let chain = promoted_chain(Some(&owned), &invalid_retain);
    assert!(matches!(
        chain.prepare_active_reblit_promoted_boot_cleanup_plan(),
        Err(ActiveReblitPromotedBootCleanupPlanError::RetainedProvenanceMismatch {
            predecessor_index: 0,
            installed_index: 0,
        })
    ));
}
