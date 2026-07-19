use std::mem::size_of;

use sha2::{Digest as _, Sha256};
use thiserror::Error;

use super::model::{
    AbortDisposition, BootRollback, CandidateOrigin, ForwardPhase, Operation, Phase, PreviousOrigin, RollbackAction,
    TransitionRecord,
};

pub(super) const MAX_QUARANTINE_NAME_BYTES: usize = 128;

pub(super) const MAGIC: &[u8; 8] = b"CASTSTJ\0";
pub(super) const FRAME_VERSION: u16 = 1;
pub(super) const PAYLOAD_FORMAT: &str = "cast-state-transition";
/// Legacy payloads remain byte-canonically readable and may advance only
/// through transitions representable by their original domain.
pub(super) const PAYLOAD_VERSION_V1: u16 = 1;
/// Current write version used for every newly prepared transition.
pub(super) const PAYLOAD_VERSION: u16 = 2;
pub(super) const MAGIC_END: usize = MAGIC.len();
pub(super) const VERSION_END: usize = MAGIC_END + size_of::<u16>();
const LENGTH_END: usize = VERSION_END + size_of::<u32>();
pub(super) const CHECKSUM_END: usize = LENGTH_END + 32;
pub(super) const HEADER_SIZE: usize = CHECKSUM_END;

/// The entire framed canonical record, not merely its JSON payload, is bounded.
pub(crate) const MAX_CANONICAL_RECORD_BYTES: usize = 16 * 1024;

#[derive(Debug, Error)]
pub(crate) enum CodecError {
    #[error("journal record exceeds the {MAX_CANONICAL_RECORD_BYTES}-byte limit: {0} bytes")]
    RecordTooLarge(usize),
    #[error("journal record is shorter than its fixed {HEADER_SIZE}-byte header: {0} bytes")]
    TruncatedHeader(usize),
    #[error("journal magic is invalid")]
    InvalidMagic,
    #[error("unsupported journal frame version {0}")]
    UnsupportedFrameVersion(u16),
    #[error("journal payload length {declared} does not match the framed length {actual}")]
    LengthMismatch { declared: usize, actual: usize },
    #[error("journal payload checksum does not match")]
    ChecksumMismatch,
    #[error("journal payload is not strict JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("journal payload is not in its canonical encoding")]
    NonCanonicalPayload,
    #[error("unsupported journal payload format `{0}`")]
    UnsupportedPayloadFormat(String),
    #[error("unsupported journal payload version {0}")]
    UnsupportedPayloadVersion(u16),
    #[error("journal payload version {version} cannot encode phase {phase:?}")]
    PayloadVersionPhaseMismatch { version: u16, phase: Phase },
    #[error("journal payload version {version} cannot encode boot rollback status {status:?}")]
    PayloadVersionBootRollbackMismatch { version: u16, status: BootRollback },
    #[error("journal generation must be nonzero")]
    ZeroGeneration,
    #[error("journal generation counter is exhausted")]
    GenerationExhausted,
    #[error("next journal generation must be {expected}, not {actual}")]
    GenerationMismatch { expected: u64, actual: u64 },
    #[error("boot ID must be one nonzero canonical lowercase UUID")]
    InvalidBootId,
    #[error("tree token must be exactly 32 nonzero lowercase hexadecimal characters")]
    InvalidTreeToken,
    #[error("quarantine name must be one bounded lowercase ASCII path component")]
    InvalidQuarantineName,
    #[error("state ID must be positive, not {0}")]
    InvalidStateId(i32),
    #[error("operation {operation:?} cannot use candidate origin {origin:?}")]
    OperationOriginMismatch {
        operation: Operation,
        origin: CandidateOrigin,
    },
    #[error("an existing candidate must include its state ID")]
    ExistingCandidateStateMissing,
    #[error("candidate state-ID presence contradicts the persisted phase")]
    CandidateStateLayout,
    #[error("an active reblit candidate must identify the previous active state")]
    ActiveReblitStateMismatch,
    #[error("an archived activation must identify distinct archived and previous active states")]
    ArchivedActivationStateMismatch,
    #[error("previous origin {origin:?} contradicts state ID {state_id:?}")]
    PreviousOriginStateMismatch {
        origin: PreviousOrigin,
        state_id: Option<i32>,
    },
    #[error("operation {operation:?} cannot use previous origin {origin:?}")]
    PreviousOriginOperationMismatch {
        operation: Operation,
        origin: PreviousOrigin,
    },
    #[error("previous origin {origin:?} contradicts archive_previous={archive_previous}")]
    ArchiveOptionMismatch {
        origin: PreviousOrigin,
        archive_previous: bool,
    },
    #[error("the creation mount-namespace identity must contain nonzero device and inode values")]
    ZeroMountNamespaceIdentity,
    #[error("runtime tree identities must contain nonzero device, inode, and mount IDs")]
    ZeroRuntimeTreeIdentity,
    #[error("a forward phase cannot carry a rollback plan")]
    RollbackPlanOnForwardPhase,
    #[error("a rollback phase requires its durable rollback plan")]
    MissingRollbackPlan,
    #[error("forward phase {0:?} cannot be rolled back")]
    InvalidRollbackSource(ForwardPhase),
    #[error("phase {0:?} is disabled by this transaction's options")]
    DisabledPhase(Phase),
    #[error("fresh allocation phases are invalid for an existing candidate")]
    FreshPhaseForExistingCandidate,
    #[error("candidate and previous /usr tree tokens must be distinct")]
    CandidatePreviousTreeTokenCollision,
    #[error("candidate and previous /usr runtime witnesses identify the same filesystem object")]
    CandidatePreviousObjectCollision,
    #[error(
        "candidate and previous /usr trees must share one filesystem for atomic exchange, not devices {candidate} and {previous}"
    )]
    CandidatePreviousFilesystemMismatch { candidate: u64, previous: u64 },
    #[error(
        "candidate and previous /usr trees must share one mount for atomic exchange, not mount IDs {candidate} and {previous}"
    )]
    CandidatePreviousMountMismatch { candidate: u64, previous: u64 },
    #[error("candidate and previous state IDs must be distinct for this operation")]
    CandidatePreviousStateCollision,
    #[error("rollback action {action} has status {status:?}, but possible={possible}")]
    InvalidRollbackRequirement {
        action: &'static str,
        status: RollbackAction,
        possible: bool,
    },
    #[error("boot rollback evidence contradicts the rollback source")]
    InvalidBootRollbackRequirement,
    #[error(
        "candidate disposition {actual:?} is invalid for {operation:?} from {rollback_source:?}; expected {expected:?}"
    )]
    InvalidCandidateDisposition {
        operation: Operation,
        rollback_source: ForwardPhase,
        expected: AbortDisposition,
        actual: AbortDisposition,
    },
    #[error("external-effects evidence is {actual}, but must be {expected}")]
    InvalidExternalEffectsEvidence { expected: bool, actual: bool },
    #[error("rollback plan status is inconsistent with phase {phase:?}")]
    RollbackPlanPhaseMismatch { phase: Phase },
    #[error("journal transition ID changed during an advance")]
    TransitionChanged,
    #[error("immutable journal transition data changed during an advance")]
    ImmutableTransitionDataChanged,
    #[error("cannot advance a terminal journal phase")]
    TerminalPhaseAdvance,
    #[error("illegal journal phase advance from {current:?} to {next:?}")]
    IllegalPhaseAdvance { current: Phase, next: Phase },
    #[error("rollback plan changed outside one observed recovery action")]
    RollbackPlanChangedIllegally,
    #[error("rollback successor requires exactly one outcome for its current recovery action")]
    RollbackActionOutcomeMismatch,
    #[error("boot-repair phase {0:?} requires an explicit typed successor")]
    ExplicitBootRepairSuccessorRequired(Phase),
    #[error("candidate state ID changed outside fresh allocation")]
    CandidateStateChangedIllegally,
}

pub(crate) fn encode(record: &TransitionRecord) -> Result<Vec<u8>, CodecError> {
    record.validate()?;
    let payload = serde_json::to_vec(record)?;
    let framed_len = HEADER_SIZE
        .checked_add(payload.len())
        .ok_or(CodecError::RecordTooLarge(usize::MAX))?;
    enforce_record_size(framed_len)?;
    let payload_len = u32::try_from(payload.len()).map_err(|_| CodecError::RecordTooLarge(framed_len))?;
    let version = FRAME_VERSION.to_be_bytes();
    let length = payload_len.to_be_bytes();
    let checksum = checksum(&version, &length, &payload);

    let mut framed = Vec::with_capacity(framed_len);
    framed.extend_from_slice(MAGIC);
    framed.extend_from_slice(&version);
    framed.extend_from_slice(&length);
    framed.extend_from_slice(&checksum);
    framed.extend_from_slice(&payload);
    debug_assert_eq!(framed.len(), framed_len);
    Ok(framed)
}

pub(crate) fn decode(framed: &[u8]) -> Result<TransitionRecord, CodecError> {
    enforce_record_size(framed.len())?;
    if framed.len() < HEADER_SIZE {
        return Err(CodecError::TruncatedHeader(framed.len()));
    }
    if &framed[..MAGIC_END] != MAGIC {
        return Err(CodecError::InvalidMagic);
    }

    let frame_version = u16::from_be_bytes(framed[MAGIC_END..VERSION_END].try_into().expect("fixed version field"));
    if frame_version != FRAME_VERSION {
        return Err(CodecError::UnsupportedFrameVersion(frame_version));
    }
    let payload_len = u32::from_be_bytes(framed[VERSION_END..LENGTH_END].try_into().expect("fixed length field"));
    let payload_len = usize::try_from(payload_len).map_err(|_| CodecError::RecordTooLarge(usize::MAX))?;
    let expected_len = HEADER_SIZE
        .checked_add(payload_len)
        .ok_or(CodecError::RecordTooLarge(usize::MAX))?;
    if expected_len != framed.len() {
        return Err(CodecError::LengthMismatch {
            declared: payload_len,
            actual: framed.len() - HEADER_SIZE,
        });
    }

    let payload = &framed[HEADER_SIZE..];
    let expected_checksum = checksum(
        &framed[MAGIC_END..VERSION_END],
        &framed[VERSION_END..LENGTH_END],
        payload,
    );
    if framed[LENGTH_END..CHECKSUM_END] != expected_checksum {
        return Err(CodecError::ChecksumMismatch);
    }

    let record: TransitionRecord = serde_json::from_slice(payload)?;
    record.validate()?;
    if serde_json::to_vec(&record)? != payload {
        return Err(CodecError::NonCanonicalPayload);
    }
    Ok(record)
}

pub(super) fn enforce_record_size(size: usize) -> Result<(), CodecError> {
    if size > MAX_CANONICAL_RECORD_BYTES {
        return Err(CodecError::RecordTooLarge(size));
    }
    Ok(())
}

pub(super) fn checksum(version: &[u8], length: &[u8], payload: &[u8]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(MAGIC);
    digest.update(version);
    digest.update(length);
    digest.update(payload);
    digest.finalize().into()
}
