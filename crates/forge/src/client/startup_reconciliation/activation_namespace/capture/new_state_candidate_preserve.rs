//! Normalized evidence for one NewState candidate-preservation move.
//!
//! The only effect admitted here moves the fixed `usr` child from the retained
//! staging wrapper into the already-existing, exact journal-named quarantine
//! wrapper. The projection permits only metadata changes inherent in that
//! cross-parent rename, while the opaque parent holder keeps every descriptor
//! needed by ordered pre-move durability, the one-shot syscall, and mandatory
//! fresh reconciliation.

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
    Budget, CaptureError, InodeWitness, NamespaceFingerprint, NamespaceSnapshot, RootAbiFingerprint,
    StateIdFingerprint, TreeLocation, UsrFingerprint, WrapperFingerprint, controlled_directory_witness, open_directory,
};

#[cfg(test)]
pub(in crate::client) use effect::arm_before_new_state_candidate_preserve_move_reconciliation_capture;
pub(in crate::client::startup_reconciliation::activation_namespace) use effect::{
    AppliedNewStateCandidatePreserveMoveReconciliation, NewStateCandidatePreserveMoveReconciliation,
};
pub(in crate::client::startup_reconciliation::activation_namespace) use post_move_durability::{
    DurableNewStateCandidatePreservePostMoveNamespace, NewStateCandidatePreservePostMoveDurabilityError,
    PendingNewStateCandidatePreservePostMoveDurability,
};
#[cfg(test)]
pub(in crate::client) use post_move_durability::{
    NewStateCandidatePreservePostMoveDurabilityEvent, NewStateCandidatePreservePostMoveDurabilityFaultPoint,
    arm_before_new_state_candidate_preserve_post_move_candidate_sync,
    arm_before_new_state_candidate_preserve_post_move_final_post_capture,
    arm_before_new_state_candidate_preserve_post_move_quarantine_parent_sync,
    arm_before_new_state_candidate_preserve_post_move_staging_parent_sync,
    arm_before_new_state_candidate_preserve_post_move_target_parent_sync,
    arm_new_state_candidate_preserve_post_move_durability_fault,
    reset_new_state_candidate_preserve_post_move_durability_events,
    take_new_state_candidate_preserve_post_move_durability_events,
};
#[cfg(test)]
pub(in crate::client) use target_durability::{
    NewStateCandidatePreserveMoveFault, NewStateCandidatePreserveTargetDurabilityEvent,
    NewStateCandidatePreserveTargetDurabilityFaultPoint,
    arm_before_new_state_candidate_preserve_quarantine_parent_sync,
    arm_before_new_state_candidate_preserve_target_durability_final_pre_capture,
    arm_before_new_state_candidate_preserve_target_durability_pre_move_revalidation,
    arm_before_new_state_candidate_preserve_target_sync,
    arm_before_usr_rollback_new_state_candidate_preserve_effect_final_pre_capture,
    arm_new_state_candidate_preserve_move_fault, arm_new_state_candidate_preserve_target_durability_fault,
    new_state_candidate_preserve_move_attempt_count, reset_new_state_candidate_preserve_move_attempt_count,
    reset_new_state_candidate_preserve_target_durability_events,
    take_new_state_candidate_preserve_target_durability_events,
};
pub(in crate::client::startup_reconciliation::activation_namespace) use target_durability::{
    NewStateCandidatePreserveTargetDurabilityError, TargetDurableNewStateCandidatePreservePre,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client::startup_reconciliation::activation_namespace) enum NewStateCandidatePreserveLayout {
    StagedWithEmptyQuarantine,
    Preserved,
}

/// Stable identity of a parent whose directory entries are changed by rename.
///
/// Directory mtime, ctime, length, and link count are deliberately absent. A
/// cross-parent directory rename may change all four. Exact child inventories
/// in the layout projection prevent that mask from admitting another entry.
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

/// Stable moved-tree metadata. Rename changes ctime but not tree contents.
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
struct NewStateCandidatePreserveInvariant {
    root: InodeWitness,
    roots: InodeWitness,
    quarantine: InodeWitness,
    epoch: RuntimeEpoch,
    live: UsrFingerprint,
    root_abi: RootAbiFingerprint,
    isolation_abi: RootAbiFingerprint,
    candidate: SemanticCandidateFingerprint,
    staging_parent: MutableParentIdentity,
    target_parent: MutableParentIdentity,
    other_root_wrappers: Vec<WrapperFingerprint>,
    other_quarantine_wrappers: Vec<WrapperFingerprint>,
}

/// Layout plus every invariant which must survive the exact candidate move.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::client::startup_reconciliation::activation_namespace) struct ProjectedNewStateCandidatePreserveNamespace {
    layout: NewStateCandidatePreserveLayout,
    invariant: NewStateCandidatePreserveInvariant,
}

impl ProjectedNewStateCandidatePreserveNamespace {
    pub(in crate::client::startup_reconciliation::activation_namespace) fn capture(
        snapshot: &NamespaceSnapshot,
        record: &TransitionRecord,
    ) -> Result<Self, NewStateCandidatePreserveCaptureError> {
        if record.operation != Operation::NewState {
            return Err(NewStateCandidatePreserveCaptureError::WrongOperation);
        }
        if record.phase != Phase::CandidatePreserveIntent {
            return Err(NewStateCandidatePreserveCaptureError::WrongPhase);
        }

        let fingerprint = snapshot.fingerprint();
        let candidate = exact_tree_for_token(fingerprint, record.candidate.tree_token.as_str())?;
        if candidate.marker.links != 1 {
            return Err(NewStateCandidatePreserveCaptureError::CandidateMarkerLinks {
                actual: candidate.marker.links,
            });
        }
        let staging = exact_wrapper(
            &fingerprint.roots_entries,
            |wrapper| wrapper.role == TreeLocation::Staging,
            NewStateCandidatePreserveCaptureError::StagingCount,
        )?;
        let target = exact_wrapper(
            &fingerprint.quarantine_entries,
            |wrapper| wrapper.role == TreeLocation::TransitionQuarantine,
            NewStateCandidatePreserveCaptureError::TargetCount,
        )?;
        if target.name != record.quarantine_name.as_str().as_bytes() {
            return Err(NewStateCandidatePreserveCaptureError::WrongTargetName);
        }
        if !target.has_exact_private_permissions() {
            return Err(NewStateCandidatePreserveCaptureError::TargetPermissions);
        }

        let layout = if candidate.location == TreeLocation::Staging
            && wrapper_contains_only(staging, candidate)
            && wrapper_is_empty(target)
        {
            NewStateCandidatePreserveLayout::StagedWithEmptyQuarantine
        } else if candidate.location == TreeLocation::TransitionQuarantine
            && wrapper_is_empty(staging)
            && wrapper_contains_only(target, candidate)
        {
            NewStateCandidatePreserveLayout::Preserved
        } else {
            return Err(NewStateCandidatePreserveCaptureError::NotMoveLayout);
        };

        let other_root_wrappers = fingerprint
            .roots_entries
            .iter()
            .filter(|wrapper| wrapper.role != TreeLocation::Staging)
            .cloned()
            .collect();
        let other_quarantine_wrappers = fingerprint
            .quarantine_entries
            .iter()
            .filter(|wrapper| wrapper.role != TreeLocation::TransitionQuarantine)
            .cloned()
            .collect();
        Ok(Self {
            layout,
            invariant: NewStateCandidatePreserveInvariant {
                root: fingerprint.root,
                roots: fingerprint.roots,
                quarantine: fingerprint.quarantine,
                epoch: fingerprint.epoch.clone(),
                live: fingerprint.live.clone(),
                root_abi: fingerprint.root_abi.clone(),
                isolation_abi: fingerprint.isolation_abi.clone(),
                candidate: candidate.into(),
                staging_parent: staging.witness.into(),
                target_parent: target.witness.into(),
                other_root_wrappers,
                other_quarantine_wrappers,
            },
        })
    }

    pub(in crate::client::startup_reconciliation::activation_namespace) fn layout(
        &self,
    ) -> NewStateCandidatePreserveLayout {
        self.layout
    }

    pub(in crate::client::startup_reconciliation::activation_namespace) fn require_staged_to_preserved(
        &self,
        after: &Self,
    ) -> Result<(), NewStateCandidatePreserveCaptureError> {
        if self.layout != NewStateCandidatePreserveLayout::StagedWithEmptyQuarantine
            || after.layout != NewStateCandidatePreserveLayout::Preserved
        {
            return Err(NewStateCandidatePreserveCaptureError::NotStagedToPreserved {
                before: self.layout,
                after: after.layout,
            });
        }
        if self.invariant != after.invariant {
            return Err(NewStateCandidatePreserveCaptureError::InvariantChanged);
        }
        Ok(())
    }
}

fn exact_tree_for_token<'a>(
    fingerprint: &'a NamespaceFingerprint,
    token: &str,
) -> Result<&'a UsrFingerprint, NewStateCandidatePreserveCaptureError> {
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
        _ => Err(NewStateCandidatePreserveCaptureError::CandidateCount { actual: matches.len() }),
    }
}

fn exact_wrapper(
    wrappers: &[WrapperFingerprint],
    predicate: impl Fn(&WrapperFingerprint) -> bool,
    error: fn(usize) -> NewStateCandidatePreserveCaptureError,
) -> Result<&WrapperFingerprint, NewStateCandidatePreserveCaptureError> {
    let matches = wrappers.iter().filter(|wrapper| predicate(wrapper)).collect::<Vec<_>>();
    match matches.as_slice() {
        [wrapper] => Ok(*wrapper),
        _ => Err(error(matches.len())),
    }
}

fn wrapper_is_empty(wrapper: &WrapperFingerprint) -> bool {
    wrapper.entries.is_empty() && wrapper.usr.is_none() && wrapper.slot.is_none()
}

fn wrapper_contains_only(wrapper: &WrapperFingerprint, candidate: &UsrFingerprint) -> bool {
    wrapper.slot.is_none()
        && wrapper.usr.as_ref() == Some(candidate)
        && matches!(wrapper.entries.as_slice(), [(name, witness)] if name == b"usr" && *witness == candidate.directory)
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
    ) -> Result<Self, NewStateCandidatePreserveCaptureError> {
        let staging = MutableParentIdentity::from(staging);
        let target = MutableParentIdentity::from(target);
        if staging.device != target.device {
            return Err(NewStateCandidatePreserveCaptureError::ParentsCrossDevice {
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
    ) -> Result<Self, NewStateCandidatePreserveCaptureError> {
        let actual = Self::from_witnesses(staging, target)?;
        if actual == self {
            Ok(actual)
        } else {
            Err(NewStateCandidatePreserveCaptureError::ParentIdentityChanged)
        }
    }
}

/// Opaque descriptors for the source and destination parent namespace.
///
/// No raw descriptor, callback, clone, or conversion API is exposed. The
/// private durability child must consume this value before the resulting
/// target-durable typestate can reach the one-shot move.
#[derive(Debug)]
pub(in crate::client::startup_reconciliation::activation_namespace) struct RetainedNewStateCandidatePreserveParents {
    root: File,
    roots: File,
    staging: File,
    quarantine: File,
    target: File,
    candidate: TreeMarkerStore,
    target_name: CString,
    root_path: PathBuf,
    roots_path: PathBuf,
    staging_path: PathBuf,
    quarantine_path: PathBuf,
    target_path: PathBuf,
    root_witness: InodeWitness,
    roots_witness: InodeWitness,
    quarantine_witness: InodeWitness,
    identity: MoveParentIdentity,
}

impl RetainedNewStateCandidatePreserveParents {
    pub(in crate::client::startup_reconciliation::activation_namespace) fn capture(
        snapshot: &NamespaceSnapshot,
        record: &TransitionRecord,
    ) -> Result<Self, NewStateCandidatePreserveCaptureError> {
        Self::capture_for_layout(
            snapshot,
            record,
            NewStateCandidatePreserveLayout::StagedWithEmptyQuarantine,
        )
    }

    /// Capture the same descriptor set from an exact already-preserved POST
    /// layout without weakening the staged-only move admission above.
    pub(in crate::client::startup_reconciliation::activation_namespace) fn capture_preserved(
        snapshot: &NamespaceSnapshot,
        record: &TransitionRecord,
    ) -> Result<Self, NewStateCandidatePreserveCaptureError> {
        Self::capture_for_layout(snapshot, record, NewStateCandidatePreserveLayout::Preserved)
    }

    fn capture_for_layout(
        snapshot: &NamespaceSnapshot,
        record: &TransitionRecord,
        expected_layout: NewStateCandidatePreserveLayout,
    ) -> Result<Self, NewStateCandidatePreserveCaptureError> {
        let projection = ProjectedNewStateCandidatePreserveNamespace::capture(snapshot, record)?;
        if projection.layout() != expected_layout {
            return Err(NewStateCandidatePreserveCaptureError::NotMoveLayout);
        }
        let staging = exact_retained_wrapper(&snapshot.roots_entries, TreeLocation::Staging, "staging")?;
        let target = exact_retained_wrapper(
            &snapshot.quarantine_entries,
            TreeLocation::TransitionQuarantine,
            "transition quarantine",
        )?;
        let target_name = CString::new(record.quarantine_name.as_str())
            .map_err(|_| NewStateCandidatePreserveCaptureError::WrongTargetName)?;
        let staging_path = snapshot.roots_path.join("staging");
        let target_path = snapshot.quarantine_path.join(record.quarantine_name.as_str());
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
            quarantine: clone_descriptor(
                &snapshot.quarantine,
                &snapshot.quarantine_path,
                "clone retained `.cast/quarantine`",
            )?,
            target: clone_descriptor(
                &target.directory,
                &target_path,
                "clone retained transition quarantine wrapper",
            )?,
            candidate,
            target_name,
            root_path: snapshot.root_path.clone(),
            roots_path: snapshot.roots_path.clone(),
            staging_path,
            quarantine_path: snapshot.quarantine_path.clone(),
            target_path,
            root_witness: snapshot.fingerprint.root,
            roots_witness: snapshot.fingerprint.roots,
            quarantine_witness: snapshot.fingerprint.quarantine,
            identity,
        })
    }

    /// Flush the exact retained candidate tree before target durability and
    /// the final PRE recapture.
    ///
    /// This remains a pre-move safety barrier. Post-move tree and changed-parent
    /// durability are deliberately outside this checkpoint.
    pub(in crate::client::startup_reconciliation::activation_namespace) fn sync_retained_candidate_for_move(
        &self,
    ) -> Result<(), NewStateCandidatePreserveCaptureError> {
        self.candidate.sync_retained_tree().map_err(CaptureError::TreeMarker)?;
        Ok(())
    }

    pub(in crate::client::startup_reconciliation::activation_namespace) fn revalidate_value_identity(
        &self,
        installation: &Installation,
    ) -> Result<(), NewStateCandidatePreserveCaptureError> {
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

        require_exact_witness(
            controlled_directory_witness(&self.quarantine, &self.quarantine_path)?,
            self.quarantine_witness,
            &self.quarantine_path,
        )?;
        let named_quarantine = open_directory(&self.root, c".cast/quarantine", &self.quarantine_path, &mut budget)?;
        require_exact_witness(
            controlled_directory_witness(&named_quarantine, &self.quarantine_path)?,
            self.quarantine_witness,
            &self.quarantine_path,
        )?;

        let retained_staging = controlled_directory_witness(&self.staging, &self.staging_path)?;
        let retained_target = controlled_directory_witness(&self.target, &self.target_path)?;
        self.identity.require_rebound(retained_staging, retained_target)?;
        let named_staging = open_directory(&self.roots, c"staging", &self.staging_path, &mut budget)?;
        let named_target = open_directory(&self.quarantine, &self.target_name, &self.target_path, &mut budget)?;
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
) -> Result<&'a super::RetainedUsr, NewStateCandidatePreserveCaptureError> {
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
        _ => Err(NewStateCandidatePreserveCaptureError::CandidateCount { actual: matches.len() }),
    }
}

fn exact_retained_wrapper<'a>(
    wrappers: &'a [super::RetainedWrapper],
    role: TreeLocation,
    label: &'static str,
) -> Result<&'a super::RetainedWrapper, NewStateCandidatePreserveCaptureError> {
    let matches = wrappers
        .iter()
        .filter(|wrapper| wrapper.fingerprint.role == role)
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [wrapper] => Ok(*wrapper),
        _ => Err(NewStateCandidatePreserveCaptureError::RetainedWrapperCount {
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
pub(in crate::client::startup_reconciliation::activation_namespace) enum NewStateCandidatePreserveCaptureError {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error("NewState candidate-preservation projection requires NewState")]
    WrongOperation,
    #[error("NewState candidate-preservation projection requires CandidatePreserveIntent")]
    WrongPhase,
    #[error("the candidate token occurs at {actual} namespace locations")]
    CandidateCount { actual: usize },
    #[error("the NewState candidate marker has {actual} links instead of one")]
    CandidateMarkerLinks { actual: u64 },
    #[error("the fixed staging wrapper occurs {0} times")]
    StagingCount(usize),
    #[error("the exact transition-quarantine wrapper occurs {0} times")]
    TargetCount(usize),
    #[error("the transition-quarantine wrapper does not have the journal-derived name")]
    WrongTargetName,
    #[error("the transition-quarantine wrapper permissions are not exactly 0700")]
    TargetPermissions,
    #[error("the namespace is not exact staged-with-empty-target or preserved NewState evidence")]
    NotMoveLayout,
    #[error("the namespace is not exact absent-target NewState preparation evidence")]
    NotAbsentTargetPreparationLayout,
    #[error("the namespace is not a safe NewState target-creation layout")]
    NotTargetCreateLayout,
    #[error("NewState target creation is not absent-to-prepared ({before:?} -> {after:?})")]
    NotAbsentToPreparedTarget {
        before: super::NewStateTargetCreateLayout,
        after: super::NewStateTargetCreateLayout,
    },
    #[error("the namespace is not exact restrictive-residue NewState preparation evidence")]
    NotResidueTargetPreparationLayout,
    #[error("the namespace is not a safe NewState target-normalization layout")]
    NotTargetNormalizeLayout,
    #[error("NewState target normalization is not restrictive-residue-to-empty-private ({before:?} -> {after:?})")]
    NotResidueToPrivateTarget {
        before: super::NewStateTargetNormalizeLayout,
        after: super::NewStateTargetNormalizeLayout,
    },
    #[error("NewState candidate preservation is not exact staged-to-preserved ({before:?} -> {after:?})")]
    NotStagedToPreserved {
        before: NewStateCandidatePreserveLayout,
        after: NewStateCandidatePreserveLayout,
    },
    #[error("namespace evidence changed beyond the exact NewState candidate move")]
    InvariantChanged,
    #[error("retained candidate-move parents cross devices ({staging} != {target})")]
    ParentsCrossDevice { staging: u64, target: u64 },
    #[error("retained NewState candidate-move parent identity changed")]
    ParentIdentityChanged,
    #[error("retained {role} wrapper occurs {actual} times")]
    RetainedWrapperCount { role: &'static str, actual: usize },
}
