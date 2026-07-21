//! Authority-free complete receipt mapping for one ActiveReblit boot plan.
//!
//! This module closes already-authenticated scalar inputs into canonical owned
//! receipt data. It does not open or retain a descriptor, inspect a live
//! namespace, stage a database row, publish an output, or grant replacement or
//! deletion authority. Per-output provenance claims are caller-supplied inert
//! assertions; a later coordinator must derive and authenticate them.

use std::{
    collections::TryReserveError,
    os::unix::ffi::OsStrExt as _,
    path::Path,
    time::Instant,
};

use sha2::{Digest as _, Sha256};

use super::{
    active_reblit_bls_renderer::BoundActiveReblitBlsPublicationPlan,
    active_reblit_desired_publication::{
        DesiredActiveReblitBootPublication, PreparedActiveReblitDesiredPublicationInventory,
    },
    active_reblit_mounted_boot_topology::{
        BoundActiveReblitMountedBootTarget, BoundActiveReblitMountedBootTopology,
    },
    active_reblit_publication_plan::{
        ActiveReblitBootDestinationLayout, ActiveReblitBootDestinationRoot, ActiveReblitBootPublicationPhase,
        ActiveReblitBootPublicationRole,
    },
};
use crate::{
    boot_publication::{
        BootPublicationDestination, BootPublicationDestinations, BootPublicationHistoricalRuntimeWitness,
        BootPublicationOutput, BootPublicationOutputProvenanceClaim, BootPublicationOutputRole,
        BootPublicationPublicationPhase, BootPublicationReceiptBody, BootPublicationReceiptFingerprint,
        BootPublicationReceiptPair, BootPublicationRoot, BootPublicationSha256, BootPublicationXxh3,
        CanonicalBootPublicationReceipt, prepare_boot_publication_receipt,
    },
    transition_journal::{TransitionRecord, encode as encode_transition_record},
};

#[path = "active_reblit_boot_publication_receipt/error.rs"]
mod error;

pub(in crate::client) use error::ActiveReblitBootPublicationReceiptError;

const PREDECESSOR_VALIDATION_FINGERPRINT: BootPublicationReceiptFingerprint =
    BootPublicationReceiptFingerprint::from_bytes([0_u8; 32]);

/// Borrowed canonical-output key carrying one inert provenance assertion.
///
/// This value binds claim data to an exact desired output without retaining a
/// descriptor or granting publication, replacement, removal, or deletion
/// authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) struct BorrowedActiveReblitBootPublicationProvenanceClaim<'inventory> {
    destination_root: ActiveReblitBootDestinationRoot,
    relative_path: &'inventory Path,
    content_sha256: BootPublicationSha256,
    claim: BootPublicationOutputProvenanceClaim,
}

impl<'inventory> BorrowedActiveReblitBootPublicationProvenanceClaim<'inventory> {
    pub(in crate::client) const fn new(
        destination_root: ActiveReblitBootDestinationRoot,
        relative_path: &'inventory Path,
        content_sha256: BootPublicationSha256,
        claim: BootPublicationOutputProvenanceClaim,
    ) -> Self {
        Self {
            destination_root,
            relative_path,
            content_sha256,
            claim,
        }
    }

    pub(in crate::client) const fn claim(self) -> BootPublicationOutputProvenanceClaim {
        self.claim
    }

    fn matches(self, desired: &DesiredActiveReblitBootPublication) -> bool {
        self.destination_root == desired.root()
            && self.relative_path == desired.relative_path()
            && self.content_sha256.as_bytes() == desired.content_identity().as_bytes()
    }
}

impl<'input, 'topology_view, 'topology_authority, 'attempt, 'stone, 'roots>
    BoundActiveReblitBlsPublicationPlan<
        'input,
        'topology_view,
        'topology_authority,
        'attempt,
        'stone,
        'roots,
    >
{
    /// Map this exact plan and its owned desired inventory into inert canonical
    /// receipt data at the retained attempt deadline.
    ///
    /// `provenance_claims` must match the canonical desired inventory
    /// one-for-one by destination root, exact relative path, and content
    /// SHA-256. Their presence records data only: even a
    /// `ClaimedPublishedByCast` value grants no effect authority. Likewise,
    /// `committed_predecessor` is copied into the body but is not authenticated
    /// against a database head here; atomic durable staging must do that.
    pub(in crate::client) fn prepare_complete_boot_publication_receipt(
        &self,
        inventory: &PreparedActiveReblitDesiredPublicationInventory,
        predecessor: &TransitionRecord,
        committed_predecessor: Option<BootPublicationReceiptFingerprint>,
        provenance_claims: &[BorrowedActiveReblitBootPublicationProvenanceClaim<'_>],
    ) -> Result<CanonicalBootPublicationReceipt, ActiveReblitBootPublicationReceiptError> {
        let mut now = Instant::now;
        prepare_bound_receipt_with_clock(
            self,
            inventory,
            predecessor,
            committed_predecessor,
            provenance_claims,
            self.input_deadline(),
            &mut now,
        )
    }
}

#[allow(clippy::too_many_arguments)]
fn prepare_bound_receipt_with_clock<
    'input,
    'topology_view,
    'topology_authority,
    'attempt,
    'stone,
    'roots,
    Clock,
>(
    plan: &BoundActiveReblitBlsPublicationPlan<
        'input,
        'topology_view,
        'topology_authority,
        'attempt,
        'stone,
        'roots,
    >,
    inventory: &PreparedActiveReblitDesiredPublicationInventory,
    predecessor: &TransitionRecord,
    committed_predecessor: Option<BootPublicationReceiptFingerprint>,
    provenance_claims: &[BorrowedActiveReblitBootPublicationProvenanceClaim<'_>],
    deadline: Instant,
    now: &mut Clock,
) -> Result<CanonicalBootPublicationReceipt, ActiveReblitBootPublicationReceiptError>
where
    Clock: FnMut() -> Instant,
{
    require_matching_deadline(plan.input_deadline(), deadline)?;
    require_deadline(deadline, "receipt mapping entry", now)?;
    require_exact_predecessor(predecessor, committed_predecessor)?;
    require_deadline(deadline, "predecessor validation", now)?;
    require_matching_inventory(plan, inventory, deadline, now)?;
    if provenance_claims.len() != inventory.outputs().len() {
        return Err(
            ActiveReblitBootPublicationReceiptError::ProvenanceClaimCountMismatch {
                expected: inventory.outputs().len(),
                actual: provenance_claims.len(),
            },
        );
    }

    let canonical_predecessor = encode_transition_record(predecessor)
        .map_err(ActiveReblitBootPublicationReceiptError::PredecessorEncoding)?;
    let predecessor_journal_sha256 = BootPublicationSha256::from_bytes(
        Sha256::digest(&canonical_predecessor).into(),
    );
    require_deadline(deadline, "predecessor canonical hash", now)?;

    let destinations = map_destinations(
        plan.destination_layout(),
        plan.mounted_topology(),
        deadline,
        now,
    )?;
    let outputs = map_outputs(inventory, provenance_claims, deadline, now)?;
    let desired_inventory_sha256 =
        BootPublicationSha256::from_bytes(*inventory.fingerprint().as_bytes());
    let body = BootPublicationReceiptBody::new(
        predecessor.transition_id.clone(),
        committed_predecessor,
        predecessor_journal_sha256,
        desired_inventory_sha256,
        destinations,
        outputs,
    )?;
    require_deadline(deadline, "receipt body validation", now)?;
    let receipt = prepare_boot_publication_receipt(body)?;
    require_deadline(deadline, "terminal canonical receipt", now)?;
    Ok(receipt)
}

fn require_exact_predecessor(
    predecessor: &TransitionRecord,
    committed_predecessor: Option<BootPublicationReceiptFingerprint>,
) -> Result<(), ActiveReblitBootPublicationReceiptError> {
    predecessor
        .boot_sync_started_successor(BootPublicationReceiptPair {
            committed: committed_predecessor,
            pending: PREDECESSOR_VALIDATION_FINGERPRINT,
        })
        .map(|_| ())
        .map_err(ActiveReblitBootPublicationReceiptError::InvalidPredecessor)
}

fn require_matching_inventory<
    'input,
    'topology_view,
    'topology_authority,
    'attempt,
    'stone,
    'roots,
    Clock,
>(
    plan: &BoundActiveReblitBlsPublicationPlan<
        'input,
        'topology_view,
        'topology_authority,
        'attempt,
        'stone,
        'roots,
    >,
    inventory: &PreparedActiveReblitDesiredPublicationInventory,
    deadline: Instant,
    now: &mut Clock,
) -> Result<(), ActiveReblitBootPublicationReceiptError>
where
    Clock: FnMut() -> Instant,
{
    require_deadline(deadline, "desired inventory comparison entry", now)?;
    if !plan.collision_domains_still_match() {
        return Err(ActiveReblitBootPublicationReceiptError::CollisionDomainDrift);
    }
    if inventory.destination_layout() != plan.destination_layout() {
        return Err(ActiveReblitBootPublicationReceiptError::DesiredInventoryMismatch {
            field: "destination layout",
        });
    }
    if inventory.outputs().len() != plan.publication_count() {
        return Err(ActiveReblitBootPublicationReceiptError::DesiredInventoryMismatch {
            field: "output count",
        });
    }
    if inventory.path_bytes() != plan.publication_path_bytes() {
        return Err(ActiveReblitBootPublicationReceiptError::DesiredInventoryMismatch {
            field: "path bytes",
        });
    }
    if inventory.logical_bytes() != plan.logical_bytes() {
        return Err(ActiveReblitBootPublicationReceiptError::DesiredInventoryMismatch {
            field: "logical bytes",
        });
    }
    for (bound, desired) in plan.outputs().zip(inventory.outputs()) {
        require_deadline(deadline, "desired inventory output comparison", now)?;
        if !desired_matches_bound(desired, &bound) {
            return Err(ActiveReblitBootPublicationReceiptError::DesiredInventoryMismatch {
                field: "canonical output",
            });
        }
    }
    require_deadline(deadline, "terminal desired inventory comparison", now)?;
    Ok(())
}

fn desired_matches_bound(
    desired: &DesiredActiveReblitBootPublication,
    bound: &super::active_reblit_bls_renderer::BoundActiveReblitBlsPublication<'_, '_>,
) -> bool {
    desired.root() == bound.root()
        && desired.phase() == bound.phase()
        && desired.role() == bound.role()
        && desired.relative_path() == bound.relative_path()
        && desired.mode() == bound.mode()
        && desired.checksum() == bound.expected_digest()
        && desired.length() == bound.expected_length()
        && desired.content_identity() == bound.expected_content_identity()
}

fn map_destinations<Clock>(
    layout: ActiveReblitBootDestinationLayout,
    topology: BoundActiveReblitMountedBootTopology<'_>,
    deadline: Instant,
    now: &mut Clock,
) -> Result<BootPublicationDestinations, ActiveReblitBootPublicationReceiptError>
where
    Clock: FnMut() -> Instant,
{
    require_deadline(deadline, "destination mapping entry", now)?;
    let destinations = match (layout, topology) {
        (
            ActiveReblitBootDestinationLayout::BootAliasesEsp,
            BoundActiveReblitMountedBootTopology::BootAliasesEsp { esp },
        ) => BootPublicationDestinations::boot_aliases_esp(map_destination(
            "esp", esp, deadline, now,
        )?),
        (
            ActiveReblitBootDestinationLayout::DistinctXbootldr,
            BoundActiveReblitMountedBootTopology::DistinctXbootldr { esp, xbootldr },
        ) => BootPublicationDestinations::distinct_xbootldr(
            map_destination("esp", esp, deadline, now)?,
            map_destination("xbootldr", xbootldr, deadline, now)?,
        ),
        _ => return Err(ActiveReblitBootPublicationReceiptError::TopologyLayoutMismatch),
    };
    require_deadline(deadline, "terminal destination mapping", now)?;
    Ok(destinations)
}

fn map_destination<Clock>(
    role: &'static str,
    target: BoundActiveReblitMountedBootTarget<'_>,
    deadline: Instant,
    now: &mut Clock,
) -> Result<BootPublicationDestination, ActiveReblitBootPublicationReceiptError>
where
    Clock: FnMut() -> Instant,
{
    if target.partuuid != target.partition_uuid.as_str() {
        return Err(
            ActiveReblitBootPublicationReceiptError::TopologyPartuuidMismatch { destination: role },
        );
    }
    if target.destination.raw_device() != target.boot_filesystem.destination_device()
        || target.destination.inode() != target.boot_filesystem.destination_inode()
    {
        return Err(
            ActiveReblitBootPublicationReceiptError::TopologyFilesystemWitnessMismatch {
                destination: role,
            },
        );
    }
    let partuuid = clone_text(target.partuuid, "destination PARTUUID", deadline, now)?;
    let witness = BootPublicationHistoricalRuntimeWitness::new(
        target.destination.raw_device(),
        target.destination.inode(),
        target.mount_id,
        target.device_major(),
        target.device_minor(),
        target.disk_sequence.map(|sequence| sequence.get()),
    );
    Ok(BootPublicationDestination::new(
        partuuid,
        target.partition_number.get(),
        witness,
    ))
}

fn map_outputs<Clock>(
    inventory: &PreparedActiveReblitDesiredPublicationInventory,
    provenance_claims: &[BorrowedActiveReblitBootPublicationProvenanceClaim<'_>],
    deadline: Instant,
    now: &mut Clock,
) -> Result<Vec<BootPublicationOutput>, ActiveReblitBootPublicationReceiptError>
where
    Clock: FnMut() -> Instant,
{
    require_deadline(deadline, "output mapping pre-allocation", now)?;
    let mut outputs = Vec::new();
    outputs
        .try_reserve_exact(inventory.outputs().len())
        .map_err(|source| ActiveReblitBootPublicationReceiptError::Allocation {
            resource: "receipt output inventory",
            source,
        })?;
    require_deadline(deadline, "output mapping post-allocation", now)?;
    for (index, (desired, provenance_claim)) in inventory
        .outputs()
        .iter()
        .zip(provenance_claims.iter().copied())
        .enumerate()
    {
        require_deadline(deadline, "receipt output mapping", now)?;
        if !provenance_claim.matches(desired) {
            return Err(
                ActiveReblitBootPublicationReceiptError::ProvenanceClaimBindingMismatch { index },
            );
        }
        let path = desired.relative_path().as_os_str().as_bytes();
        let path = std::str::from_utf8(path)
            .map_err(|_| ActiveReblitBootPublicationReceiptError::OutputPathNotUtf8 { index })?;
        let path = clone_text(path, "receipt output path", deadline, now)?;
        outputs.push(BootPublicationOutput::new(
            map_root(desired.root()),
            map_phase(desired.phase()),
            map_role(desired.role()),
            path,
            desired.mode(),
            BootPublicationXxh3::from_u128(desired.checksum()),
            desired.length(),
            BootPublicationSha256::from_bytes(*desired.content_identity().as_bytes()),
            provenance_claim.claim(),
        ));
    }
    Ok(outputs)
}

fn clone_text<Clock>(
    text: &str,
    resource: &'static str,
    deadline: Instant,
    now: &mut Clock,
) -> Result<Box<str>, ActiveReblitBootPublicationReceiptError>
where
    Clock: FnMut() -> Instant,
{
    require_deadline(deadline, "text copy pre-allocation", now)?;
    let mut owned = String::new();
    owned
        .try_reserve_exact(text.len())
        .map_err(|source: TryReserveError| ActiveReblitBootPublicationReceiptError::Allocation {
            resource,
            source,
        })?;
    owned.push_str(text);
    require_deadline(deadline, "text copy post-allocation", now)?;
    Ok(owned.into_boxed_str())
}

const fn map_root(root: ActiveReblitBootDestinationRoot) -> BootPublicationRoot {
    match root {
        ActiveReblitBootDestinationRoot::Esp => BootPublicationRoot::Esp,
        ActiveReblitBootDestinationRoot::Boot => BootPublicationRoot::Boot,
    }
}

const fn map_phase(phase: ActiveReblitBootPublicationPhase) -> BootPublicationPublicationPhase {
    match phase {
        ActiveReblitBootPublicationPhase::Payload => BootPublicationPublicationPhase::Payload,
        ActiveReblitBootPublicationPhase::Entry => BootPublicationPublicationPhase::Entry,
        ActiveReblitBootPublicationPhase::LoaderControl => BootPublicationPublicationPhase::LoaderControl,
        ActiveReblitBootPublicationPhase::Bootloader => BootPublicationPublicationPhase::Bootloader,
    }
}

const fn map_role(role: ActiveReblitBootPublicationRole) -> BootPublicationOutputRole {
    match role {
        ActiveReblitBootPublicationRole::Payload => BootPublicationOutputRole::Payload,
        ActiveReblitBootPublicationRole::Entry => BootPublicationOutputRole::Entry,
        ActiveReblitBootPublicationRole::LoaderControl => BootPublicationOutputRole::LoaderControl,
        ActiveReblitBootPublicationRole::FallbackBootloader => BootPublicationOutputRole::FallbackBootloader,
        ActiveReblitBootPublicationRole::SystemdBootloader => BootPublicationOutputRole::SystemdBootloader,
    }
}

fn require_matching_deadline(
    expected: Instant,
    actual: Instant,
) -> Result<(), ActiveReblitBootPublicationReceiptError> {
    if expected == actual {
        Ok(())
    } else {
        Err(ActiveReblitBootPublicationReceiptError::DeadlineMismatch { expected, actual })
    }
}

fn require_deadline<Clock>(
    deadline: Instant,
    checkpoint: &'static str,
    now: &mut Clock,
) -> Result<(), ActiveReblitBootPublicationReceiptError>
where
    Clock: FnMut() -> Instant,
{
    if now() > deadline {
        Err(ActiveReblitBootPublicationReceiptError::DeadlineExceeded { checkpoint })
    } else {
        Ok(())
    }
}

#[cfg(test)]
#[path = "active_reblit_boot_publication_receipt_tests.rs"]
mod tests;
