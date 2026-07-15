//! Retained capability for one external writable materialization root.

use std::{
    ffi::{CStr, OsString},
    io,
    os::{
        fd::AsRawFd as _,
        unix::{ffi::OsStringExt as _, fs::MetadataExt as _},
    },
    path::{Path, PathBuf},
};

use fs_err as fs;
use nix::{
    errno::Errno,
    sys::stat::{Mode, fchmod, mkdirat},
};

use super::{
    AssetDirectoryIdentity, AssetMaterialization, BlitExecution, Error, PendingFile, asset_directory_identity,
    asset_resolve_flags, blit_tree_into_open_root,
    candidate_metadata::{self, RetainedEphemeralUsr},
    effective_user_id, has_cast_control_topology, open_absolute_directory, openat2_frozen,
    require_disjoint_materialization_target, require_no_default_acl, require_single_component,
};
use crate::Installation;

#[cfg(test)]
thread_local! {
    static AFTER_PARENT_RETAINED_HOOK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static BEFORE_ABSENT_TARGET_CREATION_HOOK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static BEFORE_EXTERNAL_FILL_HOOK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static BEFORE_EXTERNAL_FINAL_PROOF_HOOK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

pub(super) fn reject_initial_materialization_symlink(requested: &Path) -> Result<(), Error> {
    match fs::symlink_metadata(requested) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(Error::UnsafeInitialMaterializationTarget {
            path: requested.to_owned(),
            owner: metadata.uid(),
            mode: metadata.mode() & 0o7777,
        }),
        Ok(_) => Ok(()),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(source.into()),
    }
}

fn require_parent_policy(directory: &fs::File, path: &Path) -> Result<AssetDirectoryIdentity, Error> {
    let metadata = directory.metadata()?;
    let mode = metadata.mode() & 0o7777;
    let safe = metadata.file_type().is_dir()
        && metadata.uid() == effective_user_id()
        && mode & 0o7022 == 0
        && mode & 0o700 == 0o700;
    if !safe {
        return Err(Error::UnsafeInitialMaterializationParent {
            path: path.to_owned(),
            owner: metadata.uid(),
            mode,
        });
    }

    crate::linux_fs::require_no_access_acl(directory.file(), path)?;
    require_no_default_acl(directory.file(), path)?;
    asset_directory_identity(directory)
}

fn require_target_policy(directory: &fs::File, path: &Path) -> Result<AssetDirectoryIdentity, Error> {
    let metadata = directory.metadata()?;
    let mode = metadata.mode() & 0o7777;
    let safe = metadata.file_type().is_dir()
        && metadata.uid() == effective_user_id()
        && mode & 0o7022 == 0
        && mode & 0o700 == 0o700;
    if !safe {
        return Err(Error::UnsafeInitialMaterializationTarget {
            path: path.to_owned(),
            owner: metadata.uid(),
            mode,
        });
    }

    crate::linux_fs::require_no_access_acl(directory.file(), path)?;
    require_no_default_acl(directory.file(), path)?;
    asset_directory_identity(directory)
}

/// Stable construction-time admission for one external target name.
///
/// The canonical parent identity and final component are retained separately.
/// Later preparation reopens that exact stored parent path without
/// recanonicalizing it and refuses a different inode.
#[derive(Clone, Debug)]
pub(super) struct ExternalMaterializationAdmission {
    path: PathBuf,
    parent_path: PathBuf,
    name: OsString,
    parent_identity: AssetDirectoryIdentity,
    target_identity: Option<AssetDirectoryIdentity>,
}

impl ExternalMaterializationAdmission {
    pub(super) fn admit(installation: &Installation, requested: &Path) -> Result<Self, Error> {
        reject_initial_materialization_symlink(requested)?;
        let path = require_disjoint_materialization_target(installation, requested)?;
        let parent_path = path.parent().ok_or(Error::EphemeralInstallationRoot)?.to_owned();
        let name = path.file_name().ok_or(Error::EphemeralInstallationRoot)?.to_owned();
        require_single_component(Path::new(&name))?;

        let parent = open_absolute_directory(&parent_path)?;
        let parent_identity = require_parent_policy(&parent, &parent_path)?;
        let target_identity = match open_target(&parent, &name) {
            Ok(target) => {
                let identity = require_target_policy(&target, &path)?;
                require_empty_directory(&target, &path)?;
                Some(identity)
            }
            Err(source) if source.kind() == io::ErrorKind::NotFound => None,
            Err(source) => return Err(source.into()),
        };
        let admission = Self {
            path,
            parent_path,
            name,
            parent_identity,
            target_identity,
        };
        admission.require_parent_named(installation, &parent)?;
        drop(admission.retain_admitted_target(&parent)?);
        admission.require_parent_named(installation, &parent)?;
        Ok(admission)
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    fn changed(&self) -> Error {
        Error::InitialMaterializationTargetChanged {
            path: self.path.clone(),
        }
    }

    fn require_parent_named(&self, installation: &Installation, retained: &fs::File) -> Result<(), Error> {
        require_admitted_topology(installation, self)?;
        let named = open_absolute_directory(&self.parent_path).map_err(|_| self.changed())?;
        if require_parent_policy(retained, &self.parent_path).map_err(|_| self.changed())? != self.parent_identity
            || require_parent_policy(&named, &self.parent_path).map_err(|_| self.changed())? != self.parent_identity
        {
            return Err(self.changed());
        }
        Ok(())
    }

    fn retain_admitted_target(&self, parent: &fs::File) -> Result<Option<fs::File>, Error> {
        match (self.target_identity, open_target(parent, &self.name)) {
            (None, Err(source)) if source.kind() == io::ErrorKind::NotFound => Ok(None),
            (Some(expected), Ok(target)) => {
                let current = require_target_policy(&target, &self.path).map_err(|_| self.changed())?;
                if current != expected || require_empty_directory(&target, &self.path).is_err() {
                    return Err(self.changed());
                }
                Ok(Some(target))
            }
            _ => Err(self.changed()),
        }
    }
}

fn require_admitted_topology(
    installation: &Installation,
    admission: &ExternalMaterializationAdmission,
) -> Result<(), Error> {
    installation.revalidate_root_directory()?;
    let installation_root = installation.root.canonicalize()?;
    if admission.path.starts_with(&installation_root)
        || installation_root.starts_with(&admission.path)
        || admission.path.ancestors().any(has_cast_control_topology)
    {
        return Err(Error::EphemeralInstallationRoot);
    }
    installation.revalidate_root_directory()?;
    Ok(())
}

fn open_target(parent: &fs::File, name: &OsString) -> io::Result<fs::File> {
    openat2_frozen(
        parent.as_raw_fd(),
        Path::new(name),
        nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK,
        asset_resolve_flags(),
    )
}

/// A direct external destination retained beneath one authenticated parent.
///
/// The final directory is either an exact empty inode retained before use or a
/// fresh 0700 directory created with no-replace `mkdirat`. Every later write,
/// chmod, and sync uses these descriptors; no recursive pathname cleanup is
/// available on this capability.
pub(super) struct RetainedExternalMaterializationTarget {
    admission: ExternalMaterializationAdmission,
    parent: fs::File,
    target: fs::File,
    target_identity: AssetDirectoryIdentity,
}

impl RetainedExternalMaterializationTarget {
    #[cfg(test)]
    pub(super) fn prepare(installation: &Installation, requested: &Path) -> Result<Self, Error> {
        let admission = ExternalMaterializationAdmission::admit(installation, requested)?;
        Self::prepare_from(installation, &admission)
    }

    pub(super) fn prepare_from(
        installation: &Installation,
        admission: &ExternalMaterializationAdmission,
    ) -> Result<Self, Error> {
        let parent = open_absolute_directory(&admission.parent_path).map_err(|_| admission.changed())?;
        if require_parent_policy(&parent, &admission.parent_path).map_err(|_| admission.changed())?
            != admission.parent_identity
        {
            return Err(admission.changed());
        }
        after_parent_retained();
        admission.require_parent_named(installation, &parent)?;
        let admitted_target = admission.retain_admitted_target(&parent)?;
        let mut capability = Self::open_or_create(admission.clone(), parent, admitted_target)?;
        capability.require_named(installation)?;
        require_empty_directory(&capability.target, &capability.admission.path)
            .map_err(|_| capability.admission.changed())?;
        capability.target_identity = require_target_policy(&capability.target, &capability.admission.path)
            .map_err(|_| capability.admission.changed())?;
        capability.require_named(installation)?;
        Ok(capability)
    }

    fn open_or_create(
        admission: ExternalMaterializationAdmission,
        parent: fs::File,
        admitted_target: Option<fs::File>,
    ) -> Result<Self, Error> {
        let open = || open_target(&parent, &admission.name);

        let (target, created) = match (admission.target_identity, admitted_target) {
            (Some(_), Some(target)) => (target, false),
            (None, None) => {
                before_absent_target_creation();
                loop {
                    match mkdirat(
                        parent.as_raw_fd(),
                        admission.name.as_os_str(),
                        Mode::from_bits_truncate(0o700),
                    ) {
                        Ok(()) => break,
                        Err(Errno::EINTR) => continue,
                        Err(Errno::EEXIST) => return Err(admission.changed()),
                        Err(source) => return Err(Error::Blit(source)),
                    }
                }
                parent.sync_all()?;
                (open().map_err(|_| admission.changed())?, true)
            }
            _ => return Err(admission.changed()),
        };

        if created {
            let metadata = target.metadata().map_err(|_| admission.changed())?;
            let mode = metadata.mode() & 0o7777;
            if !metadata.file_type().is_dir() || metadata.uid() != effective_user_id() || mode & !0o700 != 0 {
                return Err(admission.changed());
            }
            fchmod(target.as_raw_fd(), Mode::from_bits_truncate(0o700))?;
        }
        let target_identity = require_target_policy(&target, &admission.path).map_err(|_| admission.changed())?;

        Ok(Self {
            admission,
            parent,
            target,
            target_identity,
        })
    }

    fn require_named(&self, installation: &Installation) -> Result<(), Error> {
        self.admission.require_parent_named(installation, &self.parent)?;
        let named_target = open_target(&self.parent, &self.admission.name).map_err(|_| self.admission.changed())?;
        if require_target_policy(&self.target, &self.admission.path).map_err(|_| self.admission.changed())?
            != self.target_identity
            || require_target_policy(&named_target, &self.admission.path).map_err(|_| self.admission.changed())?
                != self.target_identity
        {
            return Err(self.admission.changed());
        }
        Ok(())
    }

    pub(super) fn materialize(
        &mut self,
        installation: &Installation,
        tree: &vfs::Tree<PendingFile>,
        materialization: AssetMaterialization,
        execution: BlitExecution,
    ) -> Result<RetainedEphemeralUsr, Error> {
        if materialization != AssetMaterialization::IndependentCopy {
            return Err(Error::FixedStagingCapabilityRequired {
                operation: "hardlink an external materialization target",
            });
        }
        before_external_fill();
        self.require_named(installation)?;
        require_empty_directory(&self.target, &self.admission.path).map_err(|_| self.admission.changed())?;
        fchmod(self.target.as_raw_fd(), Mode::from_bits_truncate(0o700))?;
        self.target_identity =
            require_target_policy(&self.target, &self.admission.path).map_err(|_| self.admission.changed())?;
        self.require_named(installation)?;
        let mut candidate_usr = candidate_metadata::retain_ephemeral_usr(self.target.file(), &self.admission.path)?;
        candidate_usr.revalidate_under(self.target.file())?;
        self.require_named(installation)?;

        blit_tree_into_open_root(
            installation,
            tree,
            self.target.as_raw_fd(),
            AssetMaterialization::IndependentCopy,
            execution,
            None,
            None,
            Some(candidate_usr.file()),
        )?;
        candidate_usr.refresh_after_materialization(self.target.file())?;
        self.require_named(installation)?;

        before_external_final_proof();
        fchmod(self.target.as_raw_fd(), Mode::from_bits_truncate(0o755))?;
        self.target.sync_all()?;
        self.parent.sync_all()?;
        self.target_identity =
            require_target_policy(&self.target, &self.admission.path).map_err(|_| self.admission.changed())?;
        self.require_named(installation)?;
        candidate_usr.revalidate_under(self.target.file())?;
        self.require_named(installation)?;
        Ok(candidate_usr)
    }

    pub(super) fn revalidate_candidate_usr(
        &self,
        installation: &Installation,
        candidate_usr: &RetainedEphemeralUsr,
    ) -> Result<(), Error> {
        self.require_named(installation)?;
        candidate_usr.revalidate_under(self.target.file())?;
        self.require_named(installation)
    }

    pub(super) fn create_root_abi(
        &self,
        installation: &Installation,
        candidate_usr: &RetainedEphemeralUsr,
    ) -> Result<super::RetainedRootAbi, Error> {
        self.revalidate_candidate_usr(installation, candidate_usr)?;
        let root_abi = super::create_root_links_retained(&self.admission.path, self.target.file())?;
        self.revalidate_candidate_usr(installation, candidate_usr)?;
        root_abi.revalidate()?;
        Ok(root_abi)
    }

    pub(super) fn path(&self) -> &Path {
        &self.admission.path
    }
}

#[cfg(test)]
pub(super) fn arm_after_parent_retained(hook: impl FnOnce() + 'static) {
    AFTER_PARENT_RETAINED_HOOK.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
pub(super) fn arm_before_absent_target_creation(hook: impl FnOnce() + 'static) {
    BEFORE_ABSENT_TARGET_CREATION_HOOK.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
pub(super) fn arm_before_external_fill(hook: impl FnOnce() + 'static) {
    BEFORE_EXTERNAL_FILL_HOOK.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
pub(super) fn arm_before_external_final_proof(hook: impl FnOnce() + 'static) {
    BEFORE_EXTERNAL_FINAL_PROOF_HOOK.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn after_parent_retained() {
    AFTER_PARENT_RETAINED_HOOK.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn after_parent_retained() {}

#[cfg(test)]
fn before_absent_target_creation() {
    BEFORE_ABSENT_TARGET_CREATION_HOOK.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn before_absent_target_creation() {}

#[cfg(test)]
fn before_external_fill() {
    BEFORE_EXTERNAL_FILL_HOOK.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn before_external_fill() {}

#[cfg(test)]
fn before_external_final_proof() {
    BEFORE_EXTERNAL_FINAL_PROOF_HOOK.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn before_external_final_proof() {}

fn require_empty_directory(directory: &fs::File, path: &Path) -> Result<(), Error> {
    if first_directory_entry(directory)?.is_some() {
        let metadata = directory.metadata()?;
        return Err(Error::UnsafeInitialMaterializationTarget {
            path: path.to_owned(),
            owner: metadata.uid(),
            mode: metadata.mode() & 0o7777,
        });
    }
    Ok(())
}

fn first_directory_entry(directory: &fs::File) -> io::Result<Option<OsString>> {
    // SAFETY: fcntl duplicates one live directory descriptor and gives this
    // function exclusive ownership of the duplicate.
    let duplicate = unsafe { nix::libc::fcntl(directory.as_raw_fd(), nix::libc::F_DUPFD_CLOEXEC, 0) };
    if duplicate == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the duplicate is live and not shared with another DIR stream.
    if unsafe { nix::libc::lseek(duplicate, 0, nix::libc::SEEK_SET) } == -1 {
        let source = io::Error::last_os_error();
        // SAFETY: fdopendir has not consumed the duplicate.
        unsafe { nix::libc::close(duplicate) };
        return Err(source);
    }
    // SAFETY: fdopendir consumes the fresh descriptor on success.
    let stream = unsafe { nix::libc::fdopendir(duplicate) };
    if stream.is_null() {
        let source = io::Error::last_os_error();
        // SAFETY: fdopendir failed and did not consume the duplicate.
        unsafe { nix::libc::close(duplicate) };
        return Err(source);
    }

    let result = loop {
        // SAFETY: Linux exposes thread-local errno through this pointer.
        unsafe { *nix::libc::__errno_location() = 0 };
        // SAFETY: stream remains live and exclusively used in this function.
        let entry = unsafe { nix::libc::readdir(stream) };
        if entry.is_null() {
            let source = io::Error::last_os_error();
            break if source.raw_os_error() == Some(0) {
                Ok(None)
            } else {
                Err(source)
            };
        }
        // SAFETY: d_name is NUL-terminated for this live dirent.
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if matches!(name, b"." | b"..") {
            continue;
        }
        break Ok(Some(OsString::from_vec(name.to_vec())));
    };

    // SAFETY: stream was returned by fdopendir and remains live.
    let closed = unsafe { nix::libc::closedir(stream) };
    if closed == -1 && result.is_ok() {
        return Err(io::Error::last_os_error());
    }
    result
}
