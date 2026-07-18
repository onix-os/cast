//! Descriptor-relative retention for the fixed machine-local topology source.

use std::{
    ffi::CStr,
    os::{
        fd::AsRawFd as _,
        unix::fs::{MetadataExt as _, PermissionsExt as _},
    },
    path::{Path, PathBuf},
};

use crate::{
    Installation,
    linux_fs::{
        controlled_resolution, descriptor_mount_id, open_path_descriptor_readonly_until, openat2_file_until,
        read_to_end_bounded, require_no_access_acl_until, require_no_default_acl_until, require_no_xattrs_until,
    },
};

use super::{ActiveReblitBootTopologyIntentError, BootTopologyIntentBudget, boot_topology_intent_path};

const DIRECTORY_COMPONENTS: [(&CStr, &str); 2] = [(c"etc", "etc"), (c"cast", "etc/cast")];
const SOURCE_NAME: &CStr = c"boot-topology.glu";

pub(super) struct RetainedBootTopologySource {
    components: Vec<RetainedDirectoryComponent>,
    source: std::fs::File,
    source_witness: SourceWitness,
    path: PathBuf,
}

struct RetainedDirectoryComponent {
    relative: &'static str,
    directory: std::fs::File,
    witness: DirectoryWitness,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DirectoryWitness {
    device: u64,
    inode: u64,
    mount_id: u64,
    owner: u32,
    group: u32,
    mode: u32,
    links: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SourceWitness {
    device: u64,
    inode: u64,
    mount_id: u64,
    owner: u32,
    group: u32,
    mode: u32,
    links: u64,
    length: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

impl RetainedBootTopologySource {
    pub(super) fn path(&self) -> &Path {
        &self.path
    }
}

pub(super) fn capture_source(
    installation: &Installation,
    budget: &mut BootTopologyIntentBudget,
) -> Result<(RetainedBootTopologySource, Box<[u8]>), ActiveReblitBootTopologyIntentError> {
    let root_path = installation.root.clone();
    let root = open_directory(
        installation.root_directory(),
        c".",
        &root_path,
        "duplicate authenticated installation root",
        budget,
    )?;
    let root_witness = directory_witness(&root, &root_path, budget)?;
    let mut components = Vec::with_capacity(DIRECTORY_COMPONENTS.len() + 1);
    components.push(RetainedDirectoryComponent {
        relative: ".",
        directory: root,
        witness: root_witness,
    });

    for (component, relative) in DIRECTORY_COMPONENTS {
        let path = installation.root.join(relative);
        let parent = &components
            .last()
            .expect("boot-topology component chain always retains its root")
            .directory;
        let directory = open_directory(
            parent,
            component,
            &path,
            "open fixed source directory component",
            budget,
        )?;
        require_directory_policy(&directory, &path, budget)?;
        let witness = directory_witness(&directory, &path, budget)?;
        components.push(RetainedDirectoryComponent {
            relative,
            directory,
            witness,
        });
    }

    require_component_chain(&components, installation, budget)?;
    let path = boot_topology_intent_path(installation);
    let parent = &components
        .last()
        .expect("boot-topology component chain always retains etc/cast")
        .directory;
    let source = open_source(parent, &path, budget)?;
    let source_witness = source_witness(&source, &path, budget)?;
    require_source_policy(&source, source_witness, &path, budget)?;
    let bytes = read_source_bytes(&source, source_witness, &path, budget)?;
    require_named_source(parent, source_witness, &path, budget)?;
    require_component_chain(&components, installation, budget)?;

    Ok((
        RetainedBootTopologySource {
            components,
            source,
            source_witness,
            path,
        },
        bytes,
    ))
}

pub(super) fn revalidate_source(
    installation: &Installation,
    retained: &RetainedBootTopologySource,
    budget: &mut BootTopologyIntentBudget,
) -> Result<Box<[u8]>, ActiveReblitBootTopologyIntentError> {
    require_component_chain(&retained.components, installation, budget)?;
    require_source_policy(&retained.source, retained.source_witness, &retained.path, budget)?;
    require_source_witness(&retained.source, retained.source_witness, &retained.path, budget)?;
    let retained_bytes = read_source_bytes(&retained.source, retained.source_witness, &retained.path, budget)?;
    let parent = &retained
        .components
        .last()
        .expect("retained boot-topology chain contains etc/cast")
        .directory;
    require_named_source(parent, retained.source_witness, &retained.path, budget)?;

    let (actual, actual_bytes) = capture_source(installation, budget)?;
    require_same_component_chains(&retained.components, &actual.components, installation, budget)?;
    if actual.source_witness != retained.source_witness {
        return Err(ActiveReblitBootTopologyIntentError::Changed {
            path: retained.path.clone(),
            reason: "fixed boot-topology source name no longer selects the retained inode",
        });
    }
    require_same_source(
        &retained.source,
        &actual.source,
        retained.source_witness,
        &retained.path,
        budget,
    )?;
    if retained_bytes != actual_bytes {
        return Err(ActiveReblitBootTopologyIntentError::Changed {
            path: retained.path.clone(),
            reason: "retained and rebound boot-topology source bytes differ",
        });
    }
    require_source_witness(&retained.source, retained.source_witness, &retained.path, budget)?;
    Ok(actual_bytes)
}

fn open_directory(
    parent: &std::fs::File,
    name: &CStr,
    path: &Path,
    operation: &'static str,
    budget: &mut BootTopologyIntentBudget,
) -> Result<std::fs::File, ActiveReblitBootTopologyIntentError> {
    budget.step(path)?;
    let probe = openat2_file_until(
        parent.as_raw_fd(),
        name,
        nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        controlled_resolution(),
        budget.deadline,
    );
    let probe = match probe {
        Ok(probe) => probe,
        Err(source) if source.raw_os_error() == Some(nix::libc::ENOENT) => {
            return Err(ActiveReblitBootTopologyIntentError::Missing {
                path: budget.source_path.clone(),
            });
        }
        Err(source) => return Err(io_error(operation, path, source)),
    };
    let metadata = probe
        .metadata()
        .map_err(|source| io_error("inspect source directory path descriptor", path, source))?;
    if !metadata.file_type().is_dir() || metadata.uid() != super::super::effective_user_id() {
        return Err(ActiveReblitBootTopologyIntentError::UnsafeInode {
            path: path.to_owned(),
            reason: "source directory component is not a same-owner directory",
        });
    }

    budget.step(path)?;
    let readable = openat2_file_until(
        parent.as_raw_fd(),
        name,
        nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK
            | nix::libc::O_NOATIME,
        0,
        controlled_resolution(),
        budget.deadline,
    )
    .map_err(|source| io_error("reopen source directory read-only", path, source))?;
    require_same_directory_inode(&probe, &readable, path, budget)?;
    Ok(readable)
}

fn open_source(
    directory: &std::fs::File,
    path: &Path,
    budget: &mut BootTopologyIntentBudget,
) -> Result<std::fs::File, ActiveReblitBootTopologyIntentError> {
    budget.step(path)?;
    match openat2_file_until(
        directory.as_raw_fd(),
        SOURCE_NAME,
        nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        controlled_resolution(),
        budget.deadline,
    ) {
        Ok(source) => Ok(source),
        Err(source) if source.raw_os_error() == Some(nix::libc::ENOENT) => {
            Err(ActiveReblitBootTopologyIntentError::Missing { path: path.to_owned() })
        }
        Err(source) => Err(io_error("open fixed source without following links", path, source)),
    }
}

fn require_named_source(
    directory: &std::fs::File,
    expected: SourceWitness,
    path: &Path,
    budget: &mut BootTopologyIntentBudget,
) -> Result<(), ActiveReblitBootTopologyIntentError> {
    let actual = open_source(directory, path, budget)?;
    if source_witness(&actual, path, budget)? == expected {
        Ok(())
    } else {
        Err(ActiveReblitBootTopologyIntentError::Changed {
            path: path.to_owned(),
            reason: "fixed boot-topology source name changed",
        })
    }
}

fn require_component_chain(
    components: &[RetainedDirectoryComponent],
    installation: &Installation,
    budget: &mut BootTopologyIntentBudget,
) -> Result<(), ActiveReblitBootTopologyIntentError> {
    for (index, component) in components.iter().enumerate() {
        let path = component_path(installation, component.relative);
        if index != 0 {
            require_directory_policy(&component.directory, &path, budget)?;
        }
        require_directory_witness(&component.directory, component.witness, &path, budget)?;
    }
    Ok(())
}

fn require_same_component_chains(
    expected: &[RetainedDirectoryComponent],
    actual: &[RetainedDirectoryComponent],
    installation: &Installation,
    budget: &mut BootTopologyIntentBudget,
) -> Result<(), ActiveReblitBootTopologyIntentError> {
    if expected.len() != actual.len() {
        return Err(ActiveReblitBootTopologyIntentError::Changed {
            path: boot_topology_intent_path(installation),
            reason: "boot-topology component-chain length changed",
        });
    }
    for (expected, actual) in expected.iter().zip(actual) {
        let path = component_path(installation, expected.relative);
        if expected.relative != actual.relative || expected.witness != actual.witness {
            return Err(ActiveReblitBootTopologyIntentError::Changed {
                path,
                reason: "boot-topology directory identity or metadata changed",
            });
        }
        require_same_directory(&expected.directory, &actual.directory, expected.witness, &path, budget)?;
    }
    Ok(())
}

fn require_directory_policy(
    directory: &std::fs::File,
    path: &Path,
    budget: &mut BootTopologyIntentBudget,
) -> Result<(), ActiveReblitBootTopologyIntentError> {
    let metadata = directory
        .metadata()
        .map_err(|source| io_error("inspect source directory policy", path, source))?;
    let mode = metadata.permissions().mode() & 0o7777;
    if !metadata.file_type().is_dir()
        || metadata.uid() != super::super::effective_user_id()
        || metadata.nlink() < 2
        || mode & 0o7000 != 0
        || mode & 0o500 != 0o500
        || mode & 0o022 != 0
    {
        return Err(ActiveReblitBootTopologyIntentError::UnsafeInode {
            path: path.to_owned(),
            reason: "source directory ownership, link count, or mode is unsafe",
        });
    }
    require_no_access_acl_until(directory, path, budget.deadline)
        .map_err(|source| io_error("reject source directory access ACL", path, source))?;
    require_no_default_acl_until(directory, path, budget.deadline)
        .map_err(|source| io_error("reject source directory default ACL", path, source))?;
    require_no_xattrs_until(directory, path, budget.deadline)
        .map_err(|source| io_error("reject source directory extended attributes", path, source))?;
    budget.require_deadline_at(path)
}

fn require_source_policy(
    source: &std::fs::File,
    witness: SourceWitness,
    path: &Path,
    budget: &mut BootTopologyIntentBudget,
) -> Result<(), ActiveReblitBootTopologyIntentError> {
    if witness.owner != super::super::effective_user_id()
        || witness.links != 1
        || witness.mode & 0o7000 != 0
        || witness.mode & 0o400 == 0
        || witness.mode & 0o133 != 0
    {
        return Err(ActiveReblitBootTopologyIntentError::UnsafeInode {
            path: path.to_owned(),
            reason: "source file ownership, links, or mode is unsafe",
        });
    }
    if witness.length > budget.policy.max_source_bytes as u64 {
        return Err(ActiveReblitBootTopologyIntentError::SourceBytesLimit {
            path: path.to_owned(),
            limit: budget.policy.max_source_bytes,
            actual: witness.length,
        });
    }
    let readable = open_path_descriptor_readonly_until(source, budget.deadline)
        .map_err(|source| io_error("reopen retained source for metadata policy", path, source))?;
    require_source_witness(&readable, witness, path, budget)?;
    require_no_access_acl_until(&readable, path, budget.deadline)
        .map_err(|source| io_error("reject source file access ACL", path, source))?;
    require_no_xattrs_until(&readable, path, budget.deadline)
        .map_err(|source| io_error("reject source file extended attributes", path, source))?;
    budget.require_deadline_at(path)
}

fn read_source_bytes(
    source: &std::fs::File,
    witness: SourceWitness,
    path: &Path,
    budget: &mut BootTopologyIntentBudget,
) -> Result<Box<[u8]>, ActiveReblitBootTopologyIntentError> {
    budget.step(path)?;
    let mut readable = open_path_descriptor_readonly_until(source, budget.deadline)
        .map_err(|source| io_error("reopen retained source for bounded read", path, source))?;
    require_source_witness(&readable, witness, path, budget)?;
    let bytes = read_to_end_bounded(&mut readable, budget.policy.max_source_bytes.saturating_add(1))
        .map_err(|source| io_error("read bounded source bytes", path, source))?;
    budget.require_deadline_at(path)?;
    if bytes.len() > budget.policy.max_source_bytes {
        return Err(ActiveReblitBootTopologyIntentError::SourceBytesLimit {
            path: path.to_owned(),
            limit: budget.policy.max_source_bytes,
            actual: bytes.len() as u64,
        });
    }
    if bytes.len() as u64 != witness.length {
        return Err(ActiveReblitBootTopologyIntentError::Changed {
            path: path.to_owned(),
            reason: "boot-topology source length changed while reading",
        });
    }
    require_source_witness(&readable, witness, path, budget)?;
    Ok(bytes.into_boxed_slice())
}

fn directory_witness(
    directory: &std::fs::File,
    path: &Path,
    budget: &mut BootTopologyIntentBudget,
) -> Result<DirectoryWitness, ActiveReblitBootTopologyIntentError> {
    budget.step(path)?;
    let metadata = directory
        .metadata()
        .map_err(|source| io_error("inspect retained source directory", path, source))?;
    budget.require_deadline_at(path)?;
    Ok(DirectoryWitness {
        device: metadata.dev(),
        inode: metadata.ino(),
        mount_id: retained_mount_id(directory, path, budget)?,
        owner: metadata.uid(),
        group: metadata.gid(),
        mode: metadata.permissions().mode() & 0o7777,
        links: metadata.nlink(),
        modified_seconds: metadata.mtime(),
        modified_nanoseconds: metadata.mtime_nsec(),
        changed_seconds: metadata.ctime(),
        changed_nanoseconds: metadata.ctime_nsec(),
    })
}

fn source_witness(
    source: &std::fs::File,
    path: &Path,
    budget: &mut BootTopologyIntentBudget,
) -> Result<SourceWitness, ActiveReblitBootTopologyIntentError> {
    budget.step(path)?;
    let metadata = source
        .metadata()
        .map_err(|source| io_error("inspect retained boot-topology source", path, source))?;
    if !metadata.file_type().is_file() {
        return Err(ActiveReblitBootTopologyIntentError::UnsafeInode {
            path: path.to_owned(),
            reason: "boot-topology source is not a regular file",
        });
    }
    budget.require_deadline_at(path)?;
    Ok(SourceWitness {
        device: metadata.dev(),
        inode: metadata.ino(),
        mount_id: retained_mount_id(source, path, budget)?,
        owner: metadata.uid(),
        group: metadata.gid(),
        mode: metadata.permissions().mode() & 0o7777,
        links: metadata.nlink(),
        length: metadata.len(),
        modified_seconds: metadata.mtime(),
        modified_nanoseconds: metadata.mtime_nsec(),
        changed_seconds: metadata.ctime(),
        changed_nanoseconds: metadata.ctime_nsec(),
    })
}

fn require_directory_witness(
    directory: &std::fs::File,
    expected: DirectoryWitness,
    path: &Path,
    budget: &mut BootTopologyIntentBudget,
) -> Result<(), ActiveReblitBootTopologyIntentError> {
    if directory_witness(directory, path, budget)? == expected {
        Ok(())
    } else {
        Err(ActiveReblitBootTopologyIntentError::Changed {
            path: path.to_owned(),
            reason: "retained boot-topology directory metadata changed",
        })
    }
}

fn require_source_witness(
    source: &std::fs::File,
    expected: SourceWitness,
    path: &Path,
    budget: &mut BootTopologyIntentBudget,
) -> Result<(), ActiveReblitBootTopologyIntentError> {
    if source_witness(source, path, budget)? == expected {
        Ok(())
    } else {
        Err(ActiveReblitBootTopologyIntentError::Changed {
            path: path.to_owned(),
            reason: "retained boot-topology source metadata changed",
        })
    }
}

fn require_same_directory(
    expected: &std::fs::File,
    actual: &std::fs::File,
    witness: DirectoryWitness,
    path: &Path,
    budget: &mut BootTopologyIntentBudget,
) -> Result<(), ActiveReblitBootTopologyIntentError> {
    if directory_witness(expected, path, budget)? == witness && directory_witness(actual, path, budget)? == witness {
        Ok(())
    } else {
        Err(ActiveReblitBootTopologyIntentError::Changed {
            path: path.to_owned(),
            reason: "rebound source directory is not the retained inode",
        })
    }
}

fn require_same_source(
    expected: &std::fs::File,
    actual: &std::fs::File,
    witness: SourceWitness,
    path: &Path,
    budget: &mut BootTopologyIntentBudget,
) -> Result<(), ActiveReblitBootTopologyIntentError> {
    if source_witness(expected, path, budget)? == witness && source_witness(actual, path, budget)? == witness {
        Ok(())
    } else {
        Err(ActiveReblitBootTopologyIntentError::Changed {
            path: path.to_owned(),
            reason: "rebound boot-topology source is not the retained inode",
        })
    }
}

fn require_same_directory_inode(
    expected: &std::fs::File,
    actual: &std::fs::File,
    path: &Path,
    budget: &mut BootTopologyIntentBudget,
) -> Result<(), ActiveReblitBootTopologyIntentError> {
    budget.step(path)?;
    let expected_mount_id = retained_mount_id(expected, path, budget)?;
    let actual_mount_id = retained_mount_id(actual, path, budget)?;
    let expected_metadata = expected
        .metadata()
        .map_err(|source| io_error("inspect source directory path descriptor", path, source))?;
    let actual_metadata = actual
        .metadata()
        .map_err(|source| io_error("inspect readable source directory", path, source))?;
    if (
        expected_metadata.dev(),
        expected_metadata.ino(),
        expected_metadata.mode(),
        expected_mount_id,
    ) == (
        actual_metadata.dev(),
        actual_metadata.ino(),
        actual_metadata.mode(),
        actual_mount_id,
    ) {
        Ok(())
    } else {
        Err(ActiveReblitBootTopologyIntentError::Changed {
            path: path.to_owned(),
            reason: "readable source directory is not the probed inode",
        })
    }
}

fn retained_mount_id(
    descriptor: &std::fs::File,
    path: &Path,
    budget: &mut BootTopologyIntentBudget,
) -> Result<u64, ActiveReblitBootTopologyIntentError> {
    budget.step(path)?;
    let mount_id = descriptor_mount_id(descriptor)
        .map_err(|source| io_error("inspect retained source mount identity", path, source))?;
    budget.require_deadline_at(path)?;
    Ok(mount_id)
}

fn component_path(installation: &Installation, relative: &str) -> PathBuf {
    if relative == "." {
        installation.root.clone()
    } else {
        installation.root.join(relative)
    }
}

fn io_error(operation: &'static str, path: &Path, source: std::io::Error) -> ActiveReblitBootTopologyIntentError {
    ActiveReblitBootTopologyIntentError::Io {
        operation,
        path: path.to_owned(),
        source,
    }
}
