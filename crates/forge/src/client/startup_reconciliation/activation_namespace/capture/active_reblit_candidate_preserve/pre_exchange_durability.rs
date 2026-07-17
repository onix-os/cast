//! Descriptor rebinding and ordered PRE durability for one wrapper exchange.

use std::{ffi::CString, fs::File, path::PathBuf};

use crate::{Installation, transition_journal::TransitionRecord, tree_marker::TreeMarkerStore};

use super::super::{
    Budget, CaptureError, InodeWitness, NamespaceSnapshot, TreeLocation, capture_snapshot,
    controlled_directory_witness, open_directory,
};
use super::{
    ActiveReblitCandidatePreserveEffectError, ActiveReblitCandidatePreserveLayout, ExchangedWrapperIdentity,
    MutableParentIdentity, ProjectedActiveReblitCandidatePreserveNamespace, clone_descriptor, exact_retained_candidate,
    exact_retained_wrapper, os_name, require_exact_witness, require_parent_identity, require_wrapper_identity,
    sync_directory,
};

/// Opaque descriptors for both exchange parents, wrappers, and candidate.
#[derive(Debug)]
pub(in crate::client::startup_reconciliation::activation_namespace) struct RetainedActiveReblitCandidatePreserveParents
{
    pub(super) root: File,
    pub(super) roots: File,
    pub(super) quarantine: File,
    candidate_wrapper: File,
    reservation_wrapper: File,
    candidate: TreeMarkerStore,
    pub(super) target_name: CString,
    root_path: PathBuf,
    roots_path: PathBuf,
    quarantine_path: PathBuf,
    candidate_wrapper_path: PathBuf,
    reservation_wrapper_path: PathBuf,
    candidate_path: PathBuf,
    root_witness: InodeWitness,
    roots_identity: MutableParentIdentity,
    quarantine_identity: MutableParentIdentity,
    candidate_wrapper_identity: ExchangedWrapperIdentity,
    reservation_wrapper_identity: ExchangedWrapperIdentity,
    candidate_witness: InodeWitness,
}

impl RetainedActiveReblitCandidatePreserveParents {
    pub(in crate::client::startup_reconciliation::activation_namespace) fn capture(
        snapshot: &NamespaceSnapshot,
        record: &TransitionRecord,
        expected_layout: ActiveReblitCandidatePreserveLayout,
    ) -> Result<Self, ActiveReblitCandidatePreserveEffectError> {
        let projection = ProjectedActiveReblitCandidatePreserveNamespace::capture(snapshot, record)?;
        if projection.layout != expected_layout {
            return Err(ActiveReblitCandidatePreserveEffectError::NotExchangeLayout);
        }
        let staging = exact_retained_wrapper(&snapshot.roots_entries, |wrapper| {
            wrapper.fingerprint.role == TreeLocation::Staging
        })?;
        let target = exact_retained_wrapper(&snapshot.quarantine_entries, |wrapper| {
            wrapper.fingerprint.name == projection.target_name
        })?;
        let (candidate_wrapper, reservation_wrapper, candidate_wrapper_path, reservation_wrapper_path) =
            match expected_layout {
                ActiveReblitCandidatePreserveLayout::Staged => (
                    staging,
                    target,
                    snapshot.roots_path.join("staging"),
                    snapshot.quarantine_path.join(os_name(&projection.target_name)),
                ),
                ActiveReblitCandidatePreserveLayout::Preserved => (
                    target,
                    staging,
                    snapshot.quarantine_path.join(os_name(&projection.target_name)),
                    snapshot.roots_path.join("staging"),
                ),
            };
        let candidate = exact_retained_candidate(snapshot, record.candidate.tree_token.as_str())?;
        let candidate_path = candidate.store.display_path().to_owned();
        let candidate_store = TreeMarkerStore::open(candidate.store.retained_directory(), candidate_path.clone())
            .map_err(CaptureError::TreeMarker)?;
        Ok(Self {
            root: clone_descriptor(&snapshot.root, &snapshot.root_path, "clone retained installation root")?,
            roots: clone_descriptor(&snapshot.roots, &snapshot.roots_path, "clone retained `.cast/root`")?,
            quarantine: clone_descriptor(
                &snapshot.quarantine,
                &snapshot.quarantine_path,
                "clone retained `.cast/quarantine`",
            )?,
            candidate_wrapper: clone_descriptor(
                &candidate_wrapper.directory,
                &candidate_wrapper_path,
                "clone retained ActiveReblit candidate wrapper",
            )?,
            reservation_wrapper: clone_descriptor(
                &reservation_wrapper.directory,
                &reservation_wrapper_path,
                "clone retained ActiveReblit reservation wrapper",
            )?,
            candidate: candidate_store,
            target_name: CString::new(projection.target_name.clone())
                .map_err(|_| ActiveReblitCandidatePreserveEffectError::WrongTargetName)?,
            root_path: snapshot.root_path.clone(),
            roots_path: snapshot.roots_path.clone(),
            quarantine_path: snapshot.quarantine_path.clone(),
            candidate_wrapper_path,
            reservation_wrapper_path,
            candidate_path,
            root_witness: snapshot.fingerprint.root,
            roots_identity: snapshot.fingerprint.roots.into(),
            quarantine_identity: snapshot.fingerprint.quarantine.into(),
            candidate_wrapper_identity: candidate_wrapper.fingerprint.witness.into(),
            reservation_wrapper_identity: reservation_wrapper.fingerprint.witness.into(),
            candidate_witness: candidate.fingerprint.directory,
        })
    }

    pub(in crate::client::startup_reconciliation::activation_namespace) fn revalidate_layout(
        &self,
        installation: &Installation,
        expected_layout: ActiveReblitCandidatePreserveLayout,
    ) -> Result<(), ActiveReblitCandidatePreserveEffectError> {
        installation.revalidate_mutable_namespace()?;
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
        require_parent_identity(
            controlled_directory_witness(&self.roots, &self.roots_path)?,
            self.roots_identity,
            &self.roots_path,
        )?;
        require_parent_identity(
            controlled_directory_witness(&self.quarantine, &self.quarantine_path)?,
            self.quarantine_identity,
            &self.quarantine_path,
        )?;
        let named_roots = open_directory(&self.root, c".cast/root", &self.roots_path, &mut budget)?;
        let named_quarantine = open_directory(&self.root, c".cast/quarantine", &self.quarantine_path, &mut budget)?;
        require_parent_identity(
            controlled_directory_witness(&named_roots, &self.roots_path)?,
            self.roots_identity,
            &self.roots_path,
        )?;
        require_parent_identity(
            controlled_directory_witness(&named_quarantine, &self.quarantine_path)?,
            self.quarantine_identity,
            &self.quarantine_path,
        )?;
        require_wrapper_identity(
            controlled_directory_witness(&self.candidate_wrapper, &self.candidate_wrapper_path)?,
            self.candidate_wrapper_identity,
            &self.candidate_wrapper_path,
        )?;
        require_wrapper_identity(
            controlled_directory_witness(&self.reservation_wrapper, &self.reservation_wrapper_path)?,
            self.reservation_wrapper_identity,
            &self.reservation_wrapper_path,
        )?;
        self.candidate
            .revalidate_directory()
            .map_err(CaptureError::TreeMarker)?;
        require_exact_witness(
            InodeWitness::read(self.candidate.retained_directory(), &self.candidate_path)?,
            self.candidate_witness,
            &self.candidate_path,
        )?;
        let retained_candidate = open_directory(&self.candidate_wrapper, c"usr", &self.candidate_path, &mut budget)?;
        require_exact_witness(
            InodeWitness::read(&retained_candidate, &self.candidate_path)?,
            self.candidate_witness,
            &self.candidate_path,
        )?;

        let named_staging = open_directory(&self.roots, c"staging", &self.roots_path.join("staging"), &mut budget)?;
        let named_target = open_directory(
            &self.quarantine,
            &self.target_name,
            &self.quarantine_path.join(os_name(self.target_name.as_bytes())),
            &mut budget,
        )?;
        let (named_candidate, named_reservation) = match expected_layout {
            ActiveReblitCandidatePreserveLayout::Staged => (&named_staging, &named_target),
            ActiveReblitCandidatePreserveLayout::Preserved => (&named_target, &named_staging),
        };
        require_wrapper_identity(
            controlled_directory_witness(named_candidate, &self.candidate_wrapper_path)?,
            self.candidate_wrapper_identity,
            &self.candidate_wrapper_path,
        )?;
        require_wrapper_identity(
            controlled_directory_witness(named_reservation, &self.reservation_wrapper_path)?,
            self.reservation_wrapper_identity,
            &self.reservation_wrapper_path,
        )?;
        let named_candidate_usr = open_directory(named_candidate, c"usr", &self.candidate_path, &mut budget)?;
        require_exact_witness(
            InodeWitness::read(&named_candidate_usr, &self.candidate_path)?,
            self.candidate_witness,
            &self.candidate_path,
        )?;
        installation.revalidate_mutable_namespace()?;
        Ok(())
    }

    /// Complete the ordered PRE durability barriers and retain one final PRE.
    pub(in crate::client::startup_reconciliation::activation_namespace) fn prepare_exchange(
        self,
        installation: &Installation,
        record: &TransitionRecord,
        authenticated_pre: NamespaceSnapshot,
        authenticated_projection: ProjectedActiveReblitCandidatePreserveNamespace,
    ) -> Result<PreparedActiveReblitCandidatePreserveExchange, ActiveReblitCandidatePreserveEffectError> {
        require_exact_pre(
            installation,
            record,
            &self,
            &authenticated_pre,
            &authenticated_projection,
        )?;
        self.candidate.sync_retained_tree().map_err(CaptureError::TreeMarker)?;
        require_exact_pre(
            installation,
            record,
            &self,
            &authenticated_pre,
            &authenticated_projection,
        )?;
        sync_directory(
            &self.candidate_wrapper,
            &self.candidate_wrapper_path,
            "sync ActiveReblit candidate wrapper",
        )?;
        require_exact_pre(
            installation,
            record,
            &self,
            &authenticated_pre,
            &authenticated_projection,
        )?;
        sync_directory(
            &self.reservation_wrapper,
            &self.reservation_wrapper_path,
            "sync ActiveReblit reservation wrapper",
        )?;
        require_exact_pre(
            installation,
            record,
            &self,
            &authenticated_pre,
            &authenticated_projection,
        )?;
        sync_directory(
            &self.roots,
            &self.roots_path,
            "sync `.cast/root` before ActiveReblit exchange",
        )?;
        require_exact_pre(
            installation,
            record,
            &self,
            &authenticated_pre,
            &authenticated_projection,
        )?;
        sync_directory(
            &self.quarantine,
            &self.quarantine_path,
            "sync `.cast/quarantine` before ActiveReblit exchange",
        )?;
        require_exact_pre(
            installation,
            record,
            &self,
            &authenticated_pre,
            &authenticated_projection,
        )?;
        let final_pre = capture_snapshot(installation, record)?;
        final_pre.revalidate_retained()?;
        if final_pre.fingerprint() != authenticated_pre.fingerprint() {
            return Err(ActiveReblitCandidatePreserveEffectError::FinalNamespaceChanged);
        }
        let final_projection = ProjectedActiveReblitCandidatePreserveNamespace::capture(&final_pre, record)?;
        if final_projection != authenticated_projection {
            return Err(ActiveReblitCandidatePreserveEffectError::FinalProjectionChanged);
        }
        require_exact_pre(installation, record, &self, &final_pre, &final_projection)?;
        Ok(PreparedActiveReblitCandidatePreserveExchange {
            parents: self,
            final_pre,
            final_projection,
        })
    }
}

/// Target-durable exact PRE which can make at most one exchange attempt.
#[must_use = "prepared ActiveReblit wrapper exchange must be consumed"]
pub(in crate::client::startup_reconciliation::activation_namespace) struct PreparedActiveReblitCandidatePreserveExchange
{
    pub(super) parents: RetainedActiveReblitCandidatePreserveParents,
    pub(super) final_pre: NamespaceSnapshot,
    pub(super) final_projection: ProjectedActiveReblitCandidatePreserveNamespace,
}

pub(super) fn require_exact_pre(
    installation: &Installation,
    record: &TransitionRecord,
    parents: &RetainedActiveReblitCandidatePreserveParents,
    snapshot: &NamespaceSnapshot,
    projection: &ProjectedActiveReblitCandidatePreserveNamespace,
) -> Result<(), ActiveReblitCandidatePreserveEffectError> {
    installation.revalidate_mutable_namespace()?;
    snapshot.revalidate_retained()?;
    if projection.layout != ActiveReblitCandidatePreserveLayout::Staged
        || ProjectedActiveReblitCandidatePreserveNamespace::capture(snapshot, record)? != *projection
    {
        return Err(ActiveReblitCandidatePreserveEffectError::PreEvidenceChanged);
    }
    parents.revalidate_layout(installation, ActiveReblitCandidatePreserveLayout::Staged)?;
    snapshot.revalidate_retained()?;
    installation.revalidate_mutable_namespace()?;
    Ok(())
}
