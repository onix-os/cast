//! Descriptor rebinding and exact Apply safety before one cleanup exchange.

use std::{ffi::CString, fs::File, path::PathBuf};

use crate::{Installation, transition_journal::TransitionRecord, tree_marker::TreeMarkerStore};

use super::super::{
    Budget, CaptureError, InodeWitness, NamespaceSnapshot, TreeLocation, capture_snapshot,
    controlled_directory_witness, open_directory,
};
use super::{
    ActiveReblitCommitCleanupEffectError, ActiveReblitCommitCleanupLayout,
    ExchangedWrapperIdentity, MutableParentIdentity, ProjectedActiveReblitCommitCleanupNamespace,
    clone_descriptor, exact_retained_previous, exact_retained_wrapper, os_name,
    require_exact_witness, require_parent_identity, require_wrapper_identity,
};

/// Opaque descriptors for the two parents, both wrappers, and corrupt
/// previous tree. The descriptors survive the wrapper exchange.
#[derive(Debug)]
pub(in crate::client::startup_reconciliation::activation_namespace) struct RetainedActiveReblitCommitCleanupParents
{
    pub(super) root: File,
    pub(super) roots: File,
    pub(super) quarantine: File,
    pub(super) previous_wrapper: File,
    pub(super) replacement_wrapper: File,
    pub(super) previous: TreeMarkerStore,
    pub(super) target_name: CString,
    root_path: PathBuf,
    pub(super) roots_path: PathBuf,
    pub(super) quarantine_path: PathBuf,
    previous_wrapper_path: PathBuf,
    replacement_wrapper_path: PathBuf,
    previous_path: PathBuf,
    root_witness: InodeWitness,
    roots_identity: MutableParentIdentity,
    quarantine_identity: MutableParentIdentity,
    previous_wrapper_identity: ExchangedWrapperIdentity,
    replacement_wrapper_identity: ExchangedWrapperIdentity,
    previous_witness: InodeWitness,
}

impl RetainedActiveReblitCommitCleanupParents {
    pub(super) fn capture(
        snapshot: &NamespaceSnapshot,
        record: &TransitionRecord,
        expected_layout: ActiveReblitCommitCleanupLayout,
    ) -> Result<Self, ActiveReblitCommitCleanupEffectError> {
        let projection = ProjectedActiveReblitCommitCleanupNamespace::capture(snapshot, record)?;
        if projection.layout != expected_layout {
            return Err(layout_error(expected_layout));
        }
        let staging = exact_retained_wrapper(&snapshot.roots_entries, |wrapper| {
            wrapper.fingerprint.role == TreeLocation::Staging
        })?;
        let target = exact_retained_wrapper(&snapshot.quarantine_entries, |wrapper| {
            wrapper.fingerprint.name == projection.target_name
        })?;
        let (previous_wrapper, replacement_wrapper, previous_wrapper_path, replacement_wrapper_path) =
            match expected_layout {
                ActiveReblitCommitCleanupLayout::Apply => (
                    staging,
                    target,
                    snapshot.roots_path.join("staging"),
                    snapshot.quarantine_path.join(os_name(&projection.target_name)),
                ),
                ActiveReblitCommitCleanupLayout::Finish => (
                    target,
                    staging,
                    snapshot.quarantine_path.join(os_name(&projection.target_name)),
                    snapshot.roots_path.join("staging"),
                ),
            };
        let previous = exact_retained_previous(snapshot, record.previous.tree_token.as_str())?;
        let previous_path = previous.store.display_path().to_owned();
        let previous_store = TreeMarkerStore::open(
            previous.store.retained_directory(),
            previous_path.clone(),
        )
        .map_err(CaptureError::TreeMarker)?;
        Ok(Self {
            root: clone_descriptor(&snapshot.root, &snapshot.root_path, "clone retained installation root")?,
            roots: clone_descriptor(&snapshot.roots, &snapshot.roots_path, "clone retained `.cast/root`")?,
            quarantine: clone_descriptor(
                &snapshot.quarantine,
                &snapshot.quarantine_path,
                "clone retained `.cast/quarantine`",
            )?,
            previous_wrapper: clone_descriptor(
                &previous_wrapper.directory,
                &previous_wrapper_path,
                "clone retained ActiveReblit previous wrapper",
            )?,
            replacement_wrapper: clone_descriptor(
                &replacement_wrapper.directory,
                &replacement_wrapper_path,
                "clone retained ActiveReblit replacement wrapper",
            )?,
            previous: previous_store,
            target_name: CString::new(projection.target_name.clone())
                .map_err(|_| ActiveReblitCommitCleanupEffectError::FinalProjectionChanged)?,
            root_path: snapshot.root_path.clone(),
            roots_path: snapshot.roots_path.clone(),
            quarantine_path: snapshot.quarantine_path.clone(),
            previous_wrapper_path,
            replacement_wrapper_path,
            previous_path,
            root_witness: snapshot.fingerprint.root,
            roots_identity: snapshot.fingerprint.roots.into(),
            quarantine_identity: snapshot.fingerprint.quarantine.into(),
            previous_wrapper_identity: previous_wrapper.fingerprint.witness.into(),
            replacement_wrapper_identity: replacement_wrapper.fingerprint.witness.into(),
            previous_witness: previous.fingerprint.directory,
        })
    }

    pub(super) fn revalidate_layout(
        &self,
        installation: &Installation,
        expected_layout: ActiveReblitCommitCleanupLayout,
    ) -> Result<(), ActiveReblitCommitCleanupEffectError> {
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
        let named_quarantine = open_directory(
            &self.root,
            c".cast/quarantine",
            &self.quarantine_path,
            &mut budget,
        )?;
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
            controlled_directory_witness(&self.previous_wrapper, &self.previous_wrapper_path)?,
            self.previous_wrapper_identity,
            &self.previous_wrapper_path,
        )?;
        require_wrapper_identity(
            controlled_directory_witness(&self.replacement_wrapper, &self.replacement_wrapper_path)?,
            self.replacement_wrapper_identity,
            &self.replacement_wrapper_path,
        )?;
        self.previous
            .revalidate_directory()
            .map_err(CaptureError::TreeMarker)?;
        require_exact_witness(
            InodeWitness::read(self.previous.retained_directory(), &self.previous_path)?,
            self.previous_witness,
            &self.previous_path,
        )?;

        let named_staging = open_directory(
            &self.roots,
            c"staging",
            &self.roots_path.join("staging"),
            &mut budget,
        )?;
        let named_target = open_directory(
            &self.quarantine,
            &self.target_name,
            &self.quarantine_path.join(os_name(self.target_name.as_bytes())),
            &mut budget,
        )?;
        let (named_previous, named_replacement) = match expected_layout {
            ActiveReblitCommitCleanupLayout::Apply => (&named_staging, &named_target),
            ActiveReblitCommitCleanupLayout::Finish => (&named_target, &named_staging),
        };
        require_wrapper_identity(
            controlled_directory_witness(named_previous, &self.previous_wrapper_path)?,
            self.previous_wrapper_identity,
            &self.previous_wrapper_path,
        )?;
        require_wrapper_identity(
            controlled_directory_witness(named_replacement, &self.replacement_wrapper_path)?,
            self.replacement_wrapper_identity,
            &self.replacement_wrapper_path,
        )?;
        let named_previous_usr = open_directory(
            named_previous,
            c"usr",
            &self.previous_path,
            &mut budget,
        )?;
        require_exact_witness(
            InodeWitness::read(&named_previous_usr, &self.previous_path)?,
            self.previous_witness,
            &self.previous_path,
        )?;
        installation.revalidate_mutable_namespace()?;
        Ok(())
    }

    fn prepare_exchange(
        self,
        installation: &Installation,
        record: &TransitionRecord,
        authenticated_apply: NamespaceSnapshot,
        authenticated_projection: ProjectedActiveReblitCommitCleanupNamespace,
    ) -> Result<PreparedActiveReblitCommitCleanupExchange, ActiveReblitCommitCleanupEffectError> {
        require_exact_apply(
            installation,
            record,
            &self,
            &authenticated_apply,
            &authenticated_projection,
        )?;
        let final_apply = capture_snapshot(installation, record)?;
        final_apply.revalidate_retained()?;
        if final_apply.fingerprint() != authenticated_apply.fingerprint() {
            return Err(ActiveReblitCommitCleanupEffectError::FinalNamespaceChanged);
        }
        let final_projection = ProjectedActiveReblitCommitCleanupNamespace::capture(&final_apply, record)?;
        if final_projection != authenticated_projection {
            return Err(ActiveReblitCommitCleanupEffectError::FinalProjectionChanged);
        }
        require_exact_apply(
            installation,
            record,
            &self,
            &final_apply,
            &final_projection,
        )?;
        Ok(PreparedActiveReblitCommitCleanupExchange {
            parents: self,
            final_apply,
            final_projection,
        })
    }
}

impl super::RetainedActiveReblitCommitCleanupNamespace {
    pub(in crate::client::startup_reconciliation::activation_namespace) fn prepare_exchange(
        self,
        installation: &Installation,
        record: &TransitionRecord,
    ) -> Result<PreparedActiveReblitCommitCleanupExchange, ActiveReblitCommitCleanupEffectError> {
        self.revalidate(record)?;
        let parents = RetainedActiveReblitCommitCleanupParents::capture(
            &self.snapshot,
            record,
            ActiveReblitCommitCleanupLayout::Apply,
        )?;
        parents.prepare_exchange(installation, record, self.snapshot, self.projection)
    }
}

/// Exact, pre-durable Apply capability which can make at most one exchange.
#[must_use = "prepared ActiveReblit cleanup exchange must be consumed"]
pub(in crate::client::startup_reconciliation) struct PreparedActiveReblitCommitCleanupExchange
{
    pub(super) parents: RetainedActiveReblitCommitCleanupParents,
    pub(super) final_apply: NamespaceSnapshot,
    pub(super) final_projection: ProjectedActiveReblitCommitCleanupNamespace,
}

pub(super) fn require_exact_apply(
    installation: &Installation,
    record: &TransitionRecord,
    parents: &RetainedActiveReblitCommitCleanupParents,
    snapshot: &NamespaceSnapshot,
    projection: &ProjectedActiveReblitCommitCleanupNamespace,
) -> Result<(), ActiveReblitCommitCleanupEffectError> {
    installation.revalidate_mutable_namespace()?;
    snapshot.revalidate_retained()?;
    if projection.layout != ActiveReblitCommitCleanupLayout::Apply
        || ProjectedActiveReblitCommitCleanupNamespace::capture(snapshot, record)? != *projection
    {
        return Err(ActiveReblitCommitCleanupEffectError::ApplyEvidenceChanged);
    }
    parents.revalidate_layout(installation, ActiveReblitCommitCleanupLayout::Apply)?;
    snapshot.revalidate_retained()?;
    installation.revalidate_mutable_namespace()?;
    Ok(())
}

fn layout_error(layout: ActiveReblitCommitCleanupLayout) -> ActiveReblitCommitCleanupEffectError {
    match layout {
        ActiveReblitCommitCleanupLayout::Apply => {
            ActiveReblitCommitCleanupEffectError::ApplyEvidenceChanged
        }
        ActiveReblitCommitCleanupLayout::Finish => {
            ActiveReblitCommitCleanupEffectError::FinishEvidenceChanged
        }
    }
}
