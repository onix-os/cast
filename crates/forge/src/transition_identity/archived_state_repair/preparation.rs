use std::ffi::CString;

use super::{
    ArchiveBaseline, ArchivedStateRepairError, ArchivedStateRepairIdentity, MAX_QUARANTINE_NAMES, STAGING_NAME,
    error::identity,
    fault_injection::{ArchivedStateRepairFaultPoint, checkpoint},
    validation::same_state_snapshot,
};
use crate::{Installation, db, state, transition_journal::QuarantineName};

impl ArchivedStateRepairIdentity {
    /// Bind one exact staged `{usr}` wrapper and reserve its exact empty 0700
    /// replacement before any metadata or transaction trigger is allowed to
    /// run. The existing canonical wrapper, when present, is retained opaquely
    /// and is never authenticated from its potentially corrupt contents.
    pub(crate) fn prepare_retained_candidate(
        installation: &Installation,
        state_db: &db::state::Database,
        expected: &state::State,
        candidate_usr: &std::fs::File,
    ) -> Result<Self, ArchivedStateRepairError> {
        Self::prepare_candidate(installation, state_db, expected, Some(candidate_usr))
    }

    fn prepare_candidate(
        installation: &Installation,
        state_db: &db::state::Database,
        expected: &state::State,
        retained_candidate_usr: Option<&std::fs::File>,
    ) -> Result<Self, ArchivedStateRepairError> {
        installation
            .revalidate_root_directory()
            .map_err(super::super::Error::from)
            .map_err(|source| identity("revalidate installation root before archived repair", source))?;
        let journal = crate::transition_journal::TransitionJournalStore::open_retained(
            installation.root_directory(),
            &installation.root,
        )
        .map_err(super::super::Error::from)
        .map_err(|source| identity("retain archived-repair journal lock", source))?;
        super::super::require_clean_baseline(&journal, state_db)
            .map_err(|source| identity("check archived-repair transition baseline", source))?;
        let active_expected = require_expected_state(installation, state_db, expected)?;
        let live_active = super::live_active::LiveActiveBaseline::retain(
            installation,
            active_expected.as_ref().map(|state| state.id),
        )?;

        let state_name = super::super::canonical_state_name(expected.id)
            .map_err(|source| identity("validate archived-repair state name", source))?;
        let roots_path = installation.root_path("");
        let roots = super::super::RetainedDirectory::open_beneath(
            installation.root_directory(),
            super::super::ROOTS_RELATIVE,
            roots_path.clone(),
        )
        .map_err(|source| identity("retain archived-repair roots", source))?;
        Self::require_safe_wrapper(&roots)?;

        let preliminary_staging = roots
            .open_child(STAGING_NAME, installation.staging_dir())
            .map_err(|source| identity("retain archived-repair staging wrapper", source))?;
        Self::require_safe_wrapper(&preliminary_staging)?;
        preliminary_staging
            .require_exact_entries(&[b"usr"])
            .map_err(|source| identity("preflight exact staged archived-repair wrapper", source))?;
        let staging_identity = (preliminary_staging.witness.device, preliminary_staging.witness.inode);
        crate::linux_fs::chmod_path_descriptor(&preliminary_staging.file, 0o700).map_err(|source| {
            ArchivedStateRepairError::Io {
                operation: "normalize archived-repair staging wrapper mode",
                path: preliminary_staging.path.clone(),
                source,
            }
        })?;
        drop(preliminary_staging);
        let staging = roots
            .open_child(STAGING_NAME, installation.staging_dir())
            .map_err(|source| identity("reopen normalized archived-repair staging wrapper", source))?;
        if (staging.witness.device, staging.witness.inode) != staging_identity {
            return Err(identity(
                "revalidate normalized archived-repair staging wrapper inode",
                super::super::Error::QuarantineDirectoryChanged {
                    path: staging.path.clone(),
                },
            ));
        }
        Self::require_safe_wrapper(&staging)?;
        if staging.witness.mode != 0o700 {
            return Err(ArchivedStateRepairError::UnsafeWrapper {
                path: staging.path.clone(),
                owner: staging.witness.owner,
                mode: staging.witness.mode,
            });
        }
        staging
            .require_exact_entries(&[b"usr"])
            .map_err(|source| identity("require exact staged archived-repair wrapper", source))?;
        Self::require_same_mount(&roots, &staging)?;

        let candidate_path = staging.path.join("usr");
        let candidate_store = super::super::open_optional_retained_tree(&staging, &candidate_path)
            .map_err(|source| identity("retain archived-repair candidate /usr", source))?
            .ok_or_else(|| {
                identity(
                    "require archived-repair candidate /usr",
                    super::super::Error::PreviousMoveTreeMissing {
                        staged: candidate_path.clone(),
                        archived: roots.path.join(state_name.to_string_lossy().as_ref()).join("usr"),
                    },
                )
            })?;
        let retained_candidate = retained_candidate_usr
            .map(|candidate_usr| crate::tree_marker::TreeMarkerStore::open(candidate_usr, &candidate_path))
            .transpose()
            .map_err(super::super::Error::from)
            .map_err(|source| identity("retain caller-authenticated archived-repair candidate /usr", source))?;
        if let Some(retained_candidate) = retained_candidate.as_ref() {
            candidate_store
                .require_same_directory(retained_candidate)
                .map_err(super::super::Error::from)
                .map_err(|source| identity("bind archived-repair candidate to retained materialization", source))?;
        }
        let candidate = super::super::RetainedIdentity::prepare_strict(candidate_store, expected.id)
            .map_err(|source| identity("retain archived-repair candidate identity", source))?;
        if let Some(retained_candidate) = retained_candidate.as_ref() {
            candidate
                .verify_store_with_state_id(retained_candidate)
                .map_err(|source| identity("revalidate retained archived-repair candidate identity", source))?;
        }

        let archive_path = roots.path.join(state_name.to_string_lossy().as_ref());
        let archive = match roots
            .open_optional_child(&state_name, archive_path)
            .map_err(|source| identity("classify canonical archived-state wrapper", source))?
        {
            Some(old) => {
                Self::require_safe_wrapper(&old)?;
                Self::require_same_mount(&roots, &old)?;
                ArchiveBaseline::Existing(old)
            }
            None => ArchiveBaseline::Missing,
        };

        let quarantine = super::super::RetainedDirectory::open_beneath(
            installation.root_directory(),
            super::super::QUARANTINE_RELATIVE,
            installation.state_quarantine_dir(),
        )
        .map_err(|source| identity("retain archived-repair quarantine", source))?;
        Self::require_safe_wrapper(&quarantine)?;
        Self::require_same_mount(&roots, &quarantine)?;

        let (quarantine_name, quarantine_path, replacement) =
            reserve_replacement(&quarantine, expected.id, candidate.marker.token().as_str())?;

        let guard = Self {
            journal,
            expected: expected.clone(),
            active_expected,
            state_name,
            roots,
            staging,
            candidate,
            live_active,
            archive,
            quarantine,
            replacement,
            quarantine_name,
            quarantine_path,
            operation: std::sync::Mutex::new(()),
        };
        match guard.finish_preparation(installation, state_db) {
            Ok(()) => Ok(guard),
            Err(primary) => match guard.retire_failed_preparation_reservation(installation) {
                Ok(()) => Err(primary),
                Err(cleanup) => Err(ArchivedStateRepairError::PreparationReservationCleanupFailed {
                    primary: Box::new(primary),
                    cleanup: Box::new(cleanup),
                }),
            },
        }
    }

    fn finish_preparation(
        &self,
        installation: &Installation,
        state_db: &db::state::Database,
    ) -> Result<(), ArchivedStateRepairError> {
        Self::require_same_mount(&self.roots, &self.replacement)?;
        checkpoint(ArchivedStateRepairFaultPoint::ReplacementPostCreate)?;
        self.replacement
            .require_exact_entries(&[])
            .map_err(|source| identity("validate empty archived-repair replacement", source))?;
        checkpoint(ArchivedStateRepairFaultPoint::ReplacementPreparationSync)?;
        self.replacement
            .sync("sync empty archived-repair replacement")
            .map_err(|source| identity("sync empty archived-repair replacement", source))?;
        checkpoint(ArchivedStateRepairFaultPoint::QuarantinePreparationSync)?;
        self.quarantine
            .sync("sync archived-repair replacement name")
            .map_err(|source| identity("sync archived-repair quarantine after reservation", source))?;
        self.candidate
            .store
            .sync_retained_tree()
            .map_err(super::super::Error::from)
            .map_err(|source| identity("sync archived-repair candidate before triggers", source))?;
        self.staging
            .sync("sync staged archived-repair wrapper before triggers")
            .map_err(|source| identity("sync staged archived-repair wrapper before triggers", source))?;
        self.roots
            .sync("sync roots after archived-repair reservation")
            .map_err(|source| identity("sync roots after archived-repair reservation", source))?;
        checkpoint(ArchivedStateRepairFaultPoint::FinalPreparationRevalidation)?;
        self.verify_candidate_snapshot(installation, state_db)
    }
}

fn require_expected_state(
    installation: &Installation,
    state_db: &db::state::Database,
    expected: &state::State,
) -> Result<Option<state::State>, ArchivedStateRepairError> {
    let actual = state_db
        .get(expected.id)
        .map_err(|source| ArchivedStateRepairError::StateLookup {
            state: i32::from(expected.id),
            source,
        })?;
    if !same_state_snapshot(expected, &actual) {
        return Err(ArchivedStateRepairError::StateChanged {
            state: i32::from(expected.id),
        });
    }
    if installation.active_state == Some(expected.id) {
        return Err(ArchivedStateRepairError::TargetBecameActive {
            state: i32::from(expected.id),
        });
    }
    installation
        .active_state
        .map(|active| {
            state_db
                .get(active)
                .map_err(|source| ArchivedStateRepairError::ActiveStateLookup {
                    state: i32::from(active),
                    source,
                })
        })
        .transpose()
}

fn reserve_replacement(
    quarantine: &super::super::RetainedDirectory,
    state: state::Id,
    token: &str,
) -> Result<(CString, std::path::PathBuf, super::super::RetainedDirectory), ArchivedStateRepairError> {
    for index in 0..MAX_QUARANTINE_NAMES {
        let name = QuarantineName::parse(format!("archived-repair-{}-{token}-{index}", i32::from(state)))
            .map_err(ArchivedStateRepairError::InvalidQuarantineName)?;
        let encoded = CString::new(name.as_str()).expect("validated quarantine name contains no NUL");
        let path = quarantine.path.join(name.as_str());
        if quarantine
            .child_name_exists(&encoded, path.clone())
            .map_err(|source| identity("probe archived-repair quarantine name", source))?
        {
            continue;
        }
        match super::super::RetainedDirectory::create_private_child(quarantine, &encoded, path.clone()) {
            Ok(replacement) => return Ok((encoded, path, replacement)),
            Err(super::super::Error::QuarantineSlotExists { .. }) => continue,
            Err(source) => return Err(identity("create empty archived-repair replacement", source)),
        }
    }
    Err(ArchivedStateRepairError::QuarantineExhausted {
        state: i32::from(state),
        limit: MAX_QUARANTINE_NAMES,
    })
}
