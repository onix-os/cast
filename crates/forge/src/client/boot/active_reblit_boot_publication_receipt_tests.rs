use std::time::{Duration, Instant};

use super::*;
use crate::{
    Installation,
    boot_publication::{
        BootPublicationOutputProvenanceClaim, BootPublicationReceiptFingerprint, BootPublicationSha256,
        decode_boot_publication_receipt,
    },
    client::{
        active_reblit_bls_renderer::RenderedActiveReblitBlsRequests,
        active_reblit_boot_inputs::PreparedActiveReblitStoneBootInputs,
        active_reblit_boot_render_inputs::PreparedActiveReblitBootRenderInputs,
        active_reblit_mounted_boot_topology::AliasFixture,
    },
    db, state,
    state::TransitionId,
    transition_journal::{
        BootId, CodecError, MountNamespaceIdentity, Operation, Phase, Previous, PreviousOrigin, QuarantineName,
        RuntimeEpoch, RuntimeTreeIdentity, TransitionRecord, TreeToken,
    },
};

#[path = "active_reblit_boot_render_inputs_tests/support.rs"]
mod support;

#[path = "active_reblit_boot_publication_receipt_tests/contracts.rs"]
mod contracts;
#[path = "active_reblit_boot_publication_receipt_tests/integration.rs"]
mod integration;
#[path = "active_reblit_boot_publication_receipt_tests/topology.rs"]
mod topology;

fn receipt_fingerprint(byte: u8) -> BootPublicationReceiptFingerprint {
    BootPublicationReceiptFingerprint::from_bytes([byte; 32])
}

fn preparing_record() -> TransitionRecord {
    TransitionRecord::preparing(
        TransitionId::parse("0123456789abcdef0123456789abcdef").unwrap(),
        RuntimeEpoch {
            boot_id: BootId::parse("01234567-89ab-4cde-8f01-23456789abcd").unwrap(),
            mount_namespace: MountNamespaceIdentity { st_dev: 30, inode: 31 },
        },
        Operation::ActiveReblit,
        Some(42),
        TreeToken::parse("a".repeat(TreeToken::TEXT_LENGTH)).unwrap(),
        RuntimeTreeIdentity {
            st_dev: 10,
            inode: 11,
            mount_id: 12,
        },
        Previous {
            id: Some(42),
            tree_token: TreeToken::parse("b".repeat(TreeToken::TEXT_LENGTH)).unwrap(),
            usr_runtime_identity: RuntimeTreeIdentity {
                st_dev: 10,
                inode: 13,
                mount_id: 12,
            },
            origin: PreviousOrigin::ActiveReblitCorrupt,
        },
        true,
        true,
        QuarantineName::parse("failed-0123456789abcdef").unwrap(),
    )
    .unwrap()
}

fn exact_boot_sync_predecessor() -> TransitionRecord {
    let mut record = preparing_record();
    loop {
        match record.forward_successor(None) {
            Ok(next) => record = next,
            Err(CodecError::ExplicitBootSyncStartedSuccessorRequired) => {
                assert_eq!(record.phase, Phase::SystemTriggersComplete);
                return record;
            }
            Err(error) => panic!("unexpected predecessor construction error: {error}"),
        }
    }
}

fn claim_bindings<'inventory>(
    inventory: &'inventory PreparedActiveReblitDesiredPublicationInventory,
    mut claim_for_index: impl FnMut(usize) -> BootPublicationOutputProvenanceClaim,
) -> Vec<BorrowedActiveReblitBootPublicationProvenanceClaim<'inventory>> {
    inventory
        .outputs()
        .iter()
        .enumerate()
        .map(|(index, output)| {
            BorrowedActiveReblitBootPublicationProvenanceClaim::new(
                output.root(),
                output.relative_path(),
                BootPublicationSha256::from_bytes(*output.content_identity().as_bytes()),
                claim_for_index(index),
            )
        })
        .collect()
}

fn inert_claim_bindings(
    inventory: &PreparedActiveReblitDesiredPublicationInventory,
) -> Vec<BorrowedActiveReblitBootPublicationProvenanceClaim<'_>> {
    claim_bindings(inventory, |index| match index % 3 {
        0 => BootPublicationOutputProvenanceClaim::BorrowedFirstAdoption,
        1 => BootPublicationOutputProvenanceClaim::UnclaimedAbsent,
        _ => BootPublicationOutputProvenanceClaim::ClaimedPublishedByCast,
    })
}

macro_rules! with_bound_alias_plan {
    (|$fixture:ident, $plan:ident| $body:block) => {{
        let deadline = support::future_deadline();
        let $fixture = support::simple_fixture();
        let stone = $fixture.stone();
        let roots = $fixture.roots(&stone);
        let prepared = support::prepare_static(&$fixture, &stone, &roots);
        let local_policy = $fixture.local_policy();
        let root_intent = $fixture.root_intent();
        let inputs = prepared
            .revalidate_until(
                &$fixture.state_db,
                &$fixture.layout_db,
                &$fixture.installation,
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

pub(super) use with_bound_alias_plan;
