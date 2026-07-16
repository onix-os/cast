impl Client {
    /// Construct a new ClientBuilder for the given [`Installation`]
    pub fn builder(client_name: impl ToString, installation: Installation) -> ClientBuilder {
        ClientBuilder {
            client_name: client_name.to_string(),
            installation,
            repositories: None,
            system_intent_path: None,
            system_intent_notice: None,
            blit_root: None,
        }
    }

    /// Construct a CLI client whose declarative-intent notice is emitted only
    /// after the startup gate, strict discovery, intent evaluation,
    /// repositories, and registry all succeed.
    pub(crate) fn for_cli(
        client_name: impl ToString,
        installation: Installation,
        verbose: bool,
    ) -> Result<Client, Error> {
        Self::builder(client_name, installation)
            .system_intent_notice(verbose)
            .build()
    }

    pub(crate) fn cli_builder(client_name: impl ToString, installation: Installation, verbose: bool) -> ClientBuilder {
        Self::builder(client_name, installation).system_intent_notice(verbose)
    }

    /// Construct a new Client for the given [`Installation`]
    pub fn new(client_name: impl ToString, installation: Installation) -> Result<Client, Error> {
        Self::builder(client_name.to_string(), installation).build()
    }

    pub(crate) fn system_intent(&self) -> Option<&LoadedSystemModel> {
        self.installation.system_model.as_ref()
    }

    pub(crate) fn into_repository_manager(self) -> repository::Manager {
        self.repositories
    }

    /// Construct a cache-only client for one frozen root materialization.
    ///
    /// The installation must have been opened with [`Installation::open_frozen`].
    /// Only the supplied active repositories are registered: authored system
    /// intent, active-state metadata, local cobble packages, and Cast config
    /// files cannot participate in exact package selection.
    pub fn frozen(
        client_name: impl ToString,
        installation: Installation,
        repositories: repository::Map,
        blit_root: impl Into<PathBuf>,
    ) -> Result<Client, Error> {
        if !installation.is_frozen_cache() {
            return Err(Error::FrozenInstallationRequired);
        }

        let requested_root = blit_root.into();
        let root_name = requested_root
            .file_name()
            .filter(|name| !name.as_bytes().contains(&0))
            .ok_or_else(|| Error::InvalidFrozenRootDestination(requested_root.clone()))?;
        let requested_parent = requested_root
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let parent = requested_parent.canonicalize()?;
        if !parent.is_dir() {
            return Err(Error::InvalidFrozenRootDestination(requested_root));
        }
        let destination_name = CString::new(root_name.as_bytes())
            .map_err(|_| Error::InvalidFrozenRootDestination(requested_root.clone()))?;
        let blit_root = require_disjoint_materialization_target(&installation, &parent.join(root_name))?;
        let destination_parent = open_frozen_destination_parent(&parent)?;
        let destination_parent_identity = frozen_root_identity(&destination_parent, &parent)?;

        let install_db = db::meta::Database::new(installation.db_path("install").to_str().unwrap_or_default())?;
        let state_db = db::state::Database::new(":memory:")?;
        let layout_db = db::layout::Database::new(installation.db_path("layout").to_str().unwrap_or_default())?;
        let repositories = repository::Manager::with_explicit(client_name, repositories, installation.clone())?;
        let registry = build_repository_registry(&repositories);

        Ok(Client {
            installation,
            registry,
            install_db,
            state_db,
            layout_db,
            config: None,
            repositories,
            scope: Scope::Frozen {
                destination: FrozenRootDestination {
                    root_path: blit_root,
                    parent_path: parent,
                    name: destination_name,
                    parent: destination_parent,
                    parent_identity: destination_parent_identity,
                },
            },
        })
    }

    /// Returns `true` if this is an ephemeral client
    pub fn is_ephemeral(&self) -> bool {
        self.scope.is_ephemeral()
    }

    fn require_non_frozen(&self) -> Result<(), Error> {
        if matches!(self.scope, Scope::Frozen { .. }) {
            Err(Error::FrozenClientProhibitedOperation)
        } else {
            Ok(())
        }
    }

    fn require_stateful_scope(&self) -> Result<(), Error> {
        match self.scope {
            Scope::Stateful => Ok(()),
            Scope::Ephemeral { .. } => Err(Error::EphemeralProhibitedOperation),
            Scope::Frozen { .. } => Err(Error::FrozenClientProhibitedOperation),
        }
    }

    fn preflight_active_state_snapshot(&self) -> Result<(), Error> {
        if matches!(&self.scope, Scope::Stateful | Scope::Ephemeral { .. }) {
            drop(active_state_snapshot::ActiveStateLease::acquire(&self.installation)?);
        }
        Ok(())
    }

    fn active_state_for_planning(&self) -> Result<Option<state::Id>, Error> {
        match &self.scope {
            Scope::Stateful | Scope::Ephemeral { .. } => {
                let active_state = active_state_snapshot::ActiveStateLease::acquire(&self.installation)?;
                active_state.revalidate(&self.installation)?;
                Ok(active_state.active())
            }
            Scope::Frozen { .. } => Err(Error::FrozenClientProhibitedOperation),
        }
    }

    fn with_registry_snapshot<T, E>(&self, read: impl FnOnce(&Registry) -> Result<T, E>) -> Result<T, E>
    where
        E: From<Error>,
    {
        let active_state = match &self.scope {
            Scope::Frozen { .. } => None,
            Scope::Stateful | Scope::Ephemeral { .. } => {
                before_registry_snapshot_acquisition();
                Some(active_state_snapshot::ActiveStateLease::acquire(&self.installation)?)
            }
        };
        self.preflight_repository_integrity()?;
        let result = read(&self.registry);
        let repository_revalidation = self.preflight_repository_integrity();
        if let Some(active_state) = active_state.as_ref() {
            active_state.revalidate(&self.installation)?;
        }
        repository_revalidation?;
        result
    }

    fn preflight_repository_integrity(&self) -> Result<(), Error> {
        self.repositories
            .preflight_active_snapshots()
            .map_err(Error::Repository)
    }

    /// Perform package installation
    pub fn install(&mut self, packages: &[&str], yes: bool, simulate: bool) -> Result<install::Timing, Error> {
        self.require_non_frozen()?;
        self.preflight_active_state_snapshot()?;
        self.preflight_repository_integrity()?;
        install(self, packages, yes, simulate).map_err(|error| Error::Install(Box::new(error)))
    }

    /// Install an exact, pre-resolved package closure.
    ///
    /// Unlike [`Self::install`], this does not resolve provider requests or
    /// traverse dependency metadata. The supplied package IDs are the complete
    /// closure selected by an external planner.
    pub fn install_exact(
        &mut self,
        packages: &[package::Id],
        yes: bool,
        simulate: bool,
    ) -> Result<install::Timing, Error> {
        self.require_non_frozen()?;
        self.preflight_active_state_snapshot()?;
        self.preflight_repository_integrity()?;
        install::install_exact(self, packages, yes, simulate).map_err(|error| Error::Install(Box::new(error)))
    }

    /// Materialize an exact package closure into a dedicated frozen root.
    ///
    /// Package IDs are treated as a canonical set and sorted by ID. No
    /// provider lookup, dependency traversal, state creation, system-model
    /// generation, triggers, boot synchronization, or isolation-root writes
    /// occur on this path. The resulting root uses independent file inodes and
    /// normalizes content, inode type, mode, atime, and mtime. Kernel-assigned
    /// inode/dev/ctime/btime values are deliberately outside this contract.
    pub fn materialize_frozen_root(
        &mut self,
        packages: &[package::Id],
        source_date_epoch: i64,
    ) -> Result<FrozenMaterialization, Error> {
        install::materialize_frozen_root(self, packages, source_date_epoch)
            .map_err(|error| Error::Install(Box::new(error)))
    }

    /// Explicitly discard the previously published root owned by this frozen
    /// client so a later absent-only materialization can publish a new one.
    ///
    /// The named root is first atomically detached into a private sibling.
    /// Cleanup then remains descriptor-rooted, never follows symlinks or mount
    /// points, restores owner access only on directories being discarded, and
    /// enforces finite entry, depth, and wall-time bounds.
    pub fn discard_frozen_root(&self) -> Result<(), Error> {
        discard_frozen_root_destination(self.frozen_destination()?)
    }

    /// Prove that every frozen executable binding is supplied by its exact
    /// resolved package and names the unchanged regular executable now present
    /// in the materialized root.
    ///
    /// This is deliberately separate from provider resolution. Callers first
    /// materialize the already-locked closure, then invoke this method before
    /// entering the build container. A metadata-only package which advertises
    /// `binary(foo)` but has no `/usr/bin/foo` layout entry therefore fails
    /// closed instead of borrowing an ambient executable from another package.
    /// Only the explicitly supported structural host-ELF subset and scripts
    /// with one strict absolute shebang path are admitted. Every shebang or ELF
    /// `PT_INTERP` target is recursively matched to one closure layout
    /// provider, pinned, fully verified, and rechecked before return. The
    /// returned guard must remain live through activation; callers borrow its
    /// revalidated root anchor immediately before constructing the container.
    pub fn require_materialized_frozen_executables(
        &self,
        materialized_root: MaterializedFrozenRoot,
        packages: &[package::Id],
        bindings: &[FrozenExecutableBinding],
    ) -> Result<FrozenRootGuard, Error> {
        if materialized_root.root_path() != self.frozen_root()? {
            return Err(Error::ForeignMaterializedFrozenRoot {
                expected: self.frozen_root()?.to_owned(),
                found: materialized_root.root_path().to_owned(),
            });
        }
        let packages = self.canonical_frozen_package_ids(packages)?;
        require_frozen_executables(self, materialized_root, &packages, bindings, |_, _| {})
    }

    /// Unit-test adapter for verifier cases which construct their filesystem
    /// fixture directly rather than through the production materializer.
    #[cfg(test)]
    fn require_frozen_executables(
        &self,
        packages: &[package::Id],
        bindings: &[FrozenExecutableBinding],
    ) -> Result<FrozenRootGuard, Error> {
        let root = test_materialized_frozen_root(self.frozen_root()?)?;
        self.require_materialized_frozen_executables(root, packages, bindings)
    }

    /// Perform package removals
    pub fn remove(&mut self, packages: &[&str], yes: bool, simulate: bool) -> Result<remove::Timing, Error> {
        self.require_non_frozen()?;
        self.preflight_active_state_snapshot()?;
        remove(self, packages, yes, simulate).map_err(|error| Error::Remove(Box::new(error)))
    }

    /// Perform package fetches
    pub fn fetch(&mut self, packages: &[&str], output_dir: &Path, verbose: bool) -> Result<fetch::Timing, Error> {
        self.require_non_frozen()?;
        self.preflight_repository_integrity()?;
        fetch(self, packages, output_dir, verbose).map_err(|error| Error::Fetch(Box::new(error)))
    }

    /// Perform a sync
    pub fn sync(&mut self, yes: bool, simulate: bool) -> Result<sync::Timing, Error> {
        self.require_non_frozen()?;
        self.preflight_active_state_snapshot()?;
        self.preflight_repository_integrity()?;
        sync(self, yes, simulate).map_err(|error| Error::Sync(Box::new(error)))
    }

    /// Transition to an ephemeral client that doesn't record state changes
    /// and blits to a different root.
    ///
    /// This is useful for installing a root to a container (for example, Mason) while
    /// using a shared cache.
    ///
    /// Returns an error if the canonical destination is equal to, beneath, or
    /// an ancestor of the installation root. Destructive ephemeral
    /// materialization must be namespace-disjoint from all persistent state.
    pub fn ephemeral(self, blit_root: impl Into<PathBuf>) -> Result<Self, Error> {
        self.require_non_frozen()?;
        let destination = ExternalMaterializationAdmission::admit(&self.installation, &blit_root.into())?;

        Ok(Self {
            scope: Scope::Ephemeral { destination },
            ..self
        })
    }

    /// Ensures all repositories have been initialized by ensuring their stone indexes
    /// are downloaded and added to the meta db
    pub async fn ensure_repos_initialized(&mut self) -> Result<usize, Error> {
        self.require_non_frozen()?;
        self.preflight_active_state_snapshot()?;
        let num_initialized = self.repositories.ensure_all_initialized().await?;
        self.rebuild_registry()?;
        Ok(num_initialized)
    }

    /// Reload all configured repositories and refreshes their index file, then update
    /// registry with all active repositories.
    pub async fn refresh_repositories(&mut self) -> Result<(), Error> {
        self.require_non_frozen()?;
        self.preflight_active_state_snapshot()?;
        // Reload manager if config sourced to pickup config changes
        // then refresh indexes
        if self.repositories.is_config_source() {
            let config = self
                .config
                .clone()
                .expect("config-sourced clients always retain their config manager");
            self.repositories = repository::Manager::with_config_manager(config, self.installation.clone())?;
        };
        self.repositories.refresh_all().await?;

        // Rebuild registry
        self.rebuild_registry()?;

        Ok(())
    }

    fn rebuild_registry(&mut self) -> Result<(), Error> {
        let registry = match &self.scope {
            Scope::Frozen { .. } => build_repository_registry(&self.repositories),
            Scope::Stateful | Scope::Ephemeral { .. } => {
                let active_state = active_state_snapshot::ActiveStateLease::acquire(&self.installation)?;
                let registry = build_registry(
                    active_state.active(),
                    &self.repositories,
                    &self.install_db,
                    &self.state_db,
                )?;
                active_state.revalidate(&self.installation)?;
                registry
            }
        };
        self.registry = registry;
        Ok(())
    }

    pub fn verify(&self, yes: bool, verbose: bool) -> Result<(), Error> {
        self.require_stateful_scope()?;
        verify(self, yes, verbose)?;
        Ok(())
    }

    /// Prune states with the provided [`prune::Strategy`].
    ///
    /// This allows automatic removal of unused states (and their associated assets)
    /// from the disk, acting as a garbage collection facility.
    pub fn prune_states(&self, strategy: prune::Strategy<'_>, yes: bool) -> Result<(), Error> {
        self.require_stateful_scope()?;
        let active_state = active_state_snapshot::ActiveStateLease::acquire(&self.installation)?;

        prune_states(self, strategy, yes, &active_state)?;

        Ok(())
    }

    /// Prune all cached data that isn't related to any states or active repositories.
    ///
    /// This will remove all downloaded stones & unpacked asset data for packages not
    /// in that set.
    pub fn prune_cache(&self) -> Result<usize, Error> {
        self.require_stateful_scope()?;
        let _stateful_coordinator = fixed_staging::lock_coordinator()?;

        prune_cache(
            &self.state_db,
            &self.install_db,
            &self.layout_db,
            &self.installation,
            &self.repositories,
        )
        .map_err(Error::Prune)
    }

    /// Resolves the provided id with the underlying registry, returning the first matching [`Package`]
    pub fn resolve_package(&self, package: &package::Id) -> Result<Package, Error> {
        self.with_registry_snapshot(|registry| {
            registry
                .by_id(package)?
                .into_iter()
                .next()
                .ok_or(Error::MissingMetadata(package.clone()))
        })
    }

    fn resolve_frozen_repository_package(&self, package: &package::Id) -> Result<Package, Error> {
        self.frozen_root()?;
        self.repositories
            .resolve_exact_package(package)?
            .map(|(_, package)| package)
            .ok_or_else(|| Error::MissingMetadata(package.clone()))
    }

    /// Resolves the provided id's with the underlying registry, returning
    /// the first [`Package`] for each id.
    ///
    /// Packages are sorted by name and deduped before returning.
    pub fn resolve_packages<'a>(
        &self,
        packages: impl IntoIterator<Item = &'a package::Id>,
    ) -> Result<Vec<Package>, Error> {
        self.with_registry_snapshot(|registry| {
            let mut metadata = packages
                .into_iter()
                .map(|id| {
                    registry
                        .by_id(id)?
                        .into_iter()
                        .next()
                        .ok_or(Error::MissingMetadata(id.clone()))
                })
                .collect::<Result<Vec<_>, _>>()?;
            metadata.sort_by_key(|p| p.meta.name.to_string());
            metadata.dedup_by_key(|p| p.meta.name.to_string());
            Ok(metadata)
        })
    }

    /// Content identities for all active repository indexes participating in
    /// available-package resolution.
    pub fn repository_index_snapshots(&self) -> Result<Vec<repository::IndexSnapshot>, Error> {
        self.repositories.index_snapshots().map_err(Error::Repository)
    }

    /// Returns all unique packages which provide the supplied [`Provider`]
    pub fn lookup_packages_by_provider(
        &self,
        provider: &Provider,
        flags: package::Flags,
    ) -> Result<Vec<Package>, Error> {
        self.with_registry_snapshot(|registry| {
            Ok(registry
                .by_provider(provider, flags)?
                .into_iter()
                .unique_by(|p| p.id.clone())
                .collect())
        })
    }

    /// Return sorted packages matching the given flags. Repository integrity
    /// failures are first-class and cannot be flattened into an empty list.
    pub fn list_packages(&self, flags: package::Flags) -> Result<Vec<Package>, Error> {
        self.with_registry_snapshot(|registry| registry.list(flags).map_err(Error::from))
    }

    /// Returns all packages with names containing the provided keyword
    /// and match the given flags
    pub fn search_packages(&self, keyword: &str, flags: package::Flags) -> Result<Vec<Package>, Error> {
        self.with_registry_snapshot(|registry| registry.by_keyword(keyword, flags).map_err(Error::from))
    }
}
