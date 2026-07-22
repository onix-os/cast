use std::time::{Duration, Instant};

use super::*;
use crate::{
    boot_publication::{
        BootPublicationHistoricalRuntimeWitness, BootPublicationOutput,
        BootPublicationOutputProvenanceClaim, BootPublicationOutputRole,
        BootPublicationPublicationPhase, BootPublicationReceiptBody,
        BootPublicationRoot, BootPublicationSha256, BootPublicationXxh3,
        prepare_boot_publication_receipt,
    },
    client::{
        active_reblit_mounted_boot_topology::{
            AliasFixture, RevalidatedActiveReblitBootPublicationTargets,
        },
    },
    db::state::{
        BootPublicationReceiptPromotionOutcome,
        BootPublicationReceiptStageOutcome, CurrentExactPromotedBootPublicationReceiptChain,
        Database,
    },
    state::TransitionId,
};

const ESP_PARTUUID: &str = "5e85a94f-b115-41c5-9d72-9d23958b5edc";
const XBOOTLDR_PARTUUID: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";

fn historical_witness(major: u32, minor: u32, seed: u64) -> BootPublicationHistoricalRuntimeWitness {
    BootPublicationHistoricalRuntimeWitness::new(
        nix::libc::makedev(major, minor),
        100 + seed,
        10 + seed,
        major,
        minor,
        Some(1_000 + seed),
    )
}

fn destination(
    partuuid: &str,
    partition_number: u32,
    major: u32,
    minor: u32,
    seed: u64,
) -> BootPublicationDestination {
    BootPublicationDestination::new(
        partuuid,
        partition_number,
        historical_witness(major, minor, seed),
    )
}

fn alias_destinations(runtime_seed: u64) -> BootPublicationDestinations {
    BootPublicationDestinations::boot_aliases_esp(destination(
        ESP_PARTUUID,
        1,
        8,
        1,
        runtime_seed,
    ))
}

fn distinct_destinations(runtime_seed: u64) -> BootPublicationDestinations {
    let esp = BootPublicationDestination::new(
        ESP_PARTUUID,
        1,
        BootPublicationHistoricalRuntimeWitness::new(
            nix::libc::makedev(8, 1),
            100 + runtime_seed,
            10 + runtime_seed,
            8,
            1,
            Some(2_000 + runtime_seed),
        ),
    );
    let xbootldr = BootPublicationDestination::new(
        XBOOTLDR_PARTUUID,
        2,
        BootPublicationHistoricalRuntimeWitness::new(
            nix::libc::makedev(8, 2),
            200 + runtime_seed,
            20 + runtime_seed,
            8,
            2,
            Some(2_000 + runtime_seed),
        ),
    );
    BootPublicationDestinations::distinct_xbootldr(esp, xbootldr)
}

fn promoted_alias_chain(
    runtime_seed: u64,
) -> ExactPromotedBootPublicationReceiptChain {
    let output = BootPublicationOutput::new(
        BootPublicationRoot::Boot,
        BootPublicationPublicationPhase::Payload,
        BootPublicationOutputRole::Payload,
        "EFI/Linux/restart-test.efi",
        0o644,
        BootPublicationXxh3::from_u128(0x44),
        64,
        BootPublicationSha256::from_bytes([0x55; 32]),
        BootPublicationOutputProvenanceClaim::UnclaimedAbsent,
    );
    let receipt = prepare_boot_publication_receipt(
        BootPublicationReceiptBody::new(
            TransitionId::parse("1".repeat(TransitionId::TEXT_LENGTH)).unwrap(),
            None,
            BootPublicationSha256::from_bytes([0x11; 32]),
            BootPublicationSha256::from_bytes([0x22; 32]),
            alias_destinations(runtime_seed),
            vec![output],
        )
        .unwrap(),
    )
    .unwrap();
    let database = Database::new(":memory:").unwrap();
    assert_eq!(
        database.stage_boot_publication_receipt(&receipt).unwrap(),
        BootPublicationReceiptStageOutcome::Staged,
    );
    assert_eq!(
        database
            .promote_boot_publication_receipt(
                &receipt,
                Instant::now() + Duration::from_secs(30),
            )
            .unwrap(),
        BootPublicationReceiptPromotionOutcome::Promoted,
    );
    let CurrentExactPromotedBootPublicationReceiptChain::Installed(chain) = database
        .load_current_exact_promoted_boot_publication_receipt_chain()
        .unwrap()
    else {
        panic!("promoted fixture did not load as installed")
    };
    chain
}

#[test]
fn fresh_alias_targets_bind_stable_receipt_identity_while_ignoring_runtime_history() {
    let fixture = AliasFixture::stable().unwrap();
    let operation_deadline = Instant::now() + Duration::from_secs(30);
    let prepared = fixture.prepare_until(operation_deadline).unwrap();
    let topology = prepared
        .revalidate_until(fixture.installation(), operation_deadline)
        .unwrap();
    let chain = promoted_alias_chain(900);

    let validated = topology
        .revalidate_promoted_receipt_targets(&chain)
        .unwrap();

    assert!(validated.aliases_esp);
    assert_eq!(
        validated.promoted_receipt,
        chain.installed_receipt().fingerprint()
    );
    assert!(matches!(
        validated.targets,
        RevalidatedActiveReblitBootPublicationTargets::BootAliasesEsp { .. }
    ));
    fixture.assert_outside_unchanged();
}

#[test]
fn stable_alias_and_distinct_destination_shapes_validate_exactly() {
    let alias = StableLiveBootDestinations::BootAliasesEsp {
        esp: StableLiveBootDestination {
            partuuid: ESP_PARTUUID,
            partition_number: 1,
        },
    };
    require_stable_destinations(alias, &alias_destinations(10)).unwrap();

    let distinct = StableLiveBootDestinations::DistinctXbootldr {
        esp: StableLiveBootDestination {
            partuuid: ESP_PARTUUID,
            partition_number: 1,
        },
        xbootldr: StableLiveBootDestination {
            partuuid: XBOOTLDR_PARTUUID,
            partition_number: 2,
        },
    };
    require_stable_destinations(distinct, &distinct_destinations(20)).unwrap();
}

#[test]
fn layout_partuuid_and_partition_number_mismatches_fail_closed() {
    let alias = StableLiveBootDestinations::BootAliasesEsp {
        esp: StableLiveBootDestination {
            partuuid: ESP_PARTUUID,
            partition_number: 1,
        },
    };
    assert!(matches!(
        require_stable_destinations(alias, &distinct_destinations(30)),
        Err(ActiveReblitBootReceiptTargetValidationError::LayoutMismatch)
    ));

    for live in [
        StableLiveBootDestination {
            partuuid: XBOOTLDR_PARTUUID,
            partition_number: 1,
        },
        StableLiveBootDestination {
            partuuid: ESP_PARTUUID,
            partition_number: 2,
        },
    ] {
        assert!(matches!(
            require_stable_destinations(
                StableLiveBootDestinations::BootAliasesEsp { esp: live },
                &alias_destinations(40),
            ),
            Err(
                ActiveReblitBootReceiptTargetValidationError::StableIdentityMismatch {
                    destination: "esp"
                }
            )
        ));
    }
}

#[test]
fn distinct_xbootldr_identity_mismatch_is_role_specific() {
    let live = StableLiveBootDestinations::DistinctXbootldr {
        esp: StableLiveBootDestination {
            partuuid: ESP_PARTUUID,
            partition_number: 1,
        },
        xbootldr: StableLiveBootDestination {
            partuuid: XBOOTLDR_PARTUUID,
            partition_number: 3,
        },
    };
    assert!(matches!(
        require_stable_destinations(live, &distinct_destinations(50)),
        Err(
            ActiveReblitBootReceiptTargetValidationError::StableIdentityMismatch {
                destination: "xbootldr"
            }
        )
    ));
}
