//! Private, alias-free materialization for inactive archived-state repair.

use thiserror::Error as ThisError;

use super::{
    AssetMaterialization, BlitExecution, Client, Error, PendingFile, Scope,
    active_state_snapshot::ActiveStateLease,
    fixed_staging::{FixedStagingError, RetainedFixedStaging},
};
use crate::package;

/// An independently copied archived-repair candidate plus the retained fixed
/// staging capability and cooperating-writer lease acquired before its first
/// possible staging mutation.
pub(super) struct ArchivedRepairCandidate {
    pub(super) tree: vfs::Tree<PendingFile>,
    pub(super) staging: RetainedFixedStaging,
    pub(super) candidate_usr: std::fs::File,
    pub(super) active_state: ActiveStateLease,
}

#[derive(Debug, ThisError)]
pub(super) enum MaterializationError {
    #[error("archived repair requires a stateful client")]
    StatefulClientRequired,
    #[error("retain and materialize the exact fixed-staging wrapper for archived repair")]
    FixedStaging {
        #[source]
        source: Box<FixedStagingError>,
    },
}

impl From<MaterializationError> for Error {
    fn from(source: MaterializationError) -> Self {
        Self::ArchivedRepairMaterialization {
            source: Box::new(source),
        }
    }
}

impl Client {
    /// Build a candidate for one inactive archived state without ever linking
    /// a writable package inode to the persistent asset pool.
    pub(super) fn materialize_archived_repair_root<'a>(
        &self,
        packages: impl IntoIterator<Item = &'a package::Id>,
    ) -> Result<ArchivedRepairCandidate, Error> {
        self.require_non_frozen()?;
        if !matches!(&self.scope, Scope::Stateful) {
            return Err(MaterializationError::StatefulClientRequired.into());
        }

        let active_state = ActiveStateLease::acquire(&self.installation)?;
        let tree = self.vfs(packages)?;
        let staging = RetainedFixedStaging::prepare_empty(&self.installation).map_err(|source| {
            MaterializationError::FixedStaging {
                source: Box::new(source),
            }
        })?;
        let candidate_usr = staging
            .materialize(
                &self.installation,
                &tree,
                AssetMaterialization::IndependentCopy,
                BlitExecution::Sequential,
            )
            .map_err(|source| MaterializationError::FixedStaging {
                source: Box::new(source),
            })?;
        active_state.revalidate(&self.installation)?;

        Ok(ArchivedRepairCandidate {
            tree,
            staging,
            candidate_usr,
            active_state,
        })
    }
}

#[cfg(test)]
pub(super) use super::fixed_staging::arm_before_staging_baseline_revalidation;
