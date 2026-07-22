use std::{
    ffi::{CStr, CString},
    fs::{File, Metadata},
    io,
    os::{
        fd::AsRawFd as _,
        unix::fs::{MetadataExt as _, PermissionsExt as _},
    },
    time::Instant,
};

use super::{
    AssessmentBinding, AttachmentIdentity, RetainedBootLeafAssessmentError,
    RetainedBootLeafAssessmentLimits, RetainedBootLeafAssessmentRequest,
    RetainedBootLeafAssessmentState, ValidatedRetainedBootLeafAssessment,
    assess_from_retained_parent_until, checkpoint, effect, open_parent_alias,
    require_parent, validate_request,
};
use crate::linux_fs::{
    controlled_resolution, descriptor_mount_id_until, openat2_file_until,
};
use crate::linux_fs::mount_namespace::attachment::{
    RevalidatedTaskRootedAttachment,
    boot_file_publication::{
        RetainedBootFilePublicationError, RetainedBootFilePublicationTarget,
    },
};

const MAX_PARENT_COMPONENTS: usize = 15;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DirectoryIdentity {
    device: u64,
    inode: u64,
    mount_id: u64,
    uid: u32,
    gid: u32,
    mode: u32,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

struct RetainedReadOnlyDirectory {
    name: CString,
    file: File,
    identity: DirectoryIdentity,
}

struct RetainedReadOnlyParent<'view, 'prepared> {
    root: &'view RevalidatedTaskRootedAttachment<'prepared>,
    root_file: File,
    root_identity: DirectoryIdentity,
    chain: Vec<RetainedReadOnlyDirectory>,
}

enum ParentWalk<'view, 'prepared> {
    Existing(RetainedReadOnlyParent<'view, 'prepared>),
    Missing {
        root: &'view RevalidatedTaskRootedAttachment<'prepared>,
        root_file: File,
        root_identity: DirectoryIdentity,
        chain: Vec<RetainedReadOnlyDirectory>,
        missing_name: CString,
    },
}

impl RevalidatedTaskRootedAttachment<'_> {
    /// Read one leaf through validated parent components without creating or
    /// synchronizing any directory. A missing parent component proves the full
    /// leaf absent only after two descriptor-rooted missing-edge sandwiches.
    pub(crate) fn assess_boot_leaf_below_parent_until(
        &self,
        validated_parent_components: &[&str],
        request: RetainedBootLeafAssessmentRequest<'_>,
        limits: RetainedBootLeafAssessmentLimits,
        deadline: Instant,
    ) -> Result<ValidatedRetainedBootLeafAssessment, RetainedBootLeafAssessmentError> {
        let (parent_names, parent_components) =
            copy_parent_components(validated_parent_components, deadline)?;
        let (_, canonical_leaf) = validate_request(request, limits, deadline)?;
        let root = self.publication_parent_identity();
        let binding = AssessmentBinding {
            root,
            parent_components,
        };
        match walk_existing_parents(self, parent_names, deadline)? {
            ParentWalk::Existing(parent) => {
                assess_from_retained_parent_until(&parent, request, limits, binding, deadline)
            }
            ParentWalk::Missing {
                root: retained_root,
                root_file,
                root_identity,
                chain,
                missing_name,
            } => {
                require_missing_edge(
                    retained_root,
                    &root_file,
                    root_identity,
                    &chain,
                    &missing_name,
                    deadline,
                )?;
                effect::before_terminal_rebind();
                require_missing_edge(
                    retained_root,
                    &root_file,
                    root_identity,
                    &chain,
                    &missing_name,
                    deadline,
                )?;
                checkpoint(deadline)?;
                Ok(ValidatedRetainedBootLeafAssessment::new(
                    RetainedBootLeafAssessmentState::Absent,
                    (root.device, root.inode, root.mount_id),
                    binding.parent_components,
                    None,
                    canonical_leaf,
                    request.expected_length(),
                    request.expected_xxh3(),
                    request.expected_sha256(),
                    None,
                ))
            }
        }
    }
}

fn copy_parent_components(
    components: &[&str],
    deadline: Instant,
) -> Result<(Vec<CString>, Box<[Box<str>]>), RetainedBootLeafAssessmentError> {
    checkpoint(deadline)?;
    if components.is_empty() {
        return Err(RetainedBootLeafAssessmentError::EmptyParentComponents);
    }
    if components.len() > MAX_PARENT_COMPONENTS {
        return Err(RetainedBootLeafAssessmentError::ParentComponentLimit {
            limit: MAX_PARENT_COMPONENTS,
            actual: components.len(),
        });
    }
    let mut names = Vec::new();
    let mut owned = Vec::new();
    names
        .try_reserve_exact(components.len())
        .map_err(|source| RetainedBootLeafAssessmentError::Allocation { source })?;
    owned
        .try_reserve_exact(components.len())
        .map_err(|source| RetainedBootLeafAssessmentError::Allocation { source })?;
    for (index, component) in components.iter().enumerate() {
        checkpoint(deadline)?;
        let bytes = component.as_bytes();
        if bytes.is_empty()
            || bytes.len() > 255
            || matches!(bytes, b"." | b"..")
            || bytes.contains(&b'/')
            || bytes.contains(&0)
        {
            return Err(RetainedBootLeafAssessmentError::InvalidParentComponent { index });
        }
        names.push(
            CString::new(bytes)
                .map_err(|_| RetainedBootLeafAssessmentError::InvalidParentComponent { index })?,
        );
        let mut copy = String::new();
        copy.try_reserve_exact(bytes.len())
            .map_err(|source| RetainedBootLeafAssessmentError::Allocation { source })?;
        copy.push_str(component);
        owned.push(copy.into_boxed_str());
    }
    Ok((names, owned.into_boxed_slice()))
}

fn walk_existing_parents<'view, 'prepared>(
    root: &'view RevalidatedTaskRootedAttachment<'prepared>,
    names: Vec<CString>,
    deadline: Instant,
) -> Result<ParentWalk<'view, 'prepared>, RetainedBootLeafAssessmentError> {
    require_parent(root, "opening read-only boot-leaf parent walk", deadline)?;
    let root_scalar = root.publication_parent_identity();
    let root_file = open_parent_alias(root.publication_parent(), root_scalar, deadline)?;
    let root_identity = observe_root_directory(&root_file, root_scalar, deadline)?;
    let mut chain = Vec::new();
    chain
        .try_reserve_exact(names.len())
        .map_err(|source| RetainedBootLeafAssessmentError::Allocation { source })?;

    for (index, name) in names.into_iter().enumerate() {
        require_existing_chain(root, &root_file, root_identity, &chain, deadline)?;
        let parent = chain
            .last()
            .map_or(&root_file, |directory: &RetainedReadOnlyDirectory| &directory.file);
        let file = match open_directory(parent, &name, index, deadline)? {
            Some(file) => file,
            None => {
                return Ok(ParentWalk::Missing {
                    root,
                    root_file,
                    root_identity,
                    chain,
                    missing_name: name,
                });
            }
        };
        let identity = observe_directory(&file, root_identity, index, deadline)?;
        chain.push(RetainedReadOnlyDirectory {
            name,
            file,
            identity,
        });
    }
    require_existing_chain(root, &root_file, root_identity, &chain, deadline)?;
    Ok(ParentWalk::Existing(RetainedReadOnlyParent {
        root,
        root_file,
        root_identity,
        chain,
    }))
}

fn open_directory(
    parent: &File,
    name: &CStr,
    index: usize,
    deadline: Instant,
) -> Result<Option<File>, RetainedBootLeafAssessmentError> {
    checkpoint(deadline)?;
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
    .map(Some)
    .or_else(|source| match source.raw_os_error() {
        Some(nix::libc::ENOENT) => Ok(None),
        Some(nix::libc::ELOOP) | Some(nix::libc::ENOTDIR) => {
            Err(RetainedBootLeafAssessmentError::UnsafeParentType { index })
        }
        Some(nix::libc::EXDEV) => Err(RetainedBootLeafAssessmentError::AttachmentIdentityChanged {
            action: "opening one read-only boot-leaf parent component",
        }),
        _ => Err(RetainedBootLeafAssessmentError::Filesystem {
            action: "opening one read-only boot-leaf parent component",
            source,
        }),
    })
}

fn observe_root_directory(
    file: &File,
    expected: AttachmentIdentity,
    deadline: Instant,
) -> Result<DirectoryIdentity, RetainedBootLeafAssessmentError> {
    let found = directory_identity(file, "observing the read-only boot-leaf root", deadline)?;
    if found.device != expected.device || found.inode != expected.inode || found.mount_id != expected.mount_id {
        return Err(RetainedBootLeafAssessmentError::AttachmentIdentityChanged {
            action: "binding the read-only boot-leaf root",
        });
    }
    Ok(found)
}

fn observe_directory(
    file: &File,
    root: DirectoryIdentity,
    index: usize,
    deadline: Instant,
) -> Result<DirectoryIdentity, RetainedBootLeafAssessmentError> {
    let found = directory_identity(file, "observing one read-only boot-leaf parent", deadline)?;
    if found.device != root.device || found.mount_id != root.mount_id {
        return Err(RetainedBootLeafAssessmentError::AttachmentIdentityChanged {
            action: "binding one read-only boot-leaf parent component",
        });
    }
    if found.uid != root.uid
        || found.gid != root.gid
        || found.mode & 0o700 != 0o700
        || found.mode & 0o022 != 0
        || found.mode & 0o7000 != 0
    {
        return Err(RetainedBootLeafAssessmentError::UnsafeParentPolicy { index });
    }
    Ok(found)
}

fn directory_identity(
    file: &File,
    action: &'static str,
    deadline: Instant,
) -> Result<DirectoryIdentity, RetainedBootLeafAssessmentError> {
    checkpoint(deadline)?;
    let metadata = file
        .metadata()
        .map_err(|source| RetainedBootLeafAssessmentError::Filesystem { action, source })?;
    if !metadata.file_type().is_dir() || metadata.dev() == 0 || metadata.ino() == 0 {
        return Err(RetainedBootLeafAssessmentError::UnsafeParentType { index: 0 });
    }
    let mount_id = descriptor_mount_id_until(file, deadline)
        .map_err(|source| RetainedBootLeafAssessmentError::Filesystem { action, source })?;
    Ok(directory_snapshot(&metadata, mount_id))
}

fn directory_snapshot(metadata: &Metadata, mount_id: u64) -> DirectoryIdentity {
    DirectoryIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
        mount_id,
        uid: metadata.uid(),
        gid: metadata.gid(),
        mode: metadata.permissions().mode() & 0o7777,
        modified_seconds: metadata.mtime(),
        modified_nanoseconds: metadata.mtime_nsec(),
        changed_seconds: metadata.ctime(),
        changed_nanoseconds: metadata.ctime_nsec(),
    }
}

fn require_existing_chain(
    root: &RevalidatedTaskRootedAttachment<'_>,
    root_file: &File,
    root_identity: DirectoryIdentity,
    chain: &[RetainedReadOnlyDirectory],
    deadline: Instant,
) -> Result<(), RetainedBootLeafAssessmentError> {
    require_parent(root, "revalidating the read-only boot-leaf parent chain", deadline)?;
    require_directory_identity(root_file, root_identity, "revalidating the read-only boot-leaf root", deadline)?;
    let mut parent = root_file;
    for (index, directory) in chain.iter().enumerate() {
        require_directory_identity(
            &directory.file,
            directory.identity,
            "revalidating a retained read-only boot-leaf parent",
            deadline,
        )?;
        let rebound = open_directory(parent, &directory.name, index, deadline)?.ok_or(
            RetainedBootLeafAssessmentError::LeafIdentityChanged {
                action: "rebinding a retained read-only boot-leaf parent",
            },
        )?;
        require_directory_identity(
            &rebound,
            directory.identity,
            "rebinding a retained read-only boot-leaf parent",
            deadline,
        )?;
        parent = &directory.file;
    }
    require_parent(root, "closing the read-only boot-leaf parent-chain revalidation", deadline)
}

fn require_directory_identity(
    file: &File,
    expected: DirectoryIdentity,
    action: &'static str,
    deadline: Instant,
) -> Result<(), RetainedBootLeafAssessmentError> {
    let found = directory_identity(file, action, deadline)?;
    if found == expected {
        Ok(())
    } else {
        Err(RetainedBootLeafAssessmentError::AttachmentIdentityChanged { action })
    }
}

fn require_missing_edge(
    root: &RevalidatedTaskRootedAttachment<'_>,
    root_file: &File,
    root_identity: DirectoryIdentity,
    chain: &[RetainedReadOnlyDirectory],
    missing_name: &CStr,
    deadline: Instant,
) -> Result<(), RetainedBootLeafAssessmentError> {
    require_existing_chain(root, root_file, root_identity, chain, deadline)?;
    let parent = chain
        .last()
        .map_or(root_file, |directory: &RetainedReadOnlyDirectory| &directory.file);
    let index = chain.len();
    if open_directory(parent, missing_name, index, deadline)?.is_some() {
        return Err(RetainedBootLeafAssessmentError::LeafIdentityChanged {
            action: "revalidating the missing read-only boot-leaf parent edge",
        });
    }
    require_existing_chain(root, root_file, root_identity, chain, deadline)
}

impl RetainedReadOnlyParent<'_, '_> {
    fn destination_identity(&self) -> DirectoryIdentity {
        self.chain
            .last()
            .expect("validated read-only parent chain is nonempty")
            .identity
    }
}

impl RetainedBootFilePublicationTarget for RetainedReadOnlyParent<'_, '_> {
    fn publication_parent(&self) -> &File {
        &self
            .chain
            .last()
            .expect("validated read-only parent chain is nonempty")
            .file
    }

    fn publication_parent_identity(&self) -> AttachmentIdentity {
        let identity = self.destination_identity();
        AttachmentIdentity {
            device: identity.device,
            inode: identity.inode,
            mount_id: identity.mount_id,
        }
    }

    fn require_publication_parent_until(
        &self,
        action: &'static str,
        deadline: Instant,
    ) -> Result<(), RetainedBootFilePublicationError> {
        require_existing_chain(
            self.root,
            &self.root_file,
            self.root_identity,
            &self.chain,
            deadline,
        )
        .map_err(|source| RetainedBootFilePublicationError::Attachment {
            action,
            source: io::Error::other(source.to_string()),
        })
    }
}
