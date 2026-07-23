impl StoredGit {
    /// Shares the exact Git repository in preparation of a frozen build and
    /// rejects any checkout whose normalized bytes differ from the source lock.
    #[cfg(test)]
    pub async fn share(&self, dest_dir: &Path, source_date_epoch: i64) -> Result<(), Error> {
        self.share_with_parent(dest_dir, source_date_epoch, None).await
    }

    pub(crate) async fn share_into_root(
        &self,
        retained_root: &std::fs::File,
        descriptor_path: &Path,
        source_date_epoch: i64,
    ) -> Result<(), Error> {
        self.share_with_parent(
            &descriptor_path.join(&self.name),
            source_date_epoch,
            Some(retained_root),
        )
        .await
    }

    async fn share_with_parent(
        &self,
        dest_dir: &Path,
        source_date_epoch: i64,
        retained_parent: Option<&std::fs::File>,
    ) -> Result<(), Error> {
        let expected = self
            .materialization_sha256
            .as_deref()
            .ok_or_else(|| Error::MissingMaterializationDigest {
                index: self.original_index,
                commit: self.resolved_hash.clone(),
            })?;
        let parent = dest_dir
            .parent()
            .ok_or_else(|| Error::MissingDestinationParent(dest_dir.to_owned()))?;
        let staging = tempfile::Builder::new()
            .prefix(".cast-git-")
            .tempdir_in(parent)
            .map_err(|source| Error::CreateStaging {
                parent: parent.to_owned(),
                source,
            })?;
        let checkout = staging.path().join("checkout");
        let sealed = self.export_normalized_sealed(&checkout, source_date_epoch).await?;
        if sealed.digest() != expected {
            return Err(Error::MaterializationDigestMismatch {
                index: self.original_index,
                commit: self.resolved_hash.clone(),
                expected: expected.to_owned(),
                found: sealed.digest().to_owned(),
            });
        }
        let installed = match retained_parent {
            Some(parent) => PinnedInstall::install_into(&checkout, dest_dir, parent),
            None => PinnedInstall::install(&checkout, dest_dir),
        }
        .map_err(|source| Error::Install {
            source_path: checkout,
            destination: dest_dir.to_owned(),
            source,
        })?;
        if let Err(source) = sealed.verify_installed_descriptor_path(dest_dir) {
            return match installed.quarantine() {
                Ok(quarantine) => Err(Error::RejectedInstalledMaterialization {
                    destination: dest_dir.to_owned(),
                    quarantine,
                    source,
                }),
                Err(cleanup) => Err(Error::RejectedInstallCleanup {
                    destination: dest_dir.to_owned(),
                    verification: Box::new(source),
                    cleanup,
                }),
            };
        }

        Ok(())
    }
}
