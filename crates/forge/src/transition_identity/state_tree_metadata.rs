//! Exact retained identity for the state ID stored inside one `/usr` tree.

use std::{
    os::{
        fd::AsRawFd as _,
        unix::fs::{FileExt as _, MetadataExt as _, PermissionsExt as _},
    },
    path::{Path, PathBuf},
};

use super::{canonical_state_name, live_usr_io};
use crate::{
    linux_fs::{controlled_resolution, openat2_file},
    state,
    tree_marker::TreeMarkerStore,
};

mod publication;

pub(crate) use publication::StateIdPublicationFailure;
#[cfg(test)]
pub(super) use publication::StateIdPublicationOutcome;
#[cfg(test)]
pub(super) use publication::{
    StateIdPublicationFaultPoint, arm_after_state_id_rename, arm_before_state_id_publish,
    arm_state_id_publication_fault, assert_state_id_publication_fault_consumed,
};

const STATE_ID_NAME: &std::ffi::CStr = c".stateID";
const STATE_ID_TEMPORARY_NAME: &std::ffi::CStr = c".cast-state-id.tmp";
const STATE_ID_MODE: u32 = 0o644;
const MAX_STATE_ID_READ_ATTEMPTS: usize = 32;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct StateIdWitness {
    device: u64,
    inode: u64,
    owner: u32,
    mode: u32,
    links: u64,
    length: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

/// One exact state-ID inode retained from guard preparation onward.
#[derive(Debug)]
pub(super) struct RetainedTreeStateId {
    file: std::fs::File,
    path: PathBuf,
    state: state::Id,
    expected: Vec<u8>,
    witness: StateIdWitness,
}

impl RetainedTreeStateId {
    #[allow(dead_code)] // consumed by the intentionally unwired journal coordinator
    pub(super) fn state(&self) -> state::Id {
        self.state
    }

    pub(super) fn retain(store: &TreeMarkerStore, state: state::Id) -> Result<Self, super::Error> {
        canonical_state_name(state)?;
        store.revalidate_directory()?;
        reject_temporary(store)?;
        let path = store.display_path().join(".stateID");
        let expected = state.to_string().into_bytes();
        let file = open_state_id(store, &path, expected.len() as u64)?;
        let witness = state_id_witness(&file, &path, expected.len() as u64)?;
        require_state_id_contents(&file, &path, &expected, state)?;
        if state_id_witness(&file, &path, expected.len() as u64)? != witness {
            return Err(changed(&path, "state ID inode changed while first retained"));
        }
        let retained = Self {
            file,
            path,
            state,
            expected,
            witness,
        };
        retained.revalidate(store, store)?;
        Ok(retained)
    }

    /// Prove both stores denote the original `/usr`, then sandwich the exact
    /// retained state-ID inode around a reopen of its canonical final name.
    pub(super) fn revalidate(
        &self,
        retained_store: &TreeMarkerStore,
        named_store: &TreeMarkerStore,
    ) -> Result<(), super::Error> {
        canonical_state_name(self.state)?;
        retained_store.require_same_directory(named_store)?;
        reject_temporary(named_store)?;
        self.require_retained()?;

        let named = open_state_id(named_store, &self.path, self.expected.len() as u64)?;
        if state_id_witness(&named, &self.path, self.expected.len() as u64)? != self.witness {
            return Err(changed(&self.path, "named state ID is not the retained inode"));
        }
        require_state_id_contents(&named, &self.path, &self.expected, self.state)?;
        if state_id_witness(&named, &self.path, self.expected.len() as u64)? != self.witness {
            return Err(changed(&self.path, "named state ID changed during revalidation"));
        }

        self.require_retained()?;
        retained_store.require_same_directory(named_store)?;
        Ok(())
    }

    fn require_retained(&self) -> Result<(), super::Error> {
        if state_id_witness(&self.file, &self.path, self.expected.len() as u64)? != self.witness {
            return Err(changed(&self.path, "retained state ID inode metadata changed"));
        }
        require_state_id_contents(&self.file, &self.path, &self.expected, self.state)?;
        if state_id_witness(&self.file, &self.path, self.expected.len() as u64)? != self.witness {
            return Err(changed(&self.path, "retained state ID changed while read"));
        }
        Ok(())
    }
}

fn reject_temporary(store: &TreeMarkerStore) -> Result<(), super::Error> {
    let path = store.display_path().join(".cast-state-id.tmp");
    match openat2_file(
        store.retained_directory().as_raw_fd(),
        STATE_ID_TEMPORARY_NAME,
        nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        controlled_resolution(),
    ) {
        Err(source) if source.raw_os_error() == Some(nix::libc::ENOENT) => Ok(()),
        Ok(_) => Err(live_usr_io(
            "reject temporary state ID evidence",
            &path,
            std::io::Error::other("temporary state ID remains present"),
        )),
        Err(source) => Err(live_usr_io("probe temporary state ID", &path, source)),
    }
}

fn open_state_id(store: &TreeMarkerStore, path: &Path, expected_length: u64) -> Result<std::fs::File, super::Error> {
    let pinned = openat2_file(
        store.retained_directory().as_raw_fd(),
        STATE_ID_NAME,
        nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        controlled_resolution(),
    )
    .map_err(|source| live_usr_io("pin retained state ID", path, source))?;
    let expected = state_id_witness(&pinned, path, expected_length)?;
    let readable = openat2_file(
        store.retained_directory().as_raw_fd(),
        STATE_ID_NAME,
        nix::libc::O_RDONLY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK
            | nix::libc::O_NOATIME,
        0,
        controlled_resolution(),
    )
    .map_err(|source| live_usr_io("open retained state ID", path, source))?;
    if state_id_witness(&readable, path, expected_length)? != expected {
        return Err(changed(path, "state ID inode changed between pin and readable reopen"));
    }
    Ok(readable)
}

fn require_state_id_contents(
    file: &std::fs::File,
    path: &Path,
    expected: &[u8],
    state: state::Id,
) -> Result<(), super::Error> {
    let mut actual = vec![0; expected.len()];
    let mut filled = 0usize;
    let mut attempts = 0usize;
    while filled < actual.len() {
        attempts += 1;
        if attempts > MAX_STATE_ID_READ_ATTEMPTS {
            return Err(live_usr_io(
                "read complete retained state ID",
                path,
                std::io::Error::other("state ID read exceeded the bounded retry limit"),
            ));
        }
        match file.read_at(&mut actual[filled..], filled as u64) {
            Ok(0) => {
                return Err(live_usr_io(
                    "read complete retained state ID",
                    path,
                    std::io::Error::from(std::io::ErrorKind::UnexpectedEof),
                ));
            }
            Ok(read) => filled += read,
            Err(source) if source.kind() == std::io::ErrorKind::Interrupted => {}
            Err(source) => return Err(live_usr_io("read retained state ID", path, source)),
        }
    }
    if actual != expected {
        return Err(live_usr_io(
            "validate retained state ID contents",
            path,
            std::io::Error::other(format!(
                "expected canonical state ID {}, found {:?}",
                i32::from(state),
                String::from_utf8_lossy(&actual)
            )),
        ));
    }
    Ok(())
}

fn state_id_witness(file: &std::fs::File, path: &Path, expected_length: u64) -> Result<StateIdWitness, super::Error> {
    let metadata = file
        .metadata()
        .map_err(|source| live_usr_io("inspect retained state ID", path, source))?;
    let witness = StateIdWitness {
        device: metadata.dev(),
        inode: metadata.ino(),
        owner: metadata.uid(),
        mode: metadata.permissions().mode() & 0o7777,
        links: metadata.nlink(),
        length: metadata.len(),
        modified_seconds: metadata.mtime(),
        modified_nanoseconds: metadata.mtime_nsec(),
        changed_seconds: metadata.ctime(),
        changed_nanoseconds: metadata.ctime_nsec(),
    };
    let owner = unsafe { nix::libc::geteuid() };
    if metadata.file_type().is_file()
        && witness.owner == owner
        && witness.mode == STATE_ID_MODE
        && witness.links == 1
        && witness.length == expected_length
    {
        Ok(witness)
    } else {
        Err(live_usr_io(
            "validate retained state ID inode",
            path,
            std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!(
                    "unsafe state ID (uid={}, mode={:04o}, links={}, length={})",
                    witness.owner, witness.mode, witness.links, witness.length
                ),
            ),
        ))
    }
}

fn changed(path: &Path, message: &'static str) -> super::Error {
    live_usr_io("revalidate retained state ID", path, std::io::Error::other(message))
}
