//! Retained capability boundary for Cast's fixed stateful staging wrapper.

use std::{
    ffi::{CStr, CString, OsString},
    io,
    os::{
        fd::AsRawFd as _,
        unix::{
            ffi::OsStringExt as _,
            fs::{MetadataExt as _, PermissionsExt as _},
        },
    },
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard},
};

use thiserror::Error as ThisError;

use super::{AssetMaterialization, BlitExecution, Client, PendingFile, Scope, blit_tree_into_open_root};
use crate::{Installation, linux_fs, package};

static FIXED_STAGING_COORDINATOR: Mutex<()> = Mutex::new(());

const ROOTS_RELATIVE: &CStr = c".cast/root";
const STAGING_NAME: &CStr = c"staging";
const PRIVATE_WRAPPER_MODE: u32 = 0o700;
const LEGACY_WRAPPER_MODE: u32 = 0o755;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DirectoryWitness {
    device: u64,
    inode: u64,
    owner: u32,
    mode: u32,
}

impl DirectoryWitness {
    fn from(file: &std::fs::File, path: &Path, operation: &'static str) -> Result<Self, FixedStagingError> {
        let metadata = file.metadata().map_err(|source| FixedStagingError::Io {
            operation,
            path: path.to_owned(),
            source,
        })?;
        Ok(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            owner: metadata.uid(),
            mode: metadata.permissions().mode() & 0o7777,
        })
    }
}

/// One exact empty fixed-staging wrapper retained beneath the authenticated
/// installation root.
///
/// Materialization writes through `staging`, never by reopening its pathname.
/// The diagnostic paths are used only for errors and exact-name revalidation.
pub(super) struct RetainedFixedStaging {
    installation_root: std::fs::File,
    roots: std::fs::File,
    roots_path: PathBuf,
    roots_witness: DirectoryWitness,
    staging: std::fs::File,
    staging_path: PathBuf,
    staging_witness: DirectoryWitness,
}

/// A stateful candidate and the cooperating-writer lease acquired before its
/// first possible staging mutation.
///
/// This token is deliberately non-cloneable and is consumed only after the
/// state row and durable tree identities have been prepared.
pub(super) struct StatefulCandidate {
    pub(super) tree: vfs::Tree<PendingFile>,
    pub(super) staging: RetainedFixedStaging,
    pub(super) candidate_usr: std::fs::File,
    pub(super) local_etc: super::transaction_root::RetainedLocalEtc,
    pub(super) active_state: super::active_state_authority::ActiveStateAuthority,
}

#[derive(Debug, ThisError)]
pub(super) enum FixedStagingError {
    #[error("stateful candidate materialization requires a stateful client")]
    StatefulClientRequired,
    #[error("revalidate the installation root during fixed-staging preparation")]
    RevalidateInstallation(#[source] crate::installation::Error),
    #[error("fixed staging is missing after installation topology provisioning: {path:?}")]
    MissingStaging { path: PathBuf },
    #[error("{operation} at {path:?}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(
        "unsafe fixed-staging parent {path:?}: expected an owner-controlled ACL-free directory, found uid={owner}, mode={mode:04o}"
    )]
    UnsafeRoots { path: PathBuf, owner: u32, mode: u32 },
    #[error(
        "unsafe fixed staging {path:?}: expected an owner-owned ACL-free 0700 directory (or exact empty legacy 0755), found uid={owner}, mode={mode:04o}"
    )]
    UnsafeStaging { path: PathBuf, owner: u32, mode: u32 },
    #[error("fixed-staging directory has a POSIX ACL: {path:?}")]
    Acl {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("fixed staging contains crash or foreign evidence and was left untouched: {path:?}, first entry {entry:?}")]
    NotEmpty { path: PathBuf, entry: OsString },
    #[error("fixed-staging capability or final name changed during preparation: {path:?}")]
    Changed { path: PathBuf },
    #[error("materialize candidate through retained fixed-staging descriptor at {path:?}")]
    Materialize {
        path: PathBuf,
        #[source]
        source: Box<super::Error>,
    },
    #[error(
        "candidate materialization failed at {path:?} ({primary}) and fixed-staging name revalidation also failed ({revalidation})"
    )]
    MaterializeAndRevalidate {
        path: PathBuf,
        primary: Box<super::Error>,
        revalidation: Box<FixedStagingError>,
    },
}

impl RetainedFixedStaging {
    pub(super) fn directory(&self) -> &std::fs::File {
        &self.staging
    }

    pub(super) fn path(&self) -> &Path {
        &self.staging_path
    }

    /// Retain an exact empty fixed wrapper without unlinking or recreating it.
    ///
    /// A legacy 0755 wrapper is normalized only after two exact empty/name
    /// proofs. Nonempty or substituted evidence is rejected before chmod.
    pub(super) fn prepare_empty(installation: &Installation) -> Result<Self, FixedStagingError> {
        installation
            .revalidate_root_directory()
            .map_err(FixedStagingError::RevalidateInstallation)?;

        let installation_root = installation
            .root_directory()
            .try_clone()
            .map_err(|source| FixedStagingError::Io {
                operation: "retain authenticated installation root",
                path: installation.root.clone(),
                source,
            })?;
        let roots_path = installation.root_path("");
        let roots = open_directory(
            installation.root_directory(),
            ROOTS_RELATIVE,
            &roots_path,
            "open retained fixed-staging parent",
        )?;
        let roots_witness = require_roots_policy(&roots, &roots_path)?;

        let staging_path = installation.staging_dir();
        let staging = match open_directory(&roots, STAGING_NAME, &staging_path, "open retained fixed staging") {
            Ok(staging) => staging,
            Err(FixedStagingError::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
                return Err(FixedStagingError::MissingStaging { path: staging_path });
            }
            Err(source) => return Err(source),
        };
        let staging_witness = require_staging_policy(&staging, &staging_path, true)?;
        require_empty(&staging, &staging_path)?;

        let mut retained = Self {
            installation_root,
            roots,
            roots_path,
            roots_witness,
            staging,
            staging_path,
            staging_witness,
        };

        before_staging_baseline_revalidation();
        retained.require_named(installation, true)?;
        require_empty(&retained.staging, &retained.staging_path)?;

        if retained.staging_witness.mode == LEGACY_WRAPPER_MODE {
            before_legacy_staging_normalization();
            retained.require_named(installation, true)?;
            require_empty(&retained.staging, &retained.staging_path)?;
            linux_fs::chmod_path_descriptor(&retained.staging, PRIVATE_WRAPPER_MODE).map_err(|source| {
                FixedStagingError::Io {
                    operation: "normalize exact empty legacy fixed staging",
                    path: retained.staging_path.clone(),
                    source,
                }
            })?;
            retained.staging.sync_all().map_err(|source| FixedStagingError::Io {
                operation: "sync normalized fixed staging",
                path: retained.staging_path.clone(),
                source,
            })?;
            retained.roots.sync_all().map_err(|source| FixedStagingError::Io {
                operation: "sync fixed-staging parent after legacy normalization",
                path: retained.roots_path.clone(),
                source,
            })?;

            let normalized = open_directory(
                &retained.roots,
                STAGING_NAME,
                &retained.staging_path,
                "reopen normalized fixed staging",
            )?;
            let normalized_witness = require_staging_policy(&normalized, &retained.staging_path, false)?;
            if (normalized_witness.device, normalized_witness.inode)
                != (retained.staging_witness.device, retained.staging_witness.inode)
            {
                return Err(FixedStagingError::Changed {
                    path: retained.staging_path,
                });
            }
            require_empty(&normalized, &retained.staging_path)?;
            retained.staging = normalized;
            retained.staging_witness = normalized_witness;
        }

        retained.require_named(installation, false)?;
        require_empty(&retained.staging, &retained.staging_path)?;
        Ok(retained)
    }

    pub(super) fn materialize(
        &self,
        installation: &Installation,
        tree: &vfs::Tree<PendingFile>,
        materialization: AssetMaterialization,
        execution: BlitExecution,
    ) -> Result<std::fs::File, FixedStagingError> {
        self.require_named(installation, false)?;
        require_empty(&self.staging, &self.staging_path)?;
        before_fixed_staging_fill();
        self.require_named(installation, false)?;
        require_empty(&self.staging, &self.staging_path)?;

        let (temporary_name, temporary_path, candidate_usr) = self.create_private_candidate_usr()?;
        let primary = blit_tree_into_open_root(
            installation,
            tree,
            self.staging.as_raw_fd(),
            materialization,
            execution,
            None,
            None,
            Some(&candidate_usr),
        );
        after_fixed_staging_fill();
        let revalidation = self.require_named(installation, false);

        match (primary, revalidation) {
            (Ok(()), Ok(())) => {}
            (Err(source), Ok(())) => Err(FixedStagingError::Materialize {
                path: self.staging_path.clone(),
                source: Box::new(source),
            })?,
            (Ok(()), Err(revalidation)) => Err(revalidation)?,
            (Err(primary), Err(revalidation)) => Err(FixedStagingError::MaterializeAndRevalidate {
                path: self.staging_path.clone(),
                primary: Box::new(primary),
                revalidation: Box::new(revalidation),
            })?,
        }

        self.publish_candidate_usr(&candidate_usr, &temporary_name, &temporary_path)?;
        self.require_candidate_usr_named(&candidate_usr)?;
        self.require_named(installation, false)?;
        Ok(candidate_usr)
    }

    /// Revalidate only the retained wrapper and its names. Candidate contents
    /// are intentionally opaque after materialization.
    pub(super) fn revalidate(&self, installation: &Installation) -> Result<(), FixedStagingError> {
        installation
            .revalidate_root_directory()
            .map_err(FixedStagingError::RevalidateInstallation)?;
        self.require_named(installation, false)
    }

    fn require_named(&self, installation: &Installation, allow_legacy: bool) -> Result<(), FixedStagingError> {
        installation
            .revalidate_root_directory()
            .map_err(FixedStagingError::RevalidateInstallation)?;
        let descendant_result = (|| {
            let named_roots = open_directory(
                &self.installation_root,
                ROOTS_RELATIVE,
                &self.roots_path,
                "reopen fixed-staging parent name",
            )?;
            let live_roots = require_roots_policy(&named_roots, &self.roots_path)?;
            if live_roots != self.roots_witness {
                Err(FixedStagingError::Changed {
                    path: self.roots_path.clone(),
                })
            } else {
                let named_staging = open_directory(
                    &self.roots,
                    STAGING_NAME,
                    &self.staging_path,
                    "reopen fixed-staging final name",
                )?;
                let named_witness = require_staging_policy(&named_staging, &self.staging_path, allow_legacy)?;
                let retained_witness = require_staging_policy(&self.staging, &self.staging_path, allow_legacy)?;
                if named_witness != retained_witness || retained_witness != self.staging_witness {
                    Err(FixedStagingError::Changed {
                        path: self.staging_path.clone(),
                    })
                } else {
                    Ok(())
                }
            }
        })();
        let root_result = installation
            .revalidate_root_directory()
            .map_err(FixedStagingError::RevalidateInstallation);
        descendant_result?;
        root_result
    }

    fn create_private_candidate_usr(&self) -> Result<(CString, PathBuf, std::fs::File), FixedStagingError> {
        let mut random = [0_u8; 16];
        loop {
            // SAFETY: getrandom receives a complete writable byte buffer.
            let read = unsafe { nix::libc::getrandom(random.as_mut_ptr().cast(), random.len(), 0) };
            if read == random.len() as isize {
                break;
            }
            let source = io::Error::last_os_error();
            if source.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(FixedStagingError::Io {
                operation: "generate private candidate /usr name",
                path: self.staging_path.clone(),
                source,
            });
        }
        let temporary_name = CString::new(format!(".cast-usr-{:032x}.tmp", u128::from_ne_bytes(random)))
            .expect("formatted random candidate /usr name contains no NUL");
        let temporary_path = self.staging_path.join(temporary_name.to_string_lossy().as_ref());
        // SAFETY: the retained staging descriptor and private component remain
        // live. mkdirat neither follows nor replaces the temporary name.
        if unsafe { nix::libc::mkdirat(self.staging.as_raw_fd(), temporary_name.as_ptr(), 0o700) } != 0 {
            return Err(FixedStagingError::Io {
                operation: "create private retained candidate /usr",
                path: temporary_path,
                source: io::Error::last_os_error(),
            });
        }
        let candidate_usr = open_directory(
            &self.staging,
            &temporary_name,
            &temporary_path,
            "retain private candidate /usr",
        )?;
        linux_fs::chmod_path_descriptor(&candidate_usr, 0o755).map_err(|source| FixedStagingError::Io {
            operation: "normalize private retained candidate /usr",
            path: temporary_path.clone(),
            source,
        })?;
        require_no_acl(&candidate_usr, &temporary_path)?;
        candidate_usr.sync_all().map_err(|source| FixedStagingError::Io {
            operation: "sync private retained candidate /usr",
            path: temporary_path.clone(),
            source,
        })?;
        require_no_acl(&candidate_usr, &temporary_path)?;
        self.require_child_named(&candidate_usr, &temporary_name, &temporary_path)?;
        Ok((temporary_name, temporary_path, candidate_usr))
    }

    fn publish_candidate_usr(
        &self,
        candidate_usr: &std::fs::File,
        temporary_name: &CStr,
        temporary_path: &Path,
    ) -> Result<(), FixedStagingError> {
        before_candidate_usr_publication();
        require_no_acl(candidate_usr, temporary_path)?;
        self.require_child_named(candidate_usr, temporary_name, temporary_path)?;

        // Never retry this syscall. An interrupted/error return may still
        // describe an applied rename; reconcile both names exactly once.
        // SAFETY: both directory descriptors and C strings remain live.
        let renamed = unsafe {
            nix::libc::syscall(
                nix::libc::SYS_renameat2,
                self.staging.as_raw_fd(),
                temporary_name.as_ptr(),
                self.staging.as_raw_fd(),
                c"usr".as_ptr(),
                nix::libc::RENAME_NOREPLACE,
            )
        };
        let rename_error = (renamed == -1).then(io::Error::last_os_error);
        let final_path = self.staging_path.join("usr");
        match self.require_child_named(candidate_usr, c"usr", &final_path) {
            Ok(()) => {}
            Err(final_error) => {
                if self
                    .require_child_named(candidate_usr, temporary_name, temporary_path)
                    .is_ok()
                {
                    return Err(FixedStagingError::Io {
                        operation: "publish retained candidate /usr without replacement",
                        path: final_path,
                        source: rename_error.unwrap_or_else(|| io::Error::other(final_error.to_string())),
                    });
                }
                return Err(final_error);
            }
        }
        self.staging.sync_all().map_err(|source| FixedStagingError::Io {
            operation: "sync fixed staging after candidate /usr publication",
            path: self.staging_path.clone(),
            source,
        })?;
        require_no_acl(candidate_usr, &final_path)?;
        Ok(())
    }

    fn require_child_named(&self, retained: &std::fs::File, name: &CStr, path: &Path) -> Result<(), FixedStagingError> {
        let expected =
            super::state_metadata_directory_witness(retained, path).map_err(|source| FixedStagingError::Io {
                operation: "authenticate retained candidate child",
                path: path.to_owned(),
                source,
            })?;
        let named = open_directory(&self.staging, name, path, "reopen retained candidate child name")?;
        let actual = super::state_metadata_directory_witness(&named, path).map_err(|source| FixedStagingError::Io {
            operation: "authenticate named candidate child",
            path: path.to_owned(),
            source,
        })?;
        if actual == expected {
            Ok(())
        } else {
            Err(FixedStagingError::Changed { path: path.to_owned() })
        }
    }

    fn require_candidate_usr_named(&self, candidate_usr: &std::fs::File) -> Result<(), FixedStagingError> {
        let path = self.staging_path.join("usr");
        let retained =
            super::state_metadata_directory_witness(candidate_usr, &path).map_err(|source| FixedStagingError::Io {
                operation: "authenticate retained candidate /usr",
                path: path.clone(),
                source,
            })?;
        let named = open_directory(&self.staging, c"usr", &path, "reopen retained candidate /usr name")?;
        let named_witness =
            super::state_metadata_directory_witness(&named, &path).map_err(|source| FixedStagingError::Io {
                operation: "authenticate named candidate /usr",
                path: path.clone(),
                source,
            })?;
        if named_witness != retained {
            return Err(FixedStagingError::Changed { path });
        }
        Ok(())
    }
}

impl Client {
    pub(super) fn materialize_stateful_candidate<'a>(
        &self,
        packages: impl IntoIterator<Item = &'a package::Id>,
    ) -> Result<StatefulCandidate, super::Error> {
        self.require_non_frozen()?;
        if !matches!(&self.scope, Scope::Stateful) {
            return Err(super::Error::StatefulCandidateMaterialization {
                source: Box::new(FixedStagingError::StatefulClientRequired),
            });
        }

        let local_etc = super::transaction_root::prepare_local_etc(&self.installation)?;
        let active_state = super::active_state_authority::ActiveStateAuthority::acquire(&self.installation)?;
        self.materialize_stateful_candidate_with_authority(packages, local_etc, active_state)
    }

    pub(super) fn materialize_active_verify_candidate<'a>(
        &self,
        packages: impl IntoIterator<Item = &'a package::Id>,
        active_state: super::active_state_authority::ActiveStateAuthority,
    ) -> Result<StatefulCandidate, super::Error> {
        if active_state.active().is_none() {
            return Err(super::Error::NoActiveState);
        }
        let local_etc = super::transaction_root::require_local_etc(&self.installation)?;
        active_state.revalidate(&self.installation)?;
        self.materialize_stateful_candidate_with_authority(packages, local_etc, active_state)
    }

    fn materialize_stateful_candidate_with_authority<'a>(
        &self,
        packages: impl IntoIterator<Item = &'a package::Id>,
        local_etc: super::transaction_root::RetainedLocalEtc,
        active_state: super::active_state_authority::ActiveStateAuthority,
    ) -> Result<StatefulCandidate, super::Error> {
        self.require_non_frozen()?;
        if !matches!(&self.scope, Scope::Stateful) {
            return Err(super::Error::StatefulCandidateMaterialization {
                source: Box::new(FixedStagingError::StatefulClientRequired),
            });
        }
        active_state.revalidate(&self.installation)?;
        local_etc.revalidate(&self.installation)?;
        let tree = self.vfs(packages)?;
        let staging = RetainedFixedStaging::prepare_empty(&self.installation).map_err(|source| {
            super::Error::StatefulCandidateMaterialization {
                source: Box::new(source),
            }
        })?;
        let candidate_usr = staging
            .materialize(
                &self.installation,
                &tree,
                AssetMaterialization::IndependentCopy,
                BlitExecution::Parallel,
            )
            .map_err(|source| super::Error::StatefulCandidateMaterialization {
                source: Box::new(source),
            })?;
        active_state.revalidate(&self.installation)?;
        local_etc.revalidate(&self.installation)?;

        Ok(StatefulCandidate {
            tree,
            staging,
            candidate_usr,
            local_etc,
            active_state,
        })
    }
}

pub(super) fn lock_coordinator() -> Result<MutexGuard<'static, ()>, super::Error> {
    before_coordinator_lock();
    FIXED_STAGING_COORDINATOR
        .lock()
        .map_err(|_| super::Error::FixedStagingCoordinatorPoisoned)
}

fn open_directory(
    parent: &std::fs::File,
    name: &CStr,
    path: &Path,
    operation: &'static str,
) -> Result<std::fs::File, FixedStagingError> {
    linux_fs::openat2_file(
        parent.as_raw_fd(),
        name,
        nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK,
        0,
        linux_fs::controlled_resolution(),
    )
    .map_err(|source| FixedStagingError::Io {
        operation,
        path: path.to_owned(),
        source,
    })
}

fn require_roots_policy(file: &std::fs::File, path: &Path) -> Result<DirectoryWitness, FixedStagingError> {
    let witness = DirectoryWitness::from(file, path, "inspect retained fixed-staging parent")?;
    if witness.owner != unsafe { nix::libc::geteuid() }
        || witness.mode & 0o7000 != 0
        || witness.mode & 0o022 != 0
        || witness.mode & 0o700 != 0o700
    {
        return Err(FixedStagingError::UnsafeRoots {
            path: path.to_owned(),
            owner: witness.owner,
            mode: witness.mode,
        });
    }
    require_no_acl(file, path)?;
    Ok(witness)
}

fn require_staging_policy(
    file: &std::fs::File,
    path: &Path,
    allow_legacy: bool,
) -> Result<DirectoryWitness, FixedStagingError> {
    let witness = DirectoryWitness::from(file, path, "inspect retained fixed staging")?;
    let admitted = witness.mode == PRIVATE_WRAPPER_MODE || (allow_legacy && witness.mode == LEGACY_WRAPPER_MODE);
    if witness.owner != unsafe { nix::libc::geteuid() } || !admitted {
        return Err(FixedStagingError::UnsafeStaging {
            path: path.to_owned(),
            owner: witness.owner,
            mode: witness.mode,
        });
    }
    require_no_acl(file, path)?;
    Ok(witness)
}

fn require_no_acl(file: &std::fs::File, path: &Path) -> Result<(), FixedStagingError> {
    linux_fs::require_no_access_acl(file, path).map_err(|source| FixedStagingError::Acl {
        path: path.to_owned(),
        source,
    })?;
    linux_fs::require_no_default_acl(file, path).map_err(|source| FixedStagingError::Acl {
        path: path.to_owned(),
        source,
    })
}

fn require_empty(file: &std::fs::File, path: &Path) -> Result<(), FixedStagingError> {
    if let Some(entry) = first_directory_entry(file, path)? {
        return Err(FixedStagingError::NotEmpty {
            path: path.to_owned(),
            entry,
        });
    }
    Ok(())
}

fn first_directory_entry(file: &std::fs::File, path: &Path) -> Result<Option<OsString>, FixedStagingError> {
    // SAFETY: fcntl receives one live directory descriptor and returns a new
    // close-on-exec descriptor on success.
    let duplicate = unsafe { nix::libc::fcntl(file.as_raw_fd(), nix::libc::F_DUPFD_CLOEXEC, 0) };
    if duplicate == -1 {
        return Err(FixedStagingError::Io {
            operation: "duplicate fixed staging for bounded inventory",
            path: path.to_owned(),
            source: io::Error::last_os_error(),
        });
    }
    // SAFETY: the fresh descriptor is uniquely owned until fdopendir consumes
    // it. Reset the shared directory offset before every bounded scan.
    if unsafe { nix::libc::lseek(duplicate, 0, nix::libc::SEEK_SET) } == -1 {
        let source = io::Error::last_os_error();
        // SAFETY: fdopendir has not consumed the descriptor.
        unsafe { nix::libc::close(duplicate) };
        return Err(FixedStagingError::Io {
            operation: "rewind fixed-staging inventory",
            path: path.to_owned(),
            source,
        });
    }
    // SAFETY: fdopendir consumes the fresh descriptor on success.
    let stream = unsafe { nix::libc::fdopendir(duplicate) };
    if stream.is_null() {
        let source = io::Error::last_os_error();
        // SAFETY: fdopendir failed and did not consume the descriptor.
        unsafe { nix::libc::close(duplicate) };
        return Err(FixedStagingError::Io {
            operation: "open fixed-staging inventory stream",
            path: path.to_owned(),
            source,
        });
    }

    let result = loop {
        // SAFETY: Linux exposes thread-local errno through this pointer.
        unsafe { *nix::libc::__errno_location() = 0 };
        // SAFETY: stream remains live and exclusively used here.
        let entry = unsafe { nix::libc::readdir(stream) };
        if entry.is_null() {
            let source = io::Error::last_os_error();
            break if source.raw_os_error() == Some(0) {
                Ok(None)
            } else {
                Err(FixedStagingError::Io {
                    operation: "read fixed-staging inventory",
                    path: path.to_owned(),
                    source,
                })
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
        return Err(FixedStagingError::Io {
            operation: "close fixed-staging inventory",
            path: path.to_owned(),
            source: io::Error::last_os_error(),
        });
    }
    result
}

#[cfg(test)]
std::thread_local! {
    static BEFORE_COORDINATOR_LOCK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static BEFORE_STAGING_BASELINE_REVALIDATION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static BEFORE_LEGACY_STAGING_NORMALIZATION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static BEFORE_FIXED_STAGING_FILL: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static AFTER_FIXED_STAGING_FILL: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static BEFORE_RETAINED_STATE_METADATA: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static BEFORE_CANDIDATE_USR_PUBLICATION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(super) fn arm_before_coordinator_lock(hook: impl FnOnce() + 'static) {
    BEFORE_COORDINATOR_LOCK.with(|slot| assert!(slot.borrow_mut().replace(Box::new(hook)).is_none()));
}

#[cfg(test)]
pub(super) fn arm_before_staging_baseline_revalidation(hook: impl FnOnce() + 'static) {
    BEFORE_STAGING_BASELINE_REVALIDATION.with(|slot| assert!(slot.borrow_mut().replace(Box::new(hook)).is_none()));
}

#[cfg(test)]
pub(super) fn arm_before_legacy_staging_normalization(hook: impl FnOnce() + 'static) {
    BEFORE_LEGACY_STAGING_NORMALIZATION.with(|slot| assert!(slot.borrow_mut().replace(Box::new(hook)).is_none()));
}

#[cfg(test)]
pub(super) fn arm_before_fixed_staging_fill(hook: impl FnOnce() + 'static) {
    BEFORE_FIXED_STAGING_FILL.with(|slot| assert!(slot.borrow_mut().replace(Box::new(hook)).is_none()));
}

#[cfg(test)]
pub(super) fn arm_after_fixed_staging_fill(hook: impl FnOnce() + 'static) {
    AFTER_FIXED_STAGING_FILL.with(|slot| assert!(slot.borrow_mut().replace(Box::new(hook)).is_none()));
}

#[cfg(test)]
pub(super) fn arm_before_retained_state_metadata(hook: impl FnOnce() + 'static) {
    BEFORE_RETAINED_STATE_METADATA.with(|slot| assert!(slot.borrow_mut().replace(Box::new(hook)).is_none()));
}

#[cfg(test)]
pub(super) fn arm_before_candidate_usr_publication(hook: impl FnOnce() + 'static) {
    BEFORE_CANDIDATE_USR_PUBLICATION.with(|slot| assert!(slot.borrow_mut().replace(Box::new(hook)).is_none()));
}

#[cfg(test)]
fn before_coordinator_lock() {
    BEFORE_COORDINATOR_LOCK.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn before_coordinator_lock() {}

#[cfg(test)]
fn before_staging_baseline_revalidation() {
    BEFORE_STAGING_BASELINE_REVALIDATION.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn before_staging_baseline_revalidation() {}

#[cfg(test)]
fn before_legacy_staging_normalization() {
    BEFORE_LEGACY_STAGING_NORMALIZATION.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn before_legacy_staging_normalization() {}

#[cfg(test)]
fn before_fixed_staging_fill() {
    BEFORE_FIXED_STAGING_FILL.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn before_fixed_staging_fill() {}

#[cfg(test)]
fn after_fixed_staging_fill() {
    AFTER_FIXED_STAGING_FILL.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn after_fixed_staging_fill() {}

#[cfg(test)]
pub(super) fn before_retained_state_metadata() {
    BEFORE_RETAINED_STATE_METADATA.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
pub(super) fn before_retained_state_metadata() {}

#[cfg(test)]
fn before_candidate_usr_publication() {
    BEFORE_CANDIDATE_USR_PUBLICATION.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn before_candidate_usr_publication() {}
