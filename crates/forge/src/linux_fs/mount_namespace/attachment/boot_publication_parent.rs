//! Descriptor-retained creation and revalidation of one boot publication parent.
//!
//! The caller supplies only parent components already admitted by the pure
//! ActiveReblit publication plan. This layer deliberately does not duplicate
//! its FAT/path grammar: it defensively accepts only bounded, NUL-free raw
//! components and performs every lookup relative to the authenticated retained
//! ESP/XBOOTLDR attachment.
//!
//! Missing directories receive one `mkdirat(2)` attempt. Every raw report is
//! reconciled by opening and retaining the named inode; no failed attempt is
//! retried and no scalar identity is promoted into authority. The complete
//! chain is restricted to the root filesystem device and mount ID, excludes
//! symlink, magic-link, and nested-mount traversal, and remains borrowed from
//! the freshly revalidated root attachment. Durability always proceeds from
//! the deepest child back through its parents before one filesystem sync and a
//! terminal root-and-edge revalidation.

use std::{
    ffi::{CStr, CString},
    fs::File,
    io,
    os::{
        fd::AsRawFd as _,
        unix::fs::{MetadataExt as _, PermissionsExt as _},
    },
    time::Instant,
};

use thiserror::Error;

use super::{
    RevalidatedTaskRootedAttachment,
    boot_file_publication::{
        AttachmentIdentity, RetainedBootFilePublicationError, RetainedBootFilePublicationLimits,
        RetainedBootFilePublicationRequest, RetainedBootFilePublicationTarget,
        ValidatedRetainedBootFilePublication, publish_immutable_boot_file_from_target_until,
    },
};
use crate::linux_fs::{
    controlled_resolution, descriptor_boot_namespace::RetainedBootNamespaceExpectedSource,
    descriptor_mount_id_until, mkdirat_once, openat2_file_until, sync_filesystem_until,
};

#[path = "boot_publication_parent/effect.rs"]
mod effect;

use effect::FixtureRetainedBootPublicationParentCheckpoint as ParentCheckpoint;

#[cfg(test)]
pub(crate) use effect::{
    FixtureRetainedBootPublicationParentCheckpoint, FixtureRetainedBootPublicationParentFault,
    arm_retained_boot_publication_parent_checkpoint_hook,
    arm_retained_boot_publication_parent_fault,
};

const MAX_PARENT_COMPONENTS: usize = 15;
const CREATED_DIRECTORY_MODE: u32 = 0o755;

#[derive(Debug, Error)]
pub(crate) enum RetainedBootPublicationParentError {
    #[error("boot publication-parent chain requires at least one admitted component")]
    EmptyComponentSet,
    #[error("boot publication-parent chain has {actual} components, exceeding the {limit}-component ceiling")]
    ComponentLimit { limit: usize, actual: usize },
    #[error("boot publication-parent component {index} is not one bounded raw component")]
    InvalidComponent { index: usize },
    #[error("boot publication-parent operation exceeded caller deadline {deadline:?}")]
    DeadlineExceeded { deadline: Instant },
    #[error("authenticated boot attachment rejected publication-parent work while {action}: {source}")]
    RootAttachment {
        action: &'static str,
        #[source]
        source: RetainedBootFilePublicationError,
    },
    #[error("boot publication-parent filesystem operation failed at component {index} while {action}: {source}")]
    Filesystem {
        index: usize,
        action: &'static str,
        #[source]
        source: io::Error,
    },
    #[error("one mkdir attempt at component {index} did not reconcile to the requested directory: {source}")]
    CreationNotApplied {
        index: usize,
        #[source]
        source: io::Error,
    },
    #[error("boot publication-parent component {index} changed identity while {action}")]
    DirectoryIdentityChanged { index: usize, action: &'static str },
    #[error("boot publication-parent component {index} has unsafe owner or writable permissions")]
    UnsafeDirectoryPolicy { index: usize },
    #[error("injected boot publication-parent stop at component {index}")]
    InjectedFault { index: usize },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DirectoryIdentity {
    device: u64,
    inode: u64,
    mount_id: u64,
    uid: u32,
    gid: u32,
    mode: u32,
}

impl From<DirectoryIdentity> for AttachmentIdentity {
    fn from(identity: DirectoryIdentity) -> Self {
        Self {
            device: identity.device,
            inode: identity.inode,
            mount_id: identity.mount_id,
        }
    }
}

struct RetainedPublicationDirectory {
    name: CString,
    file: File,
    identity: DirectoryIdentity,
}

/// One full, non-cloneable descriptor/name/identity chain below an exact boot root.
///
/// No descriptor accessor exists. The only mutation operation delegates to the
/// sealed immutable-leaf engine after revalidating the borrowed root and every
/// retained edge.
pub(crate) struct RetainedBootPublicationParent<'view, 'prepared> {
    root: &'view RevalidatedTaskRootedAttachment<'prepared>,
    root_file: File,
    root_identity: DirectoryIdentity,
    chain: Vec<RetainedPublicationDirectory>,
}

impl std::fmt::Debug for RetainedBootPublicationParent<'_, '_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RetainedBootPublicationParent")
            .field("component_count", &self.chain.len())
            .field("destination_device", &self.destination_device())
            .field("destination_inode", &self.destination_inode())
            .field("destination_mount_id", &self.destination_mount_id())
            .field("authority", &"retained; no descriptor exposure")
            .finish()
    }
}

impl<'prepared> RevalidatedTaskRootedAttachment<'prepared> {
    /// Retain one exact descendant directory chain beneath this boot root.
    ///
    /// `admitted_parent_components` must be split from a path which already
    /// passed the publication-plan grammar; these defensive checks only keep
    /// the descriptor syscall boundary to raw single components.
    pub(crate) fn retain_boot_publication_parent_until<'view>(
        &'view self,
        admitted_parent_components: &[&str],
        deadline: Instant,
    ) -> Result<RetainedBootPublicationParent<'view, 'prepared>, RetainedBootPublicationParentError> {
        require_deadline(deadline)?;
        let names = copy_components(admitted_parent_components)?;
        self.require_publication_parent_until("opening boot publication-parent creation", deadline)
            .map_err(|source| RetainedBootPublicationParentError::RootAttachment {
                action: "opening boot publication-parent creation",
                source,
            })?;
        let root_attachment = self.publication_parent_identity();
        let (root_file, root_identity) =
            open_directory_alias(self.publication_parent(), root_attachment, 0, deadline)?;
        let mut chain = Vec::new();
        chain.try_reserve_exact(names.len()).map_err(|source| {
            RetainedBootPublicationParentError::Filesystem {
                index: 0,
                action: "allocating retained publication-parent descriptors",
                source: io::Error::other(source.to_string()),
            }
        })?;

        for (index, name) in names.into_iter().enumerate() {
            require_named_chain(self, &root_file, root_identity, &chain, deadline).map_err(|source| {
                RetainedBootPublicationParentError::RootAttachment {
                    action: "revalidating boot publication-parent prefix before descent",
                    source,
                }
            })?;
            let parent = chain.last().map_or(&root_file, |entry: &RetainedPublicationDirectory| &entry.file);
            let parent_identity = chain
                .last()
                .map_or(root_identity, |entry: &RetainedPublicationDirectory| entry.identity);
            let (file, created) =
                open_or_create_component(parent, parent_identity, &name, root_identity, index, deadline)?;
            let identity = observe_directory(&file, root_identity, index, "retaining publication-parent component", deadline)?;
            chain.push(RetainedPublicationDirectory { name, file, identity });
            effect::emit(ParentCheckpoint::DirectoryRetained {
                depth: index + 1,
                created,
            });
            if created && effect::fail_after_creation(index).is_err() {
                return Err(RetainedBootPublicationParentError::InjectedFault { index });
            }
        }

        sync_chain(&root_file, root_identity, &chain, deadline)?;
        effect::emit(ParentCheckpoint::BeforeTerminalRevalidation);
        require_named_chain(self, &root_file, root_identity, &chain, deadline).map_err(|source| {
            RetainedBootPublicationParentError::RootAttachment {
                action: "terminally revalidating boot publication-parent creation",
                source,
            }
        })?;
        Ok(RetainedBootPublicationParent {
            root: self,
            root_file,
            root_identity,
            chain,
        })
    }
}

impl RetainedBootPublicationParent<'_, '_> {
    pub(crate) fn component_count(&self) -> usize {
        self.chain.len()
    }

    pub(crate) const fn root_device(&self) -> u64 {
        self.root_identity.device
    }

    pub(crate) const fn root_inode(&self) -> u64 {
        self.root_identity.inode
    }

    pub(crate) const fn root_mount_id(&self) -> u64 {
        self.root_identity.mount_id
    }

    pub(crate) fn destination_device(&self) -> u64 {
        self.destination_identity().device
    }

    pub(crate) fn destination_inode(&self) -> u64 {
        self.destination_identity().inode
    }

    pub(crate) fn destination_mount_id(&self) -> u64 {
        self.destination_identity().mount_id
    }

    pub(crate) fn publish_immutable_boot_file_until<'source>(
        &self,
        request: RetainedBootFilePublicationRequest<'_>,
        expected_source: &RetainedBootNamespaceExpectedSource<'source>,
        limits: RetainedBootFilePublicationLimits,
        deadline: Instant,
    ) -> Result<ValidatedRetainedBootFilePublication, RetainedBootFilePublicationError> {
        publish_immutable_boot_file_from_target_until(self, request, expected_source, limits, deadline)
    }

    pub(crate) fn matches_leaf_evidence(&self, evidence: &ValidatedRetainedBootFilePublication) -> bool {
        evidence.destination_device() == self.destination_device()
            && evidence.destination_inode() == self.destination_inode()
            && evidence.destination_mount_id() == self.destination_mount_id()
    }

    fn destination_identity(&self) -> DirectoryIdentity {
        self.chain
            .last()
            .unwrap_or_else(|| unreachable!("publication-parent chain is nonempty"))
            .identity
    }
}

impl RetainedBootFilePublicationTarget for RetainedBootPublicationParent<'_, '_> {
    fn publication_parent(&self) -> &File {
        &self
            .chain
            .last()
            .unwrap_or_else(|| unreachable!("publication-parent chain is nonempty"))
            .file
    }

    fn publication_parent_identity(&self) -> AttachmentIdentity {
        self.destination_identity().into()
    }

    fn require_publication_parent_until(
        &self,
        action: &'static str,
        deadline: Instant,
    ) -> Result<(), RetainedBootFilePublicationError> {
        require_named_chain(self.root, &self.root_file, self.root_identity, &self.chain, deadline).map_err(
            |source| RetainedBootFilePublicationError::Attachment {
                action,
                source: io::Error::other(source.to_string()),
            },
        )
    }
}

fn copy_components(components: &[&str]) -> Result<Vec<CString>, RetainedBootPublicationParentError> {
    if components.is_empty() {
        return Err(RetainedBootPublicationParentError::EmptyComponentSet);
    }
    if components.len() > MAX_PARENT_COMPONENTS {
        return Err(RetainedBootPublicationParentError::ComponentLimit {
            limit: MAX_PARENT_COMPONENTS,
            actual: components.len(),
        });
    }
    let mut names = Vec::new();
    names.try_reserve_exact(components.len()).map_err(|source| RetainedBootPublicationParentError::Filesystem {
        index: 0,
        action: "allocating publication-parent component names",
        source: io::Error::other(source.to_string()),
    })?;
    for (index, component) in components.iter().enumerate() {
        let bytes = component.as_bytes();
        if bytes.is_empty() || bytes.len() > 255 || matches!(bytes, b"." | b"..") || bytes.contains(&b'/') {
            return Err(RetainedBootPublicationParentError::InvalidComponent { index });
        }
        names.push(CString::new(bytes).map_err(|_| RetainedBootPublicationParentError::InvalidComponent { index })?);
    }
    Ok(names)
}

fn open_directory_alias(
    retained: &File,
    root: AttachmentIdentity,
    index: usize,
    deadline: Instant,
) -> Result<(File, DirectoryIdentity), RetainedBootPublicationParentError> {
    require_deadline(deadline)?;
    let file = openat2_file_until(
        retained.as_raw_fd(),
        c".",
        nix::libc::O_PATH
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW,
        0,
        controlled_resolution(),
        deadline,
    )
    .map_err(|source| RetainedBootPublicationParentError::Filesystem {
        index,
        action: "opening retained boot publication-parent root",
        source,
    })?;
    let found = observe_root_directory(&file, index, deadline)?;
    if found.device != root.device || found.inode != root.inode || found.mount_id != root.mount_id {
        return Err(RetainedBootPublicationParentError::DirectoryIdentityChanged {
            index,
            action: "binding retained boot publication-parent root",
        });
    }
    require_effective_root_owner(found, index)?;
    require_safe_directory_policy(found, found, index)?;
    Ok((file, found))
}

fn open_or_create_component(
    parent: &File,
    expected_parent: DirectoryIdentity,
    name: &CStr,
    root: DirectoryIdentity,
    index: usize,
    deadline: Instant,
) -> Result<(File, bool), RetainedBootPublicationParentError> {
    match open_component(parent, name, index, deadline) {
        Ok(file) => {
            observe_directory(&file, root, index, "admitting existing publication-parent component", deadline)?;
            Ok((file, false))
        }
        Err(source) if source.raw_os_error() == Some(nix::libc::ENOENT) => {
            let rebound_parent = observe_directory(
                parent,
                root,
                index,
                "revalidating retained parent before one directory-creation attempt",
                deadline,
            )?;
            if rebound_parent != expected_parent {
                return Err(RetainedBootPublicationParentError::DirectoryIdentityChanged {
                    index,
                    action: "revalidating retained parent before one directory-creation attempt",
                });
            }
            require_deadline(deadline)?;
            let mkdir_report = effect::mkdir_report(index, mkdirat_once(parent, name, CREATED_DIRECTORY_MODE));
            match open_component(parent, name, index, deadline) {
                Ok(file) => {
                    observe_directory(&file, root, index, "reconciling publication-parent creation", deadline)?;
                    Ok((file, true))
                }
                Err(reconcile_error) if reconcile_error.raw_os_error() == Some(nix::libc::ENOENT) => {
                    Err(RetainedBootPublicationParentError::CreationNotApplied {
                        index,
                        source: mkdir_report.err().unwrap_or(reconcile_error),
                    })
                }
                Err(source) => Err(RetainedBootPublicationParentError::Filesystem {
                    index,
                    action: "reconciling publication-parent creation",
                    source,
                }),
            }
        }
        Err(source) => Err(RetainedBootPublicationParentError::Filesystem {
            index,
            action: "opening existing publication-parent component",
            source,
        }),
    }
}

fn open_component(parent: &File, name: &CStr, _index: usize, deadline: Instant) -> io::Result<File> {
    openat2_file_until(
        parent.as_raw_fd(),
        name,
        nix::libc::O_PATH
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW,
        0,
        controlled_resolution(),
        deadline,
    )
}

fn observe_directory(
    file: &File,
    root: DirectoryIdentity,
    index: usize,
    action: &'static str,
    deadline: Instant,
) -> Result<DirectoryIdentity, RetainedBootPublicationParentError> {
    require_deadline(deadline)?;
    let metadata = file.metadata().map_err(|source| RetainedBootPublicationParentError::Filesystem {
        index,
        action,
        source,
    })?;
    let mount_id = descriptor_mount_id_until(file, deadline).map_err(|source| {
        RetainedBootPublicationParentError::Filesystem {
            index,
            action,
            source,
        }
    })?;
    let found = DirectoryIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
        mount_id,
        uid: metadata.uid(),
        gid: metadata.gid(),
        mode: metadata.permissions().mode() & 0o7777,
    };
    require_root_filesystem(root, found, metadata.file_type().is_dir(), index, action)?;
    require_safe_directory_policy(root, found, index)?;
    Ok(found)
}

fn observe_root_directory(
    file: &File,
    index: usize,
    deadline: Instant,
) -> Result<DirectoryIdentity, RetainedBootPublicationParentError> {
    require_deadline(deadline)?;
    let metadata = file.metadata().map_err(|source| RetainedBootPublicationParentError::Filesystem {
        index,
        action: "observing retained boot publication root",
        source,
    })?;
    let mount_id = descriptor_mount_id_until(file, deadline).map_err(|source| {
        RetainedBootPublicationParentError::Filesystem {
            index,
            action: "observing retained boot publication root",
            source,
        }
    })?;
    if !metadata.file_type().is_dir()
        || metadata.dev() == 0
        || metadata.ino() == 0
        || mount_id == 0
    {
        return Err(RetainedBootPublicationParentError::DirectoryIdentityChanged {
            index,
            action: "observing retained boot publication root",
        });
    }
    Ok(DirectoryIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
        mount_id,
        uid: metadata.uid(),
        gid: metadata.gid(),
        mode: metadata.permissions().mode() & 0o7777,
    })
}

fn require_root_filesystem(
    root: DirectoryIdentity,
    found: DirectoryIdentity,
    is_directory: bool,
    index: usize,
    action: &'static str,
) -> Result<(), RetainedBootPublicationParentError> {
    if !is_directory
        || found.device == 0
        || found.inode == 0
        || found.mount_id == 0
        || found.device != root.device
        || found.mount_id != root.mount_id
    {
        Err(RetainedBootPublicationParentError::DirectoryIdentityChanged { index, action })
    } else {
        Ok(())
    }
}

fn require_safe_directory_policy(
    root: DirectoryIdentity,
    found: DirectoryIdentity,
    index: usize,
) -> Result<(), RetainedBootPublicationParentError> {
    if found.uid != root.uid
        || found.gid != root.gid
        || found.mode & 0o700 != 0o700
        || found.mode & 0o022 != 0
        || found.mode & 0o7000 != 0
    {
        Err(RetainedBootPublicationParentError::UnsafeDirectoryPolicy { index })
    } else {
        Ok(())
    }
}

fn require_effective_root_owner(
    root: DirectoryIdentity,
    index: usize,
) -> Result<(), RetainedBootPublicationParentError> {
    let effective_uid = nix::unistd::geteuid().as_raw();
    let effective_gid = nix::unistd::getegid().as_raw();
    require_root_owner(root, effective_uid, effective_gid, index)
}

fn require_root_owner(
    root: DirectoryIdentity,
    effective_uid: u32,
    effective_gid: u32,
    index: usize,
) -> Result<(), RetainedBootPublicationParentError> {
    if root.uid != effective_uid || root.gid != effective_gid {
        Err(RetainedBootPublicationParentError::UnsafeDirectoryPolicy { index })
    } else {
        Ok(())
    }
}

fn open_readable_directory(
    retained: &File,
    root: DirectoryIdentity,
    expected: DirectoryIdentity,
    index: usize,
    deadline: Instant,
) -> Result<File, RetainedBootPublicationParentError> {
    require_deadline(deadline)?;
    let readable = openat2_file_until(
        retained.as_raw_fd(),
        c".",
        nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NOATIME,
        0,
        controlled_resolution(),
        deadline,
    )
    .map_err(|source| RetainedBootPublicationParentError::Filesystem {
        index,
        action: "opening readable publication-parent durability descriptor",
        source,
    })?;
    let found = observe_directory(
        &readable,
        root,
        index,
        "binding readable publication-parent durability descriptor",
        deadline,
    )?;
    if found != expected {
        return Err(RetainedBootPublicationParentError::DirectoryIdentityChanged {
            index,
            action: "binding readable publication-parent durability descriptor",
        });
    }
    Ok(readable)
}

fn sync_chain(
    root: &File,
    root_identity: DirectoryIdentity,
    chain: &[RetainedPublicationDirectory],
    deadline: Instant,
) -> Result<(), RetainedBootPublicationParentError> {
    for (index, directory) in chain.iter().enumerate().rev() {
        let depth = index + 1;
        effect::emit(ParentCheckpoint::BeforeDirectorySync { depth });
        require_deadline(deadline)?;
        let readable = open_readable_directory(
            &directory.file,
            root_identity,
            directory.identity,
            index,
            deadline,
        )?;
        readable
            .sync_all()
            .map_err(|source| RetainedBootPublicationParentError::Filesystem {
                index,
                action: "synchronizing retained publication-parent child",
                source,
            })?;
        require_deadline(deadline)?;
        effect::emit(ParentCheckpoint::AfterDirectorySync { depth });
    }
    effect::emit(ParentCheckpoint::BeforeDirectorySync { depth: 0 });
    let readable_root = open_readable_directory(root, root_identity, root_identity, 0, deadline)?;
    readable_root.sync_all().map_err(|source| RetainedBootPublicationParentError::Filesystem {
        index: 0,
        action: "synchronizing retained boot publication root",
        source,
    })?;
    require_deadline(deadline)?;
    effect::emit(ParentCheckpoint::AfterDirectorySync { depth: 0 });
    effect::emit(ParentCheckpoint::BeforeFilesystemSync);
    sync_filesystem_until(&readable_root, deadline).map_err(|source| RetainedBootPublicationParentError::Filesystem {
        index: 0,
        action: "synchronizing retained boot publication filesystem",
        source,
    })?;
    require_deadline(deadline)
}

fn require_named_chain(
    root: &RevalidatedTaskRootedAttachment<'_>,
    root_file: &File,
    root_identity: DirectoryIdentity,
    chain: &[RetainedPublicationDirectory],
    deadline: Instant,
) -> Result<(), RetainedBootFilePublicationError> {
    root.require_publication_parent_until("opening nested boot publication-parent revalidation", deadline)?;
    if root_identity.uid != nix::unistd::geteuid().as_raw()
        || root_identity.gid != nix::unistd::getegid().as_raw()
    {
        return Err(RetainedBootFilePublicationError::DestinationIdentityChanged {
            action: "matching retained boot root to current effective credentials",
        });
    }
    require_exact_directory(root_file, root_identity, root_identity, "revalidating retained boot root", deadline)?;
    let mut parent = root_file;
    for (index, directory) in chain.iter().enumerate() {
        require_exact_directory(
            &directory.file,
            root_identity,
            directory.identity,
            "revalidating retained publication-parent descriptor",
            deadline,
        )?;
        let rebound = open_component(parent, &directory.name, index, deadline).map_err(|source| {
            RetainedBootFilePublicationError::Attachment {
                action: "rebinding retained publication-parent edge",
                source,
            }
        })?;
        require_exact_directory(
            &rebound,
            root_identity,
            directory.identity,
            "rebinding retained publication-parent edge",
            deadline,
        )?;
        parent = &directory.file;
    }
    root.require_publication_parent_until("closing nested boot publication-parent revalidation", deadline)
}

fn require_exact_directory(
    file: &File,
    root: DirectoryIdentity,
    expected: DirectoryIdentity,
    action: &'static str,
    deadline: Instant,
) -> Result<(), RetainedBootFilePublicationError> {
    let metadata = file.metadata().map_err(|source| RetainedBootFilePublicationError::Attachment { action, source })?;
    let mount_id = descriptor_mount_id_until(file, deadline)
        .map_err(|source| RetainedBootFilePublicationError::Attachment { action, source })?;
    let found = DirectoryIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
        mount_id,
        uid: metadata.uid(),
        gid: metadata.gid(),
        mode: metadata.permissions().mode() & 0o7777,
    };
    if !metadata.file_type().is_dir()
        || found.device == 0
        || found.inode == 0
        || found.mount_id == 0
        || found.device != root.device
        || found.mount_id != root.mount_id
        || found != expected
    {
        return Err(RetainedBootFilePublicationError::DestinationIdentityChanged { action });
    }
    Ok(())
}

fn require_deadline(deadline: Instant) -> Result<(), RetainedBootPublicationParentError> {
    if Instant::now() > deadline {
        Err(RetainedBootPublicationParentError::DeadlineExceeded { deadline })
    } else {
        Ok(())
    }
}

#[cfg(test)]
pub(crate) fn validate_fixture_boot_publication_parent_identity(
    root_device: u64,
    root_mount_id: u64,
    child_device: u64,
    child_mount_id: u64,
) -> Result<(), RetainedBootPublicationParentError> {
    require_root_filesystem(
        DirectoryIdentity {
            device: root_device,
            inode: 1,
            mount_id: root_mount_id,
            uid: 0,
            gid: 0,
            mode: 0o755,
        },
        DirectoryIdentity {
            device: child_device,
            inode: 2,
            mount_id: child_mount_id,
            uid: 0,
            gid: 0,
            mode: 0o755,
        },
        true,
        0,
        "validating fixture publication-parent identity",
    )
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn validate_fixture_boot_publication_parent_policy(
    effective_uid: u32,
    effective_gid: u32,
    root_uid: u32,
    root_gid: u32,
    root_mode: u32,
    child_uid: u32,
    child_gid: u32,
    child_mode: u32,
) -> Result<(), RetainedBootPublicationParentError> {
    let root = DirectoryIdentity {
        device: 1,
        inode: 1,
        mount_id: 1,
        uid: root_uid,
        gid: root_gid,
        mode: root_mode,
    };
    let child = DirectoryIdentity {
        device: 1,
        inode: 2,
        mount_id: 1,
        uid: child_uid,
        gid: child_gid,
        mode: child_mode,
    };
    require_root_owner(root, effective_uid, effective_gid, 0)?;
    require_safe_directory_policy(root, root, 0)?;
    require_safe_directory_policy(root, child, 1)
}
