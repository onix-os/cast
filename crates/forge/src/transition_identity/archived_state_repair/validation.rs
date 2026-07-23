use super::{
    ArchiveBaseline, ArchivedStateRepairError, ArchivedStateRepairIdentity, USR_NAME,
    error::{identity, namespace_proof},
    layout::RepairLayout,
};
use crate::{Installation, db, state};

impl ArchivedStateRepairIdentity {
    /// Strict boundary proof used before and after transaction-trigger work.
    /// It is incapable of creating or repairing either candidate identity.
    pub(crate) fn verify_candidate_snapshot(
        &self,
        installation: &Installation,
        state_db: &db::state::Database,
    ) -> Result<(), ArchivedStateRepairError> {
        self.require_candidate_boundary(RepairLayout::Initial, installation, state_db)
    }

    /// Prove semantic state, then the exact wrapper/candidate namespace, then
    /// semantic state again. All three reads are attempted even when an
    /// earlier one fails so a namespace substitution is never hidden behind a
    /// simultaneous database or active-selection error.
    pub(super) fn require_candidate_boundary(
        &self,
        expected: RepairLayout,
        installation: &Installation,
        state_db: &db::state::Database,
    ) -> Result<(), ArchivedStateRepairError> {
        let semantic_before = self.require_semantic_snapshot(installation, state_db);
        let namespace = self.require_exact_candidate_namespace(expected, installation);
        let semantic_after = self.require_semantic_snapshot(installation, state_db);

        // Namespace uncertainty wins because callers must not issue or retry a
        // rename when C/S/Q authority is no longer exact.
        namespace?;
        semantic_before?;
        semantic_after
    }

    /// Preservation treats candidate contents as opaque recovery payload, but
    /// a successful result still requires target/active/journal semantics on
    /// both sides of the exact Preserved namespace proof.
    pub(super) fn require_preserved_boundary(
        &self,
        installation: &Installation,
        state_db: &db::state::Database,
    ) -> Result<(), ArchivedStateRepairError> {
        let semantic_before = self.require_semantic_snapshot(installation, state_db);
        let namespace = self.require_opaque_namespace(RepairLayout::Preserved, installation);
        let semantic_after = self.require_semantic_snapshot(installation, state_db);

        namespace?;
        semantic_before?;
        semantic_after
    }

    pub(super) fn require_semantic_snapshot(
        &self,
        installation: &Installation,
        state_db: &db::state::Database,
    ) -> Result<(), ArchivedStateRepairError> {
        super::super::require_clean_baseline(&self.journal, state_db)
            .map_err(|source| identity("check archived-repair journal and database baseline", source))?;
        installation
            .revalidate_root_directory()
            .map_err(super::super::Error::from)
            .map_err(|source| identity("revalidate archived-repair installation root", source))?;
        let actual = state_db
            .get(self.expected.id)
            .map_err(|source| ArchivedStateRepairError::StateLookup {
                state: i32::from(self.expected.id),
                source,
            })?;
        if !same_state_snapshot(&self.expected, &actual) {
            return Err(ArchivedStateRepairError::StateChanged {
                state: i32::from(self.expected.id),
            });
        }
        let expected_active = self.active_expected.as_ref().map(|state| state.id);
        // The retained live tree is the active-selection authority. The
        // discovery-time field below is only a consistency witness; matching
        // cached bytes can never authorize a replacement `/usr` or marker.
        self.live_active.revalidate(installation)?;
        if installation.active_state != expected_active {
            return Err(ArchivedStateRepairError::ActiveSelectionChanged {
                expected: expected_active.map(i32::from),
                actual: installation.active_state.map(i32::from),
            });
        }
        if let Some(expected_active) = &self.active_expected {
            let actual_active =
                state_db
                    .get(expected_active.id)
                    .map_err(|source| ArchivedStateRepairError::ActiveStateLookup {
                        state: i32::from(expected_active.id),
                        source,
                    })?;
            if !same_state_snapshot(expected_active, &actual_active) {
                return Err(ArchivedStateRepairError::ActiveStateChanged {
                    state: i32::from(expected_active.id),
                });
            }
        }
        Ok(())
    }

    fn require_exact_candidate_namespace(
        &self,
        expected: RepairLayout,
        installation: &Installation,
    ) -> Result<(), ArchivedStateRepairError> {
        let retained = self
            .require_retained_base(installation)
            .map_err(|source| namespace_proof("revalidate archived-repair namespace authority", source));
        let before = self
            .require_layout(expected)
            .map_err(|source| namespace_proof("observe archived-repair layout before candidate proof", source));
        let candidate = self.require_strict_candidate(&self.staging);
        let after = self
            .require_layout(expected)
            .map_err(|source| namespace_proof("observe archived-repair layout after candidate proof", source));

        before?;
        after?;
        retained?;
        candidate
    }

    pub(super) fn require_opaque_namespace(
        &self,
        expected: RepairLayout,
        installation: &Installation,
    ) -> Result<(), ArchivedStateRepairError> {
        let retained = self
            .require_retained_base(installation)
            .map_err(|source| namespace_proof("revalidate opaque archived-repair namespace authority", source));
        let layout = self
            .require_layout(expected)
            .map_err(|source| namespace_proof("observe opaque archived-repair layout", source));
        layout?;
        retained
    }

    pub(super) fn require_strict_candidate(
        &self,
        wrapper: &super::super::RetainedDirectory,
    ) -> Result<(), ArchivedStateRepairError> {
        wrapper
            .require_retained()
            .map_err(|source| identity("revalidate archived-repair candidate wrapper", source))?;
        wrapper
            .require_exact_entries(&[USR_NAME.to_bytes()])
            .map_err(|source| identity("require exact archived-repair candidate wrapper", source))?;
        let path = wrapper.path.join("usr");
        let store = super::super::open_optional_retained_tree(wrapper, &path)
            .map_err(|source| identity("open retained archived-repair candidate", source))?
            .ok_or_else(|| {
                identity(
                    "require retained archived-repair candidate",
                    super::super::Error::PreviousMoveTreeMissing {
                        staged: self.staging.path.join("usr"),
                        archived: self
                            .roots
                            .path
                            .join(self.state_name.to_string_lossy().as_ref())
                            .join("usr"),
                    },
                )
            })?;
        self.candidate
            .verify_store_with_state_id(&store)
            .map_err(|source| identity("authenticate archived-repair candidate marker and state ID", source))?;
        wrapper
            .require_exact_entries(&[USR_NAME.to_bytes()])
            .map_err(|source| identity("revalidate exact archived-repair candidate wrapper", source))?;
        self.candidate
            .verify_store_with_state_id(&store)
            .map_err(|source| identity("revalidate archived-repair candidate marker and state ID", source))
    }

    pub(super) fn require_retained_base(&self, installation: &Installation) -> Result<(), ArchivedStateRepairError> {
        installation
            .revalidate_root_directory()
            .map_err(super::super::Error::from)
            .map_err(|source| identity("revalidate archived-repair installation root", source))?;
        self.roots
            .revalidate_beneath(installation.root_directory(), super::super::ROOTS_RELATIVE)
            .map_err(|source| identity("revalidate archived-repair roots", source))?;
        self.quarantine
            .revalidate_beneath(installation.root_directory(), super::super::QUARANTINE_RELATIVE)
            .map_err(|source| identity("revalidate archived-repair quarantine", source))?;
        self.staging
            .require_retained()
            .map_err(|source| identity("revalidate archived-repair staging wrapper", source))?;
        self.replacement
            .require_retained()
            .map_err(|source| identity("revalidate archived-repair empty replacement", source))?;
        self.replacement
            .require_exact_entries(&[])
            .map_err(|source| identity("require exact empty archived-repair replacement", source))?;
        if let ArchiveBaseline::Existing(old) = &self.archive {
            old.require_retained()
                .map_err(|source| identity("revalidate opaque archived wrapper", source))?;
        }
        Ok(())
    }

    pub(super) fn require_safe_wrapper(
        wrapper: &super::super::RetainedDirectory,
    ) -> Result<(), ArchivedStateRepairError> {
        let owner = unsafe { nix::libc::geteuid() };
        let mode = wrapper.witness.mode;
        if wrapper.witness.owner != owner || mode & 0o7000 != 0 || mode & 0o022 != 0 || mode & 0o700 != 0o700 {
            return Err(ArchivedStateRepairError::UnsafeWrapper {
                path: wrapper.path.clone(),
                owner: wrapper.witness.owner,
                mode,
            });
        }
        Ok(())
    }

    pub(super) fn require_same_mount(
        roots: &super::super::RetainedDirectory,
        other: &super::super::RetainedDirectory,
    ) -> Result<(), ArchivedStateRepairError> {
        if roots.witness.device == other.witness.device {
            Ok(())
        } else {
            Err(ArchivedStateRepairError::CrossDevice {
                roots: roots.path.clone(),
                other: other.path.clone(),
            })
        }
    }
}

pub(super) fn same_state_snapshot(expected: &state::State, actual: &state::State) -> bool {
    let mut expected_selections = expected.selections.clone();
    let mut actual_selections = actual.selections.clone();
    let sort = |selections: &mut Vec<state::Selection>| {
        selections.sort_by(|left, right| {
            left.package
                .cmp(&right.package)
                .then(left.explicit.cmp(&right.explicit))
                .then(left.reason.cmp(&right.reason))
        });
    };
    sort(&mut expected_selections);
    sort(&mut actual_selections);
    expected.id == actual.id
        && expected.summary == actual.summary
        && expected.description == actual.description
        && expected.created == actual.created
        && expected.kind == actual.kind
        && expected_selections == actual_selections
}
