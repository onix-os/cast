impl Client {
    fn apply_ephemeral_candidate(
        &self,
        candidate: EphemeralCandidate,
        system_snapshot: SystemModel,
    ) -> Result<(), Error> {
        let EphemeralCandidate {
            tree,
            root,
            mut target,
            candidate_usr,
            active_state,
        } = candidate;
        active_state.revalidate(&self.installation)?;
        self.require_configured_ephemeral_target(&root)?;
        target.revalidate_candidate_usr(&self.installation, &candidate_usr)?;
        let result =
            self.apply_ephemeral_blit_under_guard(tree, &mut target, &candidate_usr, &active_state, system_snapshot);
        let revalidation = target.revalidate_candidate_usr(&self.installation, &candidate_usr);
        let active_revalidation = active_state.revalidate(&self.installation);
        match (result, revalidation, active_revalidation) {
            (Ok(()), Ok(()), Ok(())) => Ok(()),
            (Err(primary), _, _) => Err(primary),
            (Ok(()), Err(revalidation), _) => Err(revalidation),
            (Ok(()), Ok(()), Err(revalidation)) => Err(revalidation),
        }
    }

    fn require_configured_ephemeral_target(&self, requested: &Path) -> Result<PathBuf, Error> {
        let configured = match &self.scope {
            Scope::Ephemeral { destination } => destination.path().to_owned(),
            Scope::Stateful => return Err(Error::EphemeralProhibitedOperation),
            Scope::Frozen { .. } => return Err(Error::FrozenClientProhibitedOperation),
        };
        let requested = require_disjoint_materialization_target(&self.installation, requested)?;
        if requested != configured {
            return Err(Error::EphemeralDestinationMismatch { configured, requested });
        }
        Ok(requested)
    }

    fn apply_ephemeral_blit_under_guard(
        &self,
        fstree: vfs::Tree<PendingFile>,
        target: &mut RetainedExternalMaterializationTarget,
        candidate_usr: &candidate_metadata::RetainedEphemeralUsr,
        active_state: &active_state_snapshot::ActiveStateLease,
        system_snapshot: SystemModel,
    ) -> Result<(), Error> {
        target.revalidate_candidate_usr(&self.installation, candidate_usr)?;
        let root_abi = target.create_root_abi(&self.installation, candidate_usr)?;
        target.revalidate_candidate_usr(&self.installation, candidate_usr)?;
        let isolation_root_abi = create_root_links(&self.installation.isolation_dir())?;
        target.revalidate_candidate_usr(&self.installation, candidate_usr)?;
        let trigger_view = target.prepare_trigger_view(&self.installation, candidate_usr)?;
        trigger_view.revalidate(&self.installation)?;

        let metadata = candidate_metadata::decorate_ephemeral(candidate_usr, &system_snapshot)?;
        let revalidate = || -> Result<(), Error> {
            trigger_view.revalidate(&self.installation)?;
            metadata.revalidate()?;
            trigger_view.revalidate(&self.installation)?;
            root_abi.revalidate()?;
            isolation_root_abi.revalidate()?;
            active_state.revalidate(&self.installation)
        };
        revalidate()?;

        // ephemeral tx triggers
        before_ephemeral_transaction_triggers();
        let transaction = Self::apply_triggers(
            TriggerScope::RetainedEphemeral {
                phase: postblit::RetainedEphemeralPhase::Transaction,
                installation: &self.installation,
                isolation_root: &isolation_root_abi,
                view: trigger_view,
            },
            &fstree,
        );
        after_ephemeral_transaction_triggers();
        let transaction_revalidation = revalidate();
        match (transaction, transaction_revalidation) {
            (Ok(()), Ok(())) => {}
            (Err(primary), _) => return Err(primary.into()),
            (Ok(()), Err(revalidation)) => return Err(revalidation),
        }
        // ephemeral system triggers
        before_ephemeral_system_triggers();
        let system = Self::apply_triggers(
            TriggerScope::RetainedEphemeral {
                phase: postblit::RetainedEphemeralPhase::System,
                installation: &self.installation,
                isolation_root: &isolation_root_abi,
                view: trigger_view,
            },
            &fstree,
        );
        after_ephemeral_system_triggers();
        let system_revalidation = revalidate();
        match (system, system_revalidation) {
            (Ok(()), Ok(())) => {}
            (Err(primary), _) => return Err(primary.into()),
            (Ok(()), Err(revalidation)) => return Err(revalidation),
        }

        Ok(())
    }
}
