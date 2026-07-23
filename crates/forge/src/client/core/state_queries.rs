impl Client {
    /// Export the provided state as a [`SystemModel`]
    pub fn export_state(&self, state: state::Id) -> Result<SystemModel, Error> {
        let active_state = match &self.scope {
            Scope::Frozen { .. } => None,
            Scope::Stateful | Scope::Ephemeral { .. } => {
                Some(active_state_snapshot::ActiveStateLease::acquire(&self.installation)?)
            }
        };
        let state = self.state_db.get(state)?;
        if let Some(active_state) = active_state.as_ref() {
            active_state.revalidate(&self.installation)?;
        }
        let is_active = active_state.as_ref().and_then(|lease| lease.active()) == Some(state.id);

        let path = if is_active {
            system_model::snapshot_path(&self.installation.root)
        } else {
            system_model::snapshot_path(&self.installation.root_path(state.id.to_string()))
        };

        let active_snapshot = active_state
            .map(|active_state| active_state.suspend(&self.installation))
            .transpose()?;
        let snapshot = self.load_or_create_system_snapshot(path, &state)?;
        if let Some(active_snapshot) = active_snapshot {
            drop(active_snapshot.resume(&self.installation)?);
        }
        Ok(snapshot)
    }

    /// Print boot status to stdout
    pub fn print_boot_status(&self) -> Result<(), Error> {
        boot::print_status(&self.installation).map_err(Error::Boot)
    }

    /// Synchronize boot for the active state
    pub fn synchronize_boot(&self) -> Result<(), Error> {
        self.require_stateful_scope()?;
        let active_state = active_state_snapshot::ActiveStateLease::acquire(&self.installation)?;
        let Some(state_id) = active_state.active() else {
            return Err(Error::NoActiveState);
        };

        // The active-state lease establishes coordinator-before-journal lock
        // ordering. Retain the exact clean journal across both the database
        // read and the boot backend so an older Client cannot synchronize
        // while a journal-owned transition remains unresolved.
        let authority = clean_boot_synchronization::CleanBootSynchronizationAuthority::capture(
            &self.installation,
            &self.state_db,
            &active_state,
        )
        .map_err(boot_synchronization_authority_error)?;
        let state = self.state_db.get(state_id)?;
        authority
            .revalidate()
            .map_err(boot_synchronization_authority_error)?;

        let synchronization = boot::synchronize(self, &state, None);
        authority.before_post_revalidation();
        let post_authority = authority.revalidate();

        // Authority failure supersedes a simultaneous backend error: once
        // journal, database, namespace, or active-state evidence changed, the
        // backend result cannot be attributed to the admitted clean system.
        post_authority.map_err(boot_synchronization_authority_error)?;
        synchronization.map_err(Error::Boot)
    }

    /// List all states for this Cast [`Installation`]
    pub fn list_states(&self) -> Result<Vec<State>, Error> {
        self.state_db
            .list_ids()?
            .into_iter()
            .map(|(id, _)| self.state_db.get(id).map_err(Error::Db))
            .collect()
    }

    /// Return a [`State`] for the provided state id
    pub fn get_state(&self, id: state::Id) -> Result<State, Error> {
        self.state_db.get(id).map_err(Error::Db)
    }

    /// Return the active [`State`] for this Cast [`Installation`]
    pub fn get_active_state(&self) -> Result<Option<State>, Error> {
        let active_state = match &self.scope {
            Scope::Frozen { .. } => return Ok(None),
            Scope::Stateful | Scope::Ephemeral { .. } => {
                active_state_snapshot::ActiveStateLease::acquire(&self.installation)?
            }
        };
        let state = match active_state.active() {
            Some(id) => self.get_state(id).map(Some),
            None => Ok(None),
        }?;
        active_state.revalidate(&self.installation)?;
        Ok(state)
    }

    /// List all layout entries cached by this Cast [`Installation`], which
    /// includes packages installed across all states
    pub fn list_layouts(&self) -> Result<Vec<(package::Id, StonePayloadLayoutRecord)>, Error> {
        self.layout_db.all().map_err(Error::Db)
    }

    #[cfg(any(test, feature = "testing"))]
    pub fn mocked(installation: Installation, registry: Registry) -> Result<Client, Error> {
        let config = config::Manager::system(&installation.root, "cast");
        let install_db = db::meta::Database::new(":memory:")?;
        let state_db = db::state::Database::new(":memory:")?;
        let layout_db = db::layout::Database::new(":memory:")?;

        let repositories = repository::Manager::with_config_manager(config.clone(), installation.clone())?;

        Ok(Client {
            config: Some(config),
            installation,
            repositories,
            registry,
            install_db,
            state_db,
            layout_db,
            scope: Scope::Stateful,
        })
    }
}

fn boot_synchronization_authority_error(
    source: clean_boot_synchronization::CleanBootSynchronizationAuthorityError,
) -> Error {
    Error::BootSynchronizationAuthority {
        source: Box::new(source),
    }
}
