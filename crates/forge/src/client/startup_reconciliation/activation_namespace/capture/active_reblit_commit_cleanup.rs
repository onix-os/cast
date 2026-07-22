//! Exact retained projection for forward ActiveReblit commit cleanup.
//!
//! This module owns the descriptor-rich snapshot and exposes only layout and
//! whole-proof revalidation. It deliberately exposes no parent descriptor,
//! wrapper descriptor, rename, synchronization, or retry API. The immediately
//! following effect slice can add consuming methods here without granting a
//! generic namespace capability to callers.

use crate::transition_journal::{Operation, Phase, TransitionRecord};

use super::{
    CaptureError, InodeWitness, NamespaceFingerprint, NamespaceSnapshot, RootAbiFingerprint,
    StateIdFingerprint, TreeLocation, UsrFingerprint, WrapperFingerprint,
};

/// The only two exact namespace states admitted at `CommitDecided`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client::startup_reconciliation::activation_namespace) enum ActiveReblitCommitCleanupLayout {
    Apply,
    Finish,
}

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

/// Stable wrapper identity across a rename exchange, excluding ctime.
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
struct SemanticPreviousFingerprint {
    token: String,
    directory: InodeWitness,
    marker: InodeWitness,
    state_id: StateIdFingerprint,
    runtime: crate::transition_journal::RuntimeTreeIdentity,
}

impl From<&UsrFingerprint> for SemanticPreviousFingerprint {
    fn from(previous: &UsrFingerprint) -> Self {
        Self {
            token: previous.token.clone(),
            directory: previous.directory,
            marker: previous.marker,
            state_id: previous.state_id.clone(),
            runtime: previous.runtime,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CommitCleanupInvariant {
    root: InodeWitness,
    roots: MutableParentIdentity,
    quarantine: MutableParentIdentity,
    epoch: crate::transition_journal::RuntimeEpoch,
    live_candidate: UsrFingerprint,
    root_abi: RootAbiFingerprint,
    isolation_abi: RootAbiFingerprint,
    previous: SemanticPreviousFingerprint,
    previous_wrapper: ExchangedWrapperIdentity,
    replacement_wrapper: ExchangedWrapperIdentity,
    other_root_wrappers: Vec<WrapperFingerprint>,
    other_quarantine_wrappers: Vec<WrapperFingerprint>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ProjectedActiveReblitCommitCleanupNamespace {
    layout: ActiveReblitCommitCleanupLayout,
    wrapper_index: usize,
    target_name: Vec<u8>,
    invariant: CommitCleanupInvariant,
}

impl ProjectedActiveReblitCommitCleanupNamespace {
    fn capture(
        snapshot: &NamespaceSnapshot,
        record: &TransitionRecord,
    ) -> Result<Self, ActiveReblitCommitCleanupCaptureError> {
        if record.operation != Operation::ActiveReblit {
            return Err(ActiveReblitCommitCleanupCaptureError::WrongOperation);
        }
        if record.phase != Phase::CommitDecided || record.rollback.is_some() {
            return Err(ActiveReblitCommitCleanupCaptureError::WrongPhase);
        }
        let previous_state = record
            .previous
            .id
            .ok_or(ActiveReblitCommitCleanupCaptureError::PreviousStateMissing)?;
        let fingerprint = snapshot.fingerprint();
        let previous = exact_tree_for_token(fingerprint, record.previous.tree_token.as_str())?;
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
            "active-reblit replacement",
        )?;
        let TreeLocation::ActiveReblitWrapper {
            index: wrapper_index, ..
        } = target.role
        else {
            unreachable!("target was selected by exact ActiveReblit role")
        };
        let target_name = expected_target_name(record, wrapper_index);
        if target.name != target_name {
            return Err(ActiveReblitCommitCleanupCaptureError::WrongTargetName);
        }

        let (layout, previous_wrapper, replacement_wrapper) = if previous.location == TreeLocation::Staging
            && wrapper_contains_only(staging, previous)
            && wrapper_is_empty(target)
            && target.has_exact_private_permissions()
        {
            (ActiveReblitCommitCleanupLayout::Apply, staging, target)
        } else if previous.location
            == (TreeLocation::ActiveReblitWrapper {
                state: previous_state,
                index: wrapper_index,
            })
            && wrapper_contains_only(target, previous)
            && wrapper_is_empty(staging)
            && staging.has_exact_private_permissions()
        {
            (ActiveReblitCommitCleanupLayout::Finish, target, staging)
        } else {
            return Err(ActiveReblitCommitCleanupCaptureError::NotCleanupLayout);
        };

        require_same_device(&[
            fingerprint.root,
            fingerprint.roots,
            fingerprint.quarantine,
            previous_wrapper.witness,
            replacement_wrapper.witness,
            previous.directory,
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
            invariant: CommitCleanupInvariant {
                root: fingerprint.root,
                roots: fingerprint.roots.into(),
                quarantine: fingerprint.quarantine.into(),
                epoch: fingerprint.epoch.clone(),
                live_candidate: fingerprint.live.clone(),
                root_abi: fingerprint.root_abi.clone(),
                isolation_abi: fingerprint.isolation_abi.clone(),
                previous: previous.into(),
                previous_wrapper: previous_wrapper.witness.into(),
                replacement_wrapper: replacement_wrapper.witness.into(),
                other_root_wrappers,
                other_quarantine_wrappers,
            },
        })
    }
}

/// Opaque exact snapshot retained for the immediately following effect slice.
/// It owns every descriptor required to project either the one wrapper
/// exchange or the zero-exchange Finish suffix.
#[must_use = "retained ActiveReblit commit-cleanup evidence must remain sealed"]
#[derive(Debug)]
pub(in crate::client::startup_reconciliation::activation_namespace) struct RetainedActiveReblitCommitCleanupNamespace
{
    snapshot: NamespaceSnapshot,
    projection: ProjectedActiveReblitCommitCleanupNamespace,
}

impl RetainedActiveReblitCommitCleanupNamespace {
    pub(in crate::client::startup_reconciliation::activation_namespace) fn capture(
        snapshot: NamespaceSnapshot,
        record: &TransitionRecord,
    ) -> Result<Self, ActiveReblitCommitCleanupCaptureError> {
        let projection = ProjectedActiveReblitCommitCleanupNamespace::capture(&snapshot, record)?;
        snapshot.revalidate_retained()?;
        Ok(Self { snapshot, projection })
    }

    pub(in crate::client::startup_reconciliation::activation_namespace) fn layout(
        &self,
    ) -> ActiveReblitCommitCleanupLayout {
        self.projection.layout
    }

    pub(in crate::client::startup_reconciliation::activation_namespace) fn fingerprint(
        &self,
    ) -> &NamespaceFingerprint {
        self.snapshot.fingerprint()
    }

    pub(in crate::client::startup_reconciliation::activation_namespace) fn revalidate(
        &self,
        record: &TransitionRecord,
    ) -> Result<(), ActiveReblitCommitCleanupCaptureError> {
        self.snapshot.revalidate_retained()?;
        let actual = ProjectedActiveReblitCommitCleanupNamespace::capture(&self.snapshot, record)?;
        if actual == self.projection {
            Ok(())
        } else {
            Err(ActiveReblitCommitCleanupCaptureError::ProjectionChanged)
        }
    }
}

fn exact_tree_for_token<'a>(
    fingerprint: &'a NamespaceFingerprint,
    token: &str,
) -> Result<&'a UsrFingerprint, ActiveReblitCommitCleanupCaptureError> {
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
        [previous] => Ok(*previous),
        _ => Err(ActiveReblitCommitCleanupCaptureError::PreviousCount {
            actual: matches.len(),
        }),
    }
}

fn exact_wrapper<'a>(
    wrappers: &'a [WrapperFingerprint],
    predicate: impl Fn(&WrapperFingerprint) -> bool,
    role: &'static str,
) -> Result<&'a WrapperFingerprint, ActiveReblitCommitCleanupCaptureError> {
    let matches = wrappers.iter().filter(|wrapper| predicate(wrapper)).collect::<Vec<_>>();
    match matches.as_slice() {
        [wrapper] => Ok(*wrapper),
        _ => Err(ActiveReblitCommitCleanupCaptureError::WrapperCount {
            role,
            actual: matches.len(),
        }),
    }
}

fn wrapper_is_empty(wrapper: &WrapperFingerprint) -> bool {
    wrapper.entries.is_empty() && wrapper.usr.is_none() && wrapper.slot.is_none()
}

fn wrapper_contains_only(wrapper: &WrapperFingerprint, previous: &UsrFingerprint) -> bool {
    wrapper.slot.is_none()
        && wrapper.usr.as_ref() == Some(previous)
        && matches!(wrapper.entries.as_slice(), [(name, witness)] if name == b"usr" && *witness == previous.directory)
}

fn expected_target_name(record: &TransitionRecord, index: usize) -> Vec<u8> {
    format!(
        "replaced-active-reblit-wrapper-{}-{}-{index}",
        record.previous.id.expect("projection checked previous state"),
        record.previous.tree_token.as_str()
    )
    .into_bytes()
}

fn require_same_device(witnesses: &[InodeWitness]) -> Result<(), ActiveReblitCommitCleanupCaptureError> {
    let expected = witnesses
        .first()
        .map(|witness| witness.device)
        .ok_or(ActiveReblitCommitCleanupCaptureError::CrossDevice)?;
    if witnesses.iter().all(|witness| witness.device == expected) {
        Ok(())
    } else {
        Err(ActiveReblitCommitCleanupCaptureError::CrossDevice)
    }
}

#[derive(Debug, thiserror::Error)]
pub(in crate::client::startup_reconciliation) enum ActiveReblitCommitCleanupCaptureError {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error("ActiveReblit commit cleanup requires the ActiveReblit operation")]
    WrongOperation,
    #[error("ActiveReblit commit cleanup requires exact forward CommitDecided")]
    WrongPhase,
    #[error("ActiveReblit commit cleanup requires a previous state ID")]
    PreviousStateMissing,
    #[error("previous ActiveReblit token occurs at {actual} namespace locations")]
    PreviousCount { actual: usize },
    #[error("expected exactly one {role} wrapper, found {actual}")]
    WrapperCount { role: &'static str, actual: usize },
    #[error("ActiveReblit replacement wrapper has the wrong canonical name")]
    WrongTargetName,
    #[error("namespace is neither exact ActiveReblit cleanup Apply nor Finish")]
    NotCleanupLayout,
    #[error("ActiveReblit cleanup wrapper exchange would cross filesystems")]
    CrossDevice,
    #[error("retained ActiveReblit cleanup projection changed")]
    ProjectionChanged,
}
