impl Client {
    fn rearchive_archived_candidate(
        &self,
        tree_identity: &StatefulTreeIdentity,
        state: state::Id,
    ) -> Result<(), Error> {
        let mut preparation_retried = false;
        loop {
            match tree_identity.rearchive_archived_candidate(&self.installation, state) {
                Ok(()) => return Ok(()),
                Err(failure) if failure.outcome() == RetainedArchivedCandidateMoveOutcome::Applied => {
                    return tree_identity
                        .finish_applied_archived_candidate_rearchive(&self.installation, state)
                        .map_err(Error::from);
                }
                Err(failure)
                    if failure.outcome() == RetainedArchivedCandidateMoveOutcome::RearchivePreparationApplied
                        && !preparation_retried =>
                {
                    // The exact prerequisite rename already applied. Retry
                    // once so its retained durability suffix can finish, but
                    // never spin on a persistent sync/revalidation failure.
                    preparation_retried = true;
                }
                Err(failure) => return Err(failure.into()),
            }
        }
    }

    /// Acquire the canonical journal lock, reject unresolved journal/database
    /// evidence, and establish permanent marker identities for the staged
    /// candidate and live previous tree. The returned guard retains all three
    /// capabilities through activation and compensating recovery.
    fn prepare_stateful_tree_identity(
        &self,
        candidate_usr: &Path,
        candidate_state: state::Id,
    ) -> Result<StatefulTreeIdentity, crate::transition_identity::Error> {
        StatefulTreeIdentity::prepare(&self.installation, &self.state_db, candidate_usr, candidate_state)
    }
}
