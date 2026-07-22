//! One-shot exact replacement and rollback of a retained boot-file leaf.
//!
//! A replacement first authenticates the installed canonical inode, creates
//! and fully synchronizes one new exact private inode, then issues exactly one
//! `RENAME_EXCHANGE`. After reconciliation the new inode is canonical and the
//! old inode remains at a deterministic receipt-fingerprint-bound sidecar.
//! There is no two-rename fallback. A raw error is accepted only when fresh
//! inode evidence proves the exchange applied.
//!
//! The returned non-cloneable value is the only in-process cleanup/rollback
//! authority. Process recovery can reconstruct it only through the explicit
//! exact-pair authenticator, which requires both content identities, both
//! inodes, the retained destination, and the deterministic owner fingerprint.
//! Ordinary replacement never adopts or overwrites an existing private name.
//!
//! Linux cannot condition `renameat2` or `unlinkat` on an inode observed just
//! before the syscall. These operations therefore require the same cooperative
//! writer boundary as immutable publication. Every observable substitution is
//! preserved and rejected; an uncooperative same-credential writer in the
//! final compare/syscall window remains outside this primitive's contract.

use std::{ffi::{CStr, CString}, fs::File, io, time::Instant};

use sha2::{Digest as _, Sha256};

use super::{
    boot_file_publication::{
        AttachmentIdentity, RetainedBootFilePublicationError,
        RetainedBootFilePublicationLimits, RetainedBootFilePublicationRequest,
        RetainedBootFilePublicationTarget,
        destination::{self, FileIdentity}, effect as publication_effect,
        validate_request as validate_publication_request,
    },
    boot_publication_parent::RetainedBootPublicationParent,
};
use crate::linux_fs::{
    RETAINED_BOOT_FILE_PRIVATE_PREFIX,
    descriptor_boot_namespace::{
        BootNamespaceRequest, BoundRetainedBootFileSource,
        RetainedBootNamespaceExpectedSource,
    },
    sync_filesystem_until,
};

#[path = "boot_file_replacement/effect.rs"]
mod effect;
#[path = "boot_file_replacement/error.rs"]
mod error;
#[path = "boot_file_replacement/model.rs"]
mod model;
#[path = "boot_file_replacement/recovery.rs"]
mod recovery;

pub(crate) use error::RetainedBootFileReplacementError;
pub(crate) use model::{
    AuthenticatedRetainedBootFileStaleCleanup, RetainedBootFileMutationFingerprint,
    RetainedBootFileAppliedSidecarCleanupState,
    RetainedBootFileRestoredSidecarCleanupState,
    RetainedBootFileReplacementRequest, RetainedBootFileStaleCleanupOutcome,
    RetainedBootFileStaleCleanupRequest, RetainedBootFileStaleCleanupState,
    RetainedBootFileSidecarCleanupOutcome, ValidatedRetainedBootFileReplacement,
    ValidatedRetainedBootFileRestoration,
};
use model::ExactContent;

#[cfg(test)]
pub(crate) use effect::{
    arm_boot_file_exchange_error_after_applied,
    arm_boot_file_replacement_stop_before_exchange,
    arm_boot_file_sidecar_stop_after_unlink,
    arm_stale_boot_file_detach_error_after_applied,
    arm_stale_boot_file_stop_after_detach,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PairState {
    Applied,
    Restored,
    Ambiguous,
}

impl RetainedBootPublicationParent<'_, '_> {
    /// Prepare and exchange one exact successor while retaining the installed
    /// inode at a private rollback sidecar.
    pub(crate) fn replace_exact_boot_file_until<'source>(
        &self,
        request: RetainedBootFileReplacementRequest<'_>,
        expected_replacement_source: &RetainedBootNamespaceExpectedSource<'source>,
        limits: RetainedBootFilePublicationLimits,
        deadline: Instant,
    ) -> Result<ValidatedRetainedBootFileReplacement, RetainedBootFileReplacementError> {
        checkpoint(deadline)?;
        let names = validate_request(request, limits, deadline)?;
        let destination = self.publication_parent_identity();
        let parent = open_parent(self, destination, deadline)?;
        require_absent(&parent, &names.sidecar, deadline)?;

        let (installed_file, installed_identity) = destination::open_and_verify(
            &parent,
            &names.canonical,
            request.installed(),
            destination,
            deadline,
        )
        .map_err(|source| publication("authenticating the installed boot file", source))?;

        let replacement_file = destination::create_private_exclusive(
            &parent,
            &names.sidecar,
            destination,
            deadline,
        )
        .map_err(|source| RetainedBootFileReplacementError::PrivateStageCreation { source })?;
        let source_request = BootNamespaceRequest::new(
            request.replacement().canonical_leaf(),
            request.replacement().expected_length(),
            request.replacement().expected_xxh3(),
        );
        let expected_sources = std::slice::from_ref(expected_replacement_source);
        let mut source = BoundRetainedBootFileSource::bind_until(
            source_request,
            expected_sources,
            limits.retained_namespace,
            deadline,
        )
        .map_err(|source| publication(
            "binding the exact boot-file replacement source",
            RetainedBootFilePublicationError::Source { source },
        ))?;
        publication_effect::stream_expected_source(
            &mut source,
            &replacement_file,
            request.replacement(),
            limits,
            deadline,
        )
        .map_err(|source| publication("streaming the exact boot-file replacement", source))?;
        let replacement_identity = destination::verify_open_file(
            &replacement_file,
            request.replacement(),
            destination,
            deadline,
        )
        .map_err(|source| publication("verifying the exact boot-file replacement", source))?;
        destination::require_named_identity(&parent, &names.sidecar, replacement_identity, deadline)
            .map_err(|source| publication("binding the private boot-file replacement", source))?;

        synchronize_files(&[&replacement_file], &parent, deadline)?;
        self.require_publication_parent_until("revalidating the replacement parent", deadline)
            .map_err(|source| publication("revalidating the replacement parent", source))?;
        require_pair(
            &parent,
            &names,
            request,
            destination,
            installed_identity,
            replacement_identity,
            PairState::Restored,
            deadline,
        )?;
        if effect::stop_before_exchange() {
            return Err(RetainedBootFileReplacementError::InjectedFault {
                point: "after-private-sync-before-forward-exchange",
            });
        }

        let exchange_result = effect::exchange_once(&parent, &names.canonical, &names.sidecar);
        reconcile_exchange(
            observe_pair(&parent, &names, installed_identity, replacement_identity, deadline)?,
            PairState::Applied,
            exchange_result,
        )?;
        require_pair(
            &parent,
            &names,
            request,
            destination,
            installed_identity,
            replacement_identity,
            PairState::Applied,
            deadline,
        )?;
        synchronize_files(&[&installed_file, &replacement_file], &parent, deadline)?;
        self.require_publication_parent_until("terminally revalidating the replacement parent", deadline)
            .map_err(|source| publication("terminally revalidating the replacement parent", source))?;
        require_pair(
            &parent,
            &names,
            request,
            destination,
            installed_identity,
            replacement_identity,
            PairState::Applied,
            deadline,
        )?;

        Ok(authority(
            request,
            destination,
            names,
            installed_identity,
            replacement_identity,
        ))
    }

    /// Reconstruct exact rollback/cleanup authority after process restart.
    pub(crate) fn authenticate_applied_boot_file_replacement_until(
        &self,
        request: RetainedBootFileReplacementRequest<'_>,
        limits: RetainedBootFilePublicationLimits,
        deadline: Instant,
    ) -> Result<ValidatedRetainedBootFileReplacement, RetainedBootFileReplacementError> {
        let names = validate_request(request, limits, deadline)?;
        let destination = self.publication_parent_identity();
        let parent = open_parent(self, destination, deadline)?;
        let replacement_identity = exact_identity(
            &parent,
            &names.canonical,
            request.replacement(),
            destination,
            "authenticating the applied boot-file successor",
            deadline,
        )?;
        let installed_identity = exact_identity(
            &parent,
            &names.sidecar,
            request.installed(),
            destination,
            "authenticating the installed rollback sidecar",
            deadline,
        )?;
        self.require_publication_parent_until("closing applied boot-file authentication", deadline)
            .map_err(|source| publication("closing applied boot-file authentication", source))?;
        require_pair(
            &parent,
            &names,
            request,
            destination,
            installed_identity,
            replacement_identity,
            PairState::Applied,
            deadline,
        )?;
        Ok(authority(
            request,
            destination,
            names,
            installed_identity,
            replacement_identity,
        ))
    }

    /// Borrow and freshly validate an existing applied replacement authority.
    ///
    /// This performs no namespace mutation and neither consumes nor recreates
    /// the authority. Both names, contents, and inode identities must still be
    /// the exact applied pair bound by the original authority.
    pub(crate) fn validate_applied_boot_file_replacement_until(
        &self,
        authority: &ValidatedRetainedBootFileReplacement,
        deadline: Instant,
    ) -> Result<(), RetainedBootFileReplacementError> {
        require_authority_destination(self, authority)?;
        let request = authority_request(authority);
        let names = authority_names(authority)?;
        let parent = open_parent(self, authority.destination, deadline)?;
        require_pair(
            &parent,
            &names,
            request,
            authority.destination,
            authority.installed_file,
            authority.replacement_file,
            PairState::Applied,
            deadline,
        )?;
        self.require_publication_parent_until(
            "terminally revalidating the applied boot-file replacement",
            deadline,
        )
        .map_err(|source| {
            publication(
                "terminally revalidating the applied boot-file replacement",
                source,
            )
        })?;
        require_pair(
            &parent,
            &names,
            request,
            authority.destination,
            authority.installed_file,
            authority.replacement_file,
            PairState::Applied,
            deadline,
        )
    }

    /// Restore the installed predecessor with exactly one reverse exchange.
    pub(crate) fn restore_exact_boot_file_replacement_until(
        &self,
        authority: ValidatedRetainedBootFileReplacement,
        deadline: Instant,
    ) -> Result<ValidatedRetainedBootFileRestoration, RetainedBootFileReplacementError> {
        require_authority_destination(self, &authority)?;
        let request = authority_request(&authority);
        let names = authority_names(&authority)?;
        let parent = open_parent(self, authority.destination, deadline)?;
        require_pair(
            &parent,
            &names,
            request,
            authority.destination,
            authority.installed_file,
            authority.replacement_file,
            PairState::Applied,
            deadline,
        )?;
        let exchange_result = effect::exchange_once(&parent, &names.canonical, &names.sidecar);
        reconcile_exchange(
            observe_pair(
                &parent,
                &names,
                authority.installed_file,
                authority.replacement_file,
                deadline,
            )?,
            PairState::Restored,
            exchange_result,
        )?;
        require_pair(
            &parent,
            &names,
            request,
            authority.destination,
            authority.installed_file,
            authority.replacement_file,
            PairState::Restored,
            deadline,
        )?;
        let canonical = open_exact(
            &parent,
            &names.canonical,
            request.installed(),
            authority.destination,
            "synchronizing the restored predecessor",
            deadline,
        )?;
        let sidecar = open_exact(
            &parent,
            &names.sidecar,
            request.replacement(),
            authority.destination,
            "synchronizing the displaced replacement",
            deadline,
        )?;
        synchronize_files(&[&canonical, &sidecar], &parent, deadline)?;
        self.require_publication_parent_until("terminally revalidating restored boot file", deadline)
            .map_err(|source| publication("terminally revalidating restored boot file", source))?;
        require_pair(
            &parent,
            &names,
            request,
            authority.destination,
            authority.installed_file,
            authority.replacement_file,
            PairState::Restored,
            deadline,
        )?;
        Ok(ValidatedRetainedBootFileRestoration { replacement: authority })
    }

    /// Remove the predecessor rollback sidecar after receipt promotion.
    pub(crate) fn cleanup_replaced_boot_file_sidecar_until(
        &self,
        authority: ValidatedRetainedBootFileReplacement,
        deadline: Instant,
    ) -> Result<RetainedBootFileSidecarCleanupOutcome, RetainedBootFileReplacementError> {
        self.cleanup_sidecar(authority, PairState::Applied, deadline)?;
        Ok(RetainedBootFileSidecarCleanupOutcome::RemovedInstalledRollback)
    }

    /// Remove the displaced successor sidecar after rollback restoration.
    pub(crate) fn cleanup_restored_boot_file_sidecar_until(
        &self,
        restoration: ValidatedRetainedBootFileRestoration,
        deadline: Instant,
    ) -> Result<RetainedBootFileSidecarCleanupOutcome, RetainedBootFileReplacementError> {
        self.cleanup_sidecar(restoration.replacement, PairState::Restored, deadline)?;
        Ok(RetainedBootFileSidecarCleanupOutcome::RemovedDisplacedReplacement)
    }

    /// Authenticate one exact predecessor-owned canonical leaf for removal.
    /// The fingerprint is naming/correlation input, not ownership proof; the
    /// caller must derive this request from an authenticated predecessor receipt.
    pub(crate) fn authenticate_stale_boot_file_cleanup_until(
        &self,
        request: RetainedBootFileStaleCleanupRequest<'_>,
        limits: RetainedBootFilePublicationLimits,
        deadline: Instant,
    ) -> Result<AuthenticatedRetainedBootFileStaleCleanup, RetainedBootFileReplacementError> {
        let canonical = validate_publication_request(request.stale(), limits, deadline)
            .map_err(|source| publication("validating stale boot-file cleanup request", source))?;
        let private_leaf = deterministic_stale_cleanup_leaf(request);
        let private = component(&private_leaf)?;
        let destination = self.publication_parent_identity();
        let parent = open_parent(self, destination, deadline)?;
        require_absent(&parent, &private, deadline)?;
        let file = exact_identity(
            &parent,
            &canonical,
            request.stale(),
            destination,
            "authenticating predecessor-owned stale boot file",
            deadline,
        )?;
        self.require_publication_parent_until("closing stale boot-file authentication", deadline)
            .map_err(|source| publication("closing stale boot-file authentication", source))?;
        let closing = exact_identity(
            &parent,
            &canonical,
            request.stale(),
            destination,
            "closing predecessor-owned stale boot-file authentication",
            deadline,
        )?;
        require_absent(&parent, &private, deadline)?;
        if closing != file {
            return Err(RetainedBootFileReplacementError::DetachAmbiguous);
        }
        Ok(AuthenticatedRetainedBootFileStaleCleanup {
            destination,
            canonical_leaf: request.stale().canonical_leaf().into(),
            private_leaf: private_leaf.into_boxed_str(),
            content: ExactContent::from_request(request.stale()),
            file,
            location: model::StaleFileLocation::Canonical,
        })
    }

    /// Detach and remove one freshly authenticated predecessor-only leaf.
    pub(crate) fn cleanup_authenticated_stale_boot_file_until(
        &self,
        authority: AuthenticatedRetainedBootFileStaleCleanup,
        deadline: Instant,
    ) -> Result<RetainedBootFileStaleCleanupOutcome, RetainedBootFileReplacementError> {
        if self.publication_parent_identity() != authority.destination {
            return Err(RetainedBootFileReplacementError::AuthorityDestinationMismatch);
        }
        let canonical = component(&authority.canonical_leaf)?;
        let private = component(&authority.private_leaf)?;
        let request = authority.content.request(&authority.canonical_leaf);
        let parent = open_parent(self, authority.destination, deadline)?;
        match authority.location {
            model::StaleFileLocation::Canonical => {
                require_absent(&parent, &private, deadline)?;
                let canonical_file = open_exact(
                    &parent,
                    &canonical,
                    request,
                    authority.destination,
                    "freshly authenticating stale boot file before detach",
                    deadline,
                )?;
                destination::require_named_identity(&parent, &canonical, authority.file, deadline)
                    .map_err(|source| publication("rebinding stale boot file before detach", source))?;
                synchronize_files(&[&canonical_file], &parent, deadline)?;
                let detach_result = effect::detach_once(&parent, &canonical, &private);
                reconcile_detach(
                    destination::observe_named_identity(&parent, &canonical, deadline)
                        .map_err(|source| publication("reconciling stale canonical detach", source))?,
                    destination::observe_named_identity(&parent, &private, deadline)
                        .map_err(|source| publication("reconciling stale private detach", source))?,
                    authority.file,
                    detach_result,
                )?;
            }
            model::StaleFileLocation::Detached => {
                require_absent(&parent, &canonical, deadline)?;
            }
        }
        let private_file = open_exact(
            &parent,
            &private,
            request,
            authority.destination,
            "authenticating detached stale boot file",
            deadline,
        )?;
        destination::require_named_identity(&parent, &private, authority.file, deadline)
            .map_err(|source| publication("rebinding detached stale boot file before unlink", source))?;
        synchronize_files(&[&private_file], &parent, deadline)?;
        if effect::stop_after_stale_detach() {
            return Err(RetainedBootFileReplacementError::InjectedFault {
                point: "after-stale-detach-before-unlink",
            });
        }

        let unlink_result = effect::unlink_once(&parent, &private);
        reconcile_unlink(
            destination::observe_named_identity(&parent, &private, deadline)
                .map_err(|source| publication("reconciling detached stale boot-file unlink", source))?,
            authority.file,
            unlink_result,
        )?;
        if effect::stop_after_sidecar_unlink() {
            return Err(RetainedBootFileReplacementError::InjectedFault {
                point: "after-sidecar-unlink-before-durability",
            });
        }
        synchronize_files(&[], &parent, deadline)?;
        self.require_publication_parent_until("terminally revalidating stale boot-file cleanup", deadline)
            .map_err(|source| publication("terminally revalidating stale boot-file cleanup", source))?;
        require_absent(&parent, &canonical, deadline)?;
        require_absent(&parent, &private, deadline)?;
        Ok(RetainedBootFileStaleCleanupOutcome::RemovedPredecessorOutput)
    }

    fn cleanup_sidecar(
        &self,
        authority: ValidatedRetainedBootFileReplacement,
        state: PairState,
        deadline: Instant,
    ) -> Result<(), RetainedBootFileReplacementError> {
        require_authority_destination(self, &authority)?;
        let request = authority_request(&authority);
        let names = authority_names(&authority)?;
        let parent = open_parent(self, authority.destination, deadline)?;
        require_pair(
            &parent,
            &names,
            request,
            authority.destination,
            authority.installed_file,
            authority.replacement_file,
            state,
            deadline,
        )?;
        let (canonical_request, canonical_identity, sidecar_request, sidecar_identity) = match state {
            PairState::Applied => (
                request.replacement(),
                authority.replacement_file,
                request.installed(),
                authority.installed_file,
            ),
            PairState::Restored => (
                request.installed(),
                authority.installed_file,
                request.replacement(),
                authority.replacement_file,
            ),
            PairState::Ambiguous => unreachable!("ambiguous state is never cleanup authority"),
        };
        let canonical = open_exact(
            &parent,
            &names.canonical,
            canonical_request,
            authority.destination,
            "authenticating canonical boot file before sidecar cleanup",
            deadline,
        )?;
        let sidecar = open_exact(
            &parent,
            &names.sidecar,
            sidecar_request,
            authority.destination,
            "authenticating exact boot-file sidecar before cleanup",
            deadline,
        )?;
        synchronize_files(&[&canonical, &sidecar], &parent, deadline)?;
        destination::require_named_identity(&parent, &names.sidecar, sidecar_identity, deadline)
            .map_err(|source| publication("rebinding boot-file sidecar before unlink", source))?;
        let unlink_result = effect::unlink_once(&parent, &names.sidecar);
        reconcile_unlink(
            destination::observe_named_identity(&parent, &names.sidecar, deadline)
                .map_err(|source| publication("reconciling boot-file sidecar unlink", source))?,
            sidecar_identity,
            unlink_result,
        )?;
        if effect::stop_after_sidecar_unlink() {
            return Err(RetainedBootFileReplacementError::InjectedFault {
                point: "after-sidecar-unlink-before-durability",
            });
        }
        synchronize_files(&[&canonical], &parent, deadline)?;
        self.require_publication_parent_until("terminally revalidating boot-file sidecar cleanup", deadline)
            .map_err(|source| publication("terminally revalidating boot-file sidecar cleanup", source))?;
        let found = exact_identity(
            &parent,
            &names.canonical,
            canonical_request,
            authority.destination,
            "terminally authenticating canonical boot file after cleanup",
            deadline,
        )?;
        if found != canonical_identity {
            return Err(RetainedBootFileReplacementError::ExchangeAmbiguous);
        }
        require_absent(&parent, &names.sidecar, deadline)
    }
}

struct Names {
    canonical: CString,
    sidecar: CString,
}

fn validate_request(
    request: RetainedBootFileReplacementRequest<'_>,
    limits: RetainedBootFilePublicationLimits,
    deadline: Instant,
) -> Result<Names, RetainedBootFileReplacementError> {
    checkpoint(deadline)?;
    let canonical = validate_publication_request(request.installed(), limits, deadline)
        .map_err(|source| publication("validating the installed boot-file identity", source))?;
    let replacement = validate_publication_request(request.replacement(), limits, deadline)
        .map_err(|source| publication("validating the replacement boot-file identity", source))?;
    if canonical != replacement {
        return Err(RetainedBootFileReplacementError::LeafMismatch);
    }
    if ExactContent::from_request(request.installed()) == ExactContent::from_request(request.replacement()) {
        return Err(RetainedBootFileReplacementError::IdenticalContent);
    }
    let sidecar = component(&deterministic_sidecar_leaf(request))?;
    Ok(Names { canonical, sidecar })
}

fn deterministic_sidecar_leaf(request: RetainedBootFileReplacementRequest<'_>) -> String {
    let mut digest = Sha256::new();
    digest.update(b"cast-retained-boot-file-replacement-v1\0");
    digest.update(request.owner().as_bytes());
    digest.update((request.installed().canonical_leaf().len() as u64).to_le_bytes());
    digest.update(request.installed().canonical_leaf().as_bytes());
    for content in [request.installed(), request.replacement()] {
        digest.update(content.expected_length().to_le_bytes());
        digest.update(content.expected_xxh3().to_le_bytes());
        digest.update(content.expected_sha256());
    }
    format!("{RETAINED_BOOT_FILE_PRIVATE_PREFIX}{}.replace", hex::encode(digest.finalize()))
}

fn deterministic_stale_cleanup_leaf(request: RetainedBootFileStaleCleanupRequest<'_>) -> String {
    let mut digest = Sha256::new();
    digest.update(b"cast-retained-boot-file-stale-cleanup-v1\0");
    digest.update(request.owner().as_bytes());
    digest.update((request.stale().canonical_leaf().len() as u64).to_le_bytes());
    digest.update(request.stale().canonical_leaf().as_bytes());
    digest.update(request.stale().expected_length().to_le_bytes());
    digest.update(request.stale().expected_xxh3().to_le_bytes());
    digest.update(request.stale().expected_sha256());
    format!("{RETAINED_BOOT_FILE_PRIVATE_PREFIX}{}.stale", hex::encode(digest.finalize()))
}

fn authority(
    request: RetainedBootFileReplacementRequest<'_>,
    destination: AttachmentIdentity,
    names: Names,
    installed_file: FileIdentity,
    replacement_file: FileIdentity,
) -> ValidatedRetainedBootFileReplacement {
    ValidatedRetainedBootFileReplacement {
        destination,
        canonical_leaf: request.installed().canonical_leaf().into(),
        sidecar_leaf: names.sidecar.to_string_lossy().into_owned().into_boxed_str(),
        installed: ExactContent::from_request(request.installed()),
        replacement: ExactContent::from_request(request.replacement()),
        installed_file,
        replacement_file,
        owner: request.owner(),
    }
}

fn authority_request(authority: &ValidatedRetainedBootFileReplacement) -> RetainedBootFileReplacementRequest<'_> {
    RetainedBootFileReplacementRequest::new(
        authority.installed.request(&authority.canonical_leaf),
        authority.replacement.request(&authority.canonical_leaf),
        authority.owner,
    )
}

fn authority_names(authority: &ValidatedRetainedBootFileReplacement) -> Result<Names, RetainedBootFileReplacementError> {
    Ok(Names {
        canonical: component(&authority.canonical_leaf)?,
        sidecar: component(&authority.sidecar_leaf)?,
    })
}

fn require_authority_destination(
    parent: &RetainedBootPublicationParent<'_, '_>,
    authority: &ValidatedRetainedBootFileReplacement,
) -> Result<(), RetainedBootFileReplacementError> {
    let found = parent.publication_parent_identity();
    if found != authority.destination {
        Err(RetainedBootFileReplacementError::AuthorityDestinationMismatch)
    } else {
        Ok(())
    }
}

fn open_parent(
    target: &impl RetainedBootFilePublicationTarget,
    destination: AttachmentIdentity,
    deadline: Instant,
) -> Result<File, RetainedBootFileReplacementError> {
    target.require_publication_parent_until("opening exact boot-file replacement", deadline)
        .map_err(|source| publication("opening exact boot-file replacement", source))?;
    destination::open_parent_io(target.publication_parent(), destination, deadline)
        .map_err(|source| publication("opening replacement parent alias", source))
}

#[allow(clippy::too_many_arguments)]
fn require_pair(
    parent: &File,
    names: &Names,
    request: RetainedBootFileReplacementRequest<'_>,
    destination: AttachmentIdentity,
    installed: FileIdentity,
    replacement: FileIdentity,
    state: PairState,
    deadline: Instant,
) -> Result<(), RetainedBootFileReplacementError> {
    let (canonical_request, canonical_identity, sidecar_request, sidecar_identity) = match state {
        PairState::Applied => (request.replacement(), replacement, request.installed(), installed),
        PairState::Restored => (request.installed(), installed, request.replacement(), replacement),
        PairState::Ambiguous => return Err(RetainedBootFileReplacementError::ExchangeAmbiguous),
    };
    let found_canonical = exact_identity(
        parent,
        &names.canonical,
        canonical_request,
        destination,
        "authenticating exact canonical replacement pair",
        deadline,
    )?;
    let found_sidecar = exact_identity(
        parent,
        &names.sidecar,
        sidecar_request,
        destination,
        "authenticating exact private replacement pair",
        deadline,
    )?;
    if found_canonical != canonical_identity || found_sidecar != sidecar_identity {
        return Err(RetainedBootFileReplacementError::ExchangeAmbiguous);
    }
    Ok(())
}

fn observe_pair(
    parent: &File,
    names: &Names,
    installed: FileIdentity,
    replacement: FileIdentity,
    deadline: Instant,
) -> Result<PairState, RetainedBootFileReplacementError> {
    let canonical = destination::observe_named_identity(parent, &names.canonical, deadline)
        .map_err(|source| publication("observing canonical exchange result", source))?;
    let sidecar = destination::observe_named_identity(parent, &names.sidecar, deadline)
        .map_err(|source| publication("observing private exchange result", source))?;
    Ok(match (canonical, sidecar) {
        (Some(canonical), Some(sidecar)) if canonical == replacement && sidecar == installed => PairState::Applied,
        (Some(canonical), Some(sidecar)) if canonical == installed && sidecar == replacement => PairState::Restored,
        _ => PairState::Ambiguous,
    })
}

fn reconcile_exchange(
    found: PairState,
    expected: PairState,
    result: io::Result<()>,
) -> Result<(), RetainedBootFileReplacementError> {
    match (found, result) {
        (found, _) if found == expected => Ok(()),
        (PairState::Ambiguous, _) => Err(RetainedBootFileReplacementError::ExchangeAmbiguous),
        (_, Ok(())) => Err(RetainedBootFileReplacementError::ExchangeSuccessUnreconciled),
        (_, Err(source)) => Err(RetainedBootFileReplacementError::ExchangeNotApplied { source }),
    }
}

fn reconcile_unlink(
    found: Option<FileIdentity>,
    expected: FileIdentity,
    result: io::Result<()>,
) -> Result<(), RetainedBootFileReplacementError> {
    match (found, result) {
        (None, _) => Ok(()),
        (Some(found), Err(source)) if found == expected => {
            Err(RetainedBootFileReplacementError::UnlinkNotApplied { source })
        }
        (Some(found), Ok(())) if found == expected => {
            Err(RetainedBootFileReplacementError::UnlinkSuccessUnreconciled)
        }
        _ => Err(RetainedBootFileReplacementError::UnlinkAmbiguous),
    }
}

fn reconcile_detach(
    canonical: Option<FileIdentity>,
    private: Option<FileIdentity>,
    expected: FileIdentity,
    result: io::Result<()>,
) -> Result<(), RetainedBootFileReplacementError> {
    match ((canonical, private), result) {
        ((None, Some(found)), _) if found == expected => Ok(()),
        ((Some(found), None), Err(source)) if found == expected => {
            Err(RetainedBootFileReplacementError::DetachNotApplied { source })
        }
        ((Some(found), None), Ok(())) if found == expected => {
            Err(RetainedBootFileReplacementError::DetachSuccessUnreconciled)
        }
        _ => Err(RetainedBootFileReplacementError::DetachAmbiguous),
    }
}

fn exact_identity(
    parent: &File,
    name: &CStr,
    request: RetainedBootFilePublicationRequest<'_>,
    destination: AttachmentIdentity,
    action: &'static str,
    deadline: Instant,
) -> Result<FileIdentity, RetainedBootFileReplacementError> {
    destination::open_and_verify(parent, name, request, destination, deadline)
        .map(|(_, identity)| identity)
        .map_err(|source| publication(action, source))
}

fn open_exact(
    parent: &File,
    name: &CStr,
    request: RetainedBootFilePublicationRequest<'_>,
    destination: AttachmentIdentity,
    action: &'static str,
    deadline: Instant,
) -> Result<File, RetainedBootFileReplacementError> {
    destination::open_and_verify(parent, name, request, destination, deadline)
        .map(|(file, _)| file)
        .map_err(|source| publication(action, source))
}

fn require_absent(
    parent: &File,
    name: &CStr,
    deadline: Instant,
) -> Result<(), RetainedBootFileReplacementError> {
    match destination::observe_named_identity(parent, name, deadline) {
        Ok(None) => Ok(()),
        Ok(Some(_)) => Err(RetainedBootFileReplacementError::PrivateSidecarOccupied),
        Err(source) => Err(publication("requiring private boot-file sidecar absence", source)),
    }
}

fn synchronize_files(
    files: &[&File],
    parent: &File,
    deadline: Instant,
) -> Result<(), RetainedBootFileReplacementError> {
    for file in files {
        checkpoint(deadline)?;
        file.sync_all().map_err(|source| RetainedBootFileReplacementError::Filesystem {
            action: "synchronizing an exact boot-file replacement inode",
            source,
        })?;
    }
    checkpoint(deadline)?;
    parent.sync_all().map_err(|source| RetainedBootFileReplacementError::Filesystem {
        action: "synchronizing the retained boot-file replacement parent",
        source,
    })?;
    sync_filesystem_until(parent, deadline).map_err(|source| RetainedBootFileReplacementError::Filesystem {
        action: "synchronizing the retained boot filesystem after replacement",
        source,
    })?;
    checkpoint(deadline)
}

fn component(leaf: &str) -> Result<CString, RetainedBootFileReplacementError> {
    let bytes = leaf.as_bytes();
    if bytes.is_empty()
        || bytes.len() > 255
        || matches!(bytes, b"." | b"..")
        || !bytes.iter().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'+' | b'-'))
    {
        return Err(publication(
            "validating one boot-file replacement component",
            RetainedBootFilePublicationError::InvalidCanonicalLeaf,
        ));
    }
    CString::new(bytes).map_err(|_| publication(
        "validating one boot-file replacement component",
        RetainedBootFilePublicationError::InvalidCanonicalLeaf,
    ))
}

fn checkpoint(deadline: Instant) -> Result<(), RetainedBootFileReplacementError> {
    if Instant::now() > deadline {
        Err(RetainedBootFileReplacementError::DeadlineExceeded { deadline })
    } else {
        Ok(())
    }
}

fn publication(
    action: &'static str,
    source: RetainedBootFilePublicationError,
) -> RetainedBootFileReplacementError {
    RetainedBootFileReplacementError::Publication { action, source }
}
