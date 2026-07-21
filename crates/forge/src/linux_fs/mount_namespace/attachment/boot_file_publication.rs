//! Descriptor-retained immutable publication of one boot payload leaf.
//!
//! The callable production surfaces are inherent operations on either a
//! freshly revalidated task-root attachment or its non-cloneable retained
//! descendant-parent chain. Callers cannot mint mutation authority from
//! device, inode, or mount-ID scalars: the destination descriptor remains
//! private for validation, classification, publication, reconciliation, and
//! durability.
//!
//! This is deliberately one leaf below one already-existing parent. It does
//! not create directory trees, replace or delete entries, publish entries or
//! bootloaders, promote receipts, or wire the legacy synchronizer.
//!
//! This slice is intentionally a **non-crash-recoverable foundation**. Process
//! death after exclusive creation, during streaming, or after the final write
//! but before source validation preserves an empty, partial, or complete
//! mode-0644 deterministic residue. Any non-exact residue—normally empty or
//! partial for a nonempty payload—is preserved and makes a retry fail closed. A
//! completely written exact residue, including the empty inode of a zero-length
//! request, may resume under the explicit cooperative reserved-namespace
//! assumption while retaining the same inode. Exact-byte resumability is not
//! receipt-bound attempt ownership. The later current-plan receipt/journal
//! layer must own that decision, so this operation makes no standalone reboot,
//! power-loss, or crash-recovery claim.
//!
//! Mode 0644 is the inode's sole admitted mode for its entire observable
//! lifetime. Creation requests 0644 and immediately verifies the effective
//! mode. Filesystems such as VFAT mounted with `fmask=0133` already expose 0644
//! and receive no chmod. On an ordinary Unix filesystem where the process umask
//! removed bits, one narrowly contained pre-stream `fchmod(0644)` may normalize
//! the empty inode, followed by exact verification. Streaming never changes
//! mode.
//!
//! The parent namespace must also be cooperative with respect to writers using
//! the same credentials. Linux `renameat2` cannot bind its source name to the
//! inode authenticated immediately beforehand. A same-credential writer can
//! substitute the private name in that final window. Reconciliation refuses
//! validated evidence, but the one no-replace rename may already have placed
//! that foreign inode at the previously absent canonical name, poisoning it for
//! later attempts. Higher-level ownership or serialization must exclude such a
//! writer before this foundation can support a complete publication protocol.

use std::{ffi::CString, fs::File, io, time::Instant};

use sha2::{Digest as _, Sha256};

use super::{
    PRODUCTION_LIMITS, RevalidatedTaskRootedAttachment,
    capture::require_capture_matches,
};
use super::super::filesystem::Operation;
use crate::linux_fs::{
    descriptor_boot_namespace::{
        BootNamespaceDestinationState, BootNamespaceRequest, BoundRetainedBootFileSource,
        RetainedBootNamespaceExpectedSource, assess_retained_boot_namespace_until,
    },
    RETAINED_BOOT_FILE_PRIVATE_PREFIX, is_retained_boot_file_private_component, renameat2_noreplace_once,
    sync_filesystem_until,
};

#[path = "boot_file_publication/destination.rs"]
mod destination;
#[path = "boot_file_publication/effect.rs"]
mod effect;
#[path = "boot_file_publication/error.rs"]
mod error;
#[path = "boot_file_publication/model.rs"]
mod model;

use destination::{FileIdentity, ReconciledMove};
use effect::{checkpoint, fault};
pub(crate) use effect::FixtureRetainedBootFilePublicationFault;
pub(crate) use error::RetainedBootFilePublicationError;
pub(crate) use model::{
    RetainedBootFilePublicationLimits, RetainedBootFilePublicationOutcome, RetainedBootFilePublicationRequest,
    ValidatedRetainedBootFilePublication,
};

#[cfg(test)]
pub(crate) use effect::{
    arm_retained_boot_file_private_name_substitution, arm_retained_boot_file_publication_fault,
};
#[derive(Clone, Copy)]
pub(super) struct AttachmentIdentity {
    pub(super) device: u64,
    pub(super) inode: u64,
    pub(super) mount_id: u64,
}

/// Private capability boundary shared by the authenticated attachment root
/// and a descriptor-retained descendant parent. Implementors never expose the
/// retained directory to callers; the leaf engine alone borrows it after a
/// fresh full-chain revalidation.
pub(super) trait RetainedBootFilePublicationTarget {
    fn publication_parent(&self) -> &File;
    fn publication_parent_identity(&self) -> AttachmentIdentity;
    fn require_publication_parent_until(
        &self,
        action: &'static str,
        deadline: Instant,
    ) -> Result<(), RetainedBootFilePublicationError>;
}

impl RevalidatedTaskRootedAttachment<'_> {
    /// Publish or durably revalidate one immutable payload below this exact
    /// already-existing attachment directory.
    ///
    /// The source is borrowed through a one-element closed slice so its sealed
    /// descriptor, when present, can be reused by aggregate preflight and final
    /// assessment without ever being copied or exposed.
    /// `renameat2(RENAME_NOREPLACE)` is attempted at most once, and every result
    /// is reconciled before interpretation.
    pub(crate) fn publish_immutable_boot_file_until<'source>(
        &self,
        request: RetainedBootFilePublicationRequest<'_>,
        expected_source: &RetainedBootNamespaceExpectedSource<'source>,
        limits: RetainedBootFilePublicationLimits,
        deadline: Instant,
    ) -> Result<ValidatedRetainedBootFilePublication, RetainedBootFilePublicationError> {
        publish_immutable_boot_file_from_target_until(self, request, expected_source, limits, deadline)
    }
}

pub(super) fn publish_immutable_boot_file_from_target_until<'source>(
    target: &impl RetainedBootFilePublicationTarget,
    request: RetainedBootFilePublicationRequest<'_>,
    expected_source: &RetainedBootNamespaceExpectedSource<'source>,
    limits: RetainedBootFilePublicationLimits,
    deadline: Instant,
) -> Result<ValidatedRetainedBootFilePublication, RetainedBootFilePublicationError> {
    let canonical_name = validate_request(request, limits, deadline)?;
    let private_leaf = deterministic_private_leaf(request);
    let private_name = component(&private_leaf)?;
    let expected_sources = std::slice::from_ref(expected_source);
    let attachment = target.publication_parent_identity();

    target.require_publication_parent_until("opening boot-file publication", deadline)?;
    let retained_parent = target.publication_parent();
    let parent = destination::open_parent_io(retained_parent, attachment, deadline)?;
    let publisher = RetainedBootFilePublisher { target };
    let initial = publisher.classify_leaf(
        request.canonical_leaf(),
        request,
        expected_sources,
        limits,
        "classifying the canonical boot-file leaf",
        deadline,
    )?;

    match initial {
        BootNamespaceDestinationState::Exact => publisher.finish_existing(
            &parent,
            &canonical_name,
            &private_leaf,
            &private_name,
            request,
            expected_sources,
            limits,
            attachment,
            deadline,
        ),
        BootNamespaceDestinationState::Different => {
            Err(RetainedBootFilePublicationError::DifferentCanonicalDestination)
        }
        BootNamespaceDestinationState::Absent => publisher.publish_absent(
            &parent,
            &canonical_name,
            &private_leaf,
            &private_name,
            request,
            expected_sources,
            limits,
            attachment,
            deadline,
        ),
    }
}

struct RetainedBootFilePublisher<'target, Target: ?Sized> {
    target: &'target Target,
}

impl<Target: RetainedBootFilePublicationTarget + ?Sized> RetainedBootFilePublisher<'_, Target> {
    #[allow(clippy::too_many_arguments)]
    fn finish_existing(
        &self,
        parent: &File,
        canonical_name: &CString,
        private_leaf: &str,
        private_name: &CString,
        request: RetainedBootFilePublicationRequest<'_>,
        expected_sources: &[RetainedBootNamespaceExpectedSource<'_>],
        limits: RetainedBootFilePublicationLimits,
        attachment: AttachmentIdentity,
        deadline: Instant,
    ) -> Result<ValidatedRetainedBootFilePublication, RetainedBootFilePublicationError> {
        let private = self.classify_leaf(
            private_leaf,
            request,
            expected_sources,
            limits,
            "classifying private residue beside an exact canonical leaf",
            deadline,
        )?;
        if private != BootNamespaceDestinationState::Absent {
            return Err(RetainedBootFilePublicationError::ResidueBesideExactDestination);
        }
        let (canonical, identity) =
            destination::open_and_verify(parent, canonical_name, request, attachment, deadline)?;
        self.durability_suffix(&canonical, parent, deadline)?;
        self.terminally_revalidate(
            parent,
            canonical_name,
            private_leaf,
            private_name,
            request,
            expected_sources,
            limits,
            attachment,
            identity,
            deadline,
        )?;
        Ok(validated_result(
            RetainedBootFilePublicationOutcome::AlreadyExact,
            attachment,
            identity,
            request,
        ))
    }

    #[allow(clippy::too_many_arguments)]
    fn publish_absent(
        &self,
        parent: &File,
        canonical_name: &CString,
        private_leaf: &str,
        private_name: &CString,
        request: RetainedBootFilePublicationRequest<'_>,
        expected_sources: &[RetainedBootNamespaceExpectedSource<'_>],
        limits: RetainedBootFilePublicationLimits,
        attachment: AttachmentIdentity,
        deadline: Instant,
    ) -> Result<ValidatedRetainedBootFilePublication, RetainedBootFilePublicationError> {
        let private_state = self.classify_leaf(
            private_leaf,
            request,
            expected_sources,
            limits,
            "classifying deterministic private boot-file residue",
            deadline,
        )?;
        let (private, private_identity) = match private_state {
            BootNamespaceDestinationState::Different => {
                return Err(RetainedBootFilePublicationError::DifferentPrivateResidue);
            }
            BootNamespaceDestinationState::Exact => {
                destination::open_and_verify(parent, private_name, request, attachment, deadline)?
            }
            BootNamespaceDestinationState::Absent => {
                match destination::create_private_exclusive(parent, private_name, attachment, deadline) {
                    Ok(private) => {
                        fault(FixtureRetainedBootFilePublicationFault::AfterExclusiveCreation)?;
                        let source_request = BootNamespaceRequest::new(
                            request.canonical_leaf(),
                            request.expected_length(),
                            request.expected_xxh3(),
                        );
                        let mut source = BoundRetainedBootFileSource::bind_until(
                            source_request,
                            expected_sources,
                            limits.retained_namespace,
                            deadline,
                        )
                        .map_err(|source| RetainedBootFilePublicationError::Source { source })?;
                        effect::stream_expected_source(&mut source, &private, request, limits, deadline)?;
                        let identity = destination::verify_open_file(&private, request, attachment, deadline)?;
                        destination::require_named_identity(parent, private_name, identity, deadline)?;
                        (private, identity)
                    }
                    Err(create_error) => {
                        let reconciled = self.classify_leaf(
                            private_leaf,
                            request,
                            expected_sources,
                            limits,
                            "reconciling exclusive private boot-file creation",
                            deadline,
                        )?;
                        match reconciled {
                            BootNamespaceDestinationState::Exact => {
                                destination::open_and_verify(parent, private_name, request, attachment, deadline)?
                            }
                            BootNamespaceDestinationState::Different => {
                                return Err(RetainedBootFilePublicationError::DifferentPrivateResidue);
                            }
                            BootNamespaceDestinationState::Absent => {
                                return Err(RetainedBootFilePublicationError::PrivateCreationUnreconciled {
                                    source: create_error,
                                });
                            }
                        }
                    }
                }
            }
        };

        fault(FixtureRetainedBootFilePublicationFault::BeforePrivateSync)?;
        checkpoint(deadline)?;
        private
            .sync_all()
            .map_err(|source| RetainedBootFilePublicationError::Filesystem {
                action: "synchronizing the exact private boot-file leaf",
                source,
            })?;
        checkpoint(deadline)?;

        self.require_publication_attachment_until("revalidating the attachment before publication", deadline)?;
        require_state(
            self.classify_leaf(
                request.canonical_leaf(),
                request,
                expected_sources,
                limits,
                "revalidating canonical absence before publication",
                deadline,
            )?,
            BootNamespaceDestinationState::Absent,
            RetainedBootFilePublicationError::DifferentCanonicalDestination,
        )?;
        require_state(
            self.classify_leaf(
                private_leaf,
                request,
                expected_sources,
                limits,
                "revalidating exact private residue before publication",
                deadline,
            )?,
            BootNamespaceDestinationState::Exact,
            RetainedBootFilePublicationError::DifferentPrivateResidue,
        )?;
        let (_pre_move_private, rebound_identity) =
            destination::open_and_verify(parent, private_name, request, attachment, deadline)?;
        if rebound_identity != private_identity {
            return Err(RetainedBootFilePublicationError::DestinationIdentityChanged {
                action: "binding the exact private inode before publication",
            });
        }
        destination::require_named_identity(parent, private_name, private_identity, deadline)?;

        // This test-only boundary models the unavoidable last-name-component
        // race with an uncooperative same-credential writer. There is no second
        // rename attempt: the result below is always reconciled by inode.
        effect::before_private_name_rename();
        let mut rename_result = renameat2_noreplace_once(parent, private_name, parent, canonical_name);
        if rename_result.is_ok()
            && fault(FixtureRetainedBootFilePublicationFault::RenameReportsErrorAfterApplied).is_err()
        {
            rename_result = Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "injected error reported after the no-replace move applied",
            ));
        }
        match (destination::reconcile_move(parent, private_name, canonical_name, private_identity, deadline)?, rename_result)
        {
            (ReconciledMove::Applied, _) => {}
            (ReconciledMove::NotApplied, Err(source)) => {
                return Err(RetainedBootFilePublicationError::RenameNotApplied { source });
            }
            (ReconciledMove::NotApplied, Ok(())) => {
                return Err(RetainedBootFilePublicationError::RenameSuccessUnreconciled);
            }
            (ReconciledMove::Ambiguous, _) => return Err(RetainedBootFilePublicationError::RenameAmbiguous),
        }

        let (canonical, canonical_identity) =
            destination::open_and_verify(parent, canonical_name, request, attachment, deadline)?;
        if canonical_identity != private_identity {
            return Err(RetainedBootFilePublicationError::RenameAmbiguous);
        }
        self.durability_suffix(&canonical, parent, deadline)?;
        self.terminally_revalidate(
            parent,
            canonical_name,
            private_leaf,
            private_name,
            request,
            expected_sources,
            limits,
            attachment,
            canonical_identity,
            deadline,
        )?;
        Ok(validated_result(
            RetainedBootFilePublicationOutcome::Published,
            attachment,
            canonical_identity,
            request,
        ))
    }

    fn durability_suffix(
        &self,
        canonical: &File,
        parent: &File,
        deadline: Instant,
    ) -> Result<(), RetainedBootFilePublicationError> {
        fault(FixtureRetainedBootFilePublicationFault::BeforeCanonicalSync)?;
        checkpoint(deadline)?;
        canonical
            .sync_all()
            .map_err(|source| RetainedBootFilePublicationError::Filesystem {
                action: "synchronizing the canonical boot-file leaf",
                source,
            })?;
        checkpoint(deadline)?;
        fault(FixtureRetainedBootFilePublicationFault::BeforeParentSync)?;
        parent
            .sync_all()
            .map_err(|source| RetainedBootFilePublicationError::Filesystem {
                action: "synchronizing the retained boot-file parent",
                source,
            })?;
        checkpoint(deadline)?;
        fault(FixtureRetainedBootFilePublicationFault::BeforeFilesystemSync)?;
        sync_filesystem_until(parent, deadline).map_err(|source| RetainedBootFilePublicationError::Filesystem {
            action: "synchronizing the retained boot filesystem",
            source,
        })?;
        checkpoint(deadline)
    }

    #[allow(clippy::too_many_arguments)]
    fn terminally_revalidate(
        &self,
        parent: &File,
        canonical_name: &CString,
        private_leaf: &str,
        private_name: &CString,
        request: RetainedBootFilePublicationRequest<'_>,
        expected_sources: &[RetainedBootNamespaceExpectedSource<'_>],
        limits: RetainedBootFilePublicationLimits,
        attachment: AttachmentIdentity,
        expected_file: FileIdentity,
        deadline: Instant,
    ) -> Result<(), RetainedBootFilePublicationError> {
        self.require_publication_attachment_until("terminally revalidating the boot attachment", deadline)?;
        require_state(
            self.classify_leaf(
                request.canonical_leaf(),
                request,
                expected_sources,
                limits,
                "terminally revalidating the canonical boot-file leaf",
                deadline,
            )?,
            BootNamespaceDestinationState::Exact,
            RetainedBootFilePublicationError::DifferentCanonicalDestination,
        )?;
        require_state(
            self.classify_leaf(
                private_leaf,
                request,
                expected_sources,
                limits,
                "terminally revalidating private boot-file absence",
                deadline,
            )?,
            BootNamespaceDestinationState::Absent,
            RetainedBootFilePublicationError::DifferentPrivateResidue,
        )?;
        let (_canonical, found) =
            destination::open_and_verify(parent, canonical_name, request, attachment, deadline)?;
        if found != expected_file {
            return Err(RetainedBootFilePublicationError::DestinationIdentityChanged {
                action: "terminally rebinding the canonical boot-file inode",
            });
        }
        if destination::reconcile_move(parent, private_name, canonical_name, expected_file, deadline)?
            != ReconciledMove::Applied
        {
            return Err(RetainedBootFilePublicationError::RenameAmbiguous);
        }
        checkpoint(deadline)
    }

    fn classify_leaf(
        &self,
        leaf: &str,
        content: RetainedBootFilePublicationRequest<'_>,
        expected_sources: &[RetainedBootNamespaceExpectedSource<'_>],
        limits: RetainedBootFilePublicationLimits,
        action: &'static str,
        deadline: Instant,
    ) -> Result<BootNamespaceDestinationState, RetainedBootFilePublicationError> {
        checkpoint(deadline)?;
        let request = [BootNamespaceRequest::new(
            leaf,
            content.expected_length(),
            content.expected_xxh3(),
        )];
        let assessment = assess_retained_boot_namespace_until(
            self.target.publication_parent(),
            &request,
            expected_sources,
            limits.namespace,
            limits.retained_namespace,
            deadline,
        )
        .map_err(|source| RetainedBootFilePublicationError::Namespace { action, source })?;
        let observed = assessment.observed_root_identity().ok_or(
            RetainedBootFilePublicationError::DestinationIdentityChanged { action },
        )?;
        let expected = self.target.publication_parent_identity();
        if observed.device != expected.device
            || observed.inode != expected.inode
            || observed.mount_id != expected.mount_id
        {
            return Err(RetainedBootFilePublicationError::DestinationIdentityChanged { action });
        }
        assessment
            .states()
            .first()
            .copied()
            .ok_or(RetainedBootFilePublicationError::DestinationIdentityChanged { action })
    }

    fn require_publication_attachment_until(
        &self,
        action: &'static str,
        deadline: Instant,
    ) -> Result<(), RetainedBootFilePublicationError> {
        self.target.require_publication_parent_until(action, deadline)
    }
}

impl RetainedBootFilePublicationTarget for RevalidatedTaskRootedAttachment<'_> {
    fn publication_parent(&self) -> &File {
        self.current.destination_file()
    }

    fn publication_parent_identity(&self) -> AttachmentIdentity {
        AttachmentIdentity {
            device: self.destination_device(),
            inode: self.destination_inode(),
            mount_id: self.destination_mount_id(),
        }
    }

    fn require_publication_parent_until(
        &self,
        action: &'static str,
        deadline: Instant,
    ) -> Result<(), RetainedBootFilePublicationError> {
        checkpoint(deadline)?;
        let mut operation = Operation::production(PRODUCTION_LIMITS, deadline);
        self._prepared
            .capture
            .require_retained(&self._prepared.root, &mut operation)
            .map_err(|source| RetainedBootFilePublicationError::Attachment { action, source })?;
        self.current
            .require_retained(&self._prepared.root, &mut operation)
            .map_err(|source| RetainedBootFilePublicationError::Attachment { action, source })?;
        require_capture_matches(&self._prepared.capture, &self.current, action)
            .map_err(|source| RetainedBootFilePublicationError::Attachment { action, source })?;
        self.current
            .require_terminal_names(&self._prepared.root, &mut operation)
            .map_err(|source| RetainedBootFilePublicationError::Attachment { action, source })?;
        operation
            .checkpoint()
            .map_err(|source| RetainedBootFilePublicationError::Attachment { action, source })
    }
}

fn validate_request(
    request: RetainedBootFilePublicationRequest<'_>,
    limits: RetainedBootFilePublicationLimits,
    deadline: Instant,
) -> Result<CString, RetainedBootFilePublicationError> {
    checkpoint(deadline)?;
    let canonical_name = component(request.canonical_leaf())?;
    if is_retained_boot_file_private_component(request.canonical_leaf()) {
        return Err(RetainedBootFilePublicationError::ReservedPrivatePublicationLeaf);
    }
    if limits.max_write_bytes == 0 || limits.max_write_bytes > model::HARD_MAX_PUBLICATION_BYTES {
        return Err(RetainedBootFilePublicationError::InvalidLimit { field: "write bytes" });
    }
    if limits.max_write_calls == 0 || limits.max_write_calls > model::HARD_MAX_PUBLICATION_WRITE_CALLS {
        return Err(RetainedBootFilePublicationError::InvalidLimit { field: "write calls" });
    }
    if request.expected_length() > limits.max_write_bytes {
        return Err(RetainedBootFilePublicationError::LengthLimitExceeded {
            length: request.expected_length(),
            limit: limits.max_write_bytes,
        });
    }
    Ok(canonical_name)
}

fn component(leaf: &str) -> Result<CString, RetainedBootFilePublicationError> {
    let bytes = leaf.as_bytes();
    if bytes.is_empty()
        || bytes.len() > 255
        || matches!(bytes, b"." | b"..")
        || !bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'+' | b'-'))
    {
        return Err(RetainedBootFilePublicationError::InvalidCanonicalLeaf);
    }
    CString::new(bytes).map_err(|_| RetainedBootFilePublicationError::InvalidCanonicalLeaf)
}

fn deterministic_private_leaf(request: RetainedBootFilePublicationRequest<'_>) -> String {
    let mut digest = Sha256::new();
    digest.update(b"cast-retained-boot-file-private-v1\0");
    digest.update((request.canonical_leaf().len() as u64).to_le_bytes());
    digest.update(request.canonical_leaf().as_bytes());
    digest.update(request.expected_length().to_le_bytes());
    digest.update(request.expected_xxh3().to_le_bytes());
    digest.update(request.expected_sha256());
    format!("{RETAINED_BOOT_FILE_PRIVATE_PREFIX}{}.stage", hex::encode(digest.finalize()))
}

fn require_state(
    found: BootNamespaceDestinationState,
    expected: BootNamespaceDestinationState,
    error: RetainedBootFilePublicationError,
) -> Result<(), RetainedBootFilePublicationError> {
    if found == expected { Ok(()) } else { Err(error) }
}

fn validated_result(
    outcome: RetainedBootFilePublicationOutcome,
    attachment: AttachmentIdentity,
    file: FileIdentity,
    request: RetainedBootFilePublicationRequest<'_>,
) -> ValidatedRetainedBootFilePublication {
    ValidatedRetainedBootFilePublication::new(
        outcome,
        attachment.device,
        attachment.inode,
        attachment.mount_id,
        file.device,
        file.inode,
        request.expected_length(),
        request.expected_xxh3(),
        request.expected_sha256(),
    )
}
