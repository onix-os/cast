//! Bounded identity inventory for non-root-ABI installation-root names.

use super::*;

pub(super) fn inspect_root_entries(
    root: &File,
    root_path: &Path,
    budget: &mut Budget,
) -> Result<Vec<RetainedRootEntry>, CaptureError> {
    let mut entries = Vec::new();
    for name in directory_names(root, root_path, MAX_NAMESPACE_ENTRIES, budget)? {
        if ROOT_ABI_LINKS.iter().any(|(canonical, _)| *canonical == name) {
            continue;
        }
        let path = root_path.join(os(&name));
        let file = open_optional_path(root, cstring(&name)?.as_c_str(), &path, budget)?
            .ok_or_else(|| CaptureError::InodeChanged { path: path.clone() })?;
        let witness = InodeWitness::read(&file, &path)?;
        entries.push(RetainedRootEntry {
            file,
            fingerprint: RootEntryFingerprint {
                name,
                device: witness.device,
                inode: witness.inode,
                kind: witness.kind(),
            },
        });
    }
    Ok(entries)
}

pub(super) fn revalidate_root_entries(
    root: &File,
    root_path: &Path,
    retained: &[RetainedRootEntry],
    budget: &mut Budget,
) -> Result<(), CaptureError> {
    let current = inspect_root_entries(root, root_path, budget)?;
    let expected_fingerprints = retained
        .iter()
        .map(|entry| &entry.fingerprint)
        .collect::<Vec<_>>();
    let current_fingerprints = current
        .iter()
        .map(|entry| &entry.fingerprint)
        .collect::<Vec<_>>();
    if expected_fingerprints != current_fingerprints {
        return Err(CaptureError::DirectoryContentsChanged {
            path: root_path.to_owned(),
        });
    }
    for entry in retained {
        let path = root_path.join(os(&entry.fingerprint.name));
        let witness = InodeWitness::read(&entry.file, &path)?;
        if witness.device != entry.fingerprint.device
            || witness.inode != entry.fingerprint.inode
            || witness.kind() != entry.fingerprint.kind
        {
            return Err(CaptureError::InodeChanged { path });
        }
    }
    Ok(())
}
