//! Repeated validation of mixed immutable and owned-replacement evidence.

use std::time::Instant;

use thiserror::Error;

use crate::{
    boot_publication::BootPublicationReceiptFingerprint,
    client::{
        active_reblit_bls_renderer::BoundActiveReblitBlsPublicationPlan,
        active_reblit_boot_publication_preflight::ActiveReblitBootPublicationPreflightError,
        active_reblit_installed_boot_publication_delta::ActiveReblitBootPublicationDeltaAction,
        active_reblit_mounted_boot_topology::{
            ActiveReblitBootOwnedLeafReplacementError,
            ActiveReblitBootPublicationTargetsError, BootTargetRole,
        },
    },
    linux_fs::{
        descriptor_boot_namespace::BootNamespaceDestinationState,
        mount_namespace::RetainedBootFilePublicationOutcome,
    },
};

use super::super::ValidatedActiveReblitBootPublicationEffect;
use super::require_deadline;

/// Failure while proving that mixed historical terminal evidence is exact.
#[derive(Debug, Error)]
pub(in crate::client) enum ActiveReblitBootTerminalEvidenceValidationError {
    #[error("the terminal promotion deadline {deadline:?} expired at {checkpoint}")]
    DeadlineExceeded {
        checkpoint: &'static str,
        deadline: Instant,
    },
    #[error("prepare a fresh read-only publication preflight at {checkpoint}")]
    Preflight {
        checkpoint: &'static str,
        #[source]
        source: ActiveReblitBootPublicationPreflightError,
    },
    #[error("terminal publication count at {checkpoint} is {actual}, expected {expected}")]
    PublicationCountMismatch {
        checkpoint: &'static str,
        expected: usize,
        actual: usize,
    },
    #[error("terminal publication counters overflowed at {checkpoint}")]
    PublicationCounterOverflow { checkpoint: &'static str },
    #[error(
        "terminal publication outcomes at {checkpoint} are published={published}, already-exact={already_exact}, replaced={replaced}; retained counters are published={retained_published}, already-exact={retained_already_exact}, replaced={retained_replaced}"
    )]
    PublicationOutcomeMismatch {
        checkpoint: &'static str,
        published: usize,
        already_exact: usize,
        replaced: usize,
        retained_published: usize,
        retained_already_exact: usize,
        retained_replaced: usize,
    },
    #[error("terminal evidence for output {plan_index} differs from its retained plan at {checkpoint}")]
    EvidenceMismatch {
        checkpoint: &'static str,
        plan_index: usize,
    },
    #[error("owned replacement evidence for output {plan_index} is not bound to the promoted receipt at {checkpoint}")]
    ReplacementOwnerMismatch {
        checkpoint: &'static str,
        plan_index: usize,
    },
    #[error("fresh output {plan_index} is {state:?}, not Exact, at {checkpoint}")]
    DestinationNotExact {
        checkpoint: &'static str,
        plan_index: usize,
        state: BootNamespaceDestinationState,
    },
    #[error("capture exact boot targets while validating replacement pairs at {checkpoint}")]
    ReplacementTargets {
        checkpoint: &'static str,
        #[source]
        source: ActiveReblitBootPublicationTargetsError,
    },
    #[error("the replacement target shape no longer matches output {plan_index} at {checkpoint}")]
    ReplacementTargetShape {
        checkpoint: &'static str,
        plan_index: usize,
    },
    #[error("validate applied replacement output {plan_index} through {role:?} at {checkpoint}")]
    ReplacementPair {
        checkpoint: &'static str,
        role: BootTargetRole,
        plan_index: usize,
        #[source]
        source: ActiveReblitBootOwnedLeafReplacementError,
    },
}

#[allow(clippy::too_many_arguments)]
pub(super) fn validate_exact_terminal_evidence_snapshot<
    'input,
    'topology_view,
    'topology_authority,
    'attempt,
    'stone,
    'roots,
>(
    plan: &BoundActiveReblitBlsPublicationPlan<
        'input,
        'topology_view,
        'topology_authority,
        'attempt,
        'stone,
        'roots,
    >,
    receipt_fingerprint: BootPublicationReceiptFingerprint,
    publication_count: usize,
    published_count: usize,
    already_exact_count: usize,
    replaced_count: usize,
    evidence: &[ValidatedActiveReblitBootPublicationEffect],
    checkpoint: &'static str,
) -> Result<(), ActiveReblitBootTerminalEvidenceValidationError> {
    require_deadline(checkpoint, plan.input_deadline())?;
    let expected = plan.publication_count();
    require_count(checkpoint, expected, publication_count)?;
    require_count(checkpoint, expected, evidence.len())?;

    let mut published = 0usize;
    let mut already_exact = 0usize;
    let mut replaced = 0usize;
    for (plan_index, (retained, output)) in evidence.iter().zip(plan.outputs()).enumerate() {
        if retained.plan_index() != plan_index
            || retained.length() != output.expected_length()
            || retained.xxh3() != output.expected_digest()
            || retained.sha256() != *output.expected_content_identity().as_bytes()
        {
            return Err(
                ActiveReblitBootTerminalEvidenceValidationError::EvidenceMismatch {
                    checkpoint,
                    plan_index,
                },
            );
        }
        match retained.action() {
            ActiveReblitBootPublicationDeltaAction::PublishDesired => {
                if retained.immutable_outcome()
                    != Some(RetainedBootFilePublicationOutcome::Published)
                {
                    return Err(evidence_mismatch(checkpoint, plan_index));
                }
                published = increment(checkpoint, published)?;
            }
            ActiveReblitBootPublicationDeltaAction::RetainOwnedDesired
            | ActiveReblitBootPublicationDeltaAction::PreserveBorrowedDesired => {
                if retained.immutable_outcome()
                    != Some(RetainedBootFilePublicationOutcome::AlreadyExact)
                {
                    return Err(evidence_mismatch(checkpoint, plan_index));
                }
                already_exact = increment(checkpoint, already_exact)?;
            }
            ActiveReblitBootPublicationDeltaAction::ReplaceOwnedDesired => {
                if retained.immutable_outcome().is_some()
                    || retained.installed_length().is_none()
                    || retained.installed_xxh3().is_none()
                    || retained.installed_sha256().is_none()
                    || retained.installed_length() == Some(retained.length())
                        && retained.installed_xxh3() == Some(retained.xxh3())
                        && retained.installed_sha256() == Some(retained.sha256())
                {
                    return Err(evidence_mismatch(checkpoint, plan_index));
                }
                if !retained.owner_matches_receipt(receipt_fingerprint) {
                    return Err(
                        ActiveReblitBootTerminalEvidenceValidationError::ReplacementOwnerMismatch {
                            checkpoint,
                            plan_index,
                        },
                    );
                }
                replaced = increment(checkpoint, replaced)?;
            }
            ActiveReblitBootPublicationDeltaAction::DeleteOwnedStaleAfterPromotion
            | ActiveReblitBootPublicationDeltaAction::PreserveUnownedStale => {
                return Err(evidence_mismatch(checkpoint, plan_index));
            }
        }
    }
    if published != published_count
        || already_exact != already_exact_count
        || replaced != replaced_count
    {
        return Err(
            ActiveReblitBootTerminalEvidenceValidationError::PublicationOutcomeMismatch {
                checkpoint,
                published,
                already_exact,
                replaced,
                retained_published: published_count,
                retained_already_exact: already_exact_count,
                retained_replaced: replaced_count,
            },
        );
    }
    let accounted = published
        .checked_add(already_exact)
        .and_then(|count| count.checked_add(replaced))
        .ok_or(
            ActiveReblitBootTerminalEvidenceValidationError::PublicationCounterOverflow {
                checkpoint,
            },
        )?;
    require_count(checkpoint, expected, accounted)?;

    let preflight = plan
        .prepare_boot_publication_preflight()
        .map_err(|source| ActiveReblitBootTerminalEvidenceValidationError::Preflight {
            checkpoint,
            source,
        })?;
    require_count(checkpoint, expected, preflight.publication_count())?;
    for (plan_index, state) in preflight.initial_states().iter().copied().enumerate() {
        if state != BootNamespaceDestinationState::Exact {
            return Err(
                ActiveReblitBootTerminalEvidenceValidationError::DestinationNotExact {
                    checkpoint,
                    plan_index,
                    state,
                },
            );
        }
    }
    require_deadline(checkpoint, plan.input_deadline())
}

fn require_count(
    checkpoint: &'static str,
    expected: usize,
    actual: usize,
) -> Result<(), ActiveReblitBootTerminalEvidenceValidationError> {
    if actual == expected {
        Ok(())
    } else {
        Err(
            ActiveReblitBootTerminalEvidenceValidationError::PublicationCountMismatch {
                checkpoint,
                expected,
                actual,
            },
        )
    }
}

fn increment(
    checkpoint: &'static str,
    value: usize,
) -> Result<usize, ActiveReblitBootTerminalEvidenceValidationError> {
    value.checked_add(1).ok_or(
        ActiveReblitBootTerminalEvidenceValidationError::PublicationCounterOverflow {
            checkpoint,
        },
    )
}

const fn evidence_mismatch(
    checkpoint: &'static str,
    plan_index: usize,
) -> ActiveReblitBootTerminalEvidenceValidationError {
    ActiveReblitBootTerminalEvidenceValidationError::EvidenceMismatch {
        checkpoint,
        plan_index,
    }
}
