use std::{
    collections::HashSet,
    ffi::{CStr, OsString},
    fs::{File, Metadata},
    io::{self, Read as _},
    os::{
        fd::AsRawFd as _,
        unix::{
            ffi::{OsStrExt as _, OsStringExt as _},
            fs::MetadataExt as _,
        },
    },
    path::{Path, PathBuf},
};

use sha2::{Digest as _, Sha256};

use crate::tree_marker::TREE_MARKER_FRAME_LENGTH;

use super::{
    CandidateInventoryBoundary, CandidateInventoryError, CandidateInventoryLimits, WorkBudget,
    error::inventory_io,
    filesystem::{
        clone_bytes, clone_nul_terminated_bytes, directory_names, directory_read_flags, open_raw_fd_relative,
        open_relative, regular_read_flags, require_directory_acls, require_effective_owner,
        require_no_extended_attributes, require_regular_acl, reserve, reserve_set,
    },
};

pub(super) const MARKER_NAME: &CStr = c".cast-tree-id";
const MARKER_MODE: u32 = 0o444;
const READ_BUFFER_BYTES: usize = 64 * 1024;
const MAX_SYMLINK_TARGET_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum MarkerPolicy {
    Classify,
    MustBePresent,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum NodeKind {
    Regular,
    Directory,
    Symlink,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct MetadataWitness {
    pub(super) device: u64,
    pub(super) inode: u64,
    pub(super) kind: u32,
    pub(super) mode: u32,
    pub(super) owner: u32,
    pub(super) group: u32,
    pub(super) links: u64,
    pub(super) size: u64,
    pub(super) modified_seconds: i64,
    pub(super) modified_nanoseconds: i64,
    pub(super) changed_seconds: i64,
    pub(super) changed_nanoseconds: i64,
}

impl MetadataWitness {
    fn from_metadata(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            kind: metadata.mode() & nix::libc::S_IFMT,
            mode: metadata.mode() & 0o7777,
            owner: metadata.uid(),
            group: metadata.gid(),
            links: metadata.nlink(),
            size: metadata.size(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        }
    }

    pub(super) fn node_kind(self, path: &Path) -> Result<NodeKind, CandidateInventoryError> {
        match self.kind {
            nix::libc::S_IFREG => Ok(NodeKind::Regular),
            nix::libc::S_IFDIR => Ok(NodeKind::Directory),
            nix::libc::S_IFLNK => Ok(NodeKind::Symlink),
            kind => Err(CandidateInventoryError::SpecialInode {
                path: path.to_owned(),
                kind,
            }),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct MarkerSnapshot {
    pub(super) metadata: MetadataWitness,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Node {
    pub(super) parent: Option<usize>,
    pub(super) name: Vec<u8>,
    pub(super) depth: usize,
    pub(super) kind: NodeKind,
    pub(super) metadata: MetadataWitness,
    pub(super) children: Vec<Vec<u8>>,
    pub(super) symlink_target: Option<Vec<u8>>,
    pub(super) content_digest: Option<[u8; 32]>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Inventory {
    nodes: Vec<Node>,
    marker: Option<MarkerSnapshot>,
}

impl Inventory {
    pub(super) fn require_exact(
        &self,
        actual: &Self,
        root: &Path,
        budget: &mut WorkBudget,
    ) -> Result<(), CandidateInventoryError> {
        budget.operation(root)?;
        if self.nodes.len() != actual.nodes.len() {
            return Err(CandidateInventoryError::ChildNamesChanged { path: root.to_owned() });
        }
        for (index, (expected, actual)) in self.nodes.iter().zip(&actual.nodes).enumerate() {
            let path = self.node_path(root, index, budget)?;
            budget.operation(&path)?;
            require_locator(expected, actual, &path)?;
            require_node_payload(expected, actual, &path)?;
            require_metadata(expected.metadata, actual.metadata, &path, false)?;
        }
        budget.operation(root)?;
        match (&self.marker, &actual.marker) {
            (None, None) => Ok(()),
            (Some(expected), Some(actual)) => {
                let path = root.join(OsString::from_vec(MARKER_NAME.to_bytes().to_vec()));
                require_metadata(expected.metadata, actual.metadata, &path, false)
                    .map_err(|_| CandidateInventoryError::MarkerChanged { path })
            }
            _ => Err(CandidateInventoryError::MarkerChanged {
                path: root.join(OsString::from_vec(MARKER_NAME.to_bytes().to_vec())),
            }),
        }
    }

    pub(super) fn require_marker_delta(
        &self,
        actual: &Self,
        root: &Path,
        budget: &mut WorkBudget,
    ) -> Result<(), CandidateInventoryError> {
        budget.operation(root)?;
        if self.nodes.len() != actual.nodes.len() {
            return Err(CandidateInventoryError::ChildNamesChanged { path: root.to_owned() });
        }
        for (index, (expected, actual)) in self.nodes.iter().zip(&actual.nodes).enumerate() {
            let path = self.node_path(root, index, budget)?;
            budget.operation(&path)?;
            require_locator(expected, actual, &path)?;
            require_node_payload(expected, actual, &path)?;
            let allow_root_publication_delta = index == 0 && self.marker.is_none();
            require_metadata(expected.metadata, actual.metadata, &path, allow_root_publication_delta)?;
        }

        budget.operation(root)?;
        let marker_path = root.join(OsString::from_vec(MARKER_NAME.to_bytes().to_vec()));
        match (&self.marker, &actual.marker) {
            (None, Some(marker)) if marker.metadata.links == 1 => Ok(()),
            (None, Some(_)) => Err(CandidateInventoryError::MarkerChanged { path: marker_path }),
            (None, None) => Err(CandidateInventoryError::MarkerMissingAfterPublication { path: marker_path }),
            (Some(expected), Some(actual)) => require_metadata(expected.metadata, actual.metadata, &marker_path, false)
                .map_err(|_| CandidateInventoryError::MarkerChanged { path: marker_path }),
            (Some(_), None) => Err(CandidateInventoryError::MarkerMissingAfterPublication { path: marker_path }),
        }
    }

    pub(super) fn node(&self, index: usize) -> &Node {
        &self.nodes[index]
    }

    pub(super) fn node_count(&self) -> usize {
        self.nodes.len()
    }

    pub(super) fn marker(&self) -> Option<&MarkerSnapshot> {
        self.marker.as_ref()
    }

    pub(super) fn node_path(
        &self,
        root: &Path,
        index: usize,
        budget: &mut WorkBudget,
    ) -> Result<PathBuf, CandidateInventoryError> {
        let components = self.component_indices(index, root, budget)?;
        diagnostic_path(
            root,
            components
                .iter()
                .map(|component| self.nodes[*component].name.as_slice()),
            budget,
        )
    }

    pub(super) fn component_indices(
        &self,
        mut index: usize,
        root: &Path,
        budget: &mut WorkBudget,
    ) -> Result<Vec<usize>, CandidateInventoryError> {
        let mut indices = Vec::new();
        while let Some(parent) = self.nodes[index].parent {
            budget.operation(root)?;
            indices
                .try_reserve(1)
                .map_err(|_| CandidateInventoryError::Allocation {
                    resource: "inventory path components",
                    path: root.to_owned(),
                })?;
            indices.push(index);
            index = parent;
        }
        indices.reverse();
        budget.check(root)?;
        Ok(indices)
    }

    #[cfg(test)]
    pub(super) fn entry_count(&self) -> usize {
        self.nodes.len().saturating_sub(1)
    }
}

#[derive(Debug)]
pub(super) struct NamespaceCounter {
    limits: CandidateInventoryLimits,
    entries: usize,
    name_bytes: usize,
    regular_bytes: u64,
}

impl NamespaceCounter {
    pub(super) fn new(limits: CandidateInventoryLimits) -> Self {
        Self {
            limits,
            entries: 0,
            name_bytes: 0,
            regular_bytes: 0,
        }
    }

    pub(super) fn entry(&mut self, depth: usize, name: &[u8], path: &Path) -> Result<(), CandidateInventoryError> {
        if depth > self.limits.depth {
            return self.boundary(
                CandidateInventoryBoundary::Depth,
                u64::try_from(self.limits.depth).unwrap_or(u64::MAX),
                path,
            );
        }
        self.entries = self.entries.saturating_add(1);
        if self.entries > self.limits.entries {
            return self.boundary(
                CandidateInventoryBoundary::EntryCount,
                u64::try_from(self.limits.entries).unwrap_or(u64::MAX),
                path,
            );
        }
        self.add_name_bytes(name.len(), path)
    }

    pub(super) fn symlink_target(&mut self, bytes: usize, path: &Path) -> Result<(), CandidateInventoryError> {
        self.add_name_bytes(bytes, path)
    }

    pub(super) fn regular_file(&mut self, bytes: u64, path: &Path) -> Result<(), CandidateInventoryError> {
        self.regular_bytes = self.regular_bytes.saturating_add(bytes);
        if self.regular_bytes > self.limits.regular_bytes {
            self.boundary(
                CandidateInventoryBoundary::RegularBytes,
                self.limits.regular_bytes,
                path,
            )
        } else {
            Ok(())
        }
    }

    fn add_name_bytes(&mut self, bytes: usize, path: &Path) -> Result<(), CandidateInventoryError> {
        self.name_bytes = self.name_bytes.saturating_add(bytes);
        if self.name_bytes > self.limits.name_bytes {
            self.boundary(
                CandidateInventoryBoundary::NameBytes,
                u64::try_from(self.limits.name_bytes).unwrap_or(u64::MAX),
                path,
            )
        } else {
            Ok(())
        }
    }

    fn boundary<T>(
        &self,
        boundary: CandidateInventoryBoundary,
        limit: u64,
        path: &Path,
    ) -> Result<T, CandidateInventoryError> {
        Err(CandidateInventoryError::Boundary {
            boundary,
            limit,
            path: path.to_owned(),
        })
    }
}

#[derive(Debug)]
struct DirectoryFrame {
    directory: File,
    node: usize,
    depth: usize,
    next: usize,
}

pub(super) fn collect_inventory(
    root: &File,
    display_path: &Path,
    limits: CandidateInventoryLimits,
    marker_policy: MarkerPolicy,
    budget: &mut WorkBudget,
) -> Result<Inventory, CandidateInventoryError> {
    let root_capability = open_relative(
        root,
        c".",
        nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        display_path,
        "pin retained candidate root",
        budget,
    )?;
    let root_metadata = witness(&root_capability, display_path, budget)?;
    if root_metadata.kind != nix::libc::S_IFDIR {
        return Err(CandidateInventoryError::RootNotDirectory {
            path: display_path.to_owned(),
        });
    }
    require_effective_owner(root_metadata, display_path)?;
    require_safe_mode(root_metadata, NodeKind::Directory, display_path)?;
    let root_readable = open_relative(
        root,
        c".",
        directory_read_flags(),
        display_path,
        "open retained candidate root",
        budget,
    )?;
    require_metadata(
        root_metadata,
        witness(&root_readable, display_path, budget)?,
        display_path,
        false,
    )?;
    require_directory_acls(&root_readable, display_path, budget)?;

    let mut counter = NamespaceCounter::new(limits);
    let mut root_names = directory_names(&root_readable, display_path, 1, true, &mut counter, budget)?;
    require_metadata(
        root_metadata,
        witness(&root_readable, display_path, budget)?,
        display_path,
        false,
    )?;
    let marker_position = root_names
        .binary_search_by(|name| name.as_slice().cmp(MARKER_NAME.to_bytes()))
        .ok();
    let marker = if let Some(position) = marker_position {
        root_names.remove(position);
        Some(inspect_marker(&root_readable, root_metadata, display_path, budget)?)
    } else {
        None
    };
    if marker_policy == MarkerPolicy::MustBePresent && marker.is_none() {
        return Err(CandidateInventoryError::MarkerMissingAfterPublication {
            path: display_path.join(OsString::from_vec(MARKER_NAME.to_bytes().to_vec())),
        });
    }

    let mut nodes = Vec::new();
    reserve(&mut nodes, 1, "inventory node", display_path)?;
    nodes.push(Node {
        parent: None,
        name: Vec::new(),
        depth: 0,
        kind: NodeKind::Directory,
        metadata: root_metadata,
        children: root_names,
        symlink_target: None,
        content_digest: None,
    });
    let mut seen = HashSet::new();
    reserve_set(&mut seen, 1, display_path)?;
    seen.insert((root_metadata.device, root_metadata.inode));
    let mut frames = Vec::new();
    reserve(&mut frames, 1, "directory frame", display_path)?;
    frames.push(DirectoryFrame {
        directory: root_readable,
        node: 0,
        depth: 0,
        next: 0,
    });

    while !frames.is_empty() {
        let (parent_fd, parent, depth, name) = {
            let frame = frames.last_mut().expect("nonempty frame stack");
            if frame.next == nodes[frame.node].children.len() {
                frames.pop();
                continue;
            }
            let name = clone_bytes(
                &nodes[frame.node].children[frame.next],
                "directory entry name",
                display_path,
            )?;
            frame.next += 1;
            (frame.directory.as_raw_fd(), frame.node, frame.depth + 1, name)
        };
        let path = child_path(display_path, &nodes, parent, &name, budget)?;
        let encoded = clone_nul_terminated_bytes(&name, "encoded directory entry name", &path)?;
        // SAFETY: the helper appended one NUL to a name returned by readdir,
        // which cannot contain an embedded NUL.
        let encoded = unsafe { CStr::from_bytes_with_nul_unchecked(&encoded) };
        let capability = open_raw_fd_relative(
            parent_fd,
            encoded,
            nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
            &path,
            "pin candidate entry",
            budget,
        )?;
        let metadata = witness(&capability, &path, budget)?;
        let kind = metadata.node_kind(&path)?;
        require_effective_owner(metadata, &path)?;
        require_safe_mode(metadata, kind, &path)?;
        if kind != NodeKind::Directory && metadata.links != 1 {
            return Err(CandidateInventoryError::UnexpectedHardlink {
                path,
                links: metadata.links,
            });
        }
        reserve_set(&mut seen, 1, &path)?;
        if !seen.insert((metadata.device, metadata.inode)) {
            return Err(CandidateInventoryError::DuplicateInode {
                path,
                device: metadata.device,
                inode: metadata.inode,
            });
        }

        let (children, symlink_target, content_digest, readable_directory) = match kind {
            NodeKind::Regular => {
                counter.regular_file(metadata.size, &path)?;
                let mut readable = open_raw_fd_relative(
                    parent_fd,
                    &encoded,
                    regular_read_flags(),
                    &path,
                    "open candidate regular file",
                    budget,
                )?;
                require_metadata(metadata, witness(&readable, &path, budget)?, &path, false)?;
                require_regular_acl(&readable, &path, budget)?;
                let digest = digest_file(&mut readable, metadata, &path, budget)?;
                require_metadata(metadata, witness(&capability, &path, budget)?, &path, false)?;
                (Vec::new(), None, Some(digest), None)
            }
            NodeKind::Directory => {
                let readable = open_raw_fd_relative(
                    parent_fd,
                    &encoded,
                    directory_read_flags(),
                    &path,
                    "open candidate directory",
                    budget,
                )?;
                require_metadata(metadata, witness(&readable, &path, budget)?, &path, false)?;
                require_directory_acls(&readable, &path, budget)?;
                let names = directory_names(&readable, &path, depth.saturating_add(1), false, &mut counter, budget)?;
                require_metadata(metadata, witness(&readable, &path, budget)?, &path, false)?;
                (names, None, None, Some(readable))
            }
            NodeKind::Symlink => {
                let target = read_symlink_target(&capability, &path, limits.name_bytes, budget)?;
                counter.symlink_target(target.len(), &path)?;
                require_metadata(metadata, witness(&capability, &path, budget)?, &path, false)?;
                (Vec::new(), Some(target), None, None)
            }
        };
        let node = nodes.len();
        reserve(&mut nodes, 1, "inventory node", &path)?;
        nodes.push(Node {
            parent: Some(parent),
            name,
            depth,
            kind,
            metadata,
            children,
            symlink_target,
            content_digest,
        });
        if let Some(directory) = readable_directory {
            reserve(&mut frames, 1, "directory frame", &path)?;
            frames.push(DirectoryFrame {
                directory,
                node,
                depth,
                next: 0,
            });
        }
    }

    Ok(Inventory { nodes, marker })
}

pub(super) fn open_inventory_node(
    root: &File,
    inventory: &Inventory,
    index: usize,
    flags: i32,
    display_path: &Path,
    operation: &'static str,
    budget: &mut WorkBudget,
) -> Result<File, CandidateInventoryError> {
    if index == 0 {
        return open_relative(root, c".", flags, display_path, operation, budget);
    }
    let mut parent = open_relative(
        root,
        c".",
        nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        display_path,
        "reopen candidate inventory root",
        budget,
    )?;
    require_metadata(
        inventory.node(0).metadata,
        witness(&parent, display_path, budget)?,
        display_path,
        false,
    )?;
    let components = inventory.component_indices(index, display_path, budget)?;
    for (position, component) in components.iter().enumerate() {
        let node = inventory.node(*component);
        let path = inventory.node_path(display_path, *component, budget)?;
        let encoded = clone_nul_terminated_bytes(&node.name, "encoded inventory path component", &path)?;
        // SAFETY: the helper appended one NUL to a component returned by
        // readdir, which cannot contain an embedded NUL.
        let name = unsafe { CStr::from_bytes_with_nul_unchecked(&encoded) };
        let last = position + 1 == components.len();
        let opened = open_relative(
            &parent,
            name,
            if last {
                flags
            } else {
                nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW
            },
            &path,
            operation,
            budget,
        )?;
        require_metadata(node.metadata, witness(&opened, &path, budget)?, &path, false)?;
        parent = opened;
    }
    Ok(parent)
}

pub(super) fn open_marker(
    root: &File,
    flags: i32,
    display_path: &Path,
    operation: &'static str,
    budget: &mut WorkBudget,
) -> Result<File, CandidateInventoryError> {
    let path = display_path.join(OsString::from_vec(MARKER_NAME.to_bytes().to_vec()));
    open_relative(root, MARKER_NAME, flags, &path, operation, budget)
}

pub(super) fn witness(
    file: &File,
    path: &Path,
    budget: &mut WorkBudget,
) -> Result<MetadataWitness, CandidateInventoryError> {
    loop {
        budget.operation(path)?;
        match file.metadata() {
            Ok(metadata) => {
                budget.check(path)?;
                return Ok(MetadataWitness::from_metadata(&metadata));
            }
            Err(source) if source.kind() == io::ErrorKind::Interrupted => continue,
            Err(source) => return Err(inventory_io("inspect retained", path, source)),
        }
    }
}

pub(super) fn require_metadata(
    expected: MetadataWitness,
    actual: MetadataWitness,
    path: &Path,
    allow_directory_publication_delta: bool,
) -> Result<(), CandidateInventoryError> {
    macro_rules! require_field {
        ($field:ident) => {
            if expected.$field != actual.$field {
                return Err(CandidateInventoryError::EntryChanged {
                    path: path.to_owned(),
                    field: stringify!($field),
                });
            }
        };
    }
    require_field!(device);
    require_field!(inode);
    require_field!(kind);
    require_field!(mode);
    require_field!(owner);
    require_field!(group);
    require_field!(links);
    if !allow_directory_publication_delta {
        require_field!(size);
        require_field!(modified_seconds);
        require_field!(modified_nanoseconds);
        require_field!(changed_seconds);
        require_field!(changed_nanoseconds);
    }
    Ok(())
}

pub(super) fn digest_file(
    file: &mut File,
    expected: MetadataWitness,
    path: &Path,
    budget: &mut WorkBudget,
) -> Result<[u8; 32], CandidateInventoryError> {
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; READ_BUFFER_BYTES];
    let mut bytes = 0_u64;
    loop {
        budget.operation(path)?;
        let read = match file.read(&mut buffer) {
            Ok(read) => read,
            Err(source) if source.kind() == io::ErrorKind::Interrupted => continue,
            Err(source) => return Err(inventory_io("read regular-file content", path, source)),
        };
        budget.check(path)?;
        if read == 0 {
            break;
        }
        bytes = bytes.saturating_add(read as u64);
        if bytes > expected.size {
            return Err(CandidateInventoryError::EntryChanged {
                path: path.to_owned(),
                field: "content length",
            });
        }
        digest.update(&buffer[..read]);
    }
    if bytes != expected.size {
        return Err(CandidateInventoryError::EntryChanged {
            path: path.to_owned(),
            field: "content length",
        });
    }
    require_metadata(expected, witness(&file, path, budget)?, path, false)?;
    Ok(digest.finalize().into())
}

pub(super) fn read_symlink_target(
    file: &File,
    path: &Path,
    name_byte_limit: usize,
    budget: &mut WorkBudget,
) -> Result<Vec<u8>, CandidateInventoryError> {
    let maximum = name_byte_limit.saturating_add(1).min(MAX_SYMLINK_TARGET_BYTES).max(1);
    let mut capacity = 256.min(maximum);
    loop {
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(capacity)
            .map_err(|_| CandidateInventoryError::Allocation {
                resource: "raw symlink target buffer",
                path: path.to_owned(),
            })?;
        bytes.resize(capacity, 0);
        budget.operation(path)?;
        // SAFETY: the retained O_PATH descriptor names one symlink, the empty
        // C string is live, and the output slice is valid for `capacity` bytes.
        let read =
            unsafe { nix::libc::readlinkat(file.as_raw_fd(), c"".as_ptr(), bytes.as_mut_ptr().cast(), bytes.len()) };
        if read == -1 {
            let source = io::Error::last_os_error();
            if source.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(inventory_io("read raw symlink target", path, source));
        }
        budget.check(path)?;
        let read = usize::try_from(read).expect("nonnegative readlink length fits usize");
        if read < capacity {
            bytes.truncate(read);
            return Ok(bytes);
        }
        if capacity == maximum {
            return Err(CandidateInventoryError::Boundary {
                boundary: CandidateInventoryBoundary::NameBytes,
                limit: u64::try_from(name_byte_limit).unwrap_or(u64::MAX),
                path: path.to_owned(),
            });
        }
        capacity = capacity.saturating_mul(2).min(maximum);
    }
}

fn inspect_marker(
    root: &File,
    root_metadata: MetadataWitness,
    display_path: &Path,
    budget: &mut WorkBudget,
) -> Result<MarkerSnapshot, CandidateInventoryError> {
    let path = display_path.join(OsString::from_vec(MARKER_NAME.to_bytes().to_vec()));
    let marker = open_relative(
        root,
        MARKER_NAME,
        nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        &path,
        "pin canonical tree marker delta",
        budget,
    )?;
    let metadata = witness(&marker, &path, budget)?;
    // SAFETY: geteuid has no arguments and cannot fail.
    let effective_owner = unsafe { nix::libc::geteuid() };
    if metadata.kind != nix::libc::S_IFREG
        || metadata.device != root_metadata.device
        || metadata.owner != effective_owner
        || metadata.mode != MARKER_MODE
        || !matches!(metadata.links, 1 | 2)
        || metadata.size != u64::try_from(TREE_MARKER_FRAME_LENGTH).expect("marker frame length fits u64")
    {
        return Err(CandidateInventoryError::UnsafeMarker {
            path,
            kind: metadata.kind,
            owner: metadata.owner,
            mode: metadata.mode,
            links: metadata.links,
            length: metadata.size,
        });
    }
    let readable = open_relative(
        root,
        MARKER_NAME,
        regular_read_flags(),
        &path,
        "open canonical tree marker for xattr audit",
        budget,
    )?;
    require_metadata(metadata, witness(&readable, &path, budget)?, &path, false)?;
    require_no_extended_attributes(&readable, &path, budget)?;
    require_metadata(metadata, witness(&readable, &path, budget)?, &path, false)?;
    require_metadata(metadata, witness(&marker, &path, budget)?, &path, false)?;
    Ok(MarkerSnapshot { metadata })
}

fn require_locator(expected: &Node, actual: &Node, path: &Path) -> Result<(), CandidateInventoryError> {
    if expected.parent != actual.parent || expected.name != actual.name || expected.depth != actual.depth {
        Err(CandidateInventoryError::ChildNamesChanged { path: path.to_owned() })
    } else {
        Ok(())
    }
}

fn require_node_payload(expected: &Node, actual: &Node, path: &Path) -> Result<(), CandidateInventoryError> {
    if expected.kind != actual.kind {
        return Err(CandidateInventoryError::EntryChanged {
            path: path.to_owned(),
            field: "kind",
        });
    }
    if expected.children != actual.children {
        return Err(CandidateInventoryError::ChildNamesChanged { path: path.to_owned() });
    }
    if expected.symlink_target != actual.symlink_target {
        return Err(CandidateInventoryError::SymlinkTargetChanged { path: path.to_owned() });
    }
    if expected.content_digest != actual.content_digest {
        return Err(CandidateInventoryError::EntryChanged {
            path: path.to_owned(),
            field: "content digest",
        });
    }
    Ok(())
}

fn require_safe_mode(metadata: MetadataWitness, kind: NodeKind, path: &Path) -> Result<(), CandidateInventoryError> {
    if kind == NodeKind::Symlink || metadata.mode & (0o7000 | 0o022) == 0 {
        Ok(())
    } else {
        Err(CandidateInventoryError::UnsafeMode {
            path: path.to_owned(),
            mode: metadata.mode,
        })
    }
}

fn child_path(
    root: &Path,
    nodes: &[Node],
    parent: usize,
    name: &[u8],
    budget: &mut WorkBudget,
) -> Result<PathBuf, CandidateInventoryError> {
    let mut indices = Vec::new();
    let mut index = parent;
    while let Some(next) = nodes[index].parent {
        budget.operation(root)?;
        indices
            .try_reserve(1)
            .map_err(|_| CandidateInventoryError::Allocation {
                resource: "candidate child path components",
                path: root.to_owned(),
            })?;
        indices.push(index);
        index = next;
    }
    indices.reverse();
    diagnostic_path(
        root,
        indices
            .iter()
            .map(|index| nodes[*index].name.as_slice())
            .chain(std::iter::once(name)),
        budget,
    )
}

fn diagnostic_path<'a>(
    root: &Path,
    components: impl IntoIterator<Item = &'a [u8]>,
    budget: &mut WorkBudget,
) -> Result<PathBuf, CandidateInventoryError> {
    let mut encoded = clone_bytes(root.as_os_str().as_bytes(), "candidate diagnostic root path", root)?;
    for component in components {
        budget.operation(root)?;
        let separator = usize::from(!encoded.is_empty() && encoded.last() != Some(&b'/'));
        encoded
            .try_reserve(separator.saturating_add(component.len()))
            .map_err(|_| CandidateInventoryError::Allocation {
                resource: "candidate diagnostic path",
                path: root.to_owned(),
            })?;
        if separator != 0 {
            encoded.push(b'/');
        }
        encoded.extend_from_slice(component);
    }
    budget.check(root)?;
    Ok(PathBuf::from(OsString::from_vec(encoded)))
}
