//! Descriptor-relative capture and revalidation for local boot policy.

use std::{
    ffi::{CStr, CString, OsStr},
    io,
    os::{
        fd::AsRawFd as _,
        unix::{
            ffi::OsStrExt as _,
            fs::{MetadataExt as _, PermissionsExt as _},
        },
    },
    path::{Path, PathBuf},
};

use crate::{
    Installation,
    linux_fs::{
        descriptor_mount_id, open_path_descriptor_readonly_until, openat2_file_until, read_to_end_bounded,
        require_no_access_acl_until, require_no_default_acl_until, require_no_xattrs_until,
    },
};

use super::{
    ActiveReblitLocalBootPolicyError, LocalBootPolicyBudget, RetainedLocalCmdlineEntry, local_policy_path,
    normalize_cmdline,
};

const LOCAL_POLICY_COMPONENTS: [&CStr; 3] = [c"etc", c"kernel", c"cmdline.d"];
const MAX_READLINK_BYTES: usize = 64;
const MAX_RAW_SYSCALL_INTERRUPTS: usize = 1_024;

pub(super) enum RetainedLocalPolicyLocation {
    Absent {
        components: Vec<RetainedLocalPolicyComponent>,
        missing: CString,
    },
    Present {
        components: Vec<RetainedLocalPolicyComponent>,
    },
}

pub(super) struct RetainedLocalPolicyComponent {
    relative: CString,
    directory: std::fs::File,
    witness: DirectoryWitness,
}

impl RetainedLocalPolicyLocation {
    pub(super) fn is_absent(&self) -> bool {
        matches!(self, Self::Absent { .. })
    }

    pub(super) fn present_directory(&self) -> Option<&std::fs::File> {
        match self {
            Self::Present { components } => components.last().map(|component| &component.directory),
            Self::Absent { .. } => None,
        }
    }

    pub(super) fn present_witness(&self) -> Option<DirectoryWitness> {
        match self {
            Self::Present { components } => components.last().map(|component| component.witness),
            Self::Absent { .. } => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct DirectoryWitness {
    device: u64,
    inode: u64,
    mount_id: u64,
    owner: u32,
    mode: u32,
    links: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct EntryWitness {
    device: u64,
    inode: u64,
    mount_id: u64,
    owner: u32,
    file_type: u32,
    mode: u32,
    links: u64,
    length: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

#[cfg(test)]
impl EntryWitness {
    pub(super) fn with_length(mut self, length: u64) -> Self {
        self.length = length;
        self
    }
}

pub(super) fn capture_location(
    installation: &Installation,
    budget: &mut LocalBootPolicyBudget,
) -> Result<RetainedLocalPolicyLocation, ActiveReblitLocalBootPolicyError> {
    let root_path = installation.root.clone();
    let root = open_directory(
        installation.root_directory(),
        c".",
        &root_path,
        "duplicate authenticated installation root for local boot policy",
        budget,
    )?;
    let root_witness = directory_witness(&root, &root_path, budget)?;
    let mut components = Vec::with_capacity(LOCAL_POLICY_COMPONENTS.len() + 1);
    components.push(RetainedLocalPolicyComponent {
        relative: CString::new(".").expect("dot contains no NUL"),
        directory: root,
        witness: root_witness,
    });
    let mut relative = Vec::new();

    for (index, component) in LOCAL_POLICY_COMPONENTS.iter().enumerate() {
        let component_path = component_path(installation, &relative, component);
        let parent = &components
            .last()
            .expect("retained local-policy chain always contains the installation root")
            .directory;
        budget.step(&component_path)?;
        match open_directory(
            parent,
            component,
            &component_path,
            "open local boot policy directory component",
            budget,
        ) {
            Ok(next) => {
                require_directory_policy(&next, &component_path, budget)?;
                if !relative.is_empty() {
                    relative.push(b'/');
                }
                relative.extend_from_slice(component.to_bytes());
                let witness = directory_witness(&next, &component_path, budget)?;
                components.push(RetainedLocalPolicyComponent {
                    relative: CString::new(relative.clone()).expect("fixed local-policy path contains no NUL"),
                    directory: next,
                    witness,
                });
                if index + 1 == LOCAL_POLICY_COMPONENTS.len() {
                    require_component_chain(&components, installation, budget)?;
                    return Ok(RetainedLocalPolicyLocation::Present { components });
                }
            }
            Err(ActiveReblitLocalBootPolicyError::Io {
                operation: "open local boot policy directory component",
                source,
                ..
            }) if source.raw_os_error() == Some(nix::libc::ENOENT) => {
                let parent_path = if relative.is_empty() {
                    installation.root.clone()
                } else {
                    installation.root.join(OsStr::from_bytes(&relative))
                };
                let parent = components
                    .last()
                    .expect("retained local-policy chain always contains the absence parent");
                require_missing_child(&parent.directory, component, &component_path, budget)?;
                require_missing_child(&parent.directory, component, &component_path, budget)?;
                require_directory_witness(&parent.directory, parent.witness, &parent_path, budget)?;
                require_component_chain(&components, installation, budget)?;
                return Ok(RetainedLocalPolicyLocation::Absent {
                    components,
                    missing: (*component).to_owned(),
                });
            }
            Err(source) => return Err(source),
        }
    }
    unreachable!("fixed local-policy component list is nonempty")
}

pub(super) fn revalidate_location(
    installation: &Installation,
    retained: &RetainedLocalPolicyLocation,
    budget: &mut LocalBootPolicyBudget,
) -> Result<Option<std::fs::File>, ActiveReblitLocalBootPolicyError> {
    let recaptured = capture_location(installation, budget)?;
    match (retained, recaptured) {
        (
            RetainedLocalPolicyLocation::Present { components: expected },
            RetainedLocalPolicyLocation::Present { components: mut actual },
        ) => {
            require_same_component_chains(expected, &actual, installation, budget)?;
            Ok(Some(
                actual
                    .pop()
                    .expect("present local-policy chain contains cmdline.d")
                    .directory,
            ))
        }
        (
            RetainedLocalPolicyLocation::Absent {
                components: expected,
                missing: expected_missing,
            },
            RetainedLocalPolicyLocation::Absent {
                components: actual,
                missing: actual_missing,
            },
        ) if expected_missing == &actual_missing => {
            require_same_component_chains(expected, &actual, installation, budget)?;
            Ok(None)
        }
        _ => Err(ActiveReblitLocalBootPolicyError::Changed {
            path: local_policy_path(installation),
            reason: "local-policy component chain or retained absence shape changed",
        }),
    }
}

fn require_component_chain(
    components: &[RetainedLocalPolicyComponent],
    installation: &Installation,
    budget: &mut LocalBootPolicyBudget,
) -> Result<(), ActiveReblitLocalBootPolicyError> {
    for (index, component) in components.iter().enumerate() {
        let path = retained_component_path(installation, component);
        if index != 0 {
            require_directory_policy(&component.directory, &path, budget)?;
        }
        require_directory_witness(&component.directory, component.witness, &path, budget)?;
    }
    Ok(())
}

fn require_same_component_chains(
    expected: &[RetainedLocalPolicyComponent],
    actual: &[RetainedLocalPolicyComponent],
    installation: &Installation,
    budget: &mut LocalBootPolicyBudget,
) -> Result<(), ActiveReblitLocalBootPolicyError> {
    if expected.len() != actual.len() {
        return Err(ActiveReblitLocalBootPolicyError::Changed {
            path: local_policy_path(installation),
            reason: "local-policy component-chain length changed",
        });
    }
    for (expected, actual) in expected.iter().zip(actual) {
        let path = retained_component_path(installation, expected);
        if expected.relative != actual.relative || expected.witness != actual.witness {
            return Err(ActiveReblitLocalBootPolicyError::Changed {
                path,
                reason: "local-policy component identity or metadata changed",
            });
        }
        require_same_directory(&expected.directory, &actual.directory, &path, budget)?;
    }
    Ok(())
}

fn retained_component_path(installation: &Installation, component: &RetainedLocalPolicyComponent) -> PathBuf {
    if component.relative.as_bytes() == b"." {
        installation.root.clone()
    } else {
        installation.root.join(OsStr::from_bytes(component.relative.as_bytes()))
    }
}

pub(super) fn inventory_names(
    directory: &std::fs::File,
    path: &Path,
    budget: &mut LocalBootPolicyBudget,
) -> Result<Vec<Box<[u8]>>, ActiveReblitLocalBootPolicyError> {
    budget.step(path)?;
    let duplicate = retry_raw_syscall(path, "duplicate local-policy directory for inventory", budget, || {
        // SAFETY: fcntl receives one live descriptor and returns a fresh one.
        unsafe { nix::libc::fcntl(directory.as_raw_fd(), nix::libc::F_DUPFD_CLOEXEC, 0) as nix::libc::c_long }
    })?;
    let duplicate = i32::try_from(duplicate).map_err(|_| {
        io_error(
            "decode duplicated local-policy directory descriptor",
            path,
            io::Error::new(io::ErrorKind::InvalidData, "fcntl returned an oversized descriptor"),
        )
    })?;
    if let Err(source) = retry_raw_syscall(path, "rewind local-policy directory inventory", budget, || {
        // SAFETY: duplicate is a live directory descriptor not yet consumed.
        unsafe { nix::libc::lseek(duplicate, 0, nix::libc::SEEK_SET) as nix::libc::c_long }
    }) {
        // SAFETY: fdopendir has not consumed the fresh descriptor.
        unsafe { nix::libc::close(duplicate) };
        return Err(source);
    }
    // SAFETY: fdopendir consumes the fresh descriptor on success.
    let stream = unsafe { nix::libc::fdopendir(duplicate) };
    if stream.is_null() {
        let source = io::Error::last_os_error();
        // SAFETY: fdopendir failed and left ownership with the caller.
        unsafe { nix::libc::close(duplicate) };
        return Err(io_error("open local-policy inventory stream", path, source));
    }

    let mut names = Vec::new();
    let mut total_name_bytes = 0usize;
    let result = loop {
        if let Err(source) = budget.step(path) {
            break Err(source);
        }
        // SAFETY: errno is thread-local and readdir uses null for EOF/error.
        unsafe { *nix::libc::__errno_location() = 0 };
        // SAFETY: stream remains live and exclusively used here.
        let entry = unsafe { nix::libc::readdir(stream) };
        if entry.is_null() {
            let source = io::Error::last_os_error();
            if source.raw_os_error() == Some(0) {
                break Ok(());
            }
            if source.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            break Err(io_error("read local-policy directory inventory", path, source));
        }
        // SAFETY: d_name is NUL-terminated until the next readdir call.
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if matches!(name, b"." | b"..") {
            continue;
        }
        if name.len() > budget.policy.max_name_bytes {
            break Err(ActiveReblitLocalBootPolicyError::NameBytesLimit {
                path: path.join(OsStr::from_bytes(name)),
                limit: budget.policy.max_name_bytes,
                actual: name.len(),
            });
        }
        let actual_entries = names.len().saturating_add(1);
        if actual_entries > budget.policy.max_directory_entries {
            break Err(ActiveReblitLocalBootPolicyError::DirectoryEntryLimit {
                path: path.to_owned(),
                limit: budget.policy.max_directory_entries,
                actual: actual_entries,
            });
        }
        total_name_bytes = match total_name_bytes.checked_add(name.len()) {
            Some(total) => total,
            None => usize::MAX,
        };
        if total_name_bytes > budget.policy.max_total_name_bytes {
            break Err(ActiveReblitLocalBootPolicyError::TotalNameBytesLimit {
                path: path.to_owned(),
                limit: budget.policy.max_total_name_bytes,
                actual: total_name_bytes,
            });
        }
        if let Err(source) = names.try_reserve(1) {
            break Err(ActiveReblitLocalBootPolicyError::Allocation {
                resource: "local-policy directory names",
                path: path.to_owned(),
                source,
            });
        }
        names.push(Box::<[u8]>::from(name));
    };

    // SAFETY: stream was returned by fdopendir and remains live.
    let close_result = unsafe { nix::libc::closedir(stream) };
    result?;
    if close_result == -1 {
        return Err(io_error(
            "close local-policy directory inventory",
            path,
            io::Error::last_os_error(),
        ));
    }
    names.sort_unstable();
    if names.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(ActiveReblitLocalBootPolicyError::Changed {
            path: path.to_owned(),
            reason: "directory returned duplicate entry names",
        });
    }
    Ok(names)
}

pub(super) fn capture_entry(
    directory: &std::fs::File,
    name: &[u8],
    installation: &Installation,
    budget: &mut LocalBootPolicyBudget,
) -> Result<RetainedLocalCmdlineEntry, ActiveReblitLocalBootPolicyError> {
    let encoded = CString::new(name).expect("readdir name contains no embedded NUL");
    let path = local_policy_path(installation).join(OsStr::from_bytes(name));
    let retained = open_entry(directory, &encoded, &path, budget)?;
    let witness = entry_witness(&retained, &path, budget)?;
    match witness.file_type {
        kind if kind == nix::libc::S_IFREG => {
            require_regular_policy(&retained, witness, &path, budget)?;
            let raw = read_regular_bytes(&retained, witness, &path, budget)?;
            let snippet = normalize_cmdline(&raw, &path)?;
            require_named_entry(directory, &encoded, witness, &path, budget)?;
            Ok(RetainedLocalCmdlineEntry::Append {
                name: Box::from(name),
                retained,
                witness,
                raw,
                snippet,
            })
        }
        kind if kind == nix::libc::S_IFLNK => {
            require_symlink_policy(witness, &path)?;
            require_same_entry(&retained, witness, &path, budget)?;
            require_named_entry(directory, &encoded, witness, &path, budget)?;
            let target = read_link_target(&retained, witness, &path, budget)?;
            if target.as_ref() != b"/dev/null" {
                return Err(ActiveReblitLocalBootPolicyError::UnsafeInode {
                    path,
                    reason: "local command-line symlink is not an exact /dev/null mask",
                });
            }
            require_same_entry(&retained, witness, &path, budget)?;
            require_named_entry(directory, &encoded, witness, &path, budget)?;
            Ok(RetainedLocalCmdlineEntry::Mask {
                name: Box::from(name),
                retained,
                witness,
            })
        }
        _ => Err(ActiveReblitLocalBootPolicyError::UnsafeInode {
            path,
            reason: "local command-line entry is neither a regular file nor a /dev/null symlink",
        }),
    }
}

pub(super) fn revalidate_entry(
    directory: &std::fs::File,
    entry: &RetainedLocalCmdlineEntry,
    installation: &Installation,
    budget: &mut LocalBootPolicyBudget,
) -> Result<(), ActiveReblitLocalBootPolicyError> {
    let name = entry.name_bytes();
    let encoded = CString::new(name).expect("captured readdir name contains no NUL");
    let path = local_policy_path(installation).join(OsStr::from_bytes(name));
    match entry {
        RetainedLocalCmdlineEntry::Append {
            retained,
            witness,
            raw,
            snippet,
            ..
        } => {
            require_same_entry(retained, *witness, &path, budget)?;
            require_named_entry(directory, &encoded, *witness, &path, budget)?;
            require_regular_policy(retained, *witness, &path, budget)?;
            let actual = read_regular_bytes(retained, *witness, &path, budget)?;
            if actual.as_ref() != raw.as_ref() || normalize_cmdline(&actual, &path)?.as_ref() != snippet.as_ref() {
                return Err(ActiveReblitLocalBootPolicyError::Changed {
                    path,
                    reason: "local command-line regular-file bytes changed",
                });
            }
            require_named_entry(directory, &encoded, *witness, &path, budget)
        }
        RetainedLocalCmdlineEntry::Mask { retained, witness, .. } => {
            require_same_entry(retained, *witness, &path, budget)?;
            require_symlink_policy(*witness, &path)?;
            require_named_entry(directory, &encoded, *witness, &path, budget)?;
            if read_link_target(retained, *witness, &path, budget)?.as_ref() != b"/dev/null" {
                return Err(ActiveReblitLocalBootPolicyError::Changed {
                    path,
                    reason: "local command-line mask target changed",
                });
            }
            require_same_entry(retained, *witness, &path, budget)?;
            require_named_entry(directory, &encoded, *witness, &path, budget)
        }
    }
}

pub(super) fn require_present_directory_witness(
    directory: &std::fs::File,
    expected: DirectoryWitness,
    path: &Path,
    budget: &mut LocalBootPolicyBudget,
) -> Result<(), ActiveReblitLocalBootPolicyError> {
    require_directory_witness(directory, expected, path, budget)
}

fn open_directory(
    parent: &std::fs::File,
    name: &CStr,
    path: &Path,
    operation: &'static str,
    budget: &mut LocalBootPolicyBudget,
) -> Result<std::fs::File, ActiveReblitLocalBootPolicyError> {
    budget.step(path)?;
    let probe = openat2_file_until(
        parent.as_raw_fd(),
        name,
        nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        local_policy_resolution(),
        budget.deadline,
    )
    .map_err(|source| io_error(operation, path, source))?;
    let metadata = probe
        .metadata()
        .map_err(|source| io_error("inspect local-policy directory probe", path, source))?;
    if !metadata.file_type().is_dir() || metadata.uid() != super::super::effective_user_id() {
        return Err(ActiveReblitLocalBootPolicyError::UnsafeInode {
            path: path.to_owned(),
            reason: "local-policy directory component is not a same-owner directory",
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
        local_policy_resolution(),
        budget.deadline,
    )
    .map_err(|source| io_error("reopen local-policy directory probe read-only", path, source))?;
    require_same_inode_metadata(&probe, &readable, path, budget)?;
    Ok(readable)
}

fn open_entry(
    directory: &std::fs::File,
    name: &CStr,
    path: &Path,
    budget: &mut LocalBootPolicyBudget,
) -> Result<std::fs::File, ActiveReblitLocalBootPolicyError> {
    budget.step(path)?;
    openat2_file_until(
        directory.as_raw_fd(),
        name,
        nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        local_policy_resolution(),
        budget.deadline,
    )
    .map_err(|source| io_error("open local command-line entry without following links", path, source))
}

fn require_named_entry(
    directory: &std::fs::File,
    name: &CStr,
    expected: EntryWitness,
    path: &Path,
    budget: &mut LocalBootPolicyBudget,
) -> Result<(), ActiveReblitLocalBootPolicyError> {
    let named = open_entry(directory, name, path, budget)?;
    let actual = entry_witness(&named, path, budget)?;
    if actual == expected {
        Ok(())
    } else {
        Err(ActiveReblitLocalBootPolicyError::Changed {
            path: path.to_owned(),
            reason: "named local command-line entry is not the retained inode",
        })
    }
}

fn require_missing_child(
    parent: &std::fs::File,
    name: &CStr,
    path: &Path,
    budget: &mut LocalBootPolicyBudget,
) -> Result<(), ActiveReblitLocalBootPolicyError> {
    budget.step(path)?;
    match openat2_file_until(
        parent.as_raw_fd(),
        name,
        nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        local_policy_resolution(),
        budget.deadline,
    ) {
        Err(source) if source.raw_os_error() == Some(nix::libc::ENOENT) => Ok(()),
        Ok(_) => Err(ActiveReblitLocalBootPolicyError::Changed {
            path: path.to_owned(),
            reason: "retained local-policy absence component appeared",
        }),
        Err(source) => Err(io_error("prove local-policy component absence", path, source)),
    }
}

fn require_directory_policy(
    directory: &std::fs::File,
    path: &Path,
    budget: &mut LocalBootPolicyBudget,
) -> Result<(), ActiveReblitLocalBootPolicyError> {
    let metadata = directory
        .metadata()
        .map_err(|source| io_error("inspect local-policy directory policy", path, source))?;
    let mode = metadata.permissions().mode() & 0o7777;
    if !metadata.file_type().is_dir()
        || metadata.uid() != super::super::effective_user_id()
        || metadata.nlink() < 2
        || mode & 0o7000 != 0
        || mode & 0o500 != 0o500
        || mode & 0o022 != 0
    {
        return Err(ActiveReblitLocalBootPolicyError::UnsafeInode {
            path: path.to_owned(),
            reason: "directory ownership, link count, or mode is unsafe",
        });
    }
    require_no_access_acl_until(directory, path, budget.deadline)
        .map_err(|source| io_error("reject local-policy directory access ACL", path, source))?;
    require_no_default_acl_until(directory, path, budget.deadline)
        .map_err(|source| io_error("reject local-policy directory default ACL", path, source))?;
    require_no_xattrs_until(directory, path, budget.deadline)
        .map_err(|source| io_error("reject local-policy directory extended attributes", path, source))?;
    budget.require_deadline(path)
}

fn require_regular_policy(
    retained: &std::fs::File,
    witness: EntryWitness,
    path: &Path,
    budget: &mut LocalBootPolicyBudget,
) -> Result<(), ActiveReblitLocalBootPolicyError> {
    if witness.file_type != nix::libc::S_IFREG
        || witness.owner != super::super::effective_user_id()
        || witness.links != 1
        || witness.mode & 0o7000 != 0
        || witness.mode & 0o400 == 0
        || witness.mode & 0o133 != 0
    {
        return Err(ActiveReblitLocalBootPolicyError::UnsafeInode {
            path: path.to_owned(),
            reason: "regular-file ownership, links, or mode is unsafe",
        });
    }
    if witness.length > budget.policy.max_file_bytes as u64 {
        return Err(ActiveReblitLocalBootPolicyError::FileBytesLimit {
            path: path.to_owned(),
            limit: budget.policy.max_file_bytes,
            actual: witness.length,
        });
    }
    let readable = open_path_descriptor_readonly_until(retained, budget.deadline)
        .map_err(|source| io_error("reopen local command-line file read-only", path, source))?;
    require_same_entry(&readable, witness, path, budget)?;
    require_no_access_acl_until(&readable, path, budget.deadline)
        .map_err(|source| io_error("reject local command-line file access ACL", path, source))?;
    require_no_xattrs_until(&readable, path, budget.deadline)
        .map_err(|source| io_error("reject local command-line file extended attributes", path, source))?;
    budget.require_deadline(path)
}

fn require_symlink_policy(witness: EntryWitness, path: &Path) -> Result<(), ActiveReblitLocalBootPolicyError> {
    if witness.file_type == nix::libc::S_IFLNK
        && witness.owner == super::super::effective_user_id()
        && witness.links == 1
        && witness.length == b"/dev/null".len() as u64
    {
        Ok(())
    } else {
        Err(ActiveReblitLocalBootPolicyError::UnsafeInode {
            path: path.to_owned(),
            reason: "local command-line symlink is not an exact /dev/null mask",
        })
    }
}

fn read_regular_bytes(
    retained: &std::fs::File,
    witness: EntryWitness,
    path: &Path,
    budget: &mut LocalBootPolicyBudget,
) -> Result<Box<[u8]>, ActiveReblitLocalBootPolicyError> {
    require_regular_policy(retained, witness, path, budget)?;
    let mut readable = open_path_descriptor_readonly_until(retained, budget.deadline)
        .map_err(|source| io_error("reopen local command-line content", path, source))?;
    require_same_entry(&readable, witness, path, budget)?;
    let bytes = read_to_end_bounded(&mut readable, budget.policy.max_file_bytes.saturating_add(1))
        .map_err(|source| io_error("read bounded local command-line content", path, source))?;
    budget.require_deadline(path)?;
    if bytes.len() > budget.policy.max_file_bytes || bytes.len() as u64 != witness.length {
        return Err(ActiveReblitLocalBootPolicyError::Changed {
            path: path.to_owned(),
            reason: "local command-line length changed while reading",
        });
    }
    require_same_entry(&readable, witness, path, budget)?;
    Ok(bytes.into_boxed_slice())
}

pub(super) fn read_link_target(
    retained: &std::fs::File,
    witness: EntryWitness,
    path: &Path,
    budget: &mut LocalBootPolicyBudget,
) -> Result<Box<[u8]>, ActiveReblitLocalBootPolicyError> {
    if witness.length > MAX_READLINK_BYTES as u64 {
        return Err(ActiveReblitLocalBootPolicyError::UnsafeInode {
            path: path.to_owned(),
            reason: "local command-line mask target is oversized",
        });
    }
    let mut bytes = [0_u8; MAX_READLINK_BYTES + 1];
    let length = retry_raw_syscall(path, "read raw local command-line mask target", budget, || {
        // SAFETY: retained is a live O_PATH descriptor for the symlink, the
        // empty path requests that exact inode, and bytes is writable.
        unsafe {
            nix::libc::readlinkat(
                retained.as_raw_fd(),
                c"".as_ptr(),
                bytes.as_mut_ptr().cast(),
                bytes.len(),
            ) as nix::libc::c_long
        }
    })?;
    let length = usize::try_from(length).map_err(|_| {
        io_error(
            "decode local command-line mask target length",
            path,
            io::Error::new(io::ErrorKind::InvalidData, "negative readlink length"),
        )
    })?;
    if length > MAX_READLINK_BYTES || length as u64 != witness.length {
        return Err(ActiveReblitLocalBootPolicyError::Changed {
            path: path.to_owned(),
            reason: "retained local command-line mask target length changed",
        });
    }
    budget.require_deadline(path)?;
    Ok(Box::from(&bytes[..length]))
}

fn directory_witness(
    directory: &std::fs::File,
    path: &Path,
    budget: &mut LocalBootPolicyBudget,
) -> Result<DirectoryWitness, ActiveReblitLocalBootPolicyError> {
    budget.step(path)?;
    let metadata = directory
        .metadata()
        .map_err(|source| io_error("inspect retained local-policy directory", path, source))?;
    let mount_id = descriptor_mount_id(directory)
        .map_err(|source| io_error("inspect local-policy directory mount identity", path, source))?;
    budget.require_deadline(path)?;
    Ok(DirectoryWitness {
        device: metadata.dev(),
        inode: metadata.ino(),
        mount_id,
        owner: metadata.uid(),
        mode: metadata.permissions().mode() & 0o7777,
        links: metadata.nlink(),
        modified_seconds: metadata.mtime(),
        modified_nanoseconds: metadata.mtime_nsec(),
        changed_seconds: metadata.ctime(),
        changed_nanoseconds: metadata.ctime_nsec(),
    })
}

fn entry_witness(
    entry: &std::fs::File,
    path: &Path,
    budget: &mut LocalBootPolicyBudget,
) -> Result<EntryWitness, ActiveReblitLocalBootPolicyError> {
    budget.step(path)?;
    let metadata = entry
        .metadata()
        .map_err(|source| io_error("inspect retained local command-line entry", path, source))?;
    let mount_id = descriptor_mount_id(entry)
        .map_err(|source| io_error("inspect local command-line entry mount identity", path, source))?;
    budget.require_deadline(path)?;
    Ok(EntryWitness {
        device: metadata.dev(),
        inode: metadata.ino(),
        mount_id,
        owner: metadata.uid(),
        file_type: metadata.mode() & nix::libc::S_IFMT,
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
    budget: &mut LocalBootPolicyBudget,
) -> Result<(), ActiveReblitLocalBootPolicyError> {
    if directory_witness(directory, path, budget)? == expected {
        Ok(())
    } else {
        Err(ActiveReblitLocalBootPolicyError::Changed {
            path: path.to_owned(),
            reason: "retained local-policy directory metadata changed",
        })
    }
}

fn require_same_directory(
    expected: &std::fs::File,
    actual: &std::fs::File,
    path: &Path,
    budget: &mut LocalBootPolicyBudget,
) -> Result<(), ActiveReblitLocalBootPolicyError> {
    let expected = directory_witness(expected, path, budget)?;
    let actual = directory_witness(actual, path, budget)?;
    if expected == actual {
        Ok(())
    } else {
        Err(ActiveReblitLocalBootPolicyError::Changed {
            path: path.to_owned(),
            reason: "reopened local-policy directory is not the retained inode",
        })
    }
}

fn require_same_entry(
    actual: &std::fs::File,
    expected: EntryWitness,
    path: &Path,
    budget: &mut LocalBootPolicyBudget,
) -> Result<(), ActiveReblitLocalBootPolicyError> {
    if entry_witness(actual, path, budget)? == expected {
        Ok(())
    } else {
        Err(ActiveReblitLocalBootPolicyError::Changed {
            path: path.to_owned(),
            reason: "local command-line inode metadata changed",
        })
    }
}

fn require_same_inode_metadata(
    expected: &std::fs::File,
    actual: &std::fs::File,
    path: &Path,
    budget: &mut LocalBootPolicyBudget,
) -> Result<(), ActiveReblitLocalBootPolicyError> {
    budget.step(path)?;
    let expected = expected
        .metadata()
        .map_err(|source| io_error("inspect local-policy path descriptor", path, source))?;
    let actual = actual
        .metadata()
        .map_err(|source| io_error("inspect readable local-policy descriptor", path, source))?;
    if (expected.dev(), expected.ino(), expected.mode()) == (actual.dev(), actual.ino(), actual.mode()) {
        Ok(())
    } else {
        Err(ActiveReblitLocalBootPolicyError::Changed {
            path: path.to_owned(),
            reason: "readable local-policy descriptor is not the probed inode",
        })
    }
}

fn component_path(installation: &Installation, relative: &[u8], component: &CStr) -> PathBuf {
    if relative.is_empty() {
        installation.root.join(OsStr::from_bytes(component.to_bytes()))
    } else {
        installation
            .root
            .join(OsStr::from_bytes(relative))
            .join(OsStr::from_bytes(component.to_bytes()))
    }
}

fn local_policy_resolution() -> u64 {
    (nix::libc::RESOLVE_BENEATH | nix::libc::RESOLVE_NO_MAGICLINKS | nix::libc::RESOLVE_NO_SYMLINKS) as u64
}

pub(super) fn retry_raw_syscall(
    path: &Path,
    operation: &'static str,
    budget: &mut LocalBootPolicyBudget,
    mut syscall: impl FnMut() -> nix::libc::c_long,
) -> Result<nix::libc::c_long, ActiveReblitLocalBootPolicyError> {
    let mut interruptions = 0usize;
    loop {
        budget.step(path)?;
        let result = syscall();
        if result != -1 {
            return Ok(result);
        }
        let source = io::Error::last_os_error();
        if source.kind() != io::ErrorKind::Interrupted {
            return Err(io_error(operation, path, source));
        }
        interruptions = interruptions.saturating_add(1);
        if interruptions > MAX_RAW_SYSCALL_INTERRUPTS {
            return Err(io_error(
                operation,
                path,
                io::Error::new(
                    io::ErrorKind::Interrupted,
                    "local-policy syscall exceeded interrupted retry limit",
                ),
            ));
        }
    }
}

fn io_error(operation: &'static str, path: &Path, source: io::Error) -> ActiveReblitLocalBootPolicyError {
    ActiveReblitLocalBootPolicyError::Io {
        operation,
        path: path.to_owned(),
        source,
    }
}
