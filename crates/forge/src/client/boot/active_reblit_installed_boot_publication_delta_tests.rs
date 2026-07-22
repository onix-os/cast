use std::time::{Duration, Instant};

use super::*;
use crate::{
    Installation, db, state,
    boot_publication::{
        BootPublicationDestination, BootPublicationHistoricalRuntimeWitness,
        BootPublicationOutputProvenanceClaim, BootPublicationReceiptBody,
        BootPublicationSha256, BootPublicationXxh3, prepare_boot_publication_receipt,
    },
    client::{
        active_reblit_bls_renderer::RenderedActiveReblitBlsRequests,
        active_reblit_boot_inputs::PreparedActiveReblitStoneBootInputs,
        active_reblit_boot_render_inputs::PreparedActiveReblitBootRenderInputs,
        active_reblit_mounted_boot_topology::AliasFixture,
    },
    db::state::{
        BootPublicationReceiptPromotionOutcome, BootPublicationReceiptStageOutcome,
        Database as StateDatabase,
    },
    linux_fs::descriptor_boot_namespace::BootNamespaceDestinationState,
    state::TransitionId,
};

#[path = "active_reblit_boot_render_inputs_tests/support.rs"]
mod support;

macro_rules! with_bound_alias_plan {
    (|$plan:ident| $body:block) => {{
        let deadline = support::future_deadline();
        let fixture = support::simple_fixture();
        let stone = fixture.stone();
        let roots = fixture.roots(&stone);
        let prepared = support::prepare_static(&fixture, &stone, &roots);
        let local_policy = fixture.local_policy();
        let root_intent = fixture.root_intent();
        let inputs = prepared
            .revalidate_until(
                &fixture.state_db,
                &fixture.layout_db,
                &fixture.installation,
                &local_policy,
                &root_intent,
                deadline,
            )
            .unwrap();
        let topology_fixture = AliasFixture::stable().expect("alias topology fixture must prepare");
        let topology_prepared = topology_fixture.prepare_until(deadline).unwrap();
        let topology = topology_prepared
            .revalidate_until(topology_fixture.installation(), deadline)
            .unwrap();
        let rendered = RenderedActiveReblitBlsRequests::render(&inputs).unwrap();
        let $plan = rendered.into_publication_plan(&topology).unwrap();
        $body
        topology_fixture.assert_outside_unchanged();
    }};
}

fn expected(seed: u8) -> ActiveReblitBootPublicationDeltaExpected {
    ActiveReblitBootPublicationDeltaExpected {
        checksum: u128::from(seed),
        length: u64::from(seed),
        content_identity: BootContentIdentity::from_sha256([seed; 32]),
    }
}

fn request(
    desired: Option<ActiveReblitBootPublicationDeltaExpected>,
    installed: Option<ActiveReblitBootPublicationDeltaExpected>,
    installed_owned: bool,
) -> ActiveReblitBootPublicationDeltaRequest {
    ActiveReblitBootPublicationDeltaRequest {
        root: ActiveReblitBootDestinationRoot::Boot,
        relative_path: "EFI/cast/output".into(),
        desired,
        installed,
        installed_owned,
    }
}

fn prepared(
    requests: Vec<ActiveReblitBootPublicationDeltaRequest>,
) -> PreparedActiveReblitBootPublicationDelta {
    PreparedActiveReblitBootPublicationDelta {
        destination_layout: ActiveReblitBootDestinationLayout::BootAliasesEsp,
        requests,
    }
}

#[test]
fn desired_absent_exact_and_owned_different_have_closed_actions() {
    let old = expected(1);
    let new = expected(2);
    let requests = [
        request(Some(new), None, false),
        request(Some(new), Some(new), true),
        request(Some(new), None, false),
        request(Some(new), Some(old), true),
    ];
    assert_eq!(
        requests
            .iter()
            .zip([
                BootNamespaceDestinationState::Absent,
                BootNamespaceDestinationState::Exact,
                BootNamespaceDestinationState::Exact,
                BootNamespaceDestinationState::Different,
            ])
            .enumerate()
            .map(|(index, (request, state))| {
                live_classification::classify_desired_for_test(index, request, state).unwrap()
            })
            .collect::<Vec<_>>(),
        [
            ActiveReblitBootPublicationDeltaAction::PublishDesired,
            ActiveReblitBootPublicationDeltaAction::RetainOwnedDesired,
            ActiveReblitBootPublicationDeltaAction::PreserveBorrowedDesired,
            ActiveReblitBootPublicationDeltaAction::ReplaceOwnedDesired,
        ],
    );
}

#[test]
fn stale_owned_is_post_promotion_deletion_and_stale_unowned_is_preserved() {
    let old = expected(3);
    let requests = [
        request(None, Some(old), true),
        request(None, Some(old), false),
    ];
    assert_eq!(
        requests
            .iter()
            .enumerate()
            .map(|(index, request)| {
                live_classification::classify_stale_for_test(index, request).unwrap()
            })
            .collect::<Vec<_>>(),
        [
            ActiveReblitBootPublicationDeltaAction::DeleteOwnedStaleAfterPromotion,
            ActiveReblitBootPublicationDeltaAction::PreserveUnownedStale,
        ],
    );
}

#[test]
fn different_desired_without_authenticated_owned_predecessor_fails_closed() {
    let new = expected(4);
    let old = expected(5);
    for (installed, owned) in [(None, false), (Some(old), false)] {
        let request = request(Some(new), installed, owned);
        assert!(matches!(
            live_classification::classify_desired_for_test(
                0,
                &request,
                BootNamespaceDestinationState::Different,
            ),
            Err(ActiveReblitBootPublicationDeltaError::UnownedDifferentDesired { index: 0 })
        ));
    }
}

#[test]
fn owned_marker_without_installed_identity_fails_closed() {
    let same = expected(6);
    let request = request(Some(same), None, true);
    assert!(matches!(
        live_classification::classify_desired_for_test(
            0,
            &request,
            BootNamespaceDestinationState::Exact,
        ),
        Err(ActiveReblitBootPublicationDeltaError::OwnedOutputWithoutInstalledIdentity {
            index: 0,
        })
    ));
}

#[test]
fn alias_and_distinct_keys_use_fat_folding_without_cross_root_confusion() {
    let alias_esp = physical_key(
        ActiveReblitBootDestinationLayout::BootAliasesEsp,
        ActiveReblitBootDestinationRoot::Esp,
        "EFI/Boot/BOOTX64.EFI",
    )
    .unwrap();
    let alias_boot = physical_key(
        ActiveReblitBootDestinationLayout::BootAliasesEsp,
        ActiveReblitBootDestinationRoot::Boot,
        "efi/boot/bootx64.efi",
    )
    .unwrap();
    assert_eq!(alias_esp, alias_boot);

    let distinct_esp = physical_key(
        ActiveReblitBootDestinationLayout::DistinctXbootldr,
        ActiveReblitBootDestinationRoot::Esp,
        "EFI/Boot/BOOTX64.EFI",
    )
    .unwrap();
    let distinct_boot = physical_key(
        ActiveReblitBootDestinationLayout::DistinctXbootldr,
        ActiveReblitBootDestinationRoot::Boot,
        "efi/boot/bootx64.efi",
    )
    .unwrap();
    assert_ne!(distinct_esp, distinct_boot);
}

fn receipt(claim: BootPublicationOutputProvenanceClaim) -> crate::boot_publication::CanonicalBootPublicationReceipt {
    let transition_id = TransitionId::parse("a".repeat(TransitionId::TEXT_LENGTH)).unwrap();
    let body = BootPublicationReceiptBody::new(
        transition_id,
        None,
        BootPublicationSha256::from_bytes([1; 32]),
        BootPublicationSha256::from_bytes([2; 32]),
        BootPublicationDestinations::boot_aliases_esp(BootPublicationDestination::new(
            "11111111-2222-3333-4444-555555555555",
            1,
            BootPublicationHistoricalRuntimeWitness::new(2_049, 10, 20, 8, 1, Some(30)),
        )),
        vec![BootPublicationOutput::new(
            BootPublicationRoot::Boot,
            BootPublicationPublicationPhase::Payload,
            BootPublicationOutputRole::Payload,
            "EFI/cast/output",
            0o644,
            BootPublicationXxh3::from_u128(7),
            7,
            BootPublicationSha256::from_bytes([7; 32]),
            claim,
        )],
    )
    .unwrap();
    prepare_boot_publication_receipt(body).unwrap()
}

#[test]
fn only_strict_empty_or_promoted_database_state_can_form_installed_input() {
    let database = StateDatabase::new(":memory:").unwrap();
    let empty = database.boot_publication_receipt_state().unwrap();
    assert!(AuthenticatedActiveReblitInstalledBootPublication::from_strict_empty_state(&empty)
        .unwrap()
        .receipt()
        .is_none());

    let receipt = receipt(BootPublicationOutputProvenanceClaim::UnclaimedAbsent);
    assert_eq!(
        database.stage_boot_publication_receipt(&receipt).unwrap(),
        BootPublicationReceiptStageOutcome::Staged,
    );
    let pending = database.boot_publication_receipt_state().unwrap();
    assert!(matches!(
        AuthenticatedActiveReblitInstalledBootPublication::from_strict_empty_state(&pending),
        Err(ActiveReblitBootPublicationDeltaError::InstalledStatePending)
    ));
    assert_eq!(
        database
            .promote_boot_publication_receipt(
                &receipt,
                Instant::now() + Duration::from_secs(30),
            )
            .unwrap(),
        BootPublicationReceiptPromotionOutcome::Promoted,
    );
    let pair = crate::boot_publication::BootPublicationReceiptPair {
        committed: receipt.body().committed_predecessor(),
        pending: receipt.fingerprint(),
    };
    let promoted = database
        .load_exact_promoted_boot_publication_receipt_chain(
            receipt.body().transition_id(),
            &pair,
        )
        .unwrap();
    assert_eq!(
        AuthenticatedActiveReblitInstalledBootPublication::from_exact_promoted_chain(&promoted)
            .receipt()
            .unwrap()
            .fingerprint(),
        receipt.fingerprint(),
    );
}

#[test]
fn authenticated_claim_mapping_keeps_first_adoption_borrowed() {
    assert!(installed_claim_is_owned(
        BootPublicationOutputProvenanceClaim::UnclaimedAbsent
    ));
    assert!(installed_claim_is_owned(
        BootPublicationOutputProvenanceClaim::ClaimedPublishedByCast
    ));
    assert!(!installed_claim_is_owned(
        BootPublicationOutputProvenanceClaim::BorrowedFirstAdoption
    ));
}

fn desired_entry(
    output: &DesiredActiveReblitBootPublication,
    action: ActiveReblitBootPublicationDeltaAction,
) -> ClassifiedActiveReblitBootPublicationDeltaEntry {
    ClassifiedActiveReblitBootPublicationDeltaEntry {
        root: output.root(),
        relative_path: output.relative_path().to_str().unwrap().into(),
        desired_expected: Some(desired_expected(output)),
        installed_expected: None,
        action,
    }
}

fn publish_entries(
    inventory: &PreparedActiveReblitDesiredPublicationInventory,
) -> Vec<ClassifiedActiveReblitBootPublicationDeltaEntry> {
    inventory
        .outputs()
        .iter()
        .map(|output| desired_entry(output, ActiveReblitBootPublicationDeltaAction::PublishDesired))
        .collect()
}

#[test]
fn receipt_claim_bridge_uses_inventory_order_and_ignores_stale_entries() {
    with_bound_alias_plan!(|plan| {
        let inventory = plan.prepare_desired_publication_inventory().unwrap();
        assert!(inventory.outputs().len() >= 4);
        let mut entries: Vec<_> = inventory
            .outputs()
            .iter()
            .enumerate()
            .map(|(index, output)| {
                let action = match index % 4 {
                    0 => ActiveReblitBootPublicationDeltaAction::PublishDesired,
                    1 => ActiveReblitBootPublicationDeltaAction::RetainOwnedDesired,
                    2 => ActiveReblitBootPublicationDeltaAction::ReplaceOwnedDesired,
                    _ => ActiveReblitBootPublicationDeltaAction::PreserveBorrowedDesired,
                };
                desired_entry(output, action)
            })
            .collect();
        entries.reverse();
        entries.push(ClassifiedActiveReblitBootPublicationDeltaEntry {
            root: ActiveReblitBootDestinationRoot::Boot,
            relative_path: "EFI/cast/stale-owned".into(),
            desired_expected: None,
            installed_expected: Some(expected(90)),
            action: ActiveReblitBootPublicationDeltaAction::DeleteOwnedStaleAfterPromotion,
        });
        entries.push(ClassifiedActiveReblitBootPublicationDeltaEntry {
            root: ActiveReblitBootDestinationRoot::Esp,
            relative_path: "EFI/cast/stale-borrowed".into(),
            desired_expected: None,
            installed_expected: Some(expected(91)),
            action: ActiveReblitBootPublicationDeltaAction::PreserveUnownedStale,
        });
        let delta = ClassifiedActiveReblitBootPublicationDelta { entries };

        let claims = delta.derive_receipt_provenance_claims(&inventory).unwrap();
        let expected: Vec<_> = inventory
            .outputs()
            .iter()
            .enumerate()
            .map(|(index, output)| {
                let claim = match index % 4 {
                    0 => BootPublicationOutputProvenanceClaim::UnclaimedAbsent,
                    1 | 2 => BootPublicationOutputProvenanceClaim::ClaimedPublishedByCast,
                    _ => BootPublicationOutputProvenanceClaim::BorrowedFirstAdoption,
                };
                BorrowedActiveReblitBootPublicationProvenanceClaim::new(
                    output.root(),
                    output.relative_path(),
                    BootPublicationSha256::from_bytes(*output.content_identity().as_bytes()),
                    claim,
                )
            })
            .collect();
        assert_eq!(claims, expected);
        assert_eq!(claims.len(), inventory.outputs().len());
    });
}

#[test]
fn receipt_claim_bridge_rejects_missing_duplicate_and_stale_desired_keys() {
    with_bound_alias_plan!(|plan| {
        let inventory = plan.prepare_desired_publication_inventory().unwrap();

        let mut missing = publish_entries(&inventory);
        missing.remove(0);
        assert!(matches!(
            (ClassifiedActiveReblitBootPublicationDelta { entries: missing })
                .derive_receipt_provenance_claims(&inventory),
            Err(ActiveReblitBootPublicationDeltaError::MissingDesiredClassifiedKey {
                desired_index: 0
            })
        ));

        let mut duplicate = publish_entries(&inventory);
        let duplicate_index = duplicate.len();
        duplicate.push(desired_entry(
            &inventory.outputs()[0],
            ActiveReblitBootPublicationDeltaAction::RetainOwnedDesired,
        ));
        assert!(matches!(
            (ClassifiedActiveReblitBootPublicationDelta { entries: duplicate })
                .derive_receipt_provenance_claims(&inventory),
            Err(ActiveReblitBootPublicationDeltaError::DuplicateClassifiedKey { index })
                if index == duplicate_index
        ));

        let mut stale_desired = publish_entries(&inventory);
        stale_desired[0].action =
            ActiveReblitBootPublicationDeltaAction::DeleteOwnedStaleAfterPromotion;
        assert!(matches!(
            (ClassifiedActiveReblitBootPublicationDelta {
                entries: stale_desired,
            })
            .derive_receipt_provenance_claims(&inventory),
            Err(ActiveReblitBootPublicationDeltaError::StaleActionForDesiredKey {
                desired_index: 0,
                delta_index: 0,
            })
        ));

        let mut mismatched_expected = publish_entries(&inventory);
        mismatched_expected[0].desired_expected = Some(expected(98));
        assert!(matches!(
            (ClassifiedActiveReblitBootPublicationDelta {
                entries: mismatched_expected,
            })
            .derive_receipt_provenance_claims(&inventory),
            Err(
                ActiveReblitBootPublicationDeltaError::DesiredClassifiedExpectationMismatch {
                    desired_index: 0,
                    delta_index: 0,
                }
            )
        ));
    });
}

#[test]
fn receipt_claim_bridge_rejects_a_desired_action_with_no_desired_key() {
    with_bound_alias_plan!(|plan| {
        let inventory = plan.prepare_desired_publication_inventory().unwrap();
        let mut entries = publish_entries(&inventory);
        let unmatched_index = entries.len();
        entries.push(ClassifiedActiveReblitBootPublicationDeltaEntry {
            root: ActiveReblitBootDestinationRoot::Boot,
            relative_path: "EFI/cast/not-in-desired-inventory".into(),
            desired_expected: Some(expected(99)),
            installed_expected: None,
            action: ActiveReblitBootPublicationDeltaAction::PublishDesired,
        });
        assert!(matches!(
            (ClassifiedActiveReblitBootPublicationDelta { entries })
                .derive_receipt_provenance_claims(&inventory),
            Err(ActiveReblitBootPublicationDeltaError::UnmatchedDesiredClassifiedKey {
                delta_index
            }) if delta_index == unmatched_index
        ));
    });
}
