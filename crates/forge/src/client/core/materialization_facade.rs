impl Client {
    /// Build a [`vfs::Tree`] for the specified package IDs
    ///
    /// Returns a newly built vfs Tree to plan the filesystem operations for blitting
    /// and conflict detection.
    pub fn vfs<'a>(
        &self,
        packages: impl IntoIterator<Item = &'a package::Id>,
    ) -> Result<vfs::Tree<PendingFile>, Error> {
        vfs(self.layout_db.query(packages)?)
    }

    fn canonical_frozen_package_ids(&self, packages: &[package::Id]) -> Result<Vec<package::Id>, Error> {
        // Bound borrowed inputs before cloning or sorting them. This helper is
        // shared by public frozen materialization and executable verification,
        // so neither entry point may allocate an attacker-sized closure first.
        require_frozen_executable_package_count(packages.len())?;
        let mut closure_id_bytes = 0usize;
        for package in packages {
            account_frozen_closure_id_bytes(package, &mut closure_id_bytes)?;
        }
        let mut canonical = packages.to_vec();
        canonical.sort();
        if canonical.is_empty() {
            return Err(Error::EmptyFrozenPackageClosure);
        }
        for duplicate in canonical.windows(2) {
            if duplicate[0] == duplicate[1] {
                return Err(Error::DuplicateFrozenPackage(duplicate[0].clone()));
            }
        }
        Ok(canonical)
    }

    fn frozen_root(&self) -> Result<&Path, Error> {
        match &self.scope {
            Scope::Frozen { destination } => Ok(&destination.root_path),
            Scope::Stateful | Scope::Ephemeral { .. } => Err(Error::FrozenRootRequiresFrozenClient),
        }
    }

    fn frozen_destination(&self) -> Result<&FrozenRootDestination, Error> {
        match &self.scope {
            Scope::Frozen { destination } => Ok(destination),
            Scope::Stateful | Scope::Ephemeral { .. } => Err(Error::FrozenRootRequiresFrozenClient),
        }
    }

    /// Materialize already-cached package layouts through the frozen-root
    /// filesystem path. This deliberately bypasses every stateful and legacy
    /// ephemeral post-blit operation.
    fn blit_frozen_root(
        &self,
        packages: &[package::Id],
        source_date_epoch: i64,
    ) -> Result<MaterializedFrozenRoot, Error> {
        let destination = self.frozen_destination()?;
        let blit_target = destination.root_path.clone();
        let deadline = Instant::now() + FROZEN_MATERIALIZATION_TIMEOUT - FROZEN_NAMESPACE_RECOVERY_TIMEOUT;
        require_frozen_materialization_deadline(deadline)?;
        let _destination_lock = lock_frozen_destination_until(destination, deadline)?;
        let packages = self.canonical_frozen_package_ids(packages)?;
        let layouts = bounded_frozen_layouts(self, &packages, deadline, FrozenLayoutQueryOperation::Materialization)?;
        let fstree = frozen_vfs_until(&packages, layouts, deadline)?;
        let expected_tree = FrozenExpectedTree::from_vfs(&fstree, deadline)?;

        if frozen_named_identity_until(&destination.parent, &destination.name, &blit_target, deadline)?.is_some() {
            return Err(Error::FrozenRootDestinationExists(blit_target));
        }
        // Authenticate and account every independent regular-file copy before
        // creating staging state. Duplicate digests are charged once per
        // output inode, while canonical empty files consume zero bytes.
        let copy_manifest = FrozenCopyManifest::from_tree(&self.installation, &fstree, deadline)?;
        let stage = create_frozen_private_directory(destination, b".forge-frozen-stage-", deadline)?;
        // The random sibling wrapper remains 0700 for the entire build. The
        // publishable root can therefore carry its final 0755 mode without
        // exposing partial contents before the atomic rename.
        let stage_wrapper = stage.path.clone();
        let stage_path = stage_wrapper.join("root");
        let stage_wrapper_file = &stage.file;
        let mut retained_root = None;
        let result = (|| -> Result<MaterializedFrozenRoot, Error> {
            mkdirat(stage_wrapper_file.as_raw_fd(), "root", Mode::from_bits_truncate(0o755))?;
            let resolution = (nix::libc::RESOLVE_BENEATH
                | nix::libc::RESOLVE_NO_SYMLINKS
                | nix::libc::RESOLVE_NO_MAGICLINKS
                | nix::libc::RESOLVE_NO_XDEV) as u64;
            let root_anchor = openat2_frozen_until(
                stage_wrapper_file.as_raw_fd(),
                Path::new("root"),
                nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
                resolution,
                deadline,
            )
            .map_err(|source| frozen_materialization_io_error(deadline, source, Error::from))?;
            let wrapper_metadata = stage_wrapper_file.metadata()?;
            let root_metadata = root_anchor.metadata()?;
            // SAFETY: geteuid takes no arguments and cannot fail.
            let effective_owner = unsafe { nix::libc::geteuid() };
            if !root_metadata.is_dir()
                || root_metadata.uid() != effective_owner
                || root_metadata.dev() != wrapper_metadata.dev()
                || root_metadata.mode() & 0o7777 & !0o755 != 0
            {
                return Err(Error::FrozenNormalizationInventoryMismatch {
                    path: stage_path.clone(),
                    reason: "the descriptor-relative stage root is not a same-owner same-filesystem creation residue",
                });
            }
            let root_residue = FrozenNormalizationWitness::from_metadata(&root_metadata);
            chmod_path_descriptor_until(root_anchor.file(), 0o755, deadline).map_err(|source| {
                frozen_materialization_io_error(deadline, source, |source| Error::NormalizeFrozenEntryMode {
                    path: stage_path.clone(),
                    source,
                })
            })?;
            let root = openat2_frozen_until(
                stage_wrapper_file.as_raw_fd(),
                Path::new("root"),
                nix::libc::O_RDONLY
                    | nix::libc::O_DIRECTORY
                    | nix::libc::O_CLOEXEC
                    | nix::libc::O_NOFOLLOW
                    | nix::libc::O_NONBLOCK
                    | nix::libc::O_NOATIME,
                resolution,
                deadline,
            )
            .map_err(|source| frozen_materialization_io_error(deadline, source, Error::from))?;
            retained_root = Some(root);
            let root = retained_root
                .as_ref()
                .expect("retained frozen root was assigned immediately above");
            let normalized_root = root_residue.with_permissions(0o755);
            require_frozen_normalization_witness(Path::new("/"), &root_anchor, normalized_root)?;
            require_frozen_normalization_witness(Path::new("/"), &root, normalized_root)?;
            let empty = frozen_normalization_inventory(&root, Path::new("/"), 0, None, deadline)?;
            debug_assert!(empty.is_empty());

            blit_tree_into_open_root(
                &self.installation,
                &fstree,
                root.as_raw_fd(),
                AssetMaterialization::IndependentCopy,
                BlitExecution::Sequential,
                Some(&copy_manifest),
                Some(deadline),
                None,
            )?;
            create_frozen_root_links(root.as_raw_fd(), deadline)?;
            normalize_frozen_tree(
                &root,
                &stage_path,
                &expected_tree,
                FileTime::from_unix_time(source_date_epoch, 0),
                deadline,
            )?;
            require_frozen_materialization_deadline(deadline)?;
            // Keep the descriptor opened before blitting as provenance across
            // normalization and publication. A replacement staging name can
            // never become the returned root token.
            publish_frozen_root(&stage, destination, root, root_anchor, deadline)
        })();

        let cleanup_deadline = frozen_namespace_recovery_deadline();
        match result {
            Ok(materialized_root) => {
                // `stage_path` moved out atomically; the private wrapper is
                // now empty and can be removed without traversal.
                if let Err(error) = remove_empty_frozen_private_directory(&stage, destination, cleanup_deadline) {
                    // Publication is already complete and cannot be reported
                    // as a failed materialization. The random 0700 wrapper is
                    // never a reusable root even if its empty-dir cleanup is
                    // interrupted.
                    trace!(path = ?stage_wrapper, %error, "failed to remove empty frozen stage wrapper");
                }
                materialized_root.revalidate()?;
                Ok(materialized_root)
            }
            Err(primary) => {
                let cleanup = match retained_root.as_ref() {
                    Some(root) => discard_retained_frozen_stage(&stage, destination, root, cleanup_deadline),
                    None => require_frozen_private_directory_entries(&stage, &[], cleanup_deadline),
                }
                .and_then(|()| remove_empty_frozen_private_directory(&stage, destination, cleanup_deadline));
                match cleanup {
                    Ok(()) => Err(primary),
                    Err(cleanup) => Err(Error::CleanupFrozenStage {
                        stage: stage_wrapper,
                        primary: Box::new(primary),
                        cleanup: Box::new(cleanup),
                    }),
                }
            }
        }
    }

    /// Blit the packages to a filesystem root
    ///
    /// This functionality is core to all Cast filesystem transactions, forming the entire
    /// staging logic. For all the [`crate::package::Id`] present in the staging state,
    /// query their stored [`StonePayloadLayoutBody`] and cache into a [`vfs::Tree`].
    ///
    /// The new `/usr` filesystem is written in optimal order to a staging tree by making
    /// use of the "at" family of functions (`mkdirat`, `linkat`, etc) with relative directory
    /// file descriptors. Every writable root receives independent, digest-
    /// verified copies so trigger or build-time writes and per-path mode changes
    /// cannot mutate the persistent content-addressed store.
    ///
    /// This provides a very quick means to generate a filesystem snapshot on-demand,
    /// which can then be activated via [`Self::promote_staging`]
    pub fn blit_root<'a>(
        &self,
        packages: impl IntoIterator<Item = &'a package::Id>,
    ) -> Result<vfs::Tree<PendingFile>, Error> {
        let candidate = self.materialize_ephemeral_candidate(packages)?;
        let EphemeralCandidate {
            tree,
            root: _root,
            target: _target,
            candidate_usr: _candidate_usr,
            active_state: _active_state,
        } = candidate;
        Ok(tree)
    }

    fn materialize_ephemeral_candidate<'a>(
        &self,
        packages: impl IntoIterator<Item = &'a package::Id>,
    ) -> Result<EphemeralCandidate, Error> {
        let destination = match &self.scope {
            Scope::Stateful => {
                return Err(Error::FixedStagingCapabilityRequired {
                    operation: "materialize a stateful client root",
                });
            }
            Scope::Ephemeral { destination } => destination,
            Scope::Frozen { .. } => return Err(Error::FrozenClientProhibitedOperation),
        };
        let active_state = active_state_snapshot::ActiveStateLease::acquire(&self.installation)?;

        let tree = self.vfs(packages)?;
        active_state.revalidate(&self.installation)?;
        let mut target = RetainedExternalMaterializationTarget::prepare_from(&self.installation, destination)?;
        let candidate_usr = target.materialize(
            &self.installation,
            &tree,
            AssetMaterialization::IndependentCopy,
            BlitExecution::Parallel,
        )?;
        active_state.revalidate(&self.installation)?;
        let root = target.path().to_owned();

        Ok(EphemeralCandidate {
            tree,
            root,
            target,
            candidate_usr,
            active_state,
        })
    }

    fn load_or_create_system_snapshot(&self, path: PathBuf, state: &State) -> Result<SystemModel, Error> {
        match system_model::load(&path).map_err(Error::LoadSystemModel)? {
            Some(system_model) => SystemModel::try_from(system_model)
                .map_err(system_model::UpdateError::from)
                .map_err(Error::UpdateSystemModel),
            None => {
                let active_repos = self
                    .repositories
                    .active()
                    .map(|repo| (repo.id, repo.repository))
                    .collect::<repository::Map>();

                let packages = self
                    .resolve_packages(state.selections.iter().filter_map(|s| s.explicit.then_some(&s.package)))?
                    .into_iter()
                    .map(|package| Provider::package_name(package.meta.name.as_str()))
                    .collect();

                Ok(system_model::create(active_repos, packages))
            }
        }
    }
}
