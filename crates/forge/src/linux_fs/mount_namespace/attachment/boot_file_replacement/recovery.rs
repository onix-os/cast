//! Restart-safe classification of deterministic replacement cleanup prefixes.

use super::*;
use super::model::{
    RetainedBootFileAppliedSidecarCleanupState,
    RetainedBootFileRestoredSidecarCleanupState, RetainedBootFileStaleCleanupState,
    StaleFileLocation,
};

impl RetainedBootPublicationParent<'_, '_> {
    /// Authenticate an exact unexchanged stage or restored rollback pair.
    pub(crate) fn authenticate_restored_boot_file_replacement_until(
        &self,
        request: RetainedBootFileReplacementRequest<'_>,
        limits: RetainedBootFilePublicationLimits,
        deadline: Instant,
    ) -> Result<ValidatedRetainedBootFileRestoration, RetainedBootFileReplacementError> {
        match self.reconcile_restored_boot_file_sidecar_cleanup_until(request, limits, deadline)? {
            RetainedBootFileRestoredSidecarCleanupState::Pending(authority) => Ok(authority),
            RetainedBootFileRestoredSidecarCleanupState::AlreadyClean => {
                Err(RetainedBootFileReplacementError::CleanupStateMismatch)
            }
        }
    }

    /// Reconcile post-promotion cleanup without treating sidecar absence as
    /// sufficient: the canonical successor is authenticated and synchronized.
    pub(crate) fn reconcile_replaced_boot_file_sidecar_cleanup_until(
        &self,
        request: RetainedBootFileReplacementRequest<'_>,
        limits: RetainedBootFilePublicationLimits,
        deadline: Instant,
    ) -> Result<RetainedBootFileAppliedSidecarCleanupState, RetainedBootFileReplacementError> {
        reconcile_sidecar(self, request, limits, PairState::Applied, deadline).map(|state| match state {
            ReconciledSidecar::Pending(authority) => {
                RetainedBootFileAppliedSidecarCleanupState::Pending(authority)
            }
            ReconciledSidecar::AlreadyClean => RetainedBootFileAppliedSidecarCleanupState::AlreadyClean,
        })
    }

    /// Reconcile cleanup after rollback or a crash before the forward exchange.
    pub(crate) fn reconcile_restored_boot_file_sidecar_cleanup_until(
        &self,
        request: RetainedBootFileReplacementRequest<'_>,
        limits: RetainedBootFilePublicationLimits,
        deadline: Instant,
    ) -> Result<RetainedBootFileRestoredSidecarCleanupState, RetainedBootFileReplacementError> {
        reconcile_sidecar(self, request, limits, PairState::Restored, deadline).map(|state| match state {
            ReconciledSidecar::Pending(authority) => {
                RetainedBootFileRestoredSidecarCleanupState::Pending(
                    ValidatedRetainedBootFileRestoration { replacement: authority },
                )
            }
            ReconciledSidecar::AlreadyClean => RetainedBootFileRestoredSidecarCleanupState::AlreadyClean,
        })
    }

    /// Classify the only three admitted stale-output cleanup states.
    pub(crate) fn reconcile_stale_boot_file_cleanup_until(
        &self,
        request: RetainedBootFileStaleCleanupRequest<'_>,
        limits: RetainedBootFilePublicationLimits,
        deadline: Instant,
    ) -> Result<RetainedBootFileStaleCleanupState, RetainedBootFileReplacementError> {
        let canonical = validate_publication_request(request.stale(), limits, deadline)
            .map_err(|source| publication("validating stale boot-file recovery request", source))?;
        let private_leaf = deterministic_stale_cleanup_leaf(request);
        let private = component(&private_leaf)?;
        let destination = self.publication_parent_identity();
        let parent = open_parent(self, destination, deadline)?;
        let canonical_observed = destination::observe_named_identity(&parent, &canonical, deadline)
            .map_err(|source| publication("observing stale canonical recovery name", source))?;
        let private_observed = destination::observe_named_identity(&parent, &private, deadline)
            .map_err(|source| publication("observing stale private recovery name", source))?;

        match (canonical_observed, private_observed) {
            (Some(_), None) => {
                let file = exact_identity(
                    &parent,
                    &canonical,
                    request.stale(),
                    destination,
                    "authenticating canonical stale cleanup recovery",
                    deadline,
                )?;
                self.require_publication_parent_until("closing canonical stale cleanup recovery", deadline)
                    .map_err(|source| publication("closing canonical stale cleanup recovery", source))?;
                let closing = exact_identity(
                    &parent,
                    &canonical,
                    request.stale(),
                    destination,
                    "closing canonical stale cleanup recovery",
                    deadline,
                )?;
                require_absent(&parent, &private, deadline)?;
                if closing != file {
                    return Err(RetainedBootFileReplacementError::DetachAmbiguous);
                }
                Ok(RetainedBootFileStaleCleanupState::Canonical(stale_authority(
                    request,
                    destination,
                    private_leaf,
                    file,
                    StaleFileLocation::Canonical,
                )))
            }
            (None, Some(_)) => {
                let file = exact_identity(
                    &parent,
                    &private,
                    request.stale(),
                    destination,
                    "authenticating detached stale cleanup recovery",
                    deadline,
                )?;
                synchronize_files(&[
                    &open_exact(
                        &parent,
                        &private,
                        request.stale(),
                        destination,
                        "synchronizing detached stale cleanup recovery",
                        deadline,
                    )?,
                ], &parent, deadline)?;
                self.require_publication_parent_until("closing detached stale cleanup recovery", deadline)
                    .map_err(|source| publication("closing detached stale cleanup recovery", source))?;
                require_absent(&parent, &canonical, deadline)?;
                let closing = exact_identity(
                    &parent,
                    &private,
                    request.stale(),
                    destination,
                    "closing detached stale cleanup recovery",
                    deadline,
                )?;
                if closing != file {
                    return Err(RetainedBootFileReplacementError::DetachAmbiguous);
                }
                Ok(RetainedBootFileStaleCleanupState::Detached(stale_authority(
                    request,
                    destination,
                    private_leaf,
                    file,
                    StaleFileLocation::Detached,
                )))
            }
            (None, None) => {
                synchronize_files(&[], &parent, deadline)?;
                self.require_publication_parent_until("closing already-clean stale cleanup recovery", deadline)
                    .map_err(|source| publication("closing already-clean stale cleanup recovery", source))?;
                require_absent(&parent, &canonical, deadline)?;
                require_absent(&parent, &private, deadline)?;
                Ok(RetainedBootFileStaleCleanupState::AlreadyClean)
            }
            (Some(_), Some(_)) => Err(RetainedBootFileReplacementError::CleanupStateMismatch),
        }
    }
}
enum ReconciledSidecar {
    Pending(ValidatedRetainedBootFileReplacement),
    AlreadyClean,
}

fn reconcile_sidecar(
    target: &RetainedBootPublicationParent<'_, '_>,
    request: RetainedBootFileReplacementRequest<'_>,
    limits: RetainedBootFilePublicationLimits,
    state: PairState,
    deadline: Instant,
) -> Result<ReconciledSidecar, RetainedBootFileReplacementError> {
    let names = validate_request(request, limits, deadline)?;
    let destination = target.publication_parent_identity();
    let parent = open_parent(target, destination, deadline)?;
    let (canonical_request, sidecar_request) = match state {
        PairState::Applied => (request.replacement(), request.installed()),
        PairState::Restored => (request.installed(), request.replacement()),
        PairState::Ambiguous => return Err(RetainedBootFileReplacementError::CleanupStateMismatch),
    };
    let canonical = open_exact(
        &parent,
        &names.canonical,
        canonical_request,
        destination,
        "authenticating canonical boot file during cleanup recovery",
        deadline,
    )?;
    let canonical_identity = destination::verify_open_file(&canonical, canonical_request, destination, deadline)
        .map_err(|source| publication("binding canonical boot file during cleanup recovery", source))?;
    match destination::observe_named_identity(&parent, &names.sidecar, deadline)
        .map_err(|source| publication("observing replacement sidecar during cleanup recovery", source))?
    {
        None => {
            synchronize_files(&[&canonical], &parent, deadline)?;
            target.require_publication_parent_until("closing already-clean replacement sidecar recovery", deadline)
                .map_err(|source| publication("closing already-clean replacement sidecar recovery", source))?;
            let closing = exact_identity(
                &parent,
                &names.canonical,
                canonical_request,
                destination,
                "closing already-clean replacement sidecar recovery",
                deadline,
            )?;
            require_absent(&parent, &names.sidecar, deadline)?;
            if closing != canonical_identity {
                return Err(RetainedBootFileReplacementError::ExchangeAmbiguous);
            }
            Ok(ReconciledSidecar::AlreadyClean)
        }
        Some(_) => {
            let sidecar_identity = exact_identity(
                &parent,
                &names.sidecar,
                sidecar_request,
                destination,
                "authenticating replacement sidecar during cleanup recovery",
                deadline,
            )?;
            let (installed_file, replacement_file) = match state {
                PairState::Applied => (sidecar_identity, canonical_identity),
                PairState::Restored => (canonical_identity, sidecar_identity),
                PairState::Ambiguous => unreachable!(),
            };
            target.require_publication_parent_until("closing pending replacement sidecar recovery", deadline)
                .map_err(|source| publication("closing pending replacement sidecar recovery", source))?;
            require_pair(
                &parent,
                &names,
                request,
                destination,
                installed_file,
                replacement_file,
                state,
                deadline,
            )?;
            Ok(ReconciledSidecar::Pending(authority(
                request,
                destination,
                names,
                installed_file,
                replacement_file,
            )))
        }
    }
}

fn stale_authority(
    request: RetainedBootFileStaleCleanupRequest<'_>,
    destination: AttachmentIdentity,
    private_leaf: String,
    file: FileIdentity,
    location: StaleFileLocation,
) -> AuthenticatedRetainedBootFileStaleCleanup {
    AuthenticatedRetainedBootFileStaleCleanup {
        destination,
        canonical_leaf: request.stale().canonical_leaf().into(),
        private_leaf: private_leaf.into_boxed_str(),
        content: ExactContent::from_request(request.stale()),
        file,
        location,
    }
}
