use std::io;
use std::os::fd::{AsRawFd, FromRawFd as _, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};

use snafu::Snafu;

use super::anchored_root::{descriptor_stat, openat2_anchored};
use crate::duplicate_cloexec;

// Linux PATH_MAX includes the terminating NUL byte.
const MAX_LOCATOR_BYTES: usize = 4095;
const MAX_LOCATOR_COMPONENTS: usize = 256;
const MAX_LOCATOR_COMPONENT_BYTES: usize = 255;

/// The part of an [`AnchoredLocator`] that failed validation or reopening.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum AnchoredLocatorComponent {
    Exact,
    BeneathBase,
    BeneathLeaf,
}

/// A typed failure while constructing or reopening an [`AnchoredLocator`].
#[derive(Debug, Snafu)]
pub enum AnchoredLocatorError {
    #[snafu(display("anchored locator must be one normalized absolute path: {}", path.display()))]
    InvalidAbsolute { path: PathBuf },
    #[snafu(display("anchored beneath locator must be one normalized relative path: {}", path.display()))]
    InvalidRelative { path: PathBuf },
    #[snafu(display("duplicate {component:?} anchored locator witness for {}", path.display()))]
    DuplicateWitness {
        component: AnchoredLocatorComponent,
        path: PathBuf,
        source: io::Error,
    },
    #[snafu(display("inspect {component:?} anchored locator witness for {}", path.display()))]
    InspectWitness {
        component: AnchoredLocatorComponent,
        path: PathBuf,
        source: io::Error,
    },
    #[snafu(display(
        "{component:?} anchored locator witness for {} is not an O_PATH descriptor (status flags {status_flags:#x})",
        path.display()
    ))]
    WitnessNotPath {
        component: AnchoredLocatorComponent,
        path: PathBuf,
        status_flags: nix::libc::c_int,
    },
    #[snafu(display(
        "{component:?} anchored locator witness for {} has unsupported file type {file_type:o}",
        path.display()
    ))]
    UnsupportedWitnessType {
        component: AnchoredLocatorComponent,
        path: PathBuf,
        file_type: nix::libc::mode_t,
    },
    #[snafu(display("beneath anchored locator base {} is not a directory", path.display()))]
    BeneathBaseNotDirectory { path: PathBuf },
    #[snafu(display("open current mount-namespace root for anchored locator authentication"))]
    OpenNamespaceRoot { source: io::Error },
    #[snafu(display("reopen {component:?} anchored locator {}", path.display()))]
    Reopen {
        component: AnchoredLocatorComponent,
        path: PathBuf,
        source: io::Error,
    },
    #[snafu(display("inspect reopened {component:?} anchored locator {}", path.display()))]
    InspectReopened {
        component: AnchoredLocatorComponent,
        path: PathBuf,
        source: io::Error,
    },
    #[snafu(display(
        "reopened {component:?} anchored locator {} has identity ({actual_device},{actual_inode},{actual_file_type:o}); expected ({expected_device},{expected_inode},{expected_file_type:o})",
        path.display()
    ))]
    IdentityMismatch {
        component: AnchoredLocatorComponent,
        path: PathBuf,
        expected_device: u64,
        expected_inode: u64,
        expected_file_type: nix::libc::mode_t,
        actual_device: u64,
        actual_inode: u64,
        actual_file_type: nix::libc::mode_t,
    },
    #[snafu(display(
        "retained {component:?} anchored locator witness {} changed identity from ({expected_device},{expected_inode},{expected_file_type:o}) to ({actual_device},{actual_inode},{actual_file_type:o})",
        path.display()
    ))]
    RetainedWitnessChanged {
        component: AnchoredLocatorComponent,
        path: PathBuf,
        expected_device: u64,
        expected_inode: u64,
        expected_file_type: nix::libc::mode_t,
        actual_device: u64,
        actual_inode: u64,
        actual_file_type: nix::libc::mode_t,
    },
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
struct AnchoredIdentity {
    device: u64,
    inode: u64,
    file_type: nix::libc::mode_t,
}

#[derive(Debug)]
struct AnchoredWitness {
    descriptor: OwnedFd,
    identity: AnchoredIdentity,
}

#[derive(Debug)]
enum AnchoredLocatorKind {
    Exact {
        absolute_path: PathBuf,
        witness: AnchoredWitness,
    },
    Beneath {
        absolute_base_path: PathBuf,
        base_witness: AnchoredWitness,
        relative_path: PathBuf,
        leaf_witness: AnchoredWitness,
    },
}

/// An owned namespace-reopen locator paired with retained identity witnesses.
///
/// `exact` identifies one normalized absolute namespace path. Reopening an
/// exact locator is confined beneath an explicit namespace-root descriptor but
/// permits intended mount crossings such as `/sys/fs/cgroup` and `/nix/store`.
///
/// `beneath` identifies one normalized relative child below a normalized
/// absolute base. It retains both the base and leaf witnesses. Reopening first
/// authenticates the base without forbidding intended crossings on the path to
/// it, then resolves the child with mount crossings forbidden.
///
/// Both forms retain duplicated `O_PATH` witnesses so device/inode reuse cannot
/// occur while a candidate is compared. Reopening rejects symlinks, magic
/// links, escape, missing paths, and replacement objects. A hard link to the
/// exact same inode remains valid. This value grants no mount or namespace
/// mutation authority and never falls back to procfs descriptor aliases or a
/// stale descriptor from another mount namespace.
#[derive(Debug)]
pub struct AnchoredLocator {
    kind: AnchoredLocatorKind,
}

impl AnchoredLocator {
    /// Retain one exact absolute locator and its already-authenticated witness.
    pub fn exact(absolute_path: impl Into<PathBuf>, expected: &impl AsRawFd) -> Result<Self, AnchoredLocatorError> {
        let absolute_path = require_normalized_absolute(absolute_path.into())?;
        let witness = capture_witness(AnchoredLocatorComponent::Exact, &absolute_path, expected.as_raw_fd())?;
        let locator = Self {
            kind: AnchoredLocatorKind::Exact { absolute_path, witness },
        };
        locator.authenticate_current_namespace()?;
        Ok(locator)
    }

    /// Retain an absolute base and one exact normalized child beneath it.
    pub fn beneath(
        absolute_base_path: impl Into<PathBuf>,
        expected_base: &impl AsRawFd,
        relative_path: impl Into<PathBuf>,
        expected_leaf: &impl AsRawFd,
    ) -> Result<Self, AnchoredLocatorError> {
        let absolute_base_path = require_normalized_absolute(absolute_base_path.into())?;
        let relative_path = require_normalized_relative(relative_path.into())?;
        let base_witness = capture_witness(
            AnchoredLocatorComponent::BeneathBase,
            &absolute_base_path,
            expected_base.as_raw_fd(),
        )?;
        if base_witness.identity.file_type != nix::libc::S_IFDIR {
            return Err(AnchoredLocatorError::BeneathBaseNotDirectory {
                path: absolute_base_path,
            });
        }
        let leaf_witness = capture_witness(
            AnchoredLocatorComponent::BeneathLeaf,
            &relative_path,
            expected_leaf.as_raw_fd(),
        )?;
        let locator = Self {
            kind: AnchoredLocatorKind::Beneath {
                absolute_base_path,
                base_witness,
                relative_path,
                leaf_witness,
            },
        };
        locator.authenticate_current_namespace()?;
        Ok(locator)
    }

    /// Return the exact path or the absolute base of a beneath locator.
    pub fn absolute_base_path(&self) -> &Path {
        match &self.kind {
            AnchoredLocatorKind::Exact { absolute_path, .. } => absolute_path,
            AnchoredLocatorKind::Beneath { absolute_base_path, .. } => absolute_base_path,
        }
    }

    /// Return the relative child for a beneath locator.
    pub fn relative_path(&self) -> Option<&Path> {
        match &self.kind {
            AnchoredLocatorKind::Exact { .. } => None,
            AnchoredLocatorKind::Beneath { relative_path, .. } => Some(relative_path),
        }
    }

    #[cfg(test)]
    pub(crate) fn retained_descriptors(&self) -> (RawFd, Option<RawFd>) {
        match &self.kind {
            AnchoredLocatorKind::Exact { witness, .. } => (witness.descriptor.as_raw_fd(), None),
            AnchoredLocatorKind::Beneath {
                base_witness,
                leaf_witness,
                ..
            } => (
                base_witness.descriptor.as_raw_fd(),
                Some(leaf_witness.descriptor.as_raw_fd()),
            ),
        }
    }

    /// Reopen and authenticate this locator beneath a root capability from the
    /// namespace in which the returned descriptor will be consumed.
    pub(crate) fn reopen_from_namespace_root(&self, namespace_root: RawFd) -> Result<OwnedFd, AnchoredLocatorError> {
        match &self.kind {
            AnchoredLocatorKind::Exact { absolute_path, witness } => {
                reopen_exact(namespace_root, absolute_path, witness, AnchoredLocatorComponent::Exact)
            }
            AnchoredLocatorKind::Beneath {
                absolute_base_path,
                base_witness,
                relative_path,
                leaf_witness,
            } => {
                let base = reopen_exact(
                    namespace_root,
                    absolute_base_path,
                    base_witness,
                    AnchoredLocatorComponent::BeneathBase,
                )?;
                reopen_relative(base.as_raw_fd(), relative_path, leaf_witness)
            }
        }
    }

    fn authenticate_current_namespace(&self) -> Result<(), AnchoredLocatorError> {
        let namespace_root = open_current_namespace_root()?;
        self.reopen_from_namespace_root(namespace_root.as_raw_fd())?;
        Ok(())
    }
}

fn open_current_namespace_root() -> Result<OwnedFd, AnchoredLocatorError> {
    // SAFETY: the static C string is terminated, open borrows it only for the
    // call, and successful open returns one fresh descriptor.
    let descriptor = unsafe {
        nix::libc::open(
            c"/".as_ptr(),
            nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC,
        )
    };
    if descriptor == -1 {
        return Err(AnchoredLocatorError::OpenNamespaceRoot {
            source: io::Error::last_os_error(),
        });
    }
    // SAFETY: successful open returned one fresh owned descriptor.
    Ok(unsafe { OwnedFd::from_raw_fd(descriptor) })
}

fn require_normalized_absolute(path: PathBuf) -> Result<PathBuf, AnchoredLocatorError> {
    if valid_normalized_path(&path, true) {
        Ok(path)
    } else {
        Err(AnchoredLocatorError::InvalidAbsolute { path })
    }
}

fn require_normalized_relative(path: PathBuf) -> Result<PathBuf, AnchoredLocatorError> {
    if valid_normalized_path(&path, false) {
        Ok(path)
    } else {
        Err(AnchoredLocatorError::InvalidRelative { path })
    }
}

fn valid_normalized_path(path: &Path, absolute: bool) -> bool {
    let bytes = path.as_os_str().as_bytes();
    if bytes.is_empty() || bytes.len() > MAX_LOCATOR_BYTES || bytes.contains(&0) {
        return false;
    }
    if absolute {
        if bytes == b"/" {
            return true;
        }
        if !bytes.starts_with(b"/") || bytes.ends_with(b"/") {
            return false;
        }
    } else if bytes.starts_with(b"/") || bytes.ends_with(b"/") {
        return false;
    }
    let components = if absolute { &bytes[1..] } else { bytes };
    let mut count = 0usize;
    for component in components.split(|byte| *byte == b'/') {
        count = count.saturating_add(1);
        if component.is_empty()
            || component == b"."
            || component == b".."
            || component.len() > MAX_LOCATOR_COMPONENT_BYTES
            || count > MAX_LOCATOR_COMPONENTS
        {
            return false;
        }
    }
    true
}

fn capture_witness(
    component: AnchoredLocatorComponent,
    path: &Path,
    descriptor: RawFd,
) -> Result<AnchoredWitness, AnchoredLocatorError> {
    let descriptor = duplicate_cloexec(descriptor).map_err(|source| AnchoredLocatorError::DuplicateWitness {
        component,
        path: path.to_owned(),
        source,
    })?;
    // SAFETY: descriptor is live for this fcntl query.
    let status_flags = unsafe { nix::libc::fcntl(descriptor.as_raw_fd(), nix::libc::F_GETFL) };
    if status_flags == -1 {
        return Err(AnchoredLocatorError::InspectWitness {
            component,
            path: path.to_owned(),
            source: io::Error::last_os_error(),
        });
    }
    if status_flags & nix::libc::O_PATH != nix::libc::O_PATH {
        return Err(AnchoredLocatorError::WitnessNotPath {
            component,
            path: path.to_owned(),
            status_flags,
        });
    }
    let stat = descriptor_stat(descriptor.as_raw_fd()).map_err(|source| AnchoredLocatorError::InspectWitness {
        component,
        path: path.to_owned(),
        source,
    })?;
    let identity = identity_from_stat(&stat);
    if !matches!(identity.file_type, nix::libc::S_IFDIR | nix::libc::S_IFREG) {
        return Err(AnchoredLocatorError::UnsupportedWitnessType {
            component,
            path: path.to_owned(),
            file_type: identity.file_type,
        });
    }
    Ok(AnchoredWitness { descriptor, identity })
}

fn reopen_exact(
    namespace_root: RawFd,
    absolute_path: &Path,
    witness: &AnchoredWitness,
    component: AnchoredLocatorComponent,
) -> Result<OwnedFd, AnchoredLocatorError> {
    let relative = absolute_path
        .strip_prefix(Path::new("/"))
        .expect("validated absolute locator");
    let relative = if relative.as_os_str().is_empty() {
        Path::new(".")
    } else {
        relative
    };
    let descriptor = openat2_anchored(
        namespace_root,
        relative,
        nix::libc::O_PATH | nix::libc::O_CLOEXEC,
        0,
        nix::libc::RESOLVE_BENEATH | nix::libc::RESOLVE_NO_MAGICLINKS | nix::libc::RESOLVE_NO_SYMLINKS,
    )
    .map_err(|source| AnchoredLocatorError::Reopen {
        component,
        path: absolute_path.to_owned(),
        source,
    })?;
    let expected = require_live_witness(component, absolute_path, witness)?;
    require_identity(component, absolute_path, &descriptor, expected)?;
    Ok(descriptor)
}

fn reopen_relative(
    base: RawFd,
    relative_path: &Path,
    witness: &AnchoredWitness,
) -> Result<OwnedFd, AnchoredLocatorError> {
    let descriptor = openat2_anchored(
        base,
        relative_path,
        nix::libc::O_PATH | nix::libc::O_CLOEXEC,
        0,
        nix::libc::RESOLVE_BENEATH
            | nix::libc::RESOLVE_NO_XDEV
            | nix::libc::RESOLVE_NO_MAGICLINKS
            | nix::libc::RESOLVE_NO_SYMLINKS,
    )
    .map_err(|source| AnchoredLocatorError::Reopen {
        component: AnchoredLocatorComponent::BeneathLeaf,
        path: relative_path.to_owned(),
        source,
    })?;
    let expected = require_live_witness(AnchoredLocatorComponent::BeneathLeaf, relative_path, witness)?;
    require_identity(
        AnchoredLocatorComponent::BeneathLeaf,
        relative_path,
        &descriptor,
        expected,
    )?;
    Ok(descriptor)
}

fn require_live_witness(
    component: AnchoredLocatorComponent,
    path: &Path,
    witness: &AnchoredWitness,
) -> Result<AnchoredIdentity, AnchoredLocatorError> {
    let stat =
        descriptor_stat(witness.descriptor.as_raw_fd()).map_err(|source| AnchoredLocatorError::InspectWitness {
            component,
            path: path.to_owned(),
            source,
        })?;
    let actual = identity_from_stat(&stat);
    if actual == witness.identity {
        Ok(actual)
    } else {
        Err(AnchoredLocatorError::RetainedWitnessChanged {
            component,
            path: path.to_owned(),
            expected_device: witness.identity.device,
            expected_inode: witness.identity.inode,
            expected_file_type: witness.identity.file_type,
            actual_device: actual.device,
            actual_inode: actual.inode,
            actual_file_type: actual.file_type,
        })
    }
}

fn require_identity(
    component: AnchoredLocatorComponent,
    path: &Path,
    descriptor: &OwnedFd,
    expected: AnchoredIdentity,
) -> Result<(), AnchoredLocatorError> {
    let stat = descriptor_stat(descriptor.as_raw_fd()).map_err(|source| AnchoredLocatorError::InspectReopened {
        component,
        path: path.to_owned(),
        source,
    })?;
    let actual = identity_from_stat(&stat);
    if actual == expected {
        Ok(())
    } else {
        Err(AnchoredLocatorError::IdentityMismatch {
            component,
            path: path.to_owned(),
            expected_device: expected.device,
            expected_inode: expected.inode,
            expected_file_type: expected.file_type,
            actual_device: actual.device,
            actual_inode: actual.inode,
            actual_file_type: actual.file_type,
        })
    }
}

fn identity_from_stat(stat: &nix::libc::stat) -> AnchoredIdentity {
    AnchoredIdentity {
        device: stat.st_dev,
        inode: stat.st_ino,
        file_type: stat.st_mode & nix::libc::S_IFMT,
    }
}
