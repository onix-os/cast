//! Descriptor-bound evidence for one archived-candidate child move.
//!
//! Startup recovery deliberately does not reconstruct the legacy whole-wrapper
//! exchange. The journal-bound rollback topology already retains the exact
//! state-slot link in the canonical wrapper, so the sole namespace delta is a
//! no-replace move of `usr` from fixed staging into that wrapper.

mod effect;
mod post_move_durability;
mod target_durability;

use std::{ffi::CString, fs::File, path::PathBuf};

use crate::{
    Installation,
    transition_journal::{Operation, Phase, RuntimeEpoch, RuntimeTreeIdentity, TransitionRecord},
    tree_marker::TreeMarkerStore,
};

use super::{
    Budget, CaptureError, InodeWitness, NamespaceFingerprint, NamespaceSnapshot, RootAbiFingerprint, SlotFingerprint,
    StateIdFingerprint, TreeLocation, UsrFingerprint, WrapperFingerprint, controlled_directory_witness, open_directory,
};

pub(in crate::client::startup_reconciliation::activation_namespace) use effect::{
    AppliedArchivedCandidatePreserveMoveReconciliation, ArchivedCandidatePreserveMoveReconciliation,
};
#[cfg(test)]
pub(in crate::client) use effect::{
    arm_before_archived_candidate_preserve_move_reconciliation_capture,
    arm_before_archived_candidate_preserve_move_reconciliation_closing,
};
pub(in crate::client::startup_reconciliation::activation_namespace) use post_move_durability::{
    ArchivedCandidatePreservePostMoveDurabilityError, DurableArchivedCandidatePreservePostMoveNamespace,
    PendingArchivedCandidatePreservePostMoveDurability,
};
#[cfg(test)]
pub(in crate::client) use post_move_durability::{
    ArchivedCandidatePreservePostMoveDurabilityEvent, ArchivedCandidatePreservePostMoveDurabilityFaultPoint,
    arm_archived_candidate_preserve_post_move_durability_fault,
    arm_before_archived_candidate_preserve_durable_post_revalidation_capture,
    arm_before_archived_candidate_preserve_post_candidate_sync,
    arm_before_archived_candidate_preserve_post_final_capture,
    arm_before_archived_candidate_preserve_post_roots_parent_sync,
    arm_before_archived_candidate_preserve_post_staging_parent_sync,
    arm_before_archived_candidate_preserve_post_target_parent_sync,
    reset_archived_candidate_preserve_post_move_durability_events,
    take_archived_candidate_preserve_post_move_durability_events,
};
pub(in crate::client::startup_reconciliation::activation_namespace) use target_durability::TargetDurableArchivedCandidatePreservePre;
#[cfg(test)]
pub(in crate::client) use target_durability::{
    ArchivedCandidatePreserveMoveFault, ArchivedCandidatePreserveTargetDurabilityEvent,
    ArchivedCandidatePreserveTargetDurabilityFaultPoint, archived_candidate_preserve_move_attempt_count,
    arm_archived_candidate_preserve_move_fault, arm_archived_candidate_preserve_target_durability_fault,
    arm_before_archived_candidate_preserve_pre_candidate_sync,
    arm_before_archived_candidate_preserve_pre_final_capture,
    arm_before_archived_candidate_preserve_pre_move_revalidation,
    arm_before_archived_candidate_preserve_pre_roots_parent_sync,
    arm_before_archived_candidate_preserve_pre_staging_parent_sync,
    arm_before_archived_candidate_preserve_pre_target_parent_sync,
    reset_archived_candidate_preserve_move_attempt_count, reset_archived_candidate_preserve_target_durability_events,
    take_archived_candidate_preserve_target_durability_events,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client::startup_reconciliation::activation_namespace) enum ArchivedCandidatePreserveLayout {
    StagedWithCanonicalSlot,
    Preserved,
}

/// Stable identity of a wrapper whose child inventory changes during rename.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MutableParentIdentity {
    device: u64,
    inode: u64,
    mode: u32,
    owner: u32,
    group: u32,
}

impl From<InodeWitness> for MutableParentIdentity {
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

/// Rename changes the moved directory ctime, but no content-bearing field.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MovedUsrIdentity {
    device: u64,
    inode: u64,
    mode: u32,
    owner: u32,
    group: u32,
    links: u64,
    length: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
}

impl From<InodeWitness> for MovedUsrIdentity {
    fn from(witness: InodeWitness) -> Self {
        Self {
            device: witness.device,
            inode: witness.inode,
            mode: witness.mode,
            owner: witness.owner,
            group: witness.group,
            links: witness.links,
            length: witness.length,
            modified_seconds: witness.modified_seconds,
            modified_nanoseconds: witness.modified_nanoseconds,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SemanticCandidateFingerprint {
    token: String,
    directory: MovedUsrIdentity,
    marker: InodeWitness,
    state_id: StateIdFingerprint,
    runtime: RuntimeTreeIdentity,
}

impl From<&UsrFingerprint> for SemanticCandidateFingerprint {
    fn from(candidate: &UsrFingerprint) -> Self {
        Self {
            token: candidate.token.clone(),
            directory: candidate.directory.into(),
            marker: candidate.marker,
            state_id: candidate.state_id.clone(),
            runtime: candidate.runtime,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ArchivedCandidatePreserveInvariant {
    root: InodeWitness,
    roots: InodeWitness,
    quarantine: InodeWitness,
    epoch: RuntimeEpoch,
    live: UsrFingerprint,
    root_abi: RootAbiFingerprint,
    isolation_abi: RootAbiFingerprint,
    candidate: SemanticCandidateFingerprint,
    slot: SlotFingerprint,
    staging_parent: MutableParentIdentity,
    target_parent: MutableParentIdentity,
    other_root_wrappers: Vec<WrapperFingerprint>,
    quarantine_wrappers: Vec<WrapperFingerprint>,
}

/// Layout plus every namespace fact allowed to survive the one child move.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::client::startup_reconciliation::activation_namespace) struct ProjectedArchivedCandidatePreserveNamespace {
    layout: ArchivedCandidatePreserveLayout,
    state: i32,
    invariant: ArchivedCandidatePreserveInvariant,
}

impl ProjectedArchivedCandidatePreserveNamespace {
    pub(in crate::client::startup_reconciliation::activation_namespace) fn capture(
        snapshot: &NamespaceSnapshot,
        record: &TransitionRecord,
    ) -> Result<Self, ArchivedCandidatePreserveCaptureError> {
        if record.operation != Operation::ActivateArchived {
            return Err(ArchivedCandidatePreserveCaptureError::WrongOperation);
        }
        if record.phase != Phase::CandidatePreserveIntent {
            return Err(ArchivedCandidatePreserveCaptureError::WrongPhase);
        }
        let state = record
            .candidate
            .id
            .ok_or(ArchivedCandidatePreserveCaptureError::CandidateStateMissing)?;
        let fingerprint = snapshot.fingerprint();
        let candidate = exact_tree_for_token(fingerprint, record.candidate.tree_token.as_str())?;
        if candidate.marker.links != 2 {
            return Err(ArchivedCandidatePreserveCaptureError::CandidateMarkerLinks {
                actual: candidate.marker.links,
            });
        }
        let staging = exact_wrapper(
            &fingerprint.roots_entries,
            |wrapper| wrapper.role == TreeLocation::Staging,
            "staging",
        )?;
        let target = exact_wrapper(
            &fingerprint.roots_entries,
            |wrapper| wrapper.role == TreeLocation::State(state),
            "canonical archived candidate",
        )?;
        let slot = target
            .slot
            .as_ref()
            .ok_or(ArchivedCandidatePreserveCaptureError::CanonicalSlotMissing)?;
        if slot.state != state || slot.token != record.candidate.tree_token.as_str() || slot.witness != candidate.marker
        {
            return Err(ArchivedCandidatePreserveCaptureError::CanonicalSlotMismatch);
        }

        let layout = if candidate.location == TreeLocation::Staging
            && wrapper_contains_only_candidate(staging, candidate)
            && wrapper_contains_only_slot(target, slot)
        {
            ArchivedCandidatePreserveLayout::StagedWithCanonicalSlot
        } else if candidate.location == TreeLocation::State(state)
            && wrapper_is_empty(staging)
            && wrapper_contains_candidate_and_slot(target, candidate, slot)
        {
            ArchivedCandidatePreserveLayout::Preserved
        } else {
            return Err(ArchivedCandidatePreserveCaptureError::NotMoveLayout);
        };

        let other_root_wrappers = fingerprint
            .roots_entries
            .iter()
            .filter(|wrapper| wrapper.role != TreeLocation::Staging && wrapper.role != TreeLocation::State(state))
            .cloned()
            .collect();
        Ok(Self {
            layout,
            state,
            invariant: ArchivedCandidatePreserveInvariant {
                root: fingerprint.root,
                roots: fingerprint.roots,
                quarantine: fingerprint.quarantine,
                epoch: fingerprint.epoch.clone(),
                live: fingerprint.live.clone(),
                root_abi: fingerprint.root_abi.clone(),
                isolation_abi: fingerprint.isolation_abi.clone(),
                candidate: candidate.into(),
                slot: slot.clone(),
                staging_parent: staging.witness.into(),
                target_parent: target.witness.into(),
                other_root_wrappers,
                quarantine_wrappers: fingerprint.quarantine_entries.clone(),
            },
        })
    }

    pub(in crate::client::startup_reconciliation::activation_namespace) fn layout(
        &self,
    ) -> ArchivedCandidatePreserveLayout {
        self.layout
    }

    pub(in crate::client::startup_reconciliation::activation_namespace) fn require_staged_to_preserved(
        &self,
        after: &Self,
    ) -> Result<(), ArchivedCandidatePreserveCaptureError> {
        if self.layout != ArchivedCandidatePreserveLayout::StagedWithCanonicalSlot
            || after.layout != ArchivedCandidatePreserveLayout::Preserved
        {
            return Err(ArchivedCandidatePreserveCaptureError::NotStagedToPreserved {
                before: self.layout,
                after: after.layout,
            });
        }
        if self.state != after.state || self.invariant != after.invariant {
            return Err(ArchivedCandidatePreserveCaptureError::InvariantChanged);
        }
        Ok(())
    }
}

fn exact_tree_for_token<'a>(
    fingerprint: &'a NamespaceFingerprint,
    token: &str,
) -> Result<&'a UsrFingerprint, ArchivedCandidatePreserveCaptureError> {
    let matches = std::iter::once(&fingerprint.live)
        .chain(
            fingerprint
                .roots_entries
                .iter()
                .chain(&fingerprint.quarantine_entries)
                .filter_map(|wrapper| wrapper.usr.as_ref()),
        )
        .filter(|tree| tree.token == token)
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [candidate] => Ok(*candidate),
        _ => Err(ArchivedCandidatePreserveCaptureError::CandidateCount { actual: matches.len() }),
    }
}

fn exact_wrapper<'a>(
    wrappers: &'a [WrapperFingerprint],
    predicate: impl Fn(&WrapperFingerprint) -> bool,
    role: &'static str,
) -> Result<&'a WrapperFingerprint, ArchivedCandidatePreserveCaptureError> {
    let matches = wrappers.iter().filter(|wrapper| predicate(wrapper)).collect::<Vec<_>>();
    match matches.as_slice() {
        [wrapper] => Ok(*wrapper),
        _ => Err(ArchivedCandidatePreserveCaptureError::WrapperCount {
            role,
            actual: matches.len(),
        }),
    }
}

fn wrapper_is_empty(wrapper: &WrapperFingerprint) -> bool {
    wrapper.entries.is_empty() && wrapper.usr.is_none() && wrapper.slot.is_none()
}

fn wrapper_contains_only_candidate(wrapper: &WrapperFingerprint, candidate: &UsrFingerprint) -> bool {
    wrapper.slot.is_none()
        && wrapper.usr.as_ref() == Some(candidate)
        && matches!(wrapper.entries.as_slice(), [(name, witness)] if name == b"usr" && *witness == candidate.directory)
}

fn wrapper_contains_only_slot(wrapper: &WrapperFingerprint, slot: &SlotFingerprint) -> bool {
    wrapper.usr.is_none()
        && wrapper.slot.as_ref() == Some(slot)
        && matches!(wrapper.entries.as_slice(), [(name, witness)] if *name == slot_name(slot) && *witness == slot.witness)
}

fn wrapper_contains_candidate_and_slot(
    wrapper: &WrapperFingerprint,
    candidate: &UsrFingerprint,
    slot: &SlotFingerprint,
) -> bool {
    wrapper.usr.as_ref() == Some(candidate)
        && wrapper.slot.as_ref() == Some(slot)
        && wrapper.entries.len() == 2
        && wrapper
            .entries
            .iter()
            .any(|(name, witness)| name == b"usr" && *witness == candidate.directory)
        && wrapper
            .entries
            .iter()
            .any(|(name, witness)| *name == slot_name(slot) && *witness == slot.witness)
}

fn slot_name(slot: &SlotFingerprint) -> Vec<u8> {
    format!(".cast-state-slot-{}-{}", slot.state, slot.token).into_bytes()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MoveParentIdentity {
    staging: MutableParentIdentity,
    target: MutableParentIdentity,
}

impl MoveParentIdentity {
    fn from_witnesses(
        staging: InodeWitness,
        target: InodeWitness,
    ) -> Result<Self, ArchivedCandidatePreserveCaptureError> {
        let staging = MutableParentIdentity::from(staging);
        let target = MutableParentIdentity::from(target);
        if staging.device != target.device {
            return Err(ArchivedCandidatePreserveCaptureError::ParentsCrossDevice {
                staging: staging.device,
                target: target.device,
            });
        }
        Ok(Self { staging, target })
    }

    fn require_rebound(
        self,
        staging: InodeWitness,
        target: InodeWitness,
    ) -> Result<(), ArchivedCandidatePreserveCaptureError> {
        if Self::from_witnesses(staging, target)? == self {
            Ok(())
        } else {
            Err(ArchivedCandidatePreserveCaptureError::ParentIdentityChanged)
        }
    }
}

/// Opaque retained source, destination, candidate, and namespace descriptors.
#[derive(Debug)]
pub(in crate::client::startup_reconciliation::activation_namespace) struct RetainedArchivedCandidatePreserveParents {
    root: File,
    roots: File,
    pub(super) staging: File,
    pub(super) target: File,
    pub(super) candidate: TreeMarkerStore,
    target_name: CString,
    root_path: PathBuf,
    roots_path: PathBuf,
    pub(super) staging_path: PathBuf,
    pub(super) target_path: PathBuf,
    root_witness: InodeWitness,
    roots_witness: InodeWitness,
    identity: MoveParentIdentity,
}

impl RetainedArchivedCandidatePreserveParents {
    pub(in crate::client::startup_reconciliation::activation_namespace) fn capture(
        snapshot: &NamespaceSnapshot,
        record: &TransitionRecord,
    ) -> Result<Self, ArchivedCandidatePreserveCaptureError> {
        Self::capture_for_layout(
            snapshot,
            record,
            ArchivedCandidatePreserveLayout::StagedWithCanonicalSlot,
        )
    }

    pub(in crate::client::startup_reconciliation::activation_namespace) fn capture_preserved(
        snapshot: &NamespaceSnapshot,
        record: &TransitionRecord,
    ) -> Result<Self, ArchivedCandidatePreserveCaptureError> {
        Self::capture_for_layout(snapshot, record, ArchivedCandidatePreserveLayout::Preserved)
    }

    fn capture_for_layout(
        snapshot: &NamespaceSnapshot,
        record: &TransitionRecord,
        expected_layout: ArchivedCandidatePreserveLayout,
    ) -> Result<Self, ArchivedCandidatePreserveCaptureError> {
        let projection = ProjectedArchivedCandidatePreserveNamespace::capture(snapshot, record)?;
        if projection.layout() != expected_layout {
            return Err(ArchivedCandidatePreserveCaptureError::NotMoveLayout);
        }
        let state = record
            .candidate
            .id
            .ok_or(ArchivedCandidatePreserveCaptureError::CandidateStateMissing)?;
        let staging = exact_retained_wrapper(&snapshot.roots_entries, &TreeLocation::Staging, "staging")?;
        let target = exact_retained_wrapper(
            &snapshot.roots_entries,
            &TreeLocation::State(state),
            "canonical archived candidate",
        )?;
        let target_name =
            CString::new(state.to_string()).map_err(|_| ArchivedCandidatePreserveCaptureError::InvalidTargetName)?;
        let staging_path = snapshot.roots_path.join("staging");
        let target_path = snapshot.roots_path.join(state.to_string());
        let identity = MoveParentIdentity::from_witnesses(staging.fingerprint.witness, target.fingerprint.witness)?;
        let candidate = exact_retained_candidate(snapshot, record.candidate.tree_token.as_str())?;
        let candidate = TreeMarkerStore::open(
            candidate.store.retained_directory(),
            candidate.store.display_path().to_owned(),
        )
        .map_err(CaptureError::TreeMarker)?;
        Ok(Self {
            root: clone_descriptor(&snapshot.root, &snapshot.root_path, "clone retained installation root")?,
            roots: clone_descriptor(&snapshot.roots, &snapshot.roots_path, "clone retained `.cast/root`")?,
            staging: clone_descriptor(&staging.directory, &staging_path, "clone retained staging wrapper")?,
            target: clone_descriptor(
                &target.directory,
                &target_path,
                "clone retained archived target wrapper",
            )?,
            candidate,
            target_name,
            root_path: snapshot.root_path.clone(),
            roots_path: snapshot.roots_path.clone(),
            staging_path,
            target_path,
            root_witness: snapshot.fingerprint.root,
            roots_witness: snapshot.fingerprint.roots,
            identity,
        })
    }

    pub(in crate::client::startup_reconciliation::activation_namespace) fn revalidate_value_identity(
        &self,
        installation: &Installation,
    ) -> Result<(), ArchivedCandidatePreserveCaptureError> {
        installation
            .revalidate_mutable_namespace()
            .map_err(CaptureError::Installation)?;
        let mut budget = Budget::new()?;
        require_exact_witness(
            controlled_directory_witness(&self.root, &self.root_path)?,
            self.root_witness,
            &self.root_path,
        )?;
        require_exact_witness(
            controlled_directory_witness(installation.root_directory(), &self.root_path)?,
            self.root_witness,
            &self.root_path,
        )?;
        require_exact_witness(
            controlled_directory_witness(&self.roots, &self.roots_path)?,
            self.roots_witness,
            &self.roots_path,
        )?;
        let named_roots = open_directory(&self.root, c".cast/root", &self.roots_path, &mut budget)?;
        require_exact_witness(
            controlled_directory_witness(&named_roots, &self.roots_path)?,
            self.roots_witness,
            &self.roots_path,
        )?;
        self.identity.require_rebound(
            controlled_directory_witness(&self.staging, &self.staging_path)?,
            controlled_directory_witness(&self.target, &self.target_path)?,
        )?;
        let named_staging = open_directory(&self.roots, c"staging", &self.staging_path, &mut budget)?;
        let named_target = open_directory(&self.roots, &self.target_name, &self.target_path, &mut budget)?;
        self.identity.require_rebound(
            controlled_directory_witness(&named_staging, &self.staging_path)?,
            controlled_directory_witness(&named_target, &self.target_path)?,
        )?;
        installation
            .revalidate_mutable_namespace()
            .map_err(CaptureError::Installation)?;
        Ok(())
    }
}

fn exact_retained_candidate<'a>(
    snapshot: &'a NamespaceSnapshot,
    token: &str,
) -> Result<&'a super::RetainedUsr, ArchivedCandidatePreserveCaptureError> {
    let matches = std::iter::once(&snapshot.live)
        .chain(
            snapshot
                .roots_entries
                .iter()
                .chain(&snapshot.quarantine_entries)
                .filter_map(|wrapper| wrapper.usr.as_ref()),
        )
        .filter(|tree| tree.fingerprint.token == token)
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [candidate] => Ok(*candidate),
        _ => Err(ArchivedCandidatePreserveCaptureError::CandidateCount { actual: matches.len() }),
    }
}

fn exact_retained_wrapper<'a>(
    wrappers: &'a [super::RetainedWrapper],
    role: &TreeLocation,
    label: &'static str,
) -> Result<&'a super::RetainedWrapper, ArchivedCandidatePreserveCaptureError> {
    let matches = wrappers
        .iter()
        .filter(|wrapper| &wrapper.fingerprint.role == role)
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [wrapper] => Ok(*wrapper),
        _ => Err(ArchivedCandidatePreserveCaptureError::RetainedWrapperCount {
            role: label,
            actual: matches.len(),
        }),
    }
}

fn clone_descriptor(file: &File, path: &std::path::Path, operation: &'static str) -> Result<File, CaptureError> {
    file.try_clone().map_err(|source| CaptureError::Io {
        operation,
        path: path.to_owned(),
        source,
    })
}

fn require_exact_witness(
    actual: InodeWitness,
    expected: InodeWitness,
    path: &std::path::Path,
) -> Result<(), CaptureError> {
    if actual == expected {
        Ok(())
    } else {
        Err(CaptureError::InodeChanged { path: path.to_owned() })
    }
}

#[derive(Debug, thiserror::Error)]
pub(in crate::client::startup_reconciliation::activation_namespace) enum ArchivedCandidatePreserveCaptureError {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error("archived candidate preservation requires ActivateArchived")]
    WrongOperation,
    #[error("archived candidate preservation requires CandidatePreserveIntent")]
    WrongPhase,
    #[error("the archived candidate state ID is absent")]
    CandidateStateMissing,
    #[error("the candidate token occurs at {actual} namespace locations")]
    CandidateCount { actual: usize },
    #[error("the archived candidate marker has {actual} links instead of two")]
    CandidateMarkerLinks { actual: u64 },
    #[error("the retained {role} wrapper occurs {actual} times")]
    WrapperCount { role: &'static str, actual: usize },
    #[error("the canonical archived-candidate slot is absent")]
    CanonicalSlotMissing,
    #[error("the canonical archived-candidate slot does not authenticate the candidate marker")]
    CanonicalSlotMismatch,
    #[error("the namespace is not exact archived staged-with-slot or preserved evidence")]
    NotMoveLayout,
    #[error("archived candidate preservation is not exact staged-to-preserved ({before:?} -> {after:?})")]
    NotStagedToPreserved {
        before: ArchivedCandidatePreserveLayout,
        after: ArchivedCandidatePreserveLayout,
    },
    #[error("namespace evidence changed beyond the exact archived candidate move")]
    InvariantChanged,
    #[error("retained archived move parents cross devices ({staging} != {target})")]
    ParentsCrossDevice { staging: u64, target: u64 },
    #[error("retained archived candidate parent identity changed")]
    ParentIdentityChanged,
    #[error("the canonical archived candidate target name is invalid")]
    InvalidTargetName,
    #[error("retained {role} wrapper occurs {actual} times")]
    RetainedWrapperCount { role: &'static str, actual: usize },
}
