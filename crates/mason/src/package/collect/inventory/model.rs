use std::{
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use super::super::{
    CollectionLimits, Error,
    filesystem::{CollectionUsage, Deadline, changed, checked_add_limit, find_child, reserve},
    traversal::{DirectoryHandle, FileSnapshot, RootAnchor},
};

pub(in crate::package::collect) type DirectoryId = usize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::package::collect) enum WitnessPhase {
    Fresh,
    InitialSnapshot,
    AdmissionsOpen,
    Sealed,
    Poisoned,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::package::collect) enum WitnessEntryKind {
    Regular { hash: u128 },
    Symlink { target: String },
    Special,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::package::collect) struct EntryWitness {
    pub(in crate::package::collect) snapshot: FileSnapshot,
    pub(in crate::package::collect) kind: WitnessEntryKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::package::collect) enum WitnessChildKind {
    Directory(DirectoryId),
    Entry(EntryWitness),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::package::collect) struct WitnessChild {
    pub(in crate::package::collect) name: OsString,
    pub(in crate::package::collect) kind: WitnessChildKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::package::collect) struct DirectoryWitness {
    pub(in crate::package::collect) parent: Option<DirectoryId>,
    pub(in crate::package::collect) name: OsString,
    pub(in crate::package::collect) snapshot: FileSnapshot,
    pub(in crate::package::collect) children: Vec<WitnessChild>,
}

#[derive(Debug)]
pub(in crate::package::collect) struct WitnessState {
    pub(in crate::package::collect) phase: WitnessPhase,
    pub(in crate::package::collect) directories: Vec<DirectoryWitness>,
}

impl Default for WitnessState {
    fn default() -> Self {
        Self {
            phase: WitnessPhase::Fresh,
            directories: Vec::new(),
        }
    }
}

#[derive(Debug)]
pub(in crate::package::collect) struct WitnessGraph {
    pub(in crate::package::collect) anchor: Arc<RootAnchor>,
    pub(in crate::package::collect) limits: CollectionLimits,
    pub(in crate::package::collect) usage: Arc<Mutex<CollectionUsage>>,
    pub(in crate::package::collect) deadline: Arc<Deadline>,
    pub(in crate::package::collect) state: Mutex<WitnessState>,
}

#[derive(Debug, Clone)]
pub(crate) struct SealedTree {
    pub(in crate::package::collect) witness: Arc<WitnessGraph>,
}

impl SealedTree {
    pub(crate) fn verify(&self) -> Result<(), Error> {
        self.witness.verify_sealed()
    }
}

#[derive(Debug, Default)]
pub(in crate::package::collect) struct AdmissionDelta {
    pub(in crate::package::collect) entries: u64,
    pub(in crate::package::collect) name_bytes: u64,
    pub(in crate::package::collect) path_bytes: u64,
    pub(in crate::package::collect) symlink_target_bytes: u64,
    pub(in crate::package::collect) regular_bytes: u64,
}

#[derive(Debug)]
pub(in crate::package::collect) struct DirectoryAdmission {
    pub(in crate::package::collect) id: DirectoryId,
    pub(in crate::package::collect) snapshot: FileSnapshot,
    pub(in crate::package::collect) additions: Vec<WitnessChild>,
}

#[derive(Debug)]
pub(in crate::package::collect) struct AdmissionDraft {
    pub(in crate::package::collect) existing: Vec<DirectoryAdmission>,
    pub(in crate::package::collect) new_directories: Vec<DirectoryWitness>,
    pub(in crate::package::collect) delta: AdmissionDelta,
    pub(in crate::package::collect) scan_usage: CollectionUsage,
}

#[derive(Debug)]
pub(in crate::package::collect) struct AdmissionTask {
    pub(in crate::package::collect) directory: Arc<DirectoryHandle>,
    pub(in crate::package::collect) declaration: usize,
    pub(in crate::package::collect) existing: Option<DirectoryId>,
}

#[derive(Debug)]
pub(in crate::package::collect) struct InventoryDraft {
    pub(in crate::package::collect) directories: Vec<DirectoryWitness>,
    pub(in crate::package::collect) usage: CollectionUsage,
}

#[derive(Debug)]
pub(in crate::package::collect) struct DeclaredNode {
    pub(in crate::package::collect) terminal: bool,
    pub(in crate::package::collect) children: Vec<(OsString, usize)>,
    inventory: DeclaredInventory,
}

#[derive(Debug, Clone, Copy)]
enum DeclaredInventory {
    ExistingDirectory(DirectoryId),
    ExistingEntry,
    Missing,
}

#[derive(Debug)]
pub(in crate::package::collect) struct DeclaredTrie {
    pub(in crate::package::collect) nodes: Vec<DeclaredNode>,
}

impl DeclaredTrie {
    pub(in crate::package::collect) fn new(
        paths: &[PathBuf],
        directories: &[DirectoryWitness],
        remaining_entries: u64,
        max_edges: u64,
        deadline: &Deadline,
        root: &Path,
    ) -> Result<Self, Error> {
        let mut nodes = Vec::new();
        reserve(&mut nodes, 1, "generated path declaration trie")?;
        nodes.push(DeclaredNode {
            terminal: false,
            children: Vec::new(),
            inventory: DeclaredInventory::ExistingDirectory(0),
        });
        let mut edges = 0u64;
        let mut projected_entries = max_edges
            .checked_sub(remaining_entries)
            .ok_or(Error::ArithmeticOverflow {
                resource: "remaining generated package entries",
                path: root.to_owned(),
            })?;
        for path in paths {
            deadline.check(&root.join(path))?;
            let mut node = 0;
            for component in path.components() {
                deadline.check(&root.join(path))?;
                let std::path::Component::Normal(name) = component else {
                    return Err(Error::InvalidPath {
                        path: path.clone(),
                        detail: "generated path declaration is not normalized",
                    });
                };
                let position = nodes[node]
                    .children
                    .binary_search_by(|(candidate, _)| candidate.as_os_str().cmp(name));
                node = match position {
                    Ok(position) => nodes[node].children[position].1,
                    Err(position) => {
                        let inventory = match nodes[node].inventory {
                            DeclaredInventory::ExistingDirectory(directory) => {
                                match find_child(directories, directory, name) {
                                    Some(WitnessChild {
                                        kind: WitnessChildKind::Directory(directory),
                                        ..
                                    }) => DeclaredInventory::ExistingDirectory(*directory),
                                    Some(WitnessChild {
                                        kind: WitnessChildKind::Entry(_),
                                        ..
                                    }) => DeclaredInventory::ExistingEntry,
                                    None => {
                                        projected_entries = checked_add_limit(
                                            "total entries",
                                            projected_entries,
                                            1,
                                            max_edges,
                                            &root.join(path),
                                        )?;
                                        DeclaredInventory::Missing
                                    }
                                }
                            }
                            DeclaredInventory::Missing => {
                                projected_entries = checked_add_limit(
                                    "total entries",
                                    projected_entries,
                                    1,
                                    max_edges,
                                    &root.join(path),
                                )?;
                                DeclaredInventory::Missing
                            }
                            DeclaredInventory::ExistingEntry => {
                                return Err(changed(
                                    &root.join(path),
                                    "generated path traverses a witnessed non-directory",
                                ));
                            }
                        };
                        edges = checked_add_limit(
                            "generated path declaration trie edges",
                            edges,
                            1,
                            max_edges,
                            &root.join(path),
                        )?;
                        let child = nodes.len();
                        reserve(&mut nodes, 1, "generated path declaration trie")?;
                        nodes.push(DeclaredNode {
                            terminal: false,
                            children: Vec::new(),
                            inventory,
                        });
                        nodes[node]
                            .children
                            .try_reserve(1)
                            .map_err(|source| Error::Allocation {
                                resource: "generated path declaration edges",
                                requested: 1,
                                detail: source.to_string(),
                            })?;
                        nodes[node].children.insert(position, (name.to_owned(), child));
                        child
                    }
                };
            }
            nodes[node].terminal = true;
        }
        deadline.check(root)?;
        Ok(Self { nodes })
    }

    pub(in crate::package::collect) fn child(&self, node: usize, name: &OsStr) -> Option<usize> {
        self.nodes[node]
            .children
            .binary_search_by(|(candidate, _)| candidate.as_os_str().cmp(name))
            .ok()
            .map(|position| self.nodes[node].children[position].1)
    }
}
