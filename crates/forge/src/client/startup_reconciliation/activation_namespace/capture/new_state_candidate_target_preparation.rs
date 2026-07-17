//! Evidence and sealed one-shot preparation for NewState target prefixes.
//!
//! An absent target and an owned restrictive-mode residue are different crash
//! prefixes. Their projections deliberately mask only metadata that one child
//! creation or future descriptor-bound mode normalization may change. Read-only
//! evidence stays opaque outside activation-namespace proof code; the create
//! submodule alone consumes the retained parent through a sealed attempt.

mod create;

use crate::transition_journal::{Operation, Phase, RuntimeEpoch, TransitionRecord};

use super::{
    InodeWitness, NamespaceFingerprint, NamespaceSnapshot, NewStateCandidatePreserveCaptureError, RootAbiFingerprint,
    TreeLocation, UsrFingerprint, WrapperFingerprint,
};

pub(in crate::client::startup_reconciliation::activation_namespace) use create::NewStateTargetCreateReconciliation;
#[cfg(test)]
pub(in crate::client) use create::{
    NewStateTargetCreateFault, arm_before_new_state_target_create_attempt,
    arm_before_new_state_target_create_reconciliation_capture, arm_new_state_target_create_fault,
    new_state_target_create_attempt_count, reset_new_state_target_create_attempt_count,
};

/// Stable quarantine-parent identity across a future child creation.
///
/// Directory link count, length, mtime, and ctime are omitted because creating
/// one child may change them.  Type, permissions, ownership, and inode identity
/// remain exact.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MutableQuarantineIdentity {
    device: u64,
    inode: u64,
    mode: u32,
    owner: u32,
    group: u32,
}

impl From<InodeWitness> for MutableQuarantineIdentity {
    fn from(witness: InodeWitness) -> Self {
        Self {
            device: witness.device,
            inode: witness.inode,
            mode: witness.mode,
            owner: witness.owner,
            group: witness.group,
        }
    }
}

/// Stable residue identity across future descriptor-bound mode normalization.
///
/// Permission bits and ctime are omitted.  The directory kind, inode,
/// ownership, links, length, and content-sensitive mtime remain exact.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct StableResidueIdentity {
    device: u64,
    inode: u64,
    kind: u32,
    owner: u32,
    group: u32,
    links: u64,
    length: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
}

impl From<InodeWitness> for StableResidueIdentity {
    fn from(witness: InodeWitness) -> Self {
        Self {
            device: witness.device,
            inode: witness.inode,
            kind: witness.kind(),
            owner: witness.owner,
            group: witness.group,
            links: witness.links,
            length: witness.length,
            modified_seconds: witness.modified_seconds,
            modified_nanoseconds: witness.modified_nanoseconds,
        }
    }
}

/// Namespace state that neither target-preparation operation may change.
#[derive(Clone, Debug, Eq, PartialEq)]
struct CommonNonTargetInvariant {
    root: InodeWitness,
    roots: InodeWitness,
    epoch: RuntimeEpoch,
    live: UsrFingerprint,
    root_abi: RootAbiFingerprint,
    isolation_abi: RootAbiFingerprint,
    root_wrappers: Vec<WrapperFingerprint>,
    other_quarantine_wrappers: Vec<WrapperFingerprint>,
}

impl CommonNonTargetInvariant {
    fn capture(fingerprint: &NamespaceFingerprint) -> Self {
        Self {
            root: fingerprint.root,
            roots: fingerprint.roots,
            epoch: fingerprint.epoch.clone(),
            live: fingerprint.live.clone(),
            root_abi: fingerprint.root_abi.clone(),
            isolation_abi: fingerprint.isolation_abi.clone(),
            root_wrappers: fingerprint.roots_entries.clone(),
            other_quarantine_wrappers: fingerprint
                .quarantine_entries
                .iter()
                .filter(|wrapper| wrapper.role != TreeLocation::TransitionQuarantine)
                .cloned()
                .collect(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client::startup_reconciliation::activation_namespace) enum NewStateTargetCreateLayout {
    Absent,
    RestrictiveResidue,
    EmptyPrivate,
}

/// Projection spanning exactly the three safe target-creation outcomes.
///
/// A newly allocated target has no PRE inode identity to compare. The exact
/// journal name and every non-target invariant remain fixed, while the target
/// itself must classify as absent, an owned restrictive residue, or an empty
/// private wrapper.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::client::startup_reconciliation::activation_namespace) struct ProjectedNewStateTargetCreateNamespace {
    layout: NewStateTargetCreateLayout,
    target_name: Vec<u8>,
    quarantine: MutableQuarantineIdentity,
    invariant: CommonNonTargetInvariant,
}

impl ProjectedNewStateTargetCreateNamespace {
    pub(in crate::client::startup_reconciliation::activation_namespace) fn capture(
        snapshot: &NamespaceSnapshot,
        record: &TransitionRecord,
    ) -> Result<Self, NewStateCandidatePreserveCaptureError> {
        require_new_state_candidate_preserve(record)?;
        let fingerprint = snapshot.fingerprint();
        let targets = fingerprint
            .quarantine_entries
            .iter()
            .filter(|wrapper| wrapper.role == TreeLocation::TransitionQuarantine)
            .collect::<Vec<_>>();
        let expected_name = record.quarantine_name.as_str().as_bytes();
        let layout = match (fingerprint.new_state_target_residue.as_ref(), targets.as_slice()) {
            (None, []) => NewStateTargetCreateLayout::Absent,
            (Some(residue), []) if residue.name == expected_name => NewStateTargetCreateLayout::RestrictiveResidue,
            (None, [target])
                if target.name == expected_name
                    && target.has_exact_private_permissions()
                    && target.entries.is_empty()
                    && target.usr.is_none()
                    && target.slot.is_none() =>
            {
                NewStateTargetCreateLayout::EmptyPrivate
            }
            _ => return Err(NewStateCandidatePreserveCaptureError::NotTargetCreateLayout),
        };
        Ok(Self {
            layout,
            target_name: expected_name.to_vec(),
            quarantine: fingerprint.quarantine.into(),
            invariant: CommonNonTargetInvariant::capture(fingerprint),
        })
    }

    pub(in crate::client::startup_reconciliation::activation_namespace) fn layout(&self) -> NewStateTargetCreateLayout {
        self.layout
    }

    pub(in crate::client::startup_reconciliation::activation_namespace) fn require_absent_to_prepared(
        &self,
        after: &Self,
    ) -> Result<(), NewStateCandidatePreserveCaptureError> {
        if self.layout != NewStateTargetCreateLayout::Absent
            || !matches!(
                after.layout,
                NewStateTargetCreateLayout::RestrictiveResidue | NewStateTargetCreateLayout::EmptyPrivate
            )
        {
            return Err(NewStateCandidatePreserveCaptureError::NotAbsentToPreparedTarget {
                before: self.layout,
                after: after.layout,
            });
        }
        if self.target_name != after.target_name
            || self.quarantine != after.quarantine
            || self.invariant != after.invariant
        {
            return Err(NewStateCandidatePreserveCaptureError::InvariantChanged);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ResidueTargetProjection {
    target_name: Vec<u8>,
    quarantine: InodeWitness,
    residue: StableResidueIdentity,
    invariant: CommonNonTargetInvariant,
}

impl ResidueTargetProjection {
    fn capture(
        snapshot: &NamespaceSnapshot,
        record: &TransitionRecord,
    ) -> Result<Self, NewStateCandidatePreserveCaptureError> {
        require_new_state_candidate_preserve(record)?;
        let fingerprint = snapshot.fingerprint();
        if fingerprint
            .quarantine_entries
            .iter()
            .any(|wrapper| wrapper.role == TreeLocation::TransitionQuarantine)
        {
            return Err(NewStateCandidatePreserveCaptureError::NotResidueTargetPreparationLayout);
        }
        let residue = fingerprint
            .new_state_target_residue
            .as_ref()
            .ok_or(NewStateCandidatePreserveCaptureError::NotResidueTargetPreparationLayout)?;
        if residue.name != record.quarantine_name.as_str().as_bytes() {
            return Err(NewStateCandidatePreserveCaptureError::WrongTargetName);
        }
        Ok(Self {
            target_name: residue.name.clone(),
            quarantine: fingerprint.quarantine,
            residue: residue.witness.into(),
            invariant: CommonNonTargetInvariant::capture(fingerprint),
        })
    }
}

fn require_new_state_candidate_preserve(
    record: &TransitionRecord,
) -> Result<(), NewStateCandidatePreserveCaptureError> {
    if record.operation != Operation::NewState {
        return Err(NewStateCandidatePreserveCaptureError::WrongOperation);
    }
    if record.phase != Phase::CandidatePreserveIntent {
        return Err(NewStateCandidatePreserveCaptureError::WrongPhase);
    }
    Ok(())
}

/// Opaque exact absent-target PRE evidence for sealed one-shot creation.
pub(in crate::client::startup_reconciliation) struct UsrRollbackNewStateTargetCreateNamespaceEvidence {
    pub(in crate::client::startup_reconciliation::activation_namespace) baseline: NamespaceSnapshot,
    pub(in crate::client::startup_reconciliation::activation_namespace) projection:
        ProjectedNewStateTargetCreateNamespace,
}

impl UsrRollbackNewStateTargetCreateNamespaceEvidence {
    pub(in crate::client::startup_reconciliation::activation_namespace) fn capture(
        snapshot: NamespaceSnapshot,
        record: &TransitionRecord,
    ) -> Result<Self, NewStateCandidatePreserveCaptureError> {
        let projection = ProjectedNewStateTargetCreateNamespace::capture(&snapshot, record)?;
        if projection.layout() != NewStateTargetCreateLayout::Absent {
            return Err(NewStateCandidatePreserveCaptureError::NotAbsentTargetPreparationLayout);
        }
        Ok(Self {
            baseline: snapshot,
            projection,
        })
    }
}

/// Opaque exact restrictive-residue PRE evidence for future normalization.
#[allow(dead_code)] // consumed by the later sealed target-normalization checkpoint
pub(in crate::client::startup_reconciliation) struct UsrRollbackNewStateTargetNormalizeNamespaceEvidence {
    baseline: NamespaceSnapshot,
    projection: ResidueTargetProjection,
}

impl UsrRollbackNewStateTargetNormalizeNamespaceEvidence {
    pub(in crate::client::startup_reconciliation::activation_namespace) fn capture(
        snapshot: NamespaceSnapshot,
        record: &TransitionRecord,
    ) -> Result<Self, NewStateCandidatePreserveCaptureError> {
        let projection = ResidueTargetProjection::capture(&snapshot, record)?;
        Ok(Self {
            baseline: snapshot,
            projection,
        })
    }
}
