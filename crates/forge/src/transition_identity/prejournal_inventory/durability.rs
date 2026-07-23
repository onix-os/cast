use std::{ffi::OsString, fs::File, os::unix::ffi::OsStringExt as _, path::Path};

use crate::linux_fs::sync_filesystem_until;

use super::{
    CandidateInventoryError, CandidateInventoryLimits, WorkBudget,
    error::inventory_io,
    filesystem::{
        directory_names, directory_read_flags, regular_read_flags, require_directory_acls,
        require_no_extended_attributes, require_regular_acl,
    },
    inventory::{
        Inventory, MetadataWitness, NamespaceCounter, NodeKind, digest_file, open_inventory_node, open_marker,
        read_symlink_target, require_metadata, witness,
    },
};

const MARKER_NAME: &[u8] = b".cast-tree-id";

pub(super) fn sync_baseline(
    root: &File,
    display_path: &Path,
    inventory: &Inventory,
    limits: CandidateInventoryLimits,
    budget: &mut WorkBudget,
) -> Result<(), CandidateInventoryError> {
    let mut counter = NamespaceCounter::new(limits);
    for index in (0..inventory.node_count()).rev() {
        budget.operation(display_path)?;
        let node = inventory.node(index);
        let path = inventory.node_path(display_path, index, budget)?;
        match node.kind {
            NodeKind::Regular => {
                counter.regular_file(node.metadata.size, &path)?;
                sync_regular(root, display_path, inventory, index, &path, budget)?;
            }
            NodeKind::Directory => {
                if index == 0 {
                    if let Some(marker) = inventory.marker() {
                        sync_exact_marker(root, display_path, marker.metadata, budget)?;
                    }
                }
                sync_directory(root, display_path, inventory, index, &path, &mut counter, budget)?;
            }
            NodeKind::Symlink => {
                let target = sync_symlink(root, display_path, inventory, index, &path, limits, budget)?;
                counter.symlink_target(target, &path)?;
            }
        }
    }
    budget.operation(display_path)?;
    sync_filesystem_until(root, budget.deadline())
        .map_err(|source| inventory_io("sync filesystem containing exact candidate", display_path, source))?;
    budget.check(display_path)?;
    let root_after_sync = open_inventory_node(
        root,
        inventory,
        0,
        nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        display_path,
        "revalidate candidate root after filesystem sync",
        budget,
    )?;
    require_metadata(
        inventory.node(0).metadata,
        witness(&root_after_sync, display_path, budget)?,
        display_path,
        false,
    )?;
    Ok(())
}

pub(super) fn sync_marker_delta(
    root: &File,
    display_path: &Path,
    inventory: &Inventory,
    limits: CandidateInventoryLimits,
    budget: &mut WorkBudget,
) -> Result<(), CandidateInventoryError> {
    let marker = inventory
        .marker()
        .ok_or_else(|| CandidateInventoryError::MarkerMissingAfterPublication {
            path: display_path.join(OsString::from_vec(MARKER_NAME.to_vec())),
        })?;
    sync_exact_marker(root, display_path, marker.metadata, budget)?;

    let path = inventory.node_path(display_path, 0, budget)?;
    let directory = open_inventory_node(
        root,
        inventory,
        0,
        directory_read_flags(),
        display_path,
        "reopen post-publication candidate root",
        budget,
    )?;
    require_directory_acls(&directory, &path, budget)?;
    let mut counter = NamespaceCounter::new(limits);
    let mut names = directory_names(&directory, &path, 1, true, &mut counter, budget)?;
    remove_required_marker(&mut names, &path)?;
    if names != inventory.node(0).children {
        return Err(CandidateInventoryError::ChildNamesChanged { path });
    }
    sync_file(&directory, display_path, "sync post-publication candidate root", budget)?;
    require_metadata(
        inventory.node(0).metadata,
        witness(&directory, display_path, budget)?,
        display_path,
        false,
    )
}

fn sync_regular(
    root: &File,
    display_path: &Path,
    inventory: &Inventory,
    index: usize,
    path: &Path,
    budget: &mut WorkBudget,
) -> Result<(), CandidateInventoryError> {
    let node = inventory.node(index);
    let capability = open_inventory_node(
        root,
        inventory,
        index,
        nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        display_path,
        "reopen exact regular-file capability",
        budget,
    )?;
    let mut readable = open_inventory_node(
        root,
        inventory,
        index,
        regular_read_flags(),
        display_path,
        "reopen exact regular file for durability",
        budget,
    )?;
    require_regular_acl(&readable, path, budget)?;
    let digest = digest_file(&mut readable, node.metadata, path, budget)?;
    if Some(digest) != node.content_digest {
        return Err(CandidateInventoryError::EntryChanged {
            path: path.to_owned(),
            field: "content digest",
        });
    }
    require_metadata(node.metadata, witness(&capability, path, budget)?, path, false)?;
    sync_file(&readable, path, "sync exact candidate regular file", budget)?;
    require_metadata(node.metadata, witness(&readable, path, budget)?, path, false)?;
    let reopened = open_inventory_node(
        root,
        inventory,
        index,
        nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        display_path,
        "revalidate synced regular-file name",
        budget,
    )?;
    require_metadata(node.metadata, witness(&reopened, path, budget)?, path, false)
}

fn sync_directory(
    root: &File,
    display_path: &Path,
    inventory: &Inventory,
    index: usize,
    path: &Path,
    counter: &mut NamespaceCounter,
    budget: &mut WorkBudget,
) -> Result<(), CandidateInventoryError> {
    let node = inventory.node(index);
    let directory = open_inventory_node(
        root,
        inventory,
        index,
        directory_read_flags(),
        display_path,
        "reopen exact candidate directory for durability",
        budget,
    )?;
    require_directory_acls(&directory, path, budget)?;
    let mut names = directory_names(
        &directory,
        path,
        node.depth.saturating_add(1),
        index == 0,
        counter,
        budget,
    )?;
    if index == 0 && inventory.marker().is_some() {
        remove_required_marker(&mut names, path)?;
    }
    if names != node.children {
        return Err(CandidateInventoryError::ChildNamesChanged { path: path.to_owned() });
    }
    sync_file(&directory, path, "sync exact candidate directory", budget)?;
    require_metadata(node.metadata, witness(&directory, path, budget)?, path, false)?;
    let reopened = open_inventory_node(
        root,
        inventory,
        index,
        nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        display_path,
        "revalidate synced candidate directory name",
        budget,
    )?;
    require_metadata(node.metadata, witness(&reopened, path, budget)?, path, false)
}

fn sync_symlink(
    root: &File,
    display_path: &Path,
    inventory: &Inventory,
    index: usize,
    path: &Path,
    limits: CandidateInventoryLimits,
    budget: &mut WorkBudget,
) -> Result<usize, CandidateInventoryError> {
    let node = inventory.node(index);
    let symlink = open_inventory_node(
        root,
        inventory,
        index,
        nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        display_path,
        "reopen exact candidate symlink",
        budget,
    )?;
    let target = read_symlink_target(&symlink, path, limits.name_bytes, budget)?;
    if Some(target.as_slice()) != node.symlink_target.as_deref() {
        return Err(CandidateInventoryError::SymlinkTargetChanged { path: path.to_owned() });
    }
    require_metadata(node.metadata, witness(&symlink, path, budget)?, path, false)?;
    Ok(target.len())
}

fn sync_exact_marker(
    root: &File,
    display_path: &Path,
    expected: MetadataWitness,
    budget: &mut WorkBudget,
) -> Result<(), CandidateInventoryError> {
    let marker_path = display_path.join(OsString::from_vec(MARKER_NAME.to_vec()));
    let capability = open_marker(
        root,
        nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        display_path,
        "reopen exact canonical marker capability",
        budget,
    )?;
    require_metadata(
        expected,
        witness(&capability, &marker_path, budget)?,
        &marker_path,
        false,
    )
    .map_err(|_| CandidateInventoryError::MarkerChanged {
        path: marker_path.clone(),
    })?;
    let marker = open_marker(
        root,
        regular_read_flags(),
        display_path,
        "reopen exact canonical marker for durability",
        budget,
    )?;
    require_metadata(expected, witness(&marker, &marker_path, budget)?, &marker_path, false).map_err(|_| {
        CandidateInventoryError::MarkerChanged {
            path: marker_path.clone(),
        }
    })?;
    require_no_extended_attributes(&marker, &marker_path, budget)?;
    require_metadata(expected, witness(&marker, &marker_path, budget)?, &marker_path, false).map_err(|_| {
        CandidateInventoryError::MarkerChanged {
            path: marker_path.clone(),
        }
    })?;
    sync_file(&marker, &marker_path, "sync exact canonical marker", budget)?;
    require_metadata(expected, witness(&marker, &marker_path, budget)?, &marker_path, false)
        .map_err(|_| CandidateInventoryError::MarkerChanged { path: marker_path })
}

fn remove_required_marker(names: &mut Vec<Vec<u8>>, path: &Path) -> Result<(), CandidateInventoryError> {
    let Ok(position) = names.binary_search_by(|name| name.as_slice().cmp(MARKER_NAME)) else {
        return Err(CandidateInventoryError::MarkerMissingAfterPublication {
            path: path.join(OsString::from_vec(MARKER_NAME.to_vec())),
        });
    };
    names.remove(position);
    Ok(())
}

fn sync_file(
    file: &File,
    path: &Path,
    operation: &'static str,
    budget: &mut WorkBudget,
) -> Result<(), CandidateInventoryError> {
    loop {
        budget.operation(path)?;
        match file.sync_all() {
            Ok(()) => return budget.check(path),
            Err(source) if source.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(source) => return Err(inventory_io(operation, path, source)),
        }
    }
}
