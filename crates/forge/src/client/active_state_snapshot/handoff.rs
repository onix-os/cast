//! Active-state proof handoff after intentional live tree-marker preparation.

use super::*;
use crate::client::Error;

impl ActiveStateLease {
    /// Refresh the retained live `/usr` generation after the caller has
    /// intentionally prepared its authenticated tree marker.
    ///
    /// The pre-marker proof must already be valid. This handoff admits only
    /// the exact metadata change caused by marker preparation: namespace and
    /// state-selection identity are proved before a fresh full witness is
    /// captured and immediately revalidated.
    pub(in crate::client) fn refresh_after_tree_identity_preparation(
        &mut self,
        installation: &Installation,
    ) -> Result<(), Error> {
        if installation.active_state != self.active {
            return Err(Error::ActiveStateSnapshotChanged {
                expected: installation.active_state,
                actual: self.active,
            });
        }

        self.proof = match &self.proof {
            ActiveStateProof::MissingUsr { root } => refresh_synthesized_missing_usr(installation, *root)?,
            ActiveStateProof::PresentBaseline {
                root,
                usr,
                usr_witness,
                marker,
            } => refresh_existing_usr_after_marker(installation, *root, usr, *usr_witness, marker.as_ref(), None)?,
            ActiveStateProof::Selected {
                root,
                usr,
                usr_witness,
                state_id,
                state_witness,
                bytes,
            } => refresh_existing_usr_after_marker(
                installation,
                *root,
                usr,
                *usr_witness,
                None,
                Some((state_id, *state_witness, bytes)),
            )?,
        };

        self.revalidate(installation)
    }
}

fn refresh_synthesized_missing_usr(
    installation: &Installation,
    old_root: DirectoryWitness,
) -> Result<ActiveStateProof, Error> {
    revalidate_root(
        installation,
        "revalidate installation root after expected live /usr creation",
    )?;
    let usr_path = installation.root.join("usr");
    let usr = open_usr(installation, &usr_path)?
        .ok_or_else(|| changed(&usr_path, "tree-marker preparation did not create live /usr"))?;
    let usr_witness = directory_witness(&usr, &usr_path)?;
    require_usr_acl_policy(&usr, &usr_path)?;

    let state_path = usr_path.join(".stateID");
    if probe_state_id(&usr, &state_path)?.is_some() {
        return Err(changed(
            &state_path,
            "state ID appeared while preparing first-install tree identity",
        ));
    }
    if !require_empty_or_marker_only(&usr, &usr_path)? {
        return Err(changed(
            &usr_path,
            "first-install tree preparation did not produce marker-only live /usr",
        ));
    }
    let store = TreeMarkerStore::open(&usr, &usr_path).map_err(|source| {
        proof(
            "retain synthesized first-install tree marker",
            &usr_path,
            io::Error::other(source),
        )
    })?;
    let marker = store.read_for_recovery().map_err(|source| {
        proof(
            "authenticate synthesized first-install tree marker",
            &usr_path,
            io::Error::other(source),
        )
    })?;
    require_directory_witness(&usr, &usr_path, usr_witness)?;
    require_named_usr(installation, &usr_path, usr_witness)?;

    let root = root_witness(installation)?;
    // Traditional filesystems increment the parent's link count for mkdir;
    // btrfs and other filesystems may report a fixed directory nlink. The
    // descriptor-rooted marker-only child/name proof above supplies the exact
    // creation evidence, so admit only those two kernel representations.
    let links_expected = root.links == old_root.links || root.links == old_root.links.saturating_add(1);
    if (root.device, root.inode, root.owner, root.mode)
        != (old_root.device, old_root.inode, old_root.owner, old_root.mode)
        || !links_expected
    {
        return Err(changed(
            &installation.root,
            "installation root changed beyond expected live /usr creation",
        ));
    }
    require_root_witness(installation, root)?;
    Ok(ActiveStateProof::PresentBaseline {
        root,
        usr,
        usr_witness,
        marker: Some((store, marker)),
    })
}

fn refresh_existing_usr_after_marker(
    installation: &Installation,
    root: DirectoryWitness,
    usr: &std::fs::File,
    old_usr: DirectoryWitness,
    old_marker: Option<&(TreeMarkerStore, RetainedTreeMarker)>,
    selected: Option<(&std::fs::File, StateIdWitness, &[u8])>,
) -> Result<ActiveStateProof, Error> {
    require_root_witness(installation, root)?;
    let usr_path = installation.root.join("usr");
    let state_path = usr_path.join(".stateID");
    require_directory_identity(usr, &usr_path, old_usr)?;
    require_usr_acl_policy(usr, &usr_path)?;
    require_named_usr_identity(installation, &usr_path, old_usr)?;

    let refreshed = if let Some((state_id, state_witness, bytes)) = selected {
        require_state_id_witness_and_bytes(state_id, &state_path, state_witness, bytes)?;
        let named = probe_state_id(usr, &state_path)?
            .ok_or_else(|| changed(&state_path, "named state ID disappeared during marker handoff"))?;
        if state_id_witness(&named, &state_path)? != state_witness {
            return Err(changed(
                &state_path,
                "named state ID changed inode during marker handoff",
            ));
        }
        require_state_id_witness_and_bytes(&named, &state_path, state_witness, bytes)?;
        require_state_id_witness_and_bytes(state_id, &state_path, state_witness, bytes)?;

        let usr_witness = directory_witness(usr, &usr_path)?;
        require_named_usr(installation, &usr_path, usr_witness)?;
        ActiveStateProof::Selected {
            root,
            usr: usr.try_clone().map_err(Error::from)?,
            usr_witness,
            state_id: state_id.try_clone().map_err(Error::from)?,
            state_witness,
            bytes: bytes.to_vec(),
        }
    } else {
        if probe_state_id(usr, &state_path)?.is_some() {
            return Err(changed(&state_path, "state ID appeared during baseline marker handoff"));
        }
        if !require_empty_or_marker_only(usr, &usr_path)? {
            return Err(changed(
                &usr_path,
                "baseline tree preparation did not leave marker-only live /usr",
            ));
        }
        if let Some((store, marker)) = old_marker {
            marker.revalidate(store).map_err(|source| {
                proof(
                    "revalidate existing first-install tree marker",
                    &usr_path,
                    io::Error::other(source),
                )
            })?;
        }
        let store = TreeMarkerStore::open(usr, &usr_path).map_err(|source| {
            proof(
                "retain prepared first-install tree marker",
                &usr_path,
                io::Error::other(source),
            )
        })?;
        let marker = store.read_for_recovery().map_err(|source| {
            proof(
                "authenticate prepared first-install tree marker",
                &usr_path,
                io::Error::other(source),
            )
        })?;
        let usr_witness = directory_witness(usr, &usr_path)?;
        require_named_usr(installation, &usr_path, usr_witness)?;
        ActiveStateProof::PresentBaseline {
            root,
            usr: usr.try_clone().map_err(Error::from)?,
            usr_witness,
            marker: Some((store, marker)),
        }
    };

    require_root_witness(installation, root)?;
    Ok(refreshed)
}

fn require_directory_identity(directory: &std::fs::File, path: &Path, expected: DirectoryWitness) -> Result<(), Error> {
    let actual = directory_witness(directory, path)?;
    if (actual.device, actual.inode, actual.owner, actual.mode, actual.links)
        == (
            expected.device,
            expected.inode,
            expected.owner,
            expected.mode,
            expected.links,
        )
    {
        Ok(())
    } else {
        Err(changed(path, "live /usr identity changed during tree-marker handoff"))
    }
}

fn require_named_usr_identity(
    installation: &Installation,
    path: &Path,
    expected: DirectoryWitness,
) -> Result<(), Error> {
    let named = open_usr(installation, path)?
        .ok_or_else(|| changed(path, "live /usr disappeared during tree-marker handoff"))?;
    require_directory_identity(&named, path, expected)?;
    require_usr_acl_policy(&named, path)
}
