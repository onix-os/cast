//! Structural identity marker for a reusable state-wrapper slot.
//!
//! The named entry is not an independently forgeable token file. It is the
//! sole authorized hardlink to the exact permanent `.cast-tree-id` inode of
//! the tree whose wrapper may be reused.

use std::{
    ffi::{CStr, CString},
    io,
    os::{
        fd::AsRawFd as _,
        unix::fs::{MetadataExt as _, PermissionsExt as _},
    },
    path::{Path, PathBuf},
};

use fs_err::File;
use thiserror::Error;

use super::RetainedDirectory;
use crate::{
    linux_fs::{controlled_resolution, openat2_file},
    state,
    transition_journal::TreeToken,
    tree_marker::{RetainedTreeMarker, TreeMarkerError, TreeMarkerStore},
};

const MARKER_PREFIX: &str = ".cast-state-slot-";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MarkerWitness {
    device: u64,
    inode: u64,
    owner: u32,
    mode: u32,
    links: u64,
    length: u64,
}

#[derive(Debug)]
pub(super) struct RetainedStateSlotMarker {
    name: CString,
    file: File,
    path: PathBuf,
    witness: MarkerWitness,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum NamedMarkerState {
    Absent,
    Exact,
    Foreign,
}

#[derive(Debug, Error)]
pub(crate) enum Error {
    #[error("state-slot marker requires one positive state ID, found {state}")]
    InvalidState { state: i32 },
    #[error("state-slot marker name is not one canonical component")]
    InvalidName,
    #[error("{operation} for state-slot marker `{}`", path.display())]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{operation} for state-slot marker wrapper `{}`", path.display())]
    Directory {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: Box<super::Error>,
    },
    #[error("authenticate state-slot hardlink at `{}`", path.display())]
    TreeMarker {
        path: PathBuf,
        #[source]
        source: TreeMarkerError,
    },
    #[error("state-slot marker identity changed at `{}`", path.display())]
    Changed { path: PathBuf },
    #[error("state-slot marker publication collided at `{}`", path.display())]
    PublicationCollision { path: PathBuf },
    #[error("state-slot marker is missing at `{}`", path.display())]
    Missing { path: PathBuf },
}

impl RetainedStateSlotMarker {
    pub(super) fn prepare(
        wrapper: &RetainedDirectory,
        state: state::Id,
        tree_store: &TreeMarkerStore,
        tree_marker: &RetainedTreeMarker,
    ) -> Result<Self, Error> {
        let name = marker_name(state, tree_marker.token())?;
        let path = marker_path(wrapper, &name);
        if let Some(file) = open_named_file(wrapper, &name, &path)? {
            tree_marker
                .require_authorized_slot_link(file.file(), &path)
                .map_err(|source| tree_marker_error(path.clone(), source))?;
            let marker = Self::from_file(name, file, path)?;
            marker.sync()?;
            sync_wrapper(wrapper, &marker.path)?;
            marker.require_named(wrapper)?;
            return Ok(marker);
        }

        match tree_marker.link_state_slot_noreplace(tree_store, &wrapper.file, &name, &path) {
            Ok(()) => {}
            Err(TreeMarkerError::SlotLinkPublicationCollision { .. }) => {
                return Err(Error::PublicationCollision { path });
            }
            Err(source) => return Err(tree_marker_error(path, source)),
        }
        sync_wrapper(wrapper, &path)?;
        let marker = Self::open_expected(wrapper, state, tree_marker)?;
        marker.sync()?;
        sync_wrapper(wrapper, &marker.path)?;
        marker.require_named(wrapper)?;
        Ok(marker)
    }

    pub(super) fn open_expected(
        wrapper: &RetainedDirectory,
        state: state::Id,
        tree_marker: &RetainedTreeMarker,
    ) -> Result<Self, Error> {
        Self::open_with_authentication(wrapper, state, tree_marker, true)
    }

    pub(super) fn open_recovery_candidate(
        wrapper: &RetainedDirectory,
        state: state::Id,
        tree_marker: &RetainedTreeMarker,
    ) -> Result<Self, Error> {
        Self::open_with_authentication(wrapper, state, tree_marker, false)
    }

    fn open_with_authentication(
        wrapper: &RetainedDirectory,
        state: state::Id,
        tree_marker: &RetainedTreeMarker,
        require_authorized: bool,
    ) -> Result<Self, Error> {
        let name = marker_name(state, tree_marker.token())?;
        let path = marker_path(wrapper, &name);
        let file = open_named_file(wrapper, &name, &path)?.ok_or_else(|| Error::Missing { path: path.clone() })?;
        let result = if require_authorized {
            tree_marker.require_authorized_slot_link(file.file(), &path)
        } else {
            tree_marker.require_recovery_slot_link_candidate(file.file(), &path)
        };
        result.map_err(|source| tree_marker_error(path.clone(), source))?;
        Self::from_file(name, file, path)
    }

    pub(super) fn require_named(&self, wrapper: &RetainedDirectory) -> Result<(), Error> {
        self.require_retained()?;
        let named = open_named_file(wrapper, &self.name, &self.path)?.ok_or_else(|| Error::Missing {
            path: marker_path(wrapper, &self.name),
        })?;
        if witness(&named, &self.path)? == self.witness {
            Ok(())
        } else {
            Err(Error::Changed {
                path: marker_path(wrapper, &self.name),
            })
        }
    }

    pub(super) fn named_state(&self, wrapper: &RetainedDirectory) -> Result<NamedMarkerState, Error> {
        let path = marker_path(wrapper, &self.name);
        let Some(named) = open_named_file(wrapper, &self.name, &path)? else {
            return Ok(NamedMarkerState::Absent);
        };
        let actual = witness(&named, &path)?;
        if (actual.device, actual.inode) != (self.witness.device, self.witness.inode) {
            return Ok(NamedMarkerState::Foreign);
        }
        if actual == self.witness {
            Ok(NamedMarkerState::Exact)
        } else {
            Err(Error::Changed { path })
        }
    }

    pub(super) fn require_retained(&self) -> Result<(), Error> {
        if witness(&self.file, &self.path)? == self.witness {
            Ok(())
        } else {
            Err(Error::Changed {
                path: self.path.clone(),
            })
        }
    }

    pub(super) fn sync(&self) -> Result<(), Error> {
        self.require_retained()?;
        self.file
            .sync_all()
            .map_err(|source| io_error("sync retained state-slot marker", self.path.clone(), source))?;
        self.require_retained()
    }

    pub(super) fn name(&self) -> &CStr {
        &self.name
    }

    pub(super) fn name_bytes(&self) -> &[u8] {
        self.name.to_bytes()
    }

    fn from_file(name: CString, file: File, path: PathBuf) -> Result<Self, Error> {
        let witness = witness(&file, &path)?;
        Ok(Self {
            name,
            file,
            path,
            witness,
        })
    }
}

pub(super) fn marker_name(state: state::Id, tree_token: &TreeToken) -> Result<CString, Error> {
    let state = i32::from(state);
    if state <= 0 {
        return Err(Error::InvalidState { state });
    }
    CString::new(format!("{MARKER_PREFIX}{state}-{}", tree_token.as_str())).map_err(|_| Error::InvalidName)
}

fn open_named_file(wrapper: &RetainedDirectory, name: &CStr, path: &Path) -> Result<Option<File>, Error> {
    let probe = match openat2_file(
        wrapper.file.as_raw_fd(),
        name,
        nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        controlled_resolution(),
    ) {
        Ok(probe) => probe,
        Err(source) if source.raw_os_error() == Some(nix::libc::ENOENT) => return Ok(None),
        Err(source) => return Err(io_error("probe state-slot marker", path.to_owned(), source)),
    };
    let expected = witness_raw(&probe, path)?;
    // O_NOATIME is permitted for an inode owned by the effective user.  Check
    // that invariant through the non-reading O_PATH pin before reopening the
    // marker, so a foreign occupant is rejected without either mutating its
    // atime or turning a structural mismatch into an O_NOATIME EPERM.
    // SAFETY: geteuid takes no arguments and cannot fail.
    if expected.owner != unsafe { nix::libc::geteuid() } {
        return Err(Error::Changed { path: path.to_owned() });
    }
    let file = openat2_file(
        wrapper.file.as_raw_fd(),
        name,
        nix::libc::O_RDONLY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK
            | nix::libc::O_NOATIME,
        0,
        controlled_resolution(),
    )
    .map_err(|source| io_error("open state-slot marker", path.to_owned(), source))?;
    let file = File::from_parts(file, path.to_owned());
    if witness(&file, path)? != expected {
        return Err(Error::Changed { path: path.to_owned() });
    }
    Ok(Some(file))
}

fn marker_path(wrapper: &RetainedDirectory, name: &CStr) -> PathBuf {
    wrapper.path.join(name.to_string_lossy().as_ref())
}

fn witness(file: &File, path: &Path) -> Result<MarkerWitness, Error> {
    witness_raw(file.file(), path)
}

fn witness_raw(file: &std::fs::File, path: &Path) -> Result<MarkerWitness, Error> {
    let metadata = file
        .metadata()
        .map_err(|source| io_error("inspect state-slot marker", path.to_owned(), source))?;
    Ok(MarkerWitness {
        device: metadata.dev(),
        inode: metadata.ino(),
        owner: metadata.uid(),
        mode: metadata.permissions().mode() & 0o7777,
        links: metadata.nlink(),
        length: metadata.len(),
    })
}

fn io_error(operation: &'static str, path: PathBuf, source: io::Error) -> Error {
    Error::Io {
        operation,
        path,
        source,
    }
}

fn tree_marker_error(path: PathBuf, source: TreeMarkerError) -> Error {
    Error::TreeMarker { path, source }
}

fn sync_wrapper(wrapper: &RetainedDirectory, marker_path: &Path) -> Result<(), Error> {
    wrapper
        .sync("sync state wrapper for retained slot marker")
        .map_err(|source| Error::Directory {
            operation: "sync state wrapper for retained slot marker",
            path: marker_path.to_owned(),
            source: Box::new(source),
        })
}
