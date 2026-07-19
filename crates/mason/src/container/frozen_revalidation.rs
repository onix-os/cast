use std::fs::File;

use super::{
    DirectoryRole, DirectoryWitness, Error, FrozenMountRole, FrozenMountSource, FrozenSandbox, open_mount_directory,
    open_workspace_directory, require_owned_directory,
};

impl FrozenSandbox {
    pub(super) fn revalidate(&self) -> Result<(), Error> {
        require_owned_directory(
            &self.workspace.file,
            &self.workspace.path,
            false,
            DirectoryRole::Workspace,
        )?;
        if DirectoryWitness::for_file(&self.workspace.file).map_err(|source| Error::OpenFrozenWorkspace {
            path: self.workspace.path.clone(),
            source,
        })? != self.workspace.witness
            || DirectoryWitness::for_file(&self.workspace.path_anchor).map_err(|source| Error::OpenFrozenWorkspace {
                path: self.workspace.path.clone(),
                source,
            })? != self.workspace.witness
        {
            return Err(Error::FrozenWorkspaceReplaced(self.workspace.path.clone()));
        }

        let reopened = open_mount_directory(&self.workspace.path).map_err(|source| Error::OpenFrozenWorkspace {
            path: self.workspace.path.clone(),
            source,
        })?;
        require_owned_directory(&reopened, &self.workspace.path, false, DirectoryRole::Workspace)?;
        if DirectoryWitness::for_file(&reopened).map_err(|source| Error::OpenFrozenWorkspace {
            path: self.workspace.path.clone(),
            source,
        })? != self.workspace.witness
        {
            return Err(Error::FrozenWorkspaceReplaced(self.workspace.path.clone()));
        }

        for mount in &self.mounts {
            let FrozenMountSource::Pinned(source) = &mount.source else {
                continue;
            };
            require_owned_directory(&source.file, &mount.host, true, DirectoryRole::BindSource)?;
            let ordinary = DirectoryWitness::for_file(&source.file).map_err(|io| Error::PrepareFrozenBindSource {
                path: mount.host.clone(),
                source: io,
            })?;
            let path =
                DirectoryWitness::for_file(&source.path_anchor).map_err(|io| Error::PrepareFrozenBindSource {
                    path: mount.host.clone(),
                    source: io,
                })?;
            let reopened = open_workspace_directory(&self.workspace, &source.relative, &mount.host, true)?;
            let reopened = DirectoryWitness::for_file(&reopened).map_err(|io| Error::PrepareFrozenBindSource {
                path: mount.host.clone(),
                source: io,
            })?;
            if ordinary != source.witness || path != source.witness || reopened != source.witness {
                return Err(Error::FrozenBindSourceReplaced(mount.host.clone()));
            }
        }
        Ok(())
    }

    /// Revalidate the complete external sandbox and borrow the exact artefact
    /// directory which was mounted into the container.
    ///
    /// Host publication consumes this ordinary descriptor rather than
    /// reopening the public pathname after the payload exits.
    pub(crate) fn revalidated_artefacts(&self) -> Result<&File, Error> {
        self.revalidate()?;
        let mount = self
            .mounts
            .iter()
            .find(|mount| mount.role == FrozenMountRole::Artefacts)
            .ok_or(Error::MissingFrozenArtefactMount)?;
        let FrozenMountSource::Pinned(source) = &mount.source else {
            return Err(Error::MissingFrozenArtefactMount);
        };
        Ok(&source.file)
    }
}
