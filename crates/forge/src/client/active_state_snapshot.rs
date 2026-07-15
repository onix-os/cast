//! Descriptor-rooted proof of the live active-state selection.

use std::{
    ffi::CStr,
    io,
    os::{
        fd::AsRawFd as _,
        unix::fs::{FileExt as _, MetadataExt as _, PermissionsExt as _},
    },
    path::Path,
    sync::MutexGuard,
};

use crate::{
    Installation, linux_fs, state,
    tree_marker::{RetainedTreeMarker, TreeMarkerStore},
};

const MAX_STATE_ID_BYTES: usize = 10;
const MAX_READ_ATTEMPTS: usize = 32;
const STATE_ID_MODE: u32 = 0o644;

mod handoff;
mod read_only;

pub(super) use read_only::ReadOnlyActiveStateSnapshot;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DirectoryWitness {
    device: u64,
    inode: u64,
    owner: u32,
    mode: u32,
    links: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

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

/// One live active-state selection plus the cooperating-writer lease under
/// which it was captured and compared with Installation discovery.
///
/// The value is intentionally non-cloneable. Callers retain it through every
/// operation which treats the captured state ID as namespace authority.
pub(super) struct ActiveStateLease {
    active: Option<state::Id>,
    proof: ActiveStateProof,
    _coordinator: MutexGuard<'static, ()>,
}

/// A cooperating-writer guard reserved before the startup journal lock.
///
/// This type carries no live-state observation. System startup consumes it
/// only after presenting the retained clean-startup gate, which preserves the
/// coordinator-then-journal lock order without trusting preliminary
/// Installation discovery as authority.
pub(super) struct ActiveStateReservation {
    coordinator: MutexGuard<'static, ()>,
}

/// Exact active-namespace evidence with no process-local writer mutex. It can
/// cross an async cache operation, but authorizes no mutation by itself.
pub(super) struct ActiveStateSnapshot {
    active: Option<state::Id>,
    proof: ActiveStateProof,
}

enum ActiveStateProof {
    MissingUsr {
        root: DirectoryWitness,
    },
    PresentBaseline {
        root: DirectoryWitness,
        usr: std::fs::File,
        usr_witness: DirectoryWitness,
        marker: Option<(TreeMarkerStore, RetainedTreeMarker)>,
    },
    Selected {
        root: DirectoryWitness,
        usr: std::fs::File,
        usr_witness: DirectoryWitness,
        state_id: std::fs::File,
        state_witness: StateIdWitness,
        bytes: Vec<u8>,
    },
}

struct CapturedActiveState {
    active: Option<state::Id>,
    proof: ActiveStateProof,
}

impl ActiveStateLease {
    pub(super) fn acquire(installation: &Installation) -> Result<Self, super::Error> {
        let coordinator = super::fixed_staging::lock_coordinator()?;
        Self::acquire_with_coordinator(installation, coordinator)
    }

    fn acquire_with_coordinator(
        installation: &Installation,
        coordinator: MutexGuard<'static, ()>,
    ) -> Result<Self, super::Error> {
        let captured = capture(installation)?;
        let actual = captured.active;
        let expected = installation.active_state;
        if actual != expected {
            return Err(super::Error::ActiveStateSnapshotChanged { expected, actual });
        }
        Ok(Self {
            active: actual,
            proof: captured.proof,
            _coordinator: coordinator,
        })
    }

    pub(super) fn active(&self) -> Option<state::Id> {
        self.active
    }

    /// Revalidate the exact installation-root, `/usr`, and `.stateID`
    /// namespace proof retained at acquisition. This detects uncoordinated
    /// replacement and ABA even when the original final inode is restored.
    pub(super) fn revalidate(&self, installation: &Installation) -> Result<(), super::Error> {
        revalidate_proof(self.active, &self.proof, installation)
    }

    pub(super) fn suspend(self, installation: &Installation) -> Result<ActiveStateSnapshot, super::Error> {
        self.revalidate(installation)?;
        let Self {
            active,
            proof,
            _coordinator,
        } = self;
        drop(_coordinator);
        Ok(ActiveStateSnapshot { active, proof })
    }
}

impl ActiveStateReservation {
    pub(super) fn acquire() -> Result<Self, super::Error> {
        Ok(Self {
            coordinator: super::fixed_staging::lock_coordinator()?,
        })
    }

    /// Perform authoritative live-state discovery only after startup recovery
    /// evidence has been inspected. The preliminary Installation observation
    /// remains a stale-detection witness rather than namespace authority; a
    /// mismatch still rejects a reused Installation clone.
    pub(super) fn discover_after_startup_gate(
        self,
        installation: &Installation,
        _startup_gate: &super::startup_gate::CleanSystemStartup,
    ) -> Result<ActiveStateLease, super::Error> {
        ActiveStateLease::acquire_with_coordinator(installation, self.coordinator)
    }
}

impl ActiveStateSnapshot {
    pub(super) fn resume(self, installation: &Installation) -> Result<ActiveStateLease, super::Error> {
        let coordinator = super::fixed_staging::lock_coordinator()?;
        revalidate_proof(self.active, &self.proof, installation)?;
        Ok(ActiveStateLease {
            active: self.active,
            proof: self.proof,
            _coordinator: coordinator,
        })
    }
}

fn revalidate_proof(
    active: Option<state::Id>,
    active_proof: &ActiveStateProof,
    installation: &Installation,
) -> Result<(), super::Error> {
    before_active_state_revalidation();
    if installation.active_state != active {
        return Err(super::Error::ActiveStateSnapshotChanged {
            expected: installation.active_state,
            actual: active,
        });
    }

    revalidate_root(
        installation,
        "revalidate installation root before retained active-state proof",
    )?;
    let expected_root = match active_proof {
        ActiveStateProof::MissingUsr { root }
        | ActiveStateProof::PresentBaseline { root, .. }
        | ActiveStateProof::Selected { root, .. } => *root,
    };
    require_root_witness(installation, expected_root)?;

    match active_proof {
        ActiveStateProof::MissingUsr { .. } => revalidate_missing_usr(installation)?,
        ActiveStateProof::PresentBaseline {
            usr,
            usr_witness,
            marker,
            ..
        } => revalidate_present_baseline(installation, usr, *usr_witness, marker.as_ref())?,
        ActiveStateProof::Selected {
            usr,
            usr_witness,
            state_id,
            state_witness,
            bytes,
            ..
        } => revalidate_selected_state(installation, usr, *usr_witness, state_id, *state_witness, bytes)?,
    }

    require_root_witness(installation, expected_root)?;
    revalidate_root(
        installation,
        "revalidate installation root after retained active-state proof",
    )
}

fn capture(installation: &Installation) -> Result<CapturedActiveState, super::Error> {
    revalidate_root(
        installation,
        "revalidate installation root before live active-state capture",
    )?;
    let root_witness = root_witness(installation)?;
    let usr_path = installation.root.join("usr");
    let Some(usr) = open_usr(installation, &usr_path)? else {
        revalidate_root(
            installation,
            "revalidate installation root during live /usr absence proof",
        )?;
        if open_usr(installation, &usr_path)?.is_some() {
            return Err(changed(&usr_path, "live /usr appeared during active-state capture"));
        }
        revalidate_root(
            installation,
            "revalidate installation root after live /usr absence proof",
        )?;
        require_root_witness(installation, root_witness)?;
        return Ok(CapturedActiveState {
            active: None,
            proof: ActiveStateProof::MissingUsr { root: root_witness },
        });
    };

    let usr_witness = directory_witness(&usr, &usr_path)?;
    require_usr_acl_policy(&usr, &usr_path)?;
    let state_path = usr_path.join(".stateID");
    let Some(state_probe) = probe_state_id(&usr, &state_path)? else {
        after_state_id_absence();
        require_directory_witness(&usr, &usr_path, usr_witness)?;
        if probe_state_id(&usr, &state_path)?.is_some() {
            return Err(changed(&state_path, "state ID appeared during absence proof"));
        }
        let marker_only = require_empty_or_marker_only(&usr, &usr_path)?;
        let retained_marker = if marker_only {
            let store = TreeMarkerStore::open(&usr, &usr_path).map_err(|source| {
                proof(
                    "retain marker-only first-install retry baseline",
                    &usr_path,
                    io::Error::other(source),
                )
            })?;
            let marker = store.read_for_recovery().map_err(|source| {
                proof(
                    "authenticate marker-only first-install retry baseline",
                    &usr_path,
                    io::Error::other(source),
                )
            })?;
            Some((store, marker))
        } else {
            None
        };
        after_baseline_layout_proof();
        let final_marker_only = require_empty_or_marker_only(&usr, &usr_path)?;
        if final_marker_only != marker_only {
            return Err(changed(
                &usr_path,
                "live /usr baseline layout changed during active-state capture",
            ));
        }
        if let Some((store, marker)) = retained_marker.as_ref() {
            marker.revalidate(&store).map_err(|source| {
                proof(
                    "revalidate marker-only first-install retry baseline",
                    &usr_path,
                    io::Error::other(source),
                )
            })?;
            if !require_empty_or_marker_only(&usr, &usr_path)? {
                return Err(changed(
                    &usr_path,
                    "marker-only live /usr became empty during final proof",
                ));
            }
        }
        require_directory_witness(&usr, &usr_path, usr_witness)?;
        require_named_usr(installation, &usr_path, usr_witness)?;
        revalidate_root(
            installation,
            "revalidate installation root after state-ID absence proof",
        )?;
        require_root_witness(installation, root_witness)?;
        return Ok(CapturedActiveState {
            active: None,
            proof: ActiveStateProof::PresentBaseline {
                root: root_witness,
                usr,
                usr_witness,
                marker: retained_marker,
            },
        });
    };

    let state_witness = state_id_witness(&state_probe, &state_path)?;
    let readable_state_id = linux_fs::open_path_descriptor_readonly(&state_probe).map_err(|source| {
        proof(
            "reopen validated live state ID through retained descriptor",
            &state_path,
            source,
        )
    })?;
    if state_id_witness(&readable_state_id, &state_path)? != state_witness {
        return Err(changed(&state_path, "readable state ID is not the probed inode"));
    }
    let bytes = read_state_id(&readable_state_id, &state_path, state_witness.length as usize)?;
    if state_id_witness(&state_probe, &state_path)? != state_witness {
        return Err(changed(&state_path, "retained state ID changed while first read"));
    }
    let active = parse_state_id(&bytes, &state_path)?;

    after_state_id_read();
    require_directory_witness(&usr, &usr_path, usr_witness)?;
    let named_probe = probe_state_id(&usr, &state_path)?
        .ok_or_else(|| changed(&state_path, "state ID disappeared before final-name proof"))?;
    if state_id_witness(&named_probe, &state_path)? != state_witness {
        return Err(changed(&state_path, "named state ID is not the retained inode"));
    }
    let named = linux_fs::open_path_descriptor_readonly(&named_probe).map_err(|source| {
        proof(
            "reopen final named state ID through retained descriptor",
            &state_path,
            source,
        )
    })?;
    if state_id_witness(&named, &state_path)? != state_witness {
        return Err(changed(
            &state_path,
            "readable named state ID is not the retained inode",
        ));
    }
    let named_bytes = read_state_id(&named, &state_path, state_witness.length as usize)?;
    if named_bytes != bytes || state_id_witness(&named, &state_path)? != state_witness {
        return Err(changed(&state_path, "named state ID changed during final proof"));
    }
    if state_id_witness(&state_probe, &state_path)? != state_witness {
        return Err(changed(&state_path, "retained state ID changed during final proof"));
    }
    require_named_usr(installation, &usr_path, usr_witness)?;
    revalidate_root(
        installation,
        "revalidate installation root after live active-state capture",
    )?;
    require_root_witness(installation, root_witness)?;
    Ok(CapturedActiveState {
        active: Some(active),
        proof: ActiveStateProof::Selected {
            root: root_witness,
            usr,
            usr_witness,
            state_id: state_probe,
            state_witness,
            bytes,
        },
    })
}

fn revalidate_root(installation: &Installation, operation: &'static str) -> Result<(), super::Error> {
    installation
        .revalidate_root_directory()
        .map_err(|source| proof(operation, &installation.root, io::Error::other(source)))
}

fn root_witness(installation: &Installation) -> Result<DirectoryWitness, super::Error> {
    let metadata = installation
        .root_directory()
        .metadata()
        .map_err(|source| proof("inspect retained installation root", &installation.root, source))?;
    if !metadata.file_type().is_dir() {
        return Err(changed(
            &installation.root,
            "retained installation root is no longer a directory",
        ));
    }
    Ok(DirectoryWitness {
        device: metadata.dev(),
        inode: metadata.ino(),
        owner: metadata.uid(),
        mode: metadata.permissions().mode() & 0o7777,
        links: metadata.nlink(),
        modified_seconds: metadata.mtime(),
        modified_nanoseconds: metadata.mtime_nsec(),
        changed_seconds: metadata.ctime(),
        changed_nanoseconds: metadata.ctime_nsec(),
    })
}

fn require_root_witness(installation: &Installation, expected: DirectoryWitness) -> Result<(), super::Error> {
    if root_witness(installation)? == expected {
        Ok(())
    } else {
        Err(changed(
            &installation.root,
            "installation-root metadata changed during retained active-state lease",
        ))
    }
}

fn open_usr(installation: &Installation, path: &Path) -> Result<Option<std::fs::File>, super::Error> {
    match linux_fs::openat2_file(
        installation.root_directory().as_raw_fd(),
        c"usr",
        nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK
            | nix::libc::O_NOATIME,
        0,
        linux_fs::controlled_resolution(),
    ) {
        Ok(usr) => Ok(Some(usr)),
        Err(source) if source.raw_os_error() == Some(nix::libc::ENOENT) => Ok(None),
        Err(source) => Err(proof("open live /usr through retained installation root", path, source)),
    }
}

fn require_named_usr(installation: &Installation, path: &Path, expected: DirectoryWitness) -> Result<(), super::Error> {
    let named = open_usr(installation, path)?
        .ok_or_else(|| changed(path, "live /usr disappeared during active-state capture"))?;
    require_directory_witness(&named, path, expected)?;
    require_usr_acl_policy(&named, path)
}

fn directory_witness(directory: &std::fs::File, path: &Path) -> Result<DirectoryWitness, super::Error> {
    let metadata = directory
        .metadata()
        .map_err(|source| proof("inspect live /usr directory", path, source))?;
    let mode = metadata.permissions().mode() & 0o7777;
    if !metadata.file_type().is_dir()
        || metadata.uid() != super::effective_user_id()
        || mode & 0o7000 != 0
        || mode & 0o700 != 0o700
        || mode & 0o022 != 0
    {
        return Err(proof(
            "validate live /usr directory policy",
            path,
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("unsafe live /usr directory (uid={}, mode={mode:04o})", metadata.uid()),
            ),
        ));
    }
    Ok(DirectoryWitness {
        device: metadata.dev(),
        inode: metadata.ino(),
        owner: metadata.uid(),
        mode,
        links: metadata.nlink(),
        modified_seconds: metadata.mtime(),
        modified_nanoseconds: metadata.mtime_nsec(),
        changed_seconds: metadata.ctime(),
        changed_nanoseconds: metadata.ctime_nsec(),
    })
}

fn require_directory_witness(
    directory: &std::fs::File,
    path: &Path,
    expected: DirectoryWitness,
) -> Result<(), super::Error> {
    if directory_witness(directory, path)? == expected {
        Ok(())
    } else {
        Err(changed(
            path,
            "live /usr directory metadata changed during active-state capture",
        ))
    }
}

fn require_usr_acl_policy(directory: &std::fs::File, path: &Path) -> Result<(), super::Error> {
    linux_fs::require_no_access_acl(directory, path)
        .map_err(|source| proof("reject live /usr access ACL", path, source))?;
    linux_fs::require_no_default_acl(directory, path)
        .map_err(|source| proof("reject live /usr default ACL", path, source))
}

fn probe_state_id(directory: &std::fs::File, path: &Path) -> Result<Option<std::fs::File>, super::Error> {
    match linux_fs::openat2_file(
        directory.as_raw_fd(),
        c".stateID",
        nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        linux_fs::controlled_resolution(),
    ) {
        Ok(file) => Ok(Some(file)),
        Err(source) if source.raw_os_error() == Some(nix::libc::ENOENT) => Ok(None),
        Err(source) => Err(proof("probe live state ID through retained /usr", path, source)),
    }
}

fn revalidate_missing_usr(installation: &Installation) -> Result<(), super::Error> {
    let usr_path = installation.root.join("usr");
    if open_usr(installation, &usr_path)?.is_some() {
        Err(changed(
            &usr_path,
            "live /usr appeared after the retained absence proof",
        ))
    } else {
        Ok(())
    }
}

fn revalidate_present_baseline(
    installation: &Installation,
    usr: &std::fs::File,
    usr_witness: DirectoryWitness,
    marker: Option<&(TreeMarkerStore, RetainedTreeMarker)>,
) -> Result<(), super::Error> {
    let usr_path = installation.root.join("usr");
    let state_path = usr_path.join(".stateID");
    require_directory_witness(usr, &usr_path, usr_witness)?;
    require_usr_acl_policy(usr, &usr_path)?;
    if probe_state_id(usr, &state_path)?.is_some() {
        return Err(changed(
            &state_path,
            "state ID appeared after the retained baseline proof",
        ));
    }

    let marker_only = require_empty_or_marker_only(usr, &usr_path)?;
    if marker_only != marker.is_some() {
        return Err(changed(
            &usr_path,
            "empty or marker-only live /usr baseline changed during retained lease",
        ));
    }
    if let Some((store, marker)) = marker {
        marker.revalidate(store).map_err(|source| {
            proof(
                "revalidate retained marker-only first-install baseline",
                &usr_path,
                io::Error::other(source),
            )
        })?;
    }

    require_directory_witness(usr, &usr_path, usr_witness)?;
    require_named_usr(installation, &usr_path, usr_witness)
}

fn revalidate_selected_state(
    installation: &Installation,
    usr: &std::fs::File,
    usr_witness: DirectoryWitness,
    state_id: &std::fs::File,
    state_witness: StateIdWitness,
    bytes: &[u8],
) -> Result<(), super::Error> {
    let usr_path = installation.root.join("usr");
    let state_path = usr_path.join(".stateID");
    require_directory_witness(usr, &usr_path, usr_witness)?;
    require_usr_acl_policy(usr, &usr_path)?;
    require_state_id_witness_and_bytes(state_id, &state_path, state_witness, bytes)?;

    let named_probe = probe_state_id(usr, &state_path)?
        .ok_or_else(|| changed(&state_path, "named state ID disappeared during retained lease"))?;
    if state_id_witness(&named_probe, &state_path)? != state_witness {
        return Err(changed(
            &state_path,
            "named state ID no longer denotes the retained inode",
        ));
    }
    require_state_id_witness_and_bytes(&named_probe, &state_path, state_witness, bytes)?;
    require_state_id_witness_and_bytes(state_id, &state_path, state_witness, bytes)?;
    require_named_usr(installation, &usr_path, usr_witness)
}

fn require_state_id_witness_and_bytes(
    state_id: &std::fs::File,
    path: &Path,
    expected: StateIdWitness,
    expected_bytes: &[u8],
) -> Result<(), super::Error> {
    if state_id_witness(state_id, path)? != expected {
        return Err(changed(path, "retained state-ID inode metadata changed"));
    }
    let readable = linux_fs::open_path_descriptor_readonly(state_id).map_err(|source| {
        proof(
            "reopen retained state ID through authenticated descriptor alias",
            path,
            source,
        )
    })?;
    if state_id_witness(&readable, path)? != expected {
        return Err(changed(path, "readable state-ID alias changed inode metadata"));
    }
    if read_state_id(&readable, path, expected.length as usize)? != expected_bytes {
        return Err(changed(path, "retained state-ID bytes changed"));
    }
    if state_id_witness(state_id, path)? != expected {
        return Err(changed(path, "retained state-ID inode changed while rereading"));
    }
    Ok(())
}

/// Return true only for the exact authenticated-marker retry layout. An
/// unrelated or nonempty `/usr` without `.stateID` is not equivalent to an
/// installation with no active state.
fn require_empty_or_marker_only(directory: &std::fs::File, path: &Path) -> Result<bool, super::Error> {
    // SAFETY: fcntl returns a fresh close-on-exec descriptor on success.
    let duplicate = unsafe { nix::libc::fcntl(directory.as_raw_fd(), nix::libc::F_DUPFD_CLOEXEC, 0) };
    if duplicate == -1 {
        return Err(proof(
            "duplicate live /usr for baseline enumeration",
            path,
            io::Error::last_os_error(),
        ));
    }
    // dup shares the directory offset. Rewind every scan so an earlier EOF
    // cannot turn a nonempty directory into an empty observation.
    // SAFETY: duplicate is a fresh live directory descriptor.
    if unsafe { nix::libc::lseek(duplicate, 0, nix::libc::SEEK_SET) } == -1 {
        let source = io::Error::last_os_error();
        // SAFETY: fdopendir has not consumed duplicate.
        unsafe { nix::libc::close(duplicate) };
        return Err(proof("rewind live /usr baseline enumeration", path, source));
    }
    // SAFETY: fdopendir consumes duplicate on success.
    let stream = unsafe { nix::libc::fdopendir(duplicate) };
    if stream.is_null() {
        let source = io::Error::last_os_error();
        // SAFETY: fdopendir failed and did not consume duplicate.
        unsafe { nix::libc::close(duplicate) };
        return Err(proof("enumerate live /usr baseline", path, source));
    }

    let mut marker_seen = false;
    let result = loop {
        // SAFETY: Linux exposes thread-local errno through this pointer.
        unsafe { *nix::libc::__errno_location() = 0 };
        // SAFETY: stream remains live and exclusively used here.
        let entry = unsafe { nix::libc::readdir(stream) };
        if entry.is_null() {
            let source = io::Error::last_os_error();
            break if source.raw_os_error() == Some(0) {
                Ok(marker_seen)
            } else {
                Err(proof("enumerate live /usr baseline", path, source))
            };
        }
        // SAFETY: d_name is NUL terminated for this live dirent.
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if matches!(name, b"." | b"..") {
            continue;
        }
        if name == b".cast-tree-id" && !marker_seen {
            marker_seen = true;
            continue;
        }
        break Err(proof(
            "validate empty or marker-only live /usr baseline",
            path,
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("live /usr has no state ID and contains foreign entry {name:?}"),
            ),
        ));
    };

    // SAFETY: stream came from fdopendir and remains live.
    let closed = unsafe { nix::libc::closedir(stream) };
    if closed == -1 && result.is_ok() {
        return Err(proof(
            "close live /usr baseline enumeration",
            path,
            io::Error::last_os_error(),
        ));
    }
    result
}

fn state_id_witness(file: &std::fs::File, path: &Path) -> Result<StateIdWitness, super::Error> {
    let metadata = file
        .metadata()
        .map_err(|source| proof("inspect live state ID", path, source))?;
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
    if metadata.file_type().is_file()
        && witness.owner == super::effective_user_id()
        && witness.mode == STATE_ID_MODE
        && witness.links == 1
        && (1..=MAX_STATE_ID_BYTES as u64).contains(&witness.length)
    {
        Ok(witness)
    } else {
        Err(proof(
            "validate live state-ID inode policy",
            path,
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "unsafe live state ID (uid={}, mode={:04o}, links={}, length={})",
                    witness.owner, witness.mode, witness.links, witness.length
                ),
            ),
        ))
    }
}

fn read_state_id(file: &std::fs::File, path: &Path, length: usize) -> Result<Vec<u8>, super::Error> {
    let mut bytes = vec![0; length];
    let mut filled = 0usize;
    let mut attempts = 0usize;
    while filled < bytes.len() {
        attempts += 1;
        if attempts > MAX_READ_ATTEMPTS {
            return Err(proof(
                "read complete live state ID",
                path,
                io::Error::other("state-ID read exceeded bounded retry limit"),
            ));
        }
        match file.read_at(&mut bytes[filled..], filled as u64) {
            Ok(0) => {
                return Err(proof(
                    "read complete live state ID",
                    path,
                    io::Error::from(io::ErrorKind::UnexpectedEof),
                ));
            }
            Ok(read) => filled += read,
            Err(source) if source.kind() == io::ErrorKind::Interrupted => {}
            Err(source) => return Err(proof("read live state ID", path, source)),
        }
    }

    let mut trailing = [0u8; 1];
    loop {
        attempts += 1;
        if attempts > MAX_READ_ATTEMPTS {
            return Err(proof(
                "read live state-ID bound",
                path,
                io::Error::other("state-ID read exceeded bounded retry limit"),
            ));
        }
        match file.read_at(&mut trailing, length as u64) {
            Ok(0) => return Ok(bytes),
            Ok(_) => {
                return Err(proof(
                    "enforce live state-ID read bound",
                    path,
                    io::Error::new(io::ErrorKind::InvalidData, "state ID contains trailing bytes"),
                ));
            }
            Err(source) if source.kind() == io::ErrorKind::Interrupted => {}
            Err(source) => return Err(proof("read live state-ID bound", path, source)),
        }
    }
}

fn parse_state_id(bytes: &[u8], path: &Path) -> Result<state::Id, super::Error> {
    let canonical = bytes[0] != b'0' && bytes.iter().all(u8::is_ascii_digit);
    let parsed = canonical
        .then(|| std::str::from_utf8(bytes).ok()?.parse::<i32>().ok())
        .flatten()
        .filter(|value| *value > 0);
    parsed.map(state::Id::from).ok_or_else(|| {
        proof(
            "parse canonical live state ID",
            path,
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("state ID is not one canonical positive decimal i32: {bytes:?}"),
            ),
        )
    })
}

fn proof(operation: &'static str, path: &Path, source: io::Error) -> super::Error {
    super::Error::LiveActiveStateProof {
        operation,
        path: path.to_owned(),
        source,
    }
}

fn changed(path: &Path, message: &'static str) -> super::Error {
    proof("revalidate live active-state snapshot", path, io::Error::other(message))
}

#[cfg(test)]
thread_local! {
    static BEFORE_ACTIVE_STATE_REVALIDATION_HOOKS:
        std::cell::RefCell<std::collections::VecDeque<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(std::collections::VecDeque::new()) };
    static AFTER_STATE_ID_READ_HOOK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static AFTER_STATE_ID_ABSENCE_HOOK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static AFTER_BASELINE_LAYOUT_PROOF_HOOK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(super) fn arm_before_active_state_revalidation(hook: impl FnOnce() + 'static) {
    BEFORE_ACTIVE_STATE_REVALIDATION_HOOKS.with(|slot| {
        let mut slot = slot.borrow_mut();
        assert!(slot.len() < 8, "too many active-state revalidation hooks armed");
        slot.push_back(Box::new(hook));
    });
}

#[cfg(test)]
pub(super) fn arm_after_state_id_read(hook: impl FnOnce() + 'static) {
    AFTER_STATE_ID_READ_HOOK.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
pub(super) fn arm_after_state_id_absence(hook: impl FnOnce() + 'static) {
    AFTER_STATE_ID_ABSENCE_HOOK.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
pub(super) fn arm_after_baseline_layout_proof(hook: impl FnOnce() + 'static) {
    AFTER_BASELINE_LAYOUT_PROOF_HOOK.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn before_active_state_revalidation() {
    BEFORE_ACTIVE_STATE_REVALIDATION_HOOKS.with(|slot| {
        if let Some(hook) = slot.borrow_mut().pop_front() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn before_active_state_revalidation() {}

#[cfg(test)]
fn after_state_id_read() {
    AFTER_STATE_ID_READ_HOOK.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn after_state_id_read() {}

#[cfg(test)]
fn after_state_id_absence() {
    AFTER_STATE_ID_ABSENCE_HOOK.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn after_state_id_absence() {}

#[cfg(test)]
fn after_baseline_layout_proof() {
    AFTER_BASELINE_LAYOUT_PROOF_HOOK.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn after_baseline_layout_proof() {}
