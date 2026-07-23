//! Exact projection and retained descriptors for whole-wrapper ActiveReblit preservation.
//!
//! It models one descriptor-relative exchange between the fixed staging name
//! and the journal-derived private replacement reservation. Raw syscall status
//! is never interpreted as the semantic outcome.

mod effect;
mod post_exchange_durability;
mod pre_exchange_durability;

use std::fs::File;

use crate::transition_journal::{Operation, Phase, RuntimeEpoch, RuntimeTreeIdentity, TransitionRecord};

use super::{
    CaptureError, InodeWitness, NamespaceFingerprint, NamespaceSnapshot, RootAbiFingerprint, StateIdFingerprint,
    TreeLocation, UsrFingerprint, WrapperFingerprint,
};

#[cfg(test)]
pub(in crate::client) use effect::{
    ActiveReblitCandidatePreserveExchangeFault, active_reblit_candidate_preserve_exchange_attempt_count,
    arm_active_reblit_candidate_preserve_exchange_fault,
    arm_before_active_reblit_candidate_preserve_reconciliation_capture,
    reset_active_reblit_candidate_preserve_exchange_attempt_count,
};
pub(in crate::client::startup_reconciliation::activation_namespace) use effect::{
    ActiveReblitCandidatePreserveExchangeReconciliation, AppliedActiveReblitCandidatePreserveExchangeReconciliation,
};
pub(in crate::client::startup_reconciliation::activation_namespace) use post_exchange_durability::{
    ActiveReblitCandidatePreservePostExchangeDurabilityError,
    DurableActiveReblitCandidatePreservePostExchangeNamespace,
    PendingActiveReblitCandidatePreservePostExchangeDurability,
};
#[cfg(test)]
pub(in crate::client) use post_exchange_durability::{
    ActiveReblitCandidatePreservePostExchangeDurabilityEvent,
    ActiveReblitCandidatePreservePostExchangeDurabilityFaultPoint,
    arm_active_reblit_candidate_preserve_post_exchange_durability_fault,
    arm_before_active_reblit_candidate_preserve_durable_post_revalidation_capture,
    arm_before_active_reblit_candidate_preserve_post_exchange_candidate_sync,
    arm_before_active_reblit_candidate_preserve_post_exchange_candidate_wrapper_sync,
    arm_before_active_reblit_candidate_preserve_post_exchange_final_post_capture,
    arm_before_active_reblit_candidate_preserve_post_exchange_quarantine_parent_sync,
    arm_before_active_reblit_candidate_preserve_post_exchange_reservation_wrapper_sync,
    arm_before_active_reblit_candidate_preserve_post_exchange_roots_parent_sync,
    reset_active_reblit_candidate_preserve_post_exchange_durability_events,
    take_active_reblit_candidate_preserve_post_exchange_durability_events,
};
use pre_exchange_durability::require_exact_pre;
pub(in crate::client::startup_reconciliation::activation_namespace) use pre_exchange_durability::{
    PreparedActiveReblitCandidatePreserveExchange, RetainedActiveReblitCandidatePreserveParents,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client::startup_reconciliation::activation_namespace) enum ActiveReblitCandidatePreserveLayout {
    Staged,
    Preserved,
}

/// Stable identity of a directory whose entries may change during exchange.
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

/// Stable identity of a wrapper whose rename may update only ctime.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ExchangedWrapperIdentity {
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

impl From<InodeWitness> for ExchangedWrapperIdentity {
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
    directory: InodeWitness,
    marker: InodeWitness,
    state_id: StateIdFingerprint,
    runtime: RuntimeTreeIdentity,
}

impl From<&UsrFingerprint> for SemanticCandidateFingerprint {
    fn from(candidate: &UsrFingerprint) -> Self {
        Self {
            token: candidate.token.clone(),
            directory: candidate.directory,
            marker: candidate.marker,
            state_id: candidate.state_id.clone(),
            runtime: candidate.runtime,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ActiveReblitCandidatePreserveInvariant {
    root: InodeWitness,
    roots: MutableParentIdentity,
    quarantine: MutableParentIdentity,
    epoch: RuntimeEpoch,
    live: UsrFingerprint,
    root_abi: RootAbiFingerprint,
    isolation_abi: RootAbiFingerprint,
    candidate: SemanticCandidateFingerprint,
    candidate_wrapper: ExchangedWrapperIdentity,
    reservation_wrapper: ExchangedWrapperIdentity,
    other_root_wrappers: Vec<WrapperFingerprint>,
    other_quarantine_wrappers: Vec<WrapperFingerprint>,
}

/// Layout plus every invariant allowed to survive the one wrapper exchange.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::client::startup_reconciliation::activation_namespace) struct ProjectedActiveReblitCandidatePreserveNamespace
{
    layout: ActiveReblitCandidatePreserveLayout,
    wrapper_index: usize,
    target_name: Vec<u8>,
    invariant: ActiveReblitCandidatePreserveInvariant,
}

impl ProjectedActiveReblitCandidatePreserveNamespace {
    pub(in crate::client::startup_reconciliation::activation_namespace) fn capture(
        snapshot: &NamespaceSnapshot,
        record: &TransitionRecord,
    ) -> Result<Self, ActiveReblitCandidatePreserveEffectError> {
        if record.operation != Operation::ActiveReblit {
            return Err(ActiveReblitCandidatePreserveEffectError::WrongOperation);
        }
        if record.phase != Phase::CandidatePreserveIntent {
            return Err(ActiveReblitCandidatePreserveEffectError::WrongPhase);
        }
        let previous_state = record
            .previous
            .id
            .ok_or(ActiveReblitCandidatePreserveEffectError::PreviousStateMissing)?;
        let fingerprint = snapshot.fingerprint();
        let candidate = exact_tree_for_token(fingerprint, record.candidate.tree_token.as_str())?;
        if candidate.marker.links != 1 {
            return Err(ActiveReblitCandidatePreserveEffectError::CandidateMarkerLinks {
                actual: candidate.marker.links,
            });
        }
        let staging = exact_wrapper(
            &fingerprint.roots_entries,
            |wrapper| wrapper.role == TreeLocation::Staging,
            "staging",
        )?;
        let target = exact_wrapper(
            &fingerprint.quarantine_entries,
            |wrapper| {
                matches!(
                    wrapper.role,
                    TreeLocation::ActiveReblitWrapper { state, .. } if state == previous_state
                )
            },
            "active-reblit reservation",
        )?;
        let TreeLocation::ActiveReblitWrapper {
            index: wrapper_index, ..
        } = target.role
        else {
            unreachable!("target was selected by exact ActiveReblit role")
        };
        let target_name = expected_target_name(record, wrapper_index);
        if target.name != target_name {
            return Err(ActiveReblitCandidatePreserveEffectError::WrongTargetName);
        }

        let (layout, candidate_wrapper, reservation_wrapper) = if candidate.location == TreeLocation::Staging
            && wrapper_contains_only(staging, candidate)
            && wrapper_is_empty(target)
            && target.has_exact_private_permissions()
        {
            (ActiveReblitCandidatePreserveLayout::Staged, staging, target)
        } else if candidate.location
            == (TreeLocation::ActiveReblitWrapper {
                state: previous_state,
                index: wrapper_index,
            })
            && wrapper_contains_only(target, candidate)
            && wrapper_is_empty(staging)
            && staging.has_exact_private_permissions()
        {
            (ActiveReblitCandidatePreserveLayout::Preserved, target, staging)
        } else {
            return Err(ActiveReblitCandidatePreserveEffectError::NotExchangeLayout);
        };

        require_same_device(&[
            fingerprint.root,
            fingerprint.roots,
            fingerprint.quarantine,
            candidate_wrapper.witness,
            reservation_wrapper.witness,
            candidate.directory,
        ])?;
        let other_root_wrappers = fingerprint
            .roots_entries
            .iter()
            .filter(|wrapper| wrapper.role != TreeLocation::Staging)
            .cloned()
            .collect();
        let other_quarantine_wrappers = fingerprint
            .quarantine_entries
            .iter()
            .filter(|wrapper| wrapper.name != target_name)
            .cloned()
            .collect();
        Ok(Self {
            layout,
            wrapper_index,
            target_name,
            invariant: ActiveReblitCandidatePreserveInvariant {
                root: fingerprint.root,
                roots: fingerprint.roots.into(),
                quarantine: fingerprint.quarantine.into(),
                epoch: fingerprint.epoch.clone(),
                live: fingerprint.live.clone(),
                root_abi: fingerprint.root_abi.clone(),
                isolation_abi: fingerprint.isolation_abi.clone(),
                candidate: candidate.into(),
                candidate_wrapper: candidate_wrapper.witness.into(),
                reservation_wrapper: reservation_wrapper.witness.into(),
                other_root_wrappers,
                other_quarantine_wrappers,
            },
        })
    }

    fn require_staged_to_preserved(&self, after: &Self) -> Result<(), ActiveReblitCandidatePreserveEffectError> {
        if self.layout != ActiveReblitCandidatePreserveLayout::Staged
            || after.layout != ActiveReblitCandidatePreserveLayout::Preserved
        {
            return Err(ActiveReblitCandidatePreserveEffectError::NotStagedToPreserved {
                before: self.layout,
                after: after.layout,
            });
        }
        if self.wrapper_index != after.wrapper_index
            || self.target_name != after.target_name
            || self.invariant != after.invariant
        {
            return Err(ActiveReblitCandidatePreserveEffectError::InvariantChanged);
        }
        Ok(())
    }
}

fn exact_tree_for_token<'a>(
    fingerprint: &'a NamespaceFingerprint,
    token: &str,
) -> Result<&'a UsrFingerprint, ActiveReblitCandidatePreserveEffectError> {
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
        _ => Err(ActiveReblitCandidatePreserveEffectError::CandidateCount { actual: matches.len() }),
    }
}

fn exact_wrapper<'a>(
    wrappers: &'a [WrapperFingerprint],
    predicate: impl Fn(&WrapperFingerprint) -> bool,
    role: &'static str,
) -> Result<&'a WrapperFingerprint, ActiveReblitCandidatePreserveEffectError> {
    let matches = wrappers.iter().filter(|wrapper| predicate(wrapper)).collect::<Vec<_>>();
    match matches.as_slice() {
        [wrapper] => Ok(*wrapper),
        _ => Err(ActiveReblitCandidatePreserveEffectError::WrapperCount {
            role,
            actual: matches.len(),
        }),
    }
}

fn exact_retained_wrapper(
    wrappers: &[super::RetainedWrapper],
    predicate: impl Fn(&super::RetainedWrapper) -> bool,
) -> Result<&super::RetainedWrapper, ActiveReblitCandidatePreserveEffectError> {
    let matches = wrappers.iter().filter(|wrapper| predicate(wrapper)).collect::<Vec<_>>();
    match matches.as_slice() {
        [wrapper] => Ok(*wrapper),
        _ => Err(ActiveReblitCandidatePreserveEffectError::RetainedWrapperCount { actual: matches.len() }),
    }
}

fn exact_retained_candidate<'a>(
    snapshot: &'a NamespaceSnapshot,
    token: &str,
) -> Result<&'a super::RetainedUsr, ActiveReblitCandidatePreserveEffectError> {
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
        _ => Err(ActiveReblitCandidatePreserveEffectError::CandidateCount { actual: matches.len() }),
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

fn expected_target_name(record: &TransitionRecord, index: usize) -> Vec<u8> {
    format!(
        "replaced-active-reblit-wrapper-{}-{}-{index}",
        record.previous.id.expect("projection checked previous state"),
        record.previous.tree_token.as_str()
    )
    .into_bytes()
}

fn require_same_device(witnesses: &[InodeWitness]) -> Result<(), ActiveReblitCandidatePreserveEffectError> {
    let expected = witnesses
        .first()
        .map(|witness| witness.device)
        .ok_or(ActiveReblitCandidatePreserveEffectError::CrossDevice)?;
    if witnesses.iter().all(|witness| witness.device == expected) {
        Ok(())
    } else {
        Err(ActiveReblitCandidatePreserveEffectError::CrossDevice)
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

fn require_parent_identity(
    actual: InodeWitness,
    expected: MutableParentIdentity,
    path: &std::path::Path,
) -> Result<(), CaptureError> {
    if MutableParentIdentity::from(actual) == expected {
        Ok(())
    } else {
        Err(CaptureError::InodeChanged { path: path.to_owned() })
    }
}

fn require_wrapper_identity(
    actual: InodeWitness,
    expected: ExchangedWrapperIdentity,
    path: &std::path::Path,
) -> Result<(), CaptureError> {
    if ExchangedWrapperIdentity::from(actual) == expected {
        Ok(())
    } else {
        Err(CaptureError::InodeChanged { path: path.to_owned() })
    }
}

fn sync_directory(file: &File, path: &std::path::Path, operation: &'static str) -> Result<(), CaptureError> {
    file.sync_all().map_err(|source| CaptureError::Io {
        operation,
        path: path.to_owned(),
        source,
    })
}

fn os_name(bytes: &[u8]) -> &std::ffi::OsStr {
    use std::os::unix::ffi::OsStrExt as _;
    std::ffi::OsStr::from_bytes(bytes)
}

#[derive(Debug, thiserror::Error)]
pub(in crate::client::startup_reconciliation::activation_namespace) enum ActiveReblitCandidatePreserveEffectError {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error("revalidate the retained mutable installation namespace around ActiveReblit preservation")]
    Installation(#[from] crate::installation::Error),
    #[error("ActiveReblit wrapper preservation requires ActiveReblit")]
    WrongOperation,
    #[error("ActiveReblit wrapper preservation requires CandidatePreserveIntent")]
    WrongPhase,
    #[error("ActiveReblit wrapper preservation requires the previous state ID")]
    PreviousStateMissing,
    #[error("the ActiveReblit candidate token occurs at {actual} namespace locations")]
    CandidateCount { actual: usize },
    #[error("the ActiveReblit candidate marker has {actual} links instead of one")]
    CandidateMarkerLinks { actual: u64 },
    #[error("the exact {role} wrapper occurs {actual} times")]
    WrapperCount { role: &'static str, actual: usize },
    #[error("the retained ActiveReblit exchange wrapper occurs {actual} times")]
    RetainedWrapperCount { actual: usize },
    #[error("the ActiveReblit reservation does not have its exact journal-derived fixed name")]
    WrongTargetName,
    #[error("the namespace is not exact staged or preserved ActiveReblit wrapper evidence")]
    NotExchangeLayout,
    #[error("ActiveReblit wrapper preservation is not staged-to-preserved ({before:?} -> {after:?})")]
    NotStagedToPreserved {
        before: ActiveReblitCandidatePreserveLayout,
        after: ActiveReblitCandidatePreserveLayout,
    },
    #[error("ActiveReblit wrapper preservation evidence changed beyond the exact wrapper exchange")]
    InvariantChanged,
    #[error("ActiveReblit wrapper exchange descriptors are not on one device")]
    CrossDevice,
    #[error("authenticated ActiveReblit wrapper evidence is no longer exact PRE")]
    PreEvidenceChanged,
    #[error("the final fresh ActiveReblit PRE namespace changed")]
    FinalNamespaceChanged,
    #[error("the final fresh ActiveReblit PRE projection changed")]
    FinalProjectionChanged,
}
