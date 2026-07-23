//! Exact read-only baseline for the live active tree during archived repair.

use std::{os::fd::AsRawFd as _, path::Path};

use super::{ArchivedStateRepairError, error::identity};
use crate::{Installation, linux_fs, state, tree_marker::TreeMarkerStore};

/// Discovery-time active-state bytes are not namespace authority. This value
/// retains the exact live directory, tree-marker inode/token, and state-ID
/// inode/content before archived repair can decorate or trigger a candidate.
#[derive(Debug)]
pub(super) enum LiveActiveBaseline {
    Selected {
        expected: state::Id,
        identity: super::super::RetainedIdentity,
    },
    Unselected(TreeMarkerStore),
    MissingUsr,
}

impl LiveActiveBaseline {
    pub(super) fn retain(
        installation: &Installation,
        expected: Option<state::Id>,
    ) -> Result<Self, ArchivedStateRepairError> {
        installation
            .revalidate_root_directory()
            .map_err(super::super::Error::from)
            .map_err(|source| identity("revalidate installation root before retaining live active tree", source))?;
        let live_path = installation.root.join("usr");
        let live = open_live_usr(installation, &live_path)?;
        let baseline = match (expected, live) {
            (Some(expected), None) => {
                return Err(ArchivedStateRepairError::LiveActiveStateMissing {
                    expected: i32::from(expected),
                });
            }
            (Some(expected), Some(live)) => {
                let store = TreeMarkerStore::open(&live, &live_path)
                    .map_err(super::super::Error::from)
                    .map_err(|source| identity("retain exact live active /usr directory", source))?;
                let marker = store
                    .read_for_recovery()
                    .map_err(super::super::Error::from)
                    .map_err(|source| identity("retain exact live active tree marker", source))?;
                let state_id = super::super::state_tree_metadata::RetainedTreeStateId::retain(&store, expected)
                    .map_err(|source| identity("retain exact live active state ID", source))?;
                let retained = super::super::RetainedIdentity {
                    store,
                    marker,
                    state_id: Some(state_id),
                };
                retained
                    .verify_store_with_state_id(&retained.store)
                    .map_err(|source| identity("finish exact live active-tree retention", source))?;
                Self::Selected {
                    expected,
                    identity: retained,
                }
            }
            (None, Some(live)) => {
                let store = TreeMarkerStore::open(&live, &live_path)
                    .map_err(super::super::Error::from)
                    .map_err(|source| identity("retain unselected live /usr directory", source))?;
                require_state_id_absent(&store)?;
                Self::Unselected(store)
            }
            (None, None) => Self::MissingUsr,
        };
        installation
            .revalidate_root_directory()
            .map_err(super::super::Error::from)
            .map_err(|source| identity("revalidate installation root after retaining live active tree", source))?;
        baseline.revalidate(installation)?;
        Ok(baseline)
    }

    pub(super) fn revalidate(&self, installation: &Installation) -> Result<(), ArchivedStateRepairError> {
        installation
            .revalidate_root_directory()
            .map_err(super::super::Error::from)
            .map_err(|source| identity("revalidate installation root before live active-tree proof", source))?;
        let live_path = installation.root.join("usr");
        let live = open_live_usr(installation, &live_path)?;
        match (self, live) {
            (Self::Selected { identity: retained, .. }, Some(live)) => {
                let named = TreeMarkerStore::open(&live, &live_path)
                    .map_err(super::super::Error::from)
                    .map_err(|source| identity("reopen named live active /usr directory", source))?;
                retained
                    .verify_store_with_state_id(&named)
                    .map_err(|source| identity("revalidate exact live active tree", source))?;
            }
            (Self::Selected { expected, .. }, None) => {
                return Err(ArchivedStateRepairError::LiveActiveStateMissing {
                    expected: i32::from(*expected),
                });
            }
            (Self::Unselected(retained), Some(live)) => {
                let named = TreeMarkerStore::open(&live, &live_path)
                    .map_err(super::super::Error::from)
                    .map_err(|source| identity("reopen named unselected live /usr directory", source))?;
                retained
                    .require_same_directory(&named)
                    .map_err(super::super::Error::from)
                    .map_err(|source| identity("revalidate exact unselected live /usr directory", source))?;
                require_state_id_absent(&named)?;
                retained
                    .require_same_directory(&named)
                    .map_err(super::super::Error::from)
                    .map_err(|source| identity("finish unselected live /usr directory proof", source))?;
            }
            (Self::Unselected(_), None) => {
                return Err(ArchivedStateRepairError::UnexpectedLiveActiveState { path: live_path });
            }
            (Self::MissingUsr, None) => {}
            (Self::MissingUsr, Some(_)) => {
                return Err(ArchivedStateRepairError::UnexpectedLiveActiveState { path: live_path });
            }
        }
        installation
            .revalidate_root_directory()
            .map_err(super::super::Error::from)
            .map_err(|source| identity("revalidate installation root after live active-tree proof", source))
    }
}

fn open_live_usr(installation: &Installation, path: &Path) -> Result<Option<std::fs::File>, ArchivedStateRepairError> {
    match linux_fs::openat2_file(
        installation.root_directory().as_raw_fd(),
        c"usr",
        nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK,
        0,
        linux_fs::controlled_resolution(),
    ) {
        Ok(live) => Ok(Some(live)),
        Err(source) if source.raw_os_error() == Some(nix::libc::ENOENT) => Ok(None),
        Err(source) => Err(ArchivedStateRepairError::Io {
            operation: "open live /usr through retained installation root",
            path: path.to_owned(),
            source,
        }),
    }
}

fn require_state_id_absent(store: &TreeMarkerStore) -> Result<(), ArchivedStateRepairError> {
    store
        .revalidate_directory()
        .map_err(super::super::Error::from)
        .map_err(|source| identity("revalidate unselected live /usr before state-ID absence proof", source))?;
    let path = store.display_path().join(".stateID");
    match linux_fs::openat2_file(
        store.retained_directory().as_raw_fd(),
        c".stateID",
        nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        linux_fs::controlled_resolution(),
    ) {
        Err(source) if source.raw_os_error() == Some(nix::libc::ENOENT) => {}
        Ok(_) => return Err(ArchivedStateRepairError::UnexpectedLiveActiveState { path }),
        Err(source) => {
            return Err(ArchivedStateRepairError::Io {
                operation: "prove unselected live state-ID absence",
                path,
                source,
            });
        }
    }
    store
        .revalidate_directory()
        .map_err(super::super::Error::from)
        .map_err(|source| identity("revalidate unselected live /usr after state-ID absence proof", source))
}
