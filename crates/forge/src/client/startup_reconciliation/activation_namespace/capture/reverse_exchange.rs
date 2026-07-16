//! Normalized namespace evidence for one retained reverse `/usr` exchange.
//!
//! The projection in this module remains mutation-free: it maps candidate and
//! previous trees by permanent token, defines the sole metadata delta allowed
//! by a POST-to-PRE exchange, and retains opaque parent descriptors for later
//! value-only identity checks. The private [`effect`] child owns the single
//! consuming syscall boundary without exposing descriptors or interpreting a
//! raw syscall report as the namespace outcome.

mod effect;

use std::{fs::File, path::PathBuf};

use crate::{
    Installation,
    transition_journal::{RuntimeEpoch, RuntimeTreeIdentity, TransitionRecord},
};

use super::super::policy::UsrExchangeLayout;
#[cfg(test)]
use super::RootAbiLinkFingerprint;
use super::{
    Budget, CaptureError, InodeWitness, NamespaceFingerprint, NamespaceSnapshot, RootAbiFingerprint,
    StateIdFingerprint, TreeLocation, UsrFingerprint, WrapperFingerprint, controlled_directory_witness, open_directory,
};

#[cfg(test)]
pub(in crate::client) use effect::arm_before_reverse_exchange_reconciliation_capture;
pub(in crate::client::startup_reconciliation::activation_namespace) use effect::{
    AppliedReverseExchangeReconciliation, PendingReverseExchangeReconciliation, ReverseExchangeReconciliation,
};

/// Stable inode fields which a retained exchange must never change.
///
/// Parent mtime/ctime are deliberately absent. Exchanging their fixed `usr`
/// children changes those timestamps without changing parent identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ExchangeStableInode {
    device: u64,
    inode: u64,
    mode: u32,
    owner: u32,
    group: u32,
    links: u64,
    length: u64,
}

impl From<InodeWitness> for ExchangeStableInode {
    fn from(witness: InodeWitness) -> Self {
        Self {
            device: witness.device,
            inode: witness.inode,
            mode: witness.mode,
            owner: witness.owner,
            group: witness.group,
            links: witness.links,
            length: witness.length,
        }
    }
}

/// Stable moved-tree fields. A cross-parent rename may change the moved
/// directory's ctime, but it does not modify that directory's contents, so
/// mtime remains part of the invariant.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MovedUsrInode {
    stable: ExchangeStableInode,
    modified_seconds: i64,
    modified_nanoseconds: i64,
}

impl From<InodeWitness> for MovedUsrInode {
    fn from(witness: InodeWitness) -> Self {
        Self {
            stable: witness.into(),
            modified_seconds: witness.modified_seconds,
            modified_nanoseconds: witness.modified_nanoseconds,
        }
    }
}

/// One logical tree with pathname and rename-only ctime removed.
#[derive(Clone, Debug, Eq, PartialEq)]
struct SemanticUsrFingerprint {
    token: String,
    directory: MovedUsrInode,
    marker: InodeWitness,
    state_id: StateIdFingerprint,
    runtime: RuntimeTreeIdentity,
}

impl From<&UsrFingerprint> for SemanticUsrFingerprint {
    fn from(tree: &UsrFingerprint) -> Self {
        Self {
            token: tree.token.clone(),
            directory: tree.directory.into(),
            marker: tree.marker,
            state_id: tree.state_id.clone(),
            runtime: tree.runtime,
        }
    }
}

/// Every namespace fact which must remain invariant across POST-to-PRE.
#[derive(Clone, Debug, Eq, PartialEq)]
struct ReverseExchangeProjection {
    root_parent: ExchangeStableInode,
    staging_parent: ExchangeStableInode,
    roots: InodeWitness,
    quarantine: InodeWitness,
    epoch: RuntimeEpoch,
    candidate: SemanticUsrFingerprint,
    previous: SemanticUsrFingerprint,
    root_abi: RootAbiFingerprint,
    isolation_abi: RootAbiFingerprint,
    other_root_wrappers: Vec<WrapperFingerprint>,
    quarantine_wrappers: Vec<WrapperFingerprint>,
}

/// Layout plus its pathname-independent reverse-exchange invariant.
#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(dead_code)] // consumed by the later reverse-effect checkpoint
pub(in crate::client::startup_reconciliation::activation_namespace) struct ProjectedReverseNamespace {
    layout: UsrExchangeLayout,
    invariant: ReverseExchangeProjection,
}

impl ProjectedReverseNamespace {
    /// Project an authenticated snapshot by the record's permanent tree
    /// tokens. No current pathname is accepted as logical identity.
    #[allow(dead_code)] // consumed by the later reverse-effect checkpoint
    pub(in crate::client::startup_reconciliation::activation_namespace) fn capture(
        snapshot: &NamespaceSnapshot,
        record: &TransitionRecord,
    ) -> Result<Self, ReverseExchangeCaptureError> {
        Self::from_fingerprint(
            snapshot.fingerprint(),
            record.candidate.tree_token.as_str(),
            record.previous.tree_token.as_str(),
        )
    }

    fn from_fingerprint(
        fingerprint: &NamespaceFingerprint,
        candidate_token: &str,
        previous_token: &str,
    ) -> Result<Self, ReverseExchangeCaptureError> {
        if candidate_token == previous_token {
            return Err(ReverseExchangeCaptureError::TreeTokensEqual);
        }
        let candidate = exact_tree_for_token(fingerprint, candidate_token, "candidate")?;
        let previous = exact_tree_for_token(fingerprint, previous_token, "previous")?;
        let staging = exact_staging_wrapper(fingerprint)?;
        let staging_usr = exact_staging_usr(staging)?;
        if fingerprint.live.location != TreeLocation::Live || staging_usr.location != TreeLocation::Staging {
            return Err(ReverseExchangeCaptureError::InvalidFixedLocations);
        }

        let layout = match (fingerprint.live.token.as_str(), staging_usr.token.as_str()) {
            (live, staged) if live == candidate_token && staged == previous_token => UsrExchangeLayout::Post,
            (live, staged) if live == previous_token && staged == candidate_token => UsrExchangeLayout::Pre,
            _ => return Err(ReverseExchangeCaptureError::NotPreOrPost),
        };
        let other_root_wrappers = fingerprint
            .roots_entries
            .iter()
            .filter(|wrapper| wrapper.name != b"staging")
            .cloned()
            .collect();
        Ok(Self {
            layout,
            invariant: ReverseExchangeProjection {
                root_parent: fingerprint.root.into(),
                staging_parent: staging.witness.into(),
                roots: fingerprint.roots,
                quarantine: fingerprint.quarantine,
                epoch: fingerprint.epoch.clone(),
                candidate: candidate.into(),
                previous: previous.into(),
                root_abi: fingerprint.root_abi.clone(),
                isolation_abi: fingerprint.isolation_abi.clone(),
                other_root_wrappers,
                quarantine_wrappers: fingerprint.quarantine_entries.clone(),
            },
        })
    }

    #[allow(dead_code)] // consumed by the later reverse-effect checkpoint
    pub(in crate::client::startup_reconciliation::activation_namespace) fn layout(&self) -> UsrExchangeLayout {
        self.layout
    }

    /// Require exactly one semantic exchange and no other namespace delta.
    #[allow(dead_code)] // consumed by the later reverse-effect checkpoint
    pub(in crate::client::startup_reconciliation::activation_namespace) fn require_post_to_pre(
        &self,
        after: &Self,
    ) -> Result<(), ReverseExchangeCaptureError> {
        if self.layout != UsrExchangeLayout::Post || after.layout != UsrExchangeLayout::Pre {
            return Err(ReverseExchangeCaptureError::NotPostToPre {
                before: self.layout,
                after: after.layout,
            });
        }
        if self.invariant != after.invariant {
            return Err(ReverseExchangeCaptureError::InvariantChanged);
        }
        Ok(())
    }
}

fn exact_tree_for_token<'a>(
    fingerprint: &'a NamespaceFingerprint,
    token: &str,
    role: &'static str,
) -> Result<&'a UsrFingerprint, ReverseExchangeCaptureError> {
    let matches = trees(fingerprint)
        .filter(|tree| tree.token == token)
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [tree] => Ok(*tree),
        _ => Err(ReverseExchangeCaptureError::TreeCount {
            role,
            actual: matches.len(),
        }),
    }
}

fn trees(fingerprint: &NamespaceFingerprint) -> impl Iterator<Item = &UsrFingerprint> {
    std::iter::once(&fingerprint.live).chain(
        fingerprint
            .roots_entries
            .iter()
            .chain(&fingerprint.quarantine_entries)
            .filter_map(|wrapper| wrapper.usr.as_ref()),
    )
}

fn exact_staging_wrapper(
    fingerprint: &NamespaceFingerprint,
) -> Result<&WrapperFingerprint, ReverseExchangeCaptureError> {
    let matches = fingerprint
        .roots_entries
        .iter()
        .filter(|wrapper| wrapper.name == b"staging")
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [wrapper] if wrapper.role == TreeLocation::Staging => Ok(*wrapper),
        [_] => Err(ReverseExchangeCaptureError::InvalidStagingRole),
        _ => Err(ReverseExchangeCaptureError::StagingCount { actual: matches.len() }),
    }
}

fn exact_staging_usr(staging: &WrapperFingerprint) -> Result<&UsrFingerprint, ReverseExchangeCaptureError> {
    let Some(usr) = staging.usr.as_ref() else {
        return Err(ReverseExchangeCaptureError::InvalidStagingShape);
    };
    let [(name, witness)] = staging.entries.as_slice() else {
        return Err(ReverseExchangeCaptureError::InvalidStagingShape);
    };
    if name != b"usr" || *witness != usr.directory || staging.slot.is_some() {
        return Err(ReverseExchangeCaptureError::InvalidStagingShape);
    }
    Ok(usr)
}

/// Value-only identities for the two parents of the fixed-name exchange.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(dead_code)] // consumed by the later reverse-effect checkpoint
pub(in crate::client::startup_reconciliation::activation_namespace) struct ReverseExchangeParentIdentity {
    root: ExchangeStableInode,
    staging: ExchangeStableInode,
}

impl ReverseExchangeParentIdentity {
    fn from_witnesses(root: InodeWitness, staging: InodeWitness) -> Result<Self, ReverseExchangeCaptureError> {
        let root = ExchangeStableInode::from(root);
        let staging = ExchangeStableInode::from(staging);
        if root.device != staging.device {
            return Err(ReverseExchangeCaptureError::ParentsCrossDevice {
                root: root.device,
                staging: staging.device,
            });
        }
        Ok(Self { root, staging })
    }

    fn require_rebound(self, root: InodeWitness, staging: InodeWitness) -> Result<Self, ReverseExchangeCaptureError> {
        let actual = Self::from_witnesses(root, staging)?;
        if self == actual {
            Ok(actual)
        } else {
            Err(ReverseExchangeCaptureError::ParentIdentityChanged)
        }
    }
}

/// Opaque retained descriptors for the two exchange parents.
///
/// There is intentionally no raw descriptor getter, descriptor conversion,
/// syscall, sync, callback, or transparent dereference implementation. This
/// foundation can only report and revalidate value identity.
#[derive(Debug)]
#[allow(dead_code)] // consumed by the later reverse-effect checkpoint
pub(in crate::client::startup_reconciliation::activation_namespace) struct RetainedReverseExchangeParents {
    root: File,
    roots: File,
    staging: File,
    root_path: PathBuf,
    roots_path: PathBuf,
    staging_path: PathBuf,
    roots_witness: InodeWitness,
    identity: ReverseExchangeParentIdentity,
}

impl RetainedReverseExchangeParents {
    #[allow(dead_code)] // consumed by the later reverse-effect checkpoint
    pub(in crate::client::startup_reconciliation::activation_namespace) fn capture(
        snapshot: &NamespaceSnapshot,
        record: &TransitionRecord,
    ) -> Result<Self, ReverseExchangeCaptureError> {
        let _projection = ProjectedReverseNamespace::capture(snapshot, record)?;
        let staging = retained_staging_wrapper(snapshot)?;
        let staging_path = snapshot.roots_path.join("staging");
        let identity =
            ReverseExchangeParentIdentity::from_witnesses(snapshot.fingerprint.root, staging.fingerprint.witness)?;
        Ok(Self {
            root: clone_descriptor(&snapshot.root, &snapshot.root_path, "clone retained installation root")?,
            roots: clone_descriptor(&snapshot.roots, &snapshot.roots_path, "clone retained `.cast/root`")?,
            staging: clone_descriptor(&staging.directory, &staging_path, "clone retained staging parent")?,
            root_path: snapshot.root_path.clone(),
            roots_path: snapshot.roots_path.clone(),
            staging_path,
            roots_witness: snapshot.fingerprint.roots,
            identity,
        })
    }

    /// Return only copyable identity values; descriptors never cross this
    /// boundary.
    #[allow(dead_code)] // consumed by the later reverse-effect checkpoint
    pub(in crate::client::startup_reconciliation::activation_namespace) fn identity(
        &self,
    ) -> ReverseExchangeParentIdentity {
        self.identity
    }

    /// Rebind both retained parents beneath authenticated descriptors while
    /// comparing only fields which an exchange cannot legitimately change.
    /// This proves identity, not permission to mutate.
    #[allow(dead_code)] // consumed by the later reverse-effect checkpoint
    pub(in crate::client::startup_reconciliation::activation_namespace) fn revalidate_value_identity(
        &self,
        installation: &Installation,
    ) -> Result<ReverseExchangeParentIdentity, ReverseExchangeCaptureError> {
        installation
            .revalidate_mutable_namespace()
            .map_err(CaptureError::Installation)?;
        let mut budget = Budget::new()?;

        let retained_root = controlled_directory_witness(&self.root, &self.root_path)?;
        let installation_root = controlled_directory_witness(installation.root_directory(), &self.root_path)?;
        let named_root = open_directory(installation.root_directory(), c".", &self.root_path, &mut budget)?;
        let named_root = controlled_directory_witness(&named_root, &self.root_path)?;
        self.identity
            .require_rebound(retained_root, staging_witness(&self.staging, &self.staging_path)?)?;
        self.identity
            .require_rebound(installation_root, staging_witness(&self.staging, &self.staging_path)?)?;
        self.identity
            .require_rebound(named_root, staging_witness(&self.staging, &self.staging_path)?)?;

        let retained_roots = controlled_directory_witness(&self.roots, &self.roots_path)?;
        require_exact_witness(retained_roots, self.roots_witness, &self.roots_path)?;
        let named_roots = open_directory(&self.root, c".cast/root", &self.roots_path, &mut budget)?;
        require_exact_witness(
            controlled_directory_witness(&named_roots, &self.roots_path)?,
            self.roots_witness,
            &self.roots_path,
        )?;

        let retained_staging = staging_witness(&self.staging, &self.staging_path)?;
        let named_staging = open_directory(&self.roots, c"staging", &self.staging_path, &mut budget)?;
        let named_staging = staging_witness(&named_staging, &self.staging_path)?;
        let actual = self.identity.require_rebound(retained_root, retained_staging)?;
        self.identity.require_rebound(retained_root, named_staging)?;

        installation
            .revalidate_mutable_namespace()
            .map_err(CaptureError::Installation)?;
        Ok(actual)
    }
}

fn retained_staging_wrapper(
    snapshot: &NamespaceSnapshot,
) -> Result<&super::RetainedWrapper, ReverseExchangeCaptureError> {
    let matches = snapshot
        .roots_entries
        .iter()
        .filter(|wrapper| wrapper.fingerprint.name == b"staging")
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [wrapper] if wrapper.fingerprint.role == TreeLocation::Staging => Ok(*wrapper),
        [_] => Err(ReverseExchangeCaptureError::InvalidStagingRole),
        _ => Err(ReverseExchangeCaptureError::StagingCount { actual: matches.len() }),
    }
}

fn clone_descriptor(file: &File, path: &std::path::Path, operation: &'static str) -> Result<File, CaptureError> {
    file.try_clone().map_err(|source| CaptureError::Io {
        operation,
        path: path.to_owned(),
        source,
    })
}

fn staging_witness(file: &File, path: &std::path::Path) -> Result<InodeWitness, CaptureError> {
    controlled_directory_witness(file, path)
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
#[allow(dead_code)] // consumed by the later reverse-effect checkpoint
pub(in crate::client::startup_reconciliation::activation_namespace) enum ReverseExchangeCaptureError {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error("candidate and previous permanent tree tokens are equal")]
    TreeTokensEqual,
    #[error("reverse-exchange {role} token occurs at {actual} locations")]
    TreeCount { role: &'static str, actual: usize },
    #[error("fixed staging wrapper occurs {actual} times")]
    StagingCount { actual: usize },
    #[error("fixed staging wrapper has the wrong semantic role")]
    InvalidStagingRole,
    #[error("fixed staging wrapper is not exactly one retained `usr` child")]
    InvalidStagingShape,
    #[error("live or staging tree has the wrong fixed location")]
    InvalidFixedLocations,
    #[error("candidate and previous trees are not in an exact PRE or POST layout")]
    NotPreOrPost,
    #[error("reverse-exchange comparison is not POST-to-PRE ({before:?} -> {after:?})")]
    NotPostToPre {
        before: UsrExchangeLayout,
        after: UsrExchangeLayout,
    },
    #[error("namespace changed beyond the exact normalized reverse exchange")]
    InvariantChanged,
    #[error("retained exchange parents cross devices ({root} != {staging})")]
    ParentsCrossDevice { root: u64, staging: u64 },
    #[error("retained reverse-exchange parent identity changed")]
    ParentIdentityChanged,
}

#[cfg(test)]
mod tests;
