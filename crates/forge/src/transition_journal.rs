// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

//! Strict, durable storage for the single in-flight state transition.
//!
//! This module deliberately contains no recovery policy. It defines only the
//! versioned record contract and the descriptor-relative durability boundary
//! which a later recovery state machine can consume.

use std::{
    ffi::{CStr, CString},
    io::{self, Read as _, Write as _},
    mem::{size_of, zeroed},
    os::{
        fd::{AsRawFd as _, FromRawFd as _, OwnedFd, RawFd},
        unix::{
            ffi::OsStrExt as _,
            fs::{MetadataExt as _, PermissionsExt as _},
        },
    },
    path::{Path, PathBuf},
    sync::{
        Mutex, MutexGuard,
        atomic::{AtomicU64, Ordering},
    },
};

use nix::unistd::Uid;
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};
use sha2::{Digest as _, Sha256};
use thiserror::Error;

use crate::state::TransitionId;

const JOURNAL_DIRECTORY: &CStr = c"journal";
const CANONICAL_NAME: &CStr = c"state-transition";
const LOCK_NAME: &CStr = c"state-transition.lock";
const JOURNAL_DIRECTORY_MODE: u32 = 0o700;
const JOURNAL_FILE_MODE: u32 = 0o600;
const PROC_SUPER_MAGIC: nix::libc::c_long = 0x0000_9fa0;
const MAX_QUARANTINE_NAME_BYTES: usize = 128;
const MAX_STALE_TEMPORARIES: usize = 256;
const TEMPORARY_PREFIX: &[u8] = b".state-transition.tmp-";

const MAGIC: &[u8; 8] = b"CASTSTJ\0";
const FRAME_VERSION: u16 = 1;
const PAYLOAD_FORMAT: &str = "cast-state-transition";
const PAYLOAD_VERSION: u16 = 1;
const MAGIC_END: usize = MAGIC.len();
const VERSION_END: usize = MAGIC_END + size_of::<u16>();
const LENGTH_END: usize = VERSION_END + size_of::<u32>();
const CHECKSUM_END: usize = LENGTH_END + 32;
const HEADER_SIZE: usize = CHECKSUM_END;

/// The entire framed canonical record, not merely its JSON payload, is bounded.
pub(crate) const MAX_CANONICAL_RECORD_BYTES: usize = 16 * 1024;

static TEMPORARY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DurabilityCheckpoint {
    TemporaryFullySynced,
    CanonicalPublished,
    CanonicalExchanged,
    CanonicalUnlinked,
    DisplacedUnlinked,
    JournalDirectorySynced,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StorageFaultPoint {
    TemporarySync,
    InitialRename,
    InitialDirectorySync,
    UpdateExchange,
    UpdateFirstDirectorySync,
    DisplacedUnlink,
    UpdateFinalDirectorySync,
    CanonicalUnlink,
    DeleteDirectorySync,
}

#[cfg(test)]
std::thread_local! {
    static DURABILITY_CHECKPOINTS: std::cell::RefCell<Vec<DurabilityCheckpoint>> = const { std::cell::RefCell::new(Vec::new()) };
    static STORAGE_FAULT: std::cell::Cell<Option<StorageFaultPoint>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
fn durability_checkpoint(checkpoint: DurabilityCheckpoint) {
    DURABILITY_CHECKPOINTS.with(|checkpoints| checkpoints.borrow_mut().push(checkpoint));
}

#[cfg(not(test))]
fn durability_checkpoint(_checkpoint: DurabilityCheckpoint) {}

#[cfg(test)]
fn take_durability_checkpoints() -> Vec<DurabilityCheckpoint> {
    DURABILITY_CHECKPOINTS.with(|checkpoints| std::mem::take(&mut *checkpoints.borrow_mut()))
}

#[cfg(test)]
fn arm_storage_fault(point: StorageFaultPoint) {
    STORAGE_FAULT.with(|fault| {
        assert!(fault.replace(Some(point)).is_none(), "a storage fault is already armed");
    });
}

#[cfg(test)]
fn assert_storage_fault_consumed() {
    STORAGE_FAULT.with(|fault| assert!(fault.get().is_none(), "armed storage fault was not reached"));
}

fn storage_fault(point: StorageFaultPoint) -> io::Result<()> {
    #[cfg(test)]
    {
        let injected = STORAGE_FAULT.with(|fault| fault.get() == Some(point));
        if injected {
            STORAGE_FAULT.with(|fault| fault.set(None));
            return Err(io::Error::other(format!(
                "injected transition-journal fault at {point:?}"
            )));
        }
    }
    let _ = point;
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub(crate) struct QuarantineName(String);

impl QuarantineName {
    pub(crate) fn parse(value: impl Into<String>) -> Result<Self, CodecError> {
        let value = value.into();
        let bytes = value.as_bytes();
        if bytes.is_empty()
            || bytes.len() > MAX_QUARANTINE_NAME_BYTES
            || value == "."
            || value == ".."
            || bytes
                .iter()
                .any(|byte| !byte.is_ascii_lowercase() && !byte.is_ascii_digit() && !matches!(byte, b'-' | b'_' | b'.'))
        {
            return Err(CodecError::InvalidQuarantineName);
        }
        Ok(Self(value))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for QuarantineName {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(D::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Operation {
    NewState,
    ActivateArchived,
    ActiveReblit,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum CandidateOrigin {
    Fresh,
    Archived,
    ActiveReblit,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum PreviousOrigin {
    ActiveState,
    ActiveReblitCorrupt,
    SynthesizedEmpty,
    Unmanaged,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum AbortDisposition {
    Rearchive,
    Quarantine,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CommitDisposition {
    Archive,
    Discard,
    Quarantine,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Phase {
    Preparing,
    FreshStateAllocating,
    FreshStateAllocated,
    CandidatePrepareStarted,
    CandidatePrepared,
    TransactionTriggersStarted,
    TransactionTriggersComplete,
    UsrExchangeIntent,
    UsrExchanged,
    RootLinksComplete,
    SystemTriggersStarted,
    SystemTriggersComplete,
    PreviousArchiveIntent,
    PreviousArchived,
    BootSyncStarted,
    BootSyncComplete,
    CommitDecided,
    CommitCleanupComplete,
    Complete,
    RollbackDecided,
    PreviousRestoreIntent,
    PreviousRestoredToStaging,
    ReverseExchangeIntent,
    UsrRestored,
    CandidatePreserveIntent,
    CandidatePreserved,
    FreshDbInvalidationIntent,
    FreshDbInvalidated,
    BootRepairRequired,
    BootRepairStarted,
    BootRepairUnverified,
    RollbackComplete,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ForwardPhase {
    Preparing,
    FreshStateAllocating,
    FreshStateAllocated,
    CandidatePrepareStarted,
    CandidatePrepared,
    TransactionTriggersStarted,
    TransactionTriggersComplete,
    UsrExchangeIntent,
    UsrExchanged,
    RootLinksComplete,
    SystemTriggersStarted,
    SystemTriggersComplete,
    PreviousArchiveIntent,
    PreviousArchived,
    BootSyncStarted,
    BootSyncComplete,
    CommitDecided,
    CommitCleanupComplete,
    Complete,
}

impl ForwardPhase {
    fn ordinal(self) -> u8 {
        match self {
            Self::Preparing => 0,
            Self::FreshStateAllocating => 1,
            Self::FreshStateAllocated => 2,
            Self::CandidatePrepareStarted => 3,
            Self::CandidatePrepared => 4,
            Self::TransactionTriggersStarted => 5,
            Self::TransactionTriggersComplete => 6,
            Self::UsrExchangeIntent => 7,
            Self::UsrExchanged => 8,
            Self::RootLinksComplete => 9,
            Self::SystemTriggersStarted => 10,
            Self::SystemTriggersComplete => 11,
            Self::PreviousArchiveIntent => 12,
            Self::PreviousArchived => 13,
            Self::BootSyncStarted => 14,
            Self::BootSyncComplete => 15,
            Self::CommitDecided => 16,
            Self::CommitCleanupComplete => 17,
            Self::Complete => 18,
        }
    }
}

impl From<ForwardPhase> for Phase {
    fn from(value: ForwardPhase) -> Self {
        match value {
            ForwardPhase::Preparing => Self::Preparing,
            ForwardPhase::FreshStateAllocating => Self::FreshStateAllocating,
            ForwardPhase::FreshStateAllocated => Self::FreshStateAllocated,
            ForwardPhase::CandidatePrepareStarted => Self::CandidatePrepareStarted,
            ForwardPhase::CandidatePrepared => Self::CandidatePrepared,
            ForwardPhase::TransactionTriggersStarted => Self::TransactionTriggersStarted,
            ForwardPhase::TransactionTriggersComplete => Self::TransactionTriggersComplete,
            ForwardPhase::UsrExchangeIntent => Self::UsrExchangeIntent,
            ForwardPhase::UsrExchanged => Self::UsrExchanged,
            ForwardPhase::RootLinksComplete => Self::RootLinksComplete,
            ForwardPhase::SystemTriggersStarted => Self::SystemTriggersStarted,
            ForwardPhase::SystemTriggersComplete => Self::SystemTriggersComplete,
            ForwardPhase::PreviousArchiveIntent => Self::PreviousArchiveIntent,
            ForwardPhase::PreviousArchived => Self::PreviousArchived,
            ForwardPhase::BootSyncStarted => Self::BootSyncStarted,
            ForwardPhase::BootSyncComplete => Self::BootSyncComplete,
            ForwardPhase::CommitDecided => Self::CommitDecided,
            ForwardPhase::CommitCleanupComplete => Self::CommitCleanupComplete,
            ForwardPhase::Complete => Self::Complete,
        }
    }
}

impl Phase {
    fn forward(self) -> Option<ForwardPhase> {
        Some(match self {
            Self::Preparing => ForwardPhase::Preparing,
            Self::FreshStateAllocating => ForwardPhase::FreshStateAllocating,
            Self::FreshStateAllocated => ForwardPhase::FreshStateAllocated,
            Self::CandidatePrepareStarted => ForwardPhase::CandidatePrepareStarted,
            Self::CandidatePrepared => ForwardPhase::CandidatePrepared,
            Self::TransactionTriggersStarted => ForwardPhase::TransactionTriggersStarted,
            Self::TransactionTriggersComplete => ForwardPhase::TransactionTriggersComplete,
            Self::UsrExchangeIntent => ForwardPhase::UsrExchangeIntent,
            Self::UsrExchanged => ForwardPhase::UsrExchanged,
            Self::RootLinksComplete => ForwardPhase::RootLinksComplete,
            Self::SystemTriggersStarted => ForwardPhase::SystemTriggersStarted,
            Self::SystemTriggersComplete => ForwardPhase::SystemTriggersComplete,
            Self::PreviousArchiveIntent => ForwardPhase::PreviousArchiveIntent,
            Self::PreviousArchived => ForwardPhase::PreviousArchived,
            Self::BootSyncStarted => ForwardPhase::BootSyncStarted,
            Self::BootSyncComplete => ForwardPhase::BootSyncComplete,
            Self::CommitDecided => ForwardPhase::CommitDecided,
            Self::CommitCleanupComplete => ForwardPhase::CommitCleanupComplete,
            Self::Complete => ForwardPhase::Complete,
            Self::RollbackDecided
            | Self::PreviousRestoreIntent
            | Self::PreviousRestoredToStaging
            | Self::ReverseExchangeIntent
            | Self::UsrRestored
            | Self::CandidatePreserveIntent
            | Self::CandidatePreserved
            | Self::FreshDbInvalidationIntent
            | Self::FreshDbInvalidated
            | Self::BootRepairRequired
            | Self::BootRepairStarted
            | Self::BootRepairUnverified
            | Self::RollbackComplete => return None,
        })
    }

    fn blocks_advance(self) -> bool {
        matches!(
            self,
            Self::Complete | Self::BootRepairUnverified | Self::RollbackComplete
        )
    }

    fn deletable(self) -> bool {
        matches!(self, Self::Complete | Self::RollbackComplete)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TreeIdentity {
    pub(crate) st_dev: u64,
    pub(crate) inode: u64,
    pub(crate) statx_mount_id: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Candidate {
    pub(crate) id: Option<i32>,
    pub(crate) origin: CandidateOrigin,
    pub(crate) usr_identity: TreeIdentity,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Previous {
    pub(crate) id: Option<i32>,
    pub(crate) usr_identity: TreeIdentity,
    pub(crate) origin: PreviousOrigin,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TransitionOptions {
    pub(crate) archive_previous: bool,
    pub(crate) run_system_triggers: bool,
    pub(crate) run_boot_sync: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum RollbackAction {
    NotRequired,
    Pending,
    Applied,
    AlreadySatisfied,
}

impl RollbackAction {
    fn required(self) -> bool {
        self != Self::NotRequired
    }

    fn resolved(self) -> bool {
        matches!(self, Self::Applied | Self::AlreadySatisfied)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum BootRollback {
    NotRequired,
    PendingUnverifiable,
    Unverified,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CandidateRollback {
    pub(crate) action: RollbackAction,
    pub(crate) disposition: AbortDisposition,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RollbackPlan {
    pub(crate) source: ForwardPhase,
    pub(crate) previous_archive: RollbackAction,
    pub(crate) usr_exchange: RollbackAction,
    pub(crate) candidate: CandidateRollback,
    pub(crate) fresh_db: RollbackAction,
    pub(crate) boot: BootRollback,
    pub(crate) external_effects_may_remain: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TransitionRecord {
    pub(crate) format: String,
    pub(crate) version: u16,
    pub(crate) generation: u64,
    pub(crate) transition_id: TransitionId,
    pub(crate) operation: Operation,
    pub(crate) phase: Phase,
    pub(crate) rollback: Option<RollbackPlan>,
    pub(crate) candidate: Candidate,
    pub(crate) previous: Previous,
    pub(crate) options: TransitionOptions,
    pub(crate) quarantine_name: QuarantineName,
}

impl TransitionRecord {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn preparing(
        transition_id: TransitionId,
        operation: Operation,
        candidate_id: Option<i32>,
        candidate_usr_identity: TreeIdentity,
        previous: Previous,
        run_system_triggers: bool,
        run_boot_sync: bool,
        quarantine_name: QuarantineName,
    ) -> Result<Self, CodecError> {
        let candidate_origin = match operation {
            Operation::NewState => CandidateOrigin::Fresh,
            Operation::ActivateArchived => CandidateOrigin::Archived,
            Operation::ActiveReblit => CandidateOrigin::ActiveReblit,
        };
        let archive_previous = matches!(previous.origin, PreviousOrigin::ActiveState);
        let record = Self {
            format: PAYLOAD_FORMAT.to_owned(),
            version: PAYLOAD_VERSION,
            generation: 1,
            transition_id,
            operation,
            phase: Phase::Preparing,
            rollback: None,
            candidate: Candidate {
                id: candidate_id,
                origin: candidate_origin,
                usr_identity: candidate_usr_identity,
            },
            previous,
            options: TransitionOptions {
                archive_previous,
                run_system_triggers,
                run_boot_sync,
            },
            quarantine_name,
        };
        record.validate()?;
        Ok(record)
    }

    fn validate(&self) -> Result<(), CodecError> {
        if self.format != PAYLOAD_FORMAT {
            return Err(CodecError::UnsupportedPayloadFormat(self.format.clone()));
        }
        if self.version != PAYLOAD_VERSION {
            return Err(CodecError::UnsupportedPayloadVersion(self.version));
        }
        if self.generation == 0 {
            return Err(CodecError::ZeroGeneration);
        }
        self.previous.usr_identity.validate()?;
        self.candidate.usr_identity.validate()?;
        for id in [self.candidate.id, self.previous.id].into_iter().flatten() {
            if id <= 0 {
                return Err(CodecError::InvalidStateId(id));
            }
        }

        let expected_origin = match self.operation {
            Operation::NewState => CandidateOrigin::Fresh,
            Operation::ActivateArchived => CandidateOrigin::Archived,
            Operation::ActiveReblit => CandidateOrigin::ActiveReblit,
        };
        if self.candidate.origin != expected_origin {
            return Err(CodecError::OperationOriginMismatch {
                operation: self.operation,
                origin: self.candidate.origin,
            });
        }

        let layout_phase = match (self.phase.forward(), self.rollback.as_ref()) {
            (Some(phase), None) => phase,
            (Some(_), Some(_)) => return Err(CodecError::RollbackPlanOnForwardPhase),
            (None, Some(rollback)) if rollback_allowed(self, rollback.source) => rollback.source,
            (None, Some(rollback)) => return Err(CodecError::InvalidRollbackSource(rollback.source)),
            (None, None) => return Err(CodecError::MissingRollbackPlan),
        };
        self.validate_option_reachability(layout_phase)?;
        self.validate_candidate_layout(layout_phase)?;
        self.validate_relationships()?;
        if let Some(rollback) = &self.rollback {
            self.validate_rollback_plan(rollback)?;
            validate_rollback_phase(self.phase, rollback)?;
        }
        Ok(())
    }

    fn validate_option_reachability(&self, phase: ForwardPhase) -> Result<(), CodecError> {
        if matches!(
            phase,
            ForwardPhase::FreshStateAllocating | ForwardPhase::FreshStateAllocated
        ) && !matches!(self.operation, Operation::NewState)
        {
            return Err(CodecError::FreshPhaseForExistingCandidate);
        }
        if matches!(
            phase,
            ForwardPhase::TransactionTriggersStarted | ForwardPhase::TransactionTriggersComplete
        ) && !self.runs_transaction_triggers()
        {
            return Err(CodecError::DisabledPhase(Phase::from(phase)));
        }
        if matches!(
            phase,
            ForwardPhase::SystemTriggersStarted | ForwardPhase::SystemTriggersComplete
        ) && !self.options.run_system_triggers
        {
            return Err(CodecError::DisabledPhase(Phase::from(phase)));
        }
        if matches!(
            phase,
            ForwardPhase::PreviousArchiveIntent | ForwardPhase::PreviousArchived
        ) && !self.options.archive_previous
        {
            return Err(CodecError::DisabledPhase(Phase::from(phase)));
        }
        if matches!(phase, ForwardPhase::BootSyncStarted | ForwardPhase::BootSyncComplete)
            && !self.options.run_boot_sync
        {
            return Err(CodecError::DisabledPhase(Phase::from(phase)));
        }
        Ok(())
    }

    fn validate_candidate_layout(&self, phase: ForwardPhase) -> Result<(), CodecError> {
        match self.operation {
            Operation::NewState => {
                let id_required = match phase {
                    ForwardPhase::Preparing => false,
                    ForwardPhase::FreshStateAllocating => {
                        let Some(rollback) = self.rollback.as_ref() else {
                            return require_option_presence(self.candidate.id, false, CodecError::CandidateStateLayout);
                        };
                        match rollback.fresh_db {
                            RollbackAction::Pending | RollbackAction::Applied => true,
                            // An absent allocation is already satisfied with no
                            // ID, while a concurrently removed observed row
                            // retains its immutable ID as recovery evidence.
                            RollbackAction::AlreadySatisfied => return Ok(()),
                            RollbackAction::NotRequired => false,
                        }
                    }
                    ForwardPhase::FreshStateAllocated
                    | ForwardPhase::CandidatePrepareStarted
                    | ForwardPhase::CandidatePrepared
                    | ForwardPhase::TransactionTriggersStarted
                    | ForwardPhase::TransactionTriggersComplete
                    | ForwardPhase::UsrExchangeIntent
                    | ForwardPhase::UsrExchanged
                    | ForwardPhase::RootLinksComplete
                    | ForwardPhase::SystemTriggersStarted
                    | ForwardPhase::SystemTriggersComplete
                    | ForwardPhase::PreviousArchiveIntent
                    | ForwardPhase::PreviousArchived
                    | ForwardPhase::BootSyncStarted
                    | ForwardPhase::BootSyncComplete
                    | ForwardPhase::CommitDecided
                    | ForwardPhase::CommitCleanupComplete
                    | ForwardPhase::Complete => true,
                };
                require_option_presence(self.candidate.id, id_required, CodecError::CandidateStateLayout)?;
            }
            Operation::ActivateArchived | Operation::ActiveReblit => {
                if self.candidate.id.is_none() {
                    return Err(CodecError::ExistingCandidateStateMissing);
                }
            }
        }
        Ok(())
    }

    fn validate_relationships(&self) -> Result<(), CodecError> {
        if self.candidate.usr_identity == self.previous.usr_identity {
            return Err(CodecError::CandidatePreviousIdentityCollision);
        }

        let archive_previous = matches!(self.previous.origin, PreviousOrigin::ActiveState);
        if self.options.archive_previous != archive_previous {
            return Err(CodecError::ArchiveOptionMismatch {
                origin: self.previous.origin,
                archive_previous: self.options.archive_previous,
            });
        }

        match self.operation {
            Operation::ActiveReblit => {
                if self.previous.origin != PreviousOrigin::ActiveReblitCorrupt
                    || self.previous.id.is_none()
                    || self.candidate.id != self.previous.id
                {
                    return Err(CodecError::ActiveReblitStateMismatch);
                }
            }
            Operation::ActivateArchived => {
                if self.previous.origin != PreviousOrigin::ActiveState
                    || self.previous.id.is_none()
                    || self.candidate.id.is_none()
                {
                    return Err(CodecError::ArchivedActivationStateMismatch);
                }
                if self.candidate.id.is_some() && self.candidate.id == self.previous.id {
                    return Err(CodecError::CandidatePreviousStateCollision);
                }
            }
            Operation::NewState => {
                match self.previous.origin {
                    PreviousOrigin::ActiveState if self.previous.id.is_none() => {
                        return Err(CodecError::PreviousOriginStateMismatch {
                            origin: self.previous.origin,
                            state_id: self.previous.id,
                        });
                    }
                    PreviousOrigin::SynthesizedEmpty | PreviousOrigin::Unmanaged if self.previous.id.is_some() => {
                        return Err(CodecError::PreviousOriginStateMismatch {
                            origin: self.previous.origin,
                            state_id: self.previous.id,
                        });
                    }
                    PreviousOrigin::ActiveReblitCorrupt => {
                        return Err(CodecError::PreviousOriginOperationMismatch {
                            operation: self.operation,
                            origin: self.previous.origin,
                        });
                    }
                    _ => {}
                }
                if self.candidate.id.is_some() && self.candidate.id == self.previous.id {
                    return Err(CodecError::CandidatePreviousStateCollision);
                }
            }
        }
        Ok(())
    }

    fn runs_transaction_triggers(&self) -> bool {
        matches!(self.operation, Operation::NewState | Operation::ActiveReblit)
    }

    fn candidate_disposition_for(&self, source: ForwardPhase) -> AbortDisposition {
        match self.operation {
            Operation::NewState | Operation::ActiveReblit => AbortDisposition::Quarantine,
            Operation::ActivateArchived if source == ForwardPhase::SystemTriggersStarted => {
                AbortDisposition::Quarantine
            }
            Operation::ActivateArchived => AbortDisposition::Rearchive,
        }
    }

    pub(crate) fn commit_disposition(&self) -> CommitDisposition {
        match self.previous.origin {
            PreviousOrigin::ActiveState => CommitDisposition::Archive,
            PreviousOrigin::ActiveReblitCorrupt | PreviousOrigin::SynthesizedEmpty => CommitDisposition::Discard,
            PreviousOrigin::Unmanaged => CommitDisposition::Quarantine,
        }
    }

    fn validate_rollback_plan(&self, rollback: &RollbackPlan) -> Result<(), CodecError> {
        validate_rollback_requirement(
            "previous-archive",
            rollback.previous_archive,
            self.options.archive_previous && rollback.source.ordinal() >= ForwardPhase::PreviousArchiveIntent.ordinal(),
        )?;
        validate_rollback_requirement(
            "usr-exchange",
            rollback.usr_exchange,
            rollback.source.ordinal() >= ForwardPhase::UsrExchangeIntent.ordinal(),
        )?;
        validate_rollback_requirement("candidate", rollback.candidate.action, true)?;
        validate_rollback_requirement(
            "fresh-db",
            rollback.fresh_db,
            matches!(self.operation, Operation::NewState)
                && rollback.source.ordinal() >= ForwardPhase::FreshStateAllocating.ordinal(),
        )?;

        let boot_possible = rollback.source == ForwardPhase::BootSyncStarted;
        match (boot_possible, rollback.boot) {
            (false, BootRollback::NotRequired)
            | (true, BootRollback::PendingUnverifiable | BootRollback::Unverified) => {}
            _ => return Err(CodecError::InvalidBootRollbackRequirement),
        }

        let expected_disposition = self.candidate_disposition_for(rollback.source);
        if rollback.candidate.disposition != expected_disposition {
            return Err(CodecError::InvalidCandidateDisposition {
                operation: self.operation,
                rollback_source: rollback.source,
                expected: expected_disposition,
                actual: rollback.candidate.disposition,
            });
        }

        let external_effects_may_remain = (self.runs_transaction_triggers()
            && rollback.source.ordinal() >= ForwardPhase::TransactionTriggersStarted.ordinal())
            || (self.options.run_system_triggers
                && rollback.source.ordinal() >= ForwardPhase::SystemTriggersStarted.ordinal())
            || rollback.source == ForwardPhase::BootSyncStarted;
        if rollback.external_effects_may_remain != external_effects_may_remain {
            return Err(CodecError::InvalidExternalEffectsEvidence {
                expected: external_effects_may_remain,
                actual: rollback.external_effects_may_remain,
            });
        }
        Ok(())
    }
}

impl TreeIdentity {
    fn validate(self) -> Result<(), CodecError> {
        if self.st_dev == 0 || self.inode == 0 || self.statx_mount_id == 0 {
            return Err(CodecError::ZeroTreeIdentity);
        }
        Ok(())
    }
}

fn require_option_presence<T>(value: Option<T>, required: bool, error: CodecError) -> Result<(), CodecError> {
    if value.is_some() != required {
        return Err(error);
    }
    Ok(())
}

fn next_forward_phase(record: &TransitionRecord, current: ForwardPhase) -> Option<ForwardPhase> {
    let after_system = || {
        if record.options.archive_previous {
            ForwardPhase::PreviousArchiveIntent
        } else if record.options.run_boot_sync {
            ForwardPhase::BootSyncStarted
        } else {
            ForwardPhase::CommitDecided
        }
    };
    let after_archive = || {
        if record.options.run_boot_sync {
            ForwardPhase::BootSyncStarted
        } else {
            ForwardPhase::CommitDecided
        }
    };
    Some(match current {
        ForwardPhase::Preparing if matches!(record.operation, Operation::NewState) => {
            ForwardPhase::FreshStateAllocating
        }
        ForwardPhase::Preparing => ForwardPhase::CandidatePrepareStarted,
        ForwardPhase::FreshStateAllocating => ForwardPhase::FreshStateAllocated,
        ForwardPhase::FreshStateAllocated => ForwardPhase::CandidatePrepareStarted,
        ForwardPhase::CandidatePrepareStarted => ForwardPhase::CandidatePrepared,
        ForwardPhase::CandidatePrepared if record.runs_transaction_triggers() => {
            ForwardPhase::TransactionTriggersStarted
        }
        ForwardPhase::CandidatePrepared => ForwardPhase::UsrExchangeIntent,
        ForwardPhase::TransactionTriggersStarted => ForwardPhase::TransactionTriggersComplete,
        ForwardPhase::TransactionTriggersComplete => ForwardPhase::UsrExchangeIntent,
        ForwardPhase::UsrExchangeIntent => ForwardPhase::UsrExchanged,
        ForwardPhase::UsrExchanged => ForwardPhase::RootLinksComplete,
        ForwardPhase::RootLinksComplete if record.options.run_system_triggers => ForwardPhase::SystemTriggersStarted,
        ForwardPhase::RootLinksComplete => after_system(),
        ForwardPhase::SystemTriggersStarted => ForwardPhase::SystemTriggersComplete,
        ForwardPhase::SystemTriggersComplete => after_system(),
        ForwardPhase::PreviousArchiveIntent => ForwardPhase::PreviousArchived,
        ForwardPhase::PreviousArchived => after_archive(),
        ForwardPhase::BootSyncStarted => ForwardPhase::BootSyncComplete,
        ForwardPhase::BootSyncComplete => ForwardPhase::CommitDecided,
        ForwardPhase::CommitDecided => ForwardPhase::CommitCleanupComplete,
        ForwardPhase::CommitCleanupComplete => ForwardPhase::Complete,
        ForwardPhase::Complete => return None,
    })
}

fn rollback_allowed(_record: &TransitionRecord, source: ForwardPhase) -> bool {
    source.ordinal() < ForwardPhase::CommitDecided.ordinal() && source != ForwardPhase::BootSyncComplete
}

fn validate_rollback_requirement(
    action: &'static str,
    status: RollbackAction,
    possible: bool,
) -> Result<(), CodecError> {
    if possible == status.required() {
        Ok(())
    } else {
        Err(CodecError::InvalidRollbackRequirement {
            action,
            status,
            possible,
        })
    }
}

fn rollback_actions(plan: &RollbackPlan) -> [RollbackAction; 4] {
    [
        plan.previous_archive,
        plan.usr_exchange,
        plan.candidate.action,
        plan.fresh_db,
    ]
}

fn ordinary_actions_resolved(plan: &RollbackPlan) -> bool {
    rollback_actions(plan)
        .into_iter()
        .all(|action| action == RollbackAction::NotRequired || action.resolved())
}

fn rollback_action_phase(phase: Phase) -> Option<(usize, bool)> {
    Some(match phase {
        Phase::PreviousRestoreIntent => (0, false),
        Phase::PreviousRestoredToStaging => (0, true),
        Phase::ReverseExchangeIntent => (1, false),
        Phase::UsrRestored => (1, true),
        Phase::CandidatePreserveIntent => (2, false),
        Phase::CandidatePreserved => (2, true),
        Phase::FreshDbInvalidationIntent => (3, false),
        Phase::FreshDbInvalidated => (3, true),
        _ => return None,
    })
}

fn validate_rollback_phase(phase: Phase, plan: &RollbackPlan) -> Result<(), CodecError> {
    let actions = rollback_actions(plan);
    let matches_phase = match phase {
        Phase::RollbackDecided => {
            actions.into_iter().all(|action| action != RollbackAction::Applied) && plan.boot != BootRollback::Unverified
        }
        Phase::BootRepairRequired | Phase::BootRepairStarted => {
            ordinary_actions_resolved(plan) && plan.boot == BootRollback::PendingUnverifiable
        }
        Phase::BootRepairUnverified => ordinary_actions_resolved(plan) && plan.boot == BootRollback::Unverified,
        Phase::RollbackComplete => ordinary_actions_resolved(plan) && plan.boot == BootRollback::NotRequired,
        _ => {
            let Some((current, completed)) = rollback_action_phase(phase) else {
                return Err(CodecError::RollbackPlanOnForwardPhase);
            };
            let prior_resolved = actions[..current]
                .iter()
                .all(|action| *action == RollbackAction::NotRequired || action.resolved());
            let current_matches = if completed {
                actions[current].resolved()
            } else {
                actions[current] == RollbackAction::Pending
            };
            let later_unapplied = actions[current + 1..]
                .iter()
                .all(|action| *action != RollbackAction::Applied);
            prior_resolved && current_matches && later_unapplied && plan.boot != BootRollback::Unverified
        }
    };
    if matches_phase {
        Ok(())
    } else {
        Err(CodecError::RollbackPlanPhaseMismatch { phase })
    }
}

fn next_rollback_phase(plan: &RollbackPlan, current: Phase) -> Option<Phase> {
    match current {
        Phase::PreviousRestoreIntent => return Some(Phase::PreviousRestoredToStaging),
        Phase::ReverseExchangeIntent => return Some(Phase::UsrRestored),
        Phase::CandidatePreserveIntent => return Some(Phase::CandidatePreserved),
        Phase::FreshDbInvalidationIntent => return Some(Phase::FreshDbInvalidated),
        Phase::BootRepairRequired => return Some(Phase::BootRepairStarted),
        Phase::BootRepairStarted => return Some(Phase::BootRepairUnverified),
        Phase::BootRepairUnverified | Phase::RollbackComplete => return None,
        Phase::RollbackDecided
        | Phase::PreviousRestoredToStaging
        | Phase::UsrRestored
        | Phase::CandidatePreserved
        | Phase::FreshDbInvalidated => {}
        _ => return None,
    }

    for (action, intent) in rollback_actions(plan).into_iter().zip([
        Phase::PreviousRestoreIntent,
        Phase::ReverseExchangeIntent,
        Phase::CandidatePreserveIntent,
        Phase::FreshDbInvalidationIntent,
    ]) {
        if action == RollbackAction::Pending {
            return Some(intent);
        }
    }
    match plan.boot {
        BootRollback::PendingUnverifiable => Some(Phase::BootRepairRequired),
        BootRollback::NotRequired => Some(Phase::RollbackComplete),
        BootRollback::Unverified => None,
    }
}

fn validate_rollback_plan_advance(
    expected: &RollbackPlan,
    next: &RollbackPlan,
    current_phase: Phase,
    next_phase: Phase,
) -> Result<(), CodecError> {
    if expected.source != next.source
        || expected.candidate.disposition != next.candidate.disposition
        || expected.external_effects_may_remain != next.external_effects_may_remain
    {
        return Err(CodecError::RollbackPlanChangedIllegally);
    }

    let completed_action = match (current_phase, next_phase) {
        (Phase::PreviousRestoreIntent, Phase::PreviousRestoredToStaging) => Some(0),
        (Phase::ReverseExchangeIntent, Phase::UsrRestored) => Some(1),
        (Phase::CandidatePreserveIntent, Phase::CandidatePreserved) => Some(2),
        (Phase::FreshDbInvalidationIntent, Phase::FreshDbInvalidated) => Some(3),
        _ => None,
    };
    for (index, (before, after)) in rollback_actions(expected)
        .into_iter()
        .zip(rollback_actions(next))
        .enumerate()
    {
        if Some(index) == completed_action {
            if before != RollbackAction::Pending || !after.resolved() {
                return Err(CodecError::RollbackPlanChangedIllegally);
            }
        } else if before != after {
            return Err(CodecError::RollbackPlanChangedIllegally);
        }
    }

    if (current_phase, next_phase) == (Phase::BootRepairStarted, Phase::BootRepairUnverified) {
        if expected.boot != BootRollback::PendingUnverifiable || next.boot != BootRollback::Unverified {
            return Err(CodecError::RollbackPlanChangedIllegally);
        }
    } else if expected.boot != next.boot {
        return Err(CodecError::RollbackPlanChangedIllegally);
    }
    Ok(())
}

fn validate_advance(expected: &TransitionRecord, next: &TransitionRecord) -> Result<(), CodecError> {
    expected.validate()?;
    next.validate()?;

    if expected.phase.blocks_advance() {
        return Err(CodecError::TerminalPhaseAdvance);
    }

    let expected_generation = expected
        .generation
        .checked_add(1)
        .ok_or(CodecError::GenerationExhausted)?;
    if next.generation != expected_generation {
        return Err(CodecError::GenerationMismatch {
            expected: expected_generation,
            actual: next.generation,
        });
    }
    if expected.transition_id != next.transition_id {
        return Err(CodecError::TransitionChanged);
    }
    if expected.format != next.format
        || expected.version != next.version
        || expected.operation != next.operation
        || expected.previous != next.previous
        || expected.options != next.options
        || expected.quarantine_name != next.quarantine_name
        || expected.candidate.origin != next.candidate.origin
        || expected.candidate.usr_identity != next.candidate.usr_identity
    {
        return Err(CodecError::ImmutableTransitionDataChanged);
    }

    match (expected.phase.forward(), next.phase.forward()) {
        (Some(current), Some(actual)) => {
            let wanted = next_forward_phase(expected, current).ok_or(CodecError::TerminalPhaseAdvance)?;
            if actual != wanted {
                return Err(CodecError::IllegalPhaseAdvance {
                    current: expected.phase,
                    next: next.phase,
                });
            }
        }
        (Some(current), None) => {
            let rollback_source = next.rollback.as_ref().map(|rollback| rollback.source);
            if next.phase != Phase::RollbackDecided
                || !rollback_allowed(expected, current)
                || rollback_source != Some(current)
            {
                return Err(CodecError::IllegalPhaseAdvance {
                    current: expected.phase,
                    next: next.phase,
                });
            }
        }
        (None, Some(_)) => {
            return Err(CodecError::IllegalPhaseAdvance {
                current: expected.phase,
                next: next.phase,
            });
        }
        (None, None) => {
            let expected_plan = expected.rollback.as_ref().expect("validated rollback plan");
            let next_plan = next.rollback.as_ref().expect("validated rollback plan");
            if next_rollback_phase(expected_plan, expected.phase) != Some(next.phase) {
                return Err(CodecError::IllegalPhaseAdvance {
                    current: expected.phase,
                    next: next.phase,
                });
            }
            validate_rollback_plan_advance(expected_plan, next_plan, expected.phase, next.phase)?;
        }
    }

    let allocation_completed = expected.phase == Phase::FreshStateAllocating
        && next.phase == Phase::FreshStateAllocated
        && expected.candidate.id.is_none()
        && next.candidate.id.is_some();
    let allocation_observed_during_rollback = expected.phase == Phase::FreshStateAllocating
        && next.phase == Phase::RollbackDecided
        && expected.candidate.id.is_none()
        && next.candidate.id.is_some()
        && next
            .rollback
            .as_ref()
            .is_some_and(|rollback| rollback.fresh_db == RollbackAction::Pending);
    if expected.candidate.id != next.candidate.id && !allocation_completed && !allocation_observed_during_rollback {
        return Err(CodecError::CandidateStateChangedIllegally);
    }
    Ok(())
}

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
    #[error("journal generation must be nonzero")]
    ZeroGeneration,
    #[error("journal generation counter is exhausted")]
    GenerationExhausted,
    #[error("next journal generation must be {expected}, not {actual}")]
    GenerationMismatch { expected: u64, actual: u64 },
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
    #[error("tree identities must contain nonzero device, inode, and mount IDs")]
    ZeroTreeIdentity,
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
    #[error("candidate and previous /usr identities must be distinct")]
    CandidatePreviousIdentityCollision,
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

fn enforce_record_size(size: usize) -> Result<(), CodecError> {
    if size > MAX_CANONICAL_RECORD_BYTES {
        return Err(CodecError::RecordTooLarge(size));
    }
    Ok(())
}

fn checksum(version: &[u8], length: &[u8], payload: &[u8]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(MAGIC);
    digest.update(version);
    digest.update(length);
    digest.update(payload);
    digest.finalize().into()
}

#[derive(Debug)]
pub(crate) struct TransitionJournalStore {
    directory: std::fs::File,
    _lock: std::fs::File,
    operation_lock: Mutex<()>,
    path: PathBuf,
}

#[derive(Debug)]
struct LoadedRecord {
    record: TransitionRecord,
    _file: std::fs::File,
    identity: InodeIdentity,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct InodeIdentity {
    device: u64,
    inode: u64,
}

#[derive(Debug)]
struct TemporaryRecord {
    name: CString,
    file: std::fs::File,
    identity: InodeIdentity,
}

impl TransitionJournalStore {
    /// Open the owner-controlled `.cast/journal` directory, creating only its
    /// final fixed component when absent.
    pub(crate) fn open(root: &Path) -> Result<Self, StorageError> {
        let root_directory = open_directory_path(root).map_err(|source| StorageError::OpenRoot {
            path: root.to_owned(),
            source,
        })?;
        let cast_path = root.join(".cast");
        let cast = open_existing_directory(&root_directory, c".cast", &cast_path, DirectoryPolicy::Controlled)
            .map_err(|source| StorageError::OpenCastDirectory {
                path: cast_path.clone(),
                source,
            })?;
        let path = cast_path.join("journal");
        let directory =
            ensure_journal_directory(&cast, &path).map_err(|source| StorageError::OpenJournalDirectory {
                path: path.clone(),
                source,
            })?;
        let lock = open_and_lock(&directory, &path)?;
        let store = Self {
            directory,
            _lock: lock,
            operation_lock: Mutex::new(()),
            path,
        };
        store.cleanup_stale_temporaries()?;
        Ok(store)
    }

    /// Read only the canonical record. Temporary files are never recovery
    /// candidates, including when the canonical file is corrupt or absent.
    pub(crate) fn load(&self) -> Result<Option<TransitionRecord>, StorageError> {
        let _operation = self.lock_operation()?;
        Ok(self.load_pinned()?.map(|loaded| loaded.record))
    }

    /// Durably create the first `preparing` record for one transaction.
    pub(crate) fn create(&self, record: &TransitionRecord) -> Result<(), StorageError> {
        let _operation = self.lock_operation()?;
        let framed = encode(record).map_err(StorageError::Encode)?;
        if record.generation != 1 || record.phase != Phase::Preparing || record.rollback.is_some() {
            return Err(StorageError::InvalidCreationRecord);
        }
        if self.load_pinned()?.is_some() {
            return Err(StorageError::CanonicalAlreadyExists);
        }
        self.publish_record(&framed, None)
    }

    /// Conditionally advance the exact current record by one legal phase and
    /// one generation. A stale caller can never overwrite newer evidence.
    pub(crate) fn advance(&self, expected: &TransitionRecord, next: &TransitionRecord) -> Result<(), StorageError> {
        let _operation = self.lock_operation()?;
        validate_advance(expected, next).map_err(StorageError::InvalidAdvance)?;
        let framed = encode(next).map_err(StorageError::Encode)?;
        let Some(existing) = self.load_pinned()? else {
            return Err(StorageError::CanonicalMissing);
        };
        if existing.record != *expected {
            return Err(StorageError::ExpectedRecordMismatch);
        }
        self.publish_record(&framed, Some(existing))
    }

    fn publish_record(&self, framed: &[u8], existing: Option<LoadedRecord>) -> Result<(), StorageError> {
        let mut temporary = self.create_temporary()?;
        if let Err(source) = temporary.file.write_all(&framed) {
            self.cleanup_temporary(&temporary)?;
            return Err(StorageError::WriteTemporary { source });
        }
        // Full fsync persists both record contents and the exact 0600 mode set
        // after exclusive creation.
        if let Err(source) = storage_fault(StorageFaultPoint::TemporarySync).and_then(|()| temporary.file.sync_all()) {
            self.cleanup_temporary(&temporary)?;
            return Err(StorageError::SyncTemporary { source });
        }
        durability_checkpoint(DurabilityCheckpoint::TemporaryFullySynced);
        if let Err(source) = require_safe_regular_file(
            &temporary.file,
            &self.path.join(temporary.name.to_string_lossy().as_ref()),
        ) {
            self.cleanup_temporary(&temporary)?;
            return Err(StorageError::ValidateTemporary { source });
        }
        temporary.identity = match inode_identity(&temporary.file) {
            Ok(identity) => identity,
            Err(source) => {
                self.cleanup_temporary(&temporary)?;
                return Err(StorageError::ValidateTemporary { source });
            }
        };

        match existing {
            None => self.publish_initial(&temporary),
            Some(existing) => self.publish_update(&temporary, &existing),
        }
    }

    /// Remove the canonical record after validating both its storage metadata
    /// and payload. Returns false only when the canonical name is absent.
    pub(crate) fn delete(&self, expected: &TransitionRecord) -> Result<bool, StorageError> {
        let _operation = self.lock_operation()?;
        expected.validate().map_err(StorageError::Decode)?;
        if !expected.phase.deletable() {
            return Err(StorageError::DeleteNonterminal);
        }
        let Some(existing) = self.load_pinned()? else {
            return Ok(false);
        };
        if existing.record != *expected {
            return Err(StorageError::ExpectedRecordMismatch);
        }
        let named = self.open_named(CANONICAL_NAME)?.ok_or(StorageError::CanonicalChanged)?;
        require_same_inode(
            existing.identity,
            inode_identity(&named).map_err(|source| StorageError::ValidateCanonical { source })?,
        )?;
        storage_fault(StorageFaultPoint::CanonicalUnlink)
            .and_then(|()| unlinkat(self.directory.as_raw_fd(), CANONICAL_NAME))
            .map_err(|source| StorageError::DeleteCanonical { source })?;
        durability_checkpoint(DurabilityCheckpoint::CanonicalUnlinked);
        storage_fault(StorageFaultPoint::DeleteDirectorySync)
            .and_then(|()| self.directory.sync_all())
            .map_err(|source| StorageError::SyncJournalDirectory { source })?;
        durability_checkpoint(DurabilityCheckpoint::JournalDirectorySynced);
        Ok(true)
    }

    fn lock_operation(&self) -> Result<MutexGuard<'_, ()>, StorageError> {
        self.operation_lock
            .lock()
            .map_err(|_| StorageError::OperationLockPoisoned)
    }

    fn load_pinned(&self) -> Result<Option<LoadedRecord>, StorageError> {
        let Some(mut file) = self.open_named(CANONICAL_NAME)? else {
            return Ok(None);
        };
        let identity = inode_identity(&file).map_err(|source| StorageError::ValidateCanonical { source })?;
        let framed = read_bounded(&mut file).map_err(|source| StorageError::ReadCanonical { source })?;
        require_safe_regular_file(&file, &self.path.join("state-transition"))
            .map_err(|source| StorageError::ValidateCanonical { source })?;
        let record = decode(&framed).map_err(StorageError::Decode)?;
        Ok(Some(LoadedRecord {
            record,
            _file: file,
            identity,
        }))
    }

    fn open_named(&self, name: &CStr) -> Result<Option<std::fs::File>, StorageError> {
        match openat2_file(
            self.directory.as_raw_fd(),
            name,
            nix::libc::O_RDONLY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK,
            0,
            controlled_resolution(),
        ) {
            Ok(file) => {
                require_safe_regular_file(&file, &self.path.join(name.to_string_lossy().as_ref()))
                    .map_err(|source| StorageError::ValidateCanonical { source })?;
                Ok(Some(file))
            }
            Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(StorageError::OpenCanonical { source }),
        }
    }

    fn create_temporary(&self) -> Result<TemporaryRecord, StorageError> {
        const MAX_ATTEMPTS: usize = 128;
        for _ in 0..MAX_ATTEMPTS {
            let name = temporary_name();
            match openat2_file(
                self.directory.as_raw_fd(),
                &name,
                nix::libc::O_WRONLY
                    | nix::libc::O_CLOEXEC
                    | nix::libc::O_NOFOLLOW
                    | nix::libc::O_NONBLOCK
                    | nix::libc::O_CREAT
                    | nix::libc::O_EXCL,
                JOURNAL_FILE_MODE,
                controlled_resolution(),
            ) {
                Ok(file) => {
                    let identity = match inode_identity(&file) {
                        Ok(identity) => identity,
                        Err(source) => {
                            unlinkat(self.directory.as_raw_fd(), &name).map_err(|cleanup| {
                                StorageError::CleanupTemporary {
                                    name: name.to_string_lossy().into_owned(),
                                    source: cleanup,
                                }
                            })?;
                            self.directory
                                .sync_all()
                                .map_err(|source| StorageError::SyncJournalDirectory { source })?;
                            return Err(StorageError::ValidateTemporary { source });
                        }
                    };
                    if let Err(source) = file.set_permissions(std::fs::Permissions::from_mode(JOURNAL_FILE_MODE)) {
                        self.cleanup_temporary_identity(&name, identity)?;
                        return Err(StorageError::CreateTemporary { source });
                    }
                    if let Err(source) =
                        require_safe_regular_file(&file, &self.path.join(name.to_string_lossy().as_ref()))
                    {
                        self.cleanup_temporary_identity(&name, identity)?;
                        return Err(StorageError::ValidateTemporary { source });
                    }
                    return Ok(TemporaryRecord { name, file, identity });
                }
                Err(source) if source.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(source) => return Err(StorageError::CreateTemporary { source }),
            }
        }
        Err(StorageError::TemporaryNamesExhausted)
    }

    fn publish_initial(&self, temporary: &TemporaryRecord) -> Result<(), StorageError> {
        if let Err(source) = storage_fault(StorageFaultPoint::InitialRename).and_then(|()| {
            renameat2(
                self.directory.as_raw_fd(),
                &temporary.name,
                self.directory.as_raw_fd(),
                CANONICAL_NAME,
                nix::libc::RENAME_NOREPLACE,
            )
        }) {
            self.cleanup_temporary(temporary)?;
            return Err(StorageError::PublishCanonical { source });
        }
        durability_checkpoint(DurabilityCheckpoint::CanonicalPublished);
        let canonical = self.open_named(CANONICAL_NAME)?.ok_or(StorageError::CanonicalChanged)?;
        require_same_inode(
            temporary.identity,
            inode_identity(&canonical).map_err(|source| StorageError::ValidateCanonical { source })?,
        )?;
        storage_fault(StorageFaultPoint::InitialDirectorySync)
            .and_then(|()| self.directory.sync_all())
            .map_err(|source| StorageError::SyncJournalDirectory { source })?;
        durability_checkpoint(DurabilityCheckpoint::JournalDirectorySynced);
        Ok(())
    }

    fn publish_update(&self, temporary: &TemporaryRecord, existing: &LoadedRecord) -> Result<(), StorageError> {
        // Reauthenticate the fixed canonical name immediately before the
        // exchange. The retained descriptor keeps the decoded inode pinned.
        let named = self.open_named(CANONICAL_NAME)?.ok_or(StorageError::CanonicalChanged)?;
        require_same_inode(
            existing.identity,
            inode_identity(&named).map_err(|source| StorageError::ValidateCanonical { source })?,
        )?;

        if let Err(source) = storage_fault(StorageFaultPoint::UpdateExchange).and_then(|()| {
            renameat2(
                self.directory.as_raw_fd(),
                &temporary.name,
                self.directory.as_raw_fd(),
                CANONICAL_NAME,
                nix::libc::RENAME_EXCHANGE,
            )
        }) {
            self.cleanup_temporary(temporary)?;
            return Err(StorageError::PublishCanonical { source });
        }
        durability_checkpoint(DurabilityCheckpoint::CanonicalExchanged);

        // After exchange, the canonical name must identify the fdatasynced
        // inode and the temporary name must identify the old decoded inode.
        // If either proof fails, preserve both names for diagnosis.
        let canonical = self.open_named(CANONICAL_NAME)?.ok_or(StorageError::CanonicalChanged)?;
        require_same_inode(
            temporary.identity,
            inode_identity(&canonical).map_err(|source| StorageError::ValidateCanonical { source })?,
        )?;
        let displaced = self
            .open_named(&temporary.name)?
            .ok_or(StorageError::CanonicalChanged)?;
        require_same_inode(
            existing.identity,
            inode_identity(&displaced).map_err(|source| StorageError::ValidateCanonical { source })?,
        )?;

        storage_fault(StorageFaultPoint::UpdateFirstDirectorySync)
            .and_then(|()| self.directory.sync_all())
            .map_err(|source| StorageError::SyncJournalDirectory { source })?;
        durability_checkpoint(DurabilityCheckpoint::JournalDirectorySynced);
        storage_fault(StorageFaultPoint::DisplacedUnlink)
            .and_then(|()| unlinkat(self.directory.as_raw_fd(), &temporary.name))
            .map_err(|source| StorageError::DeleteDisplaced { source })?;
        durability_checkpoint(DurabilityCheckpoint::DisplacedUnlinked);
        storage_fault(StorageFaultPoint::UpdateFinalDirectorySync)
            .and_then(|()| self.directory.sync_all())
            .map_err(|source| StorageError::SyncJournalDirectory { source })?;
        durability_checkpoint(DurabilityCheckpoint::JournalDirectorySynced);
        Ok(())
    }

    fn cleanup_temporary(&self, temporary: &TemporaryRecord) -> Result<(), StorageError> {
        self.cleanup_temporary_identity(&temporary.name, temporary.identity)
    }

    fn cleanup_temporary_identity(&self, name: &CStr, expected: InodeIdentity) -> Result<(), StorageError> {
        let display = name.to_string_lossy().into_owned();
        let named = openat2_file(
            self.directory.as_raw_fd(),
            name,
            nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
            0,
            controlled_resolution(),
        )
        .map_err(|source| StorageError::CleanupTemporary {
            name: display.clone(),
            source,
        })?;
        let actual = inode_identity(&named).map_err(|source| StorageError::CleanupTemporary {
            name: display.clone(),
            source,
        })?;
        if actual != expected {
            return Err(StorageError::CanonicalChanged);
        }
        unlinkat(self.directory.as_raw_fd(), name)
            .map_err(|source| StorageError::CleanupTemporary { name: display, source })?;
        self.directory
            .sync_all()
            .map_err(|source| StorageError::SyncJournalDirectory { source })
    }

    fn cleanup_stale_temporaries(&self) -> Result<(), StorageError> {
        let entries = directory_entries(&self.directory).map_err(|source| StorageError::EnumerateJournal { source })?;
        let mut stale = Vec::new();
        for name in entries {
            match name.to_bytes() {
                b"state-transition" | b"state-transition.lock" => {}
                bytes if valid_temporary_name(bytes) => {
                    stale.push(name);
                    if stale.len() > MAX_STALE_TEMPORARIES {
                        return Err(StorageError::TooManyStaleTemporaries);
                    }
                }
                _ => {
                    return Err(StorageError::UnexpectedJournalEntry(
                        name.to_string_lossy().into_owned(),
                    ));
                }
            }
        }
        let mut authenticated = Vec::with_capacity(stale.len());
        for name in stale {
            let display = name.to_string_lossy().into_owned();
            let file = openat2_file(
                self.directory.as_raw_fd(),
                &name,
                nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
                0,
                controlled_resolution(),
            )
            .map_err(|source| StorageError::ValidateStaleTemporary {
                name: display.clone(),
                source,
            })?;
            require_safe_stale_temporary(&file, &self.path.join(&display)).map_err(|source| {
                StorageError::ValidateStaleTemporary {
                    name: display.clone(),
                    source,
                }
            })?;
            let identity = inode_identity(&file)
                .map_err(|source| StorageError::ValidateStaleTemporary { name: display, source })?;
            authenticated.push((name, identity));
        }
        for (name, identity) in authenticated {
            self.cleanup_temporary_identity(&name, identity)?;
        }
        Ok(())
    }
}

#[derive(Debug, Error)]
pub(crate) enum StorageError {
    #[error("the in-process transition-journal operation lock is poisoned")]
    OperationLockPoisoned,
    #[error("open transition-journal root `{}`", path.display())]
    OpenRoot {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("open owner-controlled Cast directory `{}`", path.display())]
    OpenCastDirectory {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("open owner-controlled journal directory `{}`", path.display())]
    OpenJournalDirectory {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("open canonical state-transition journal")]
    OpenCanonical {
        #[source]
        source: io::Error,
    },
    #[error("validate canonical state-transition journal")]
    ValidateCanonical {
        #[source]
        source: io::Error,
    },
    #[error("read canonical state-transition journal")]
    ReadCanonical {
        #[source]
        source: io::Error,
    },
    #[error("decode canonical state-transition journal")]
    Decode(#[source] CodecError),
    #[error("encode state-transition journal")]
    Encode(#[source] CodecError),
    #[error("the initial journal record must be generation 1 in preparing phase")]
    InvalidCreationRecord,
    #[error("canonical state-transition journal already exists")]
    CanonicalAlreadyExists,
    #[error("canonical state-transition journal is absent")]
    CanonicalMissing,
    #[error("canonical state-transition journal does not match the caller's exact expected record")]
    ExpectedRecordMismatch,
    #[error("invalid state-transition journal advance")]
    InvalidAdvance(#[source] CodecError),
    #[error("a nonterminal state-transition journal cannot be deleted")]
    DeleteNonterminal,
    #[error("create or open the internal transition-journal lock")]
    OpenLock {
        #[source]
        source: io::Error,
    },
    #[error("validate the internal transition-journal lock")]
    ValidateLock {
        #[source]
        source: io::Error,
    },
    #[error("acquire the internal exclusive transition-journal lock")]
    AcquireLock {
        #[source]
        source: io::Error,
    },
    #[error("journal directory contains unexpected entry `{0}`")]
    UnexpectedJournalEntry(String),
    #[error("journal contains more than {MAX_STALE_TEMPORARIES} stale temporary records")]
    TooManyStaleTemporaries,
    #[error("enumerate bounded transition-journal entries")]
    EnumerateJournal {
        #[source]
        source: io::Error,
    },
    #[error("validate stale transition-journal temporary `{name}`")]
    ValidateStaleTemporary {
        name: String,
        #[source]
        source: io::Error,
    },
    #[error("remove authenticated transition-journal temporary `{name}`")]
    CleanupTemporary {
        name: String,
        #[source]
        source: io::Error,
    },
    #[error("create an exclusive state-transition temporary file")]
    CreateTemporary {
        #[source]
        source: io::Error,
    },
    #[error("all bounded state-transition temporary names already exist")]
    TemporaryNamesExhausted,
    #[error("validate state-transition temporary file")]
    ValidateTemporary {
        #[source]
        source: io::Error,
    },
    #[error("write complete state-transition temporary file")]
    WriteTemporary {
        #[source]
        source: io::Error,
    },
    #[error("fdatasync state-transition temporary file")]
    SyncTemporary {
        #[source]
        source: io::Error,
    },
    #[error("atomically publish state-transition journal")]
    PublishCanonical {
        #[source]
        source: io::Error,
    },
    #[error("canonical state-transition inode changed during the operation")]
    CanonicalChanged,
    #[error("delete canonical state-transition journal")]
    DeleteCanonical {
        #[source]
        source: io::Error,
    },
    #[error("delete displaced state-transition journal")]
    DeleteDisplaced {
        #[source]
        source: io::Error,
    },
    #[error("fsync state-transition journal directory")]
    SyncJournalDirectory {
        #[source]
        source: io::Error,
    },
}

fn temporary_name() -> CString {
    let process = std::process::id();
    let sequence = TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    CString::new(format!(".state-transition.tmp-{process:08x}-{sequence:016x}"))
        .expect("internally generated journal temporary name contains no NUL")
}

fn open_and_lock(directory: &std::fs::File, path: &Path) -> Result<std::fs::File, StorageError> {
    let create_flags = nix::libc::O_RDWR
        | nix::libc::O_CLOEXEC
        | nix::libc::O_NOFOLLOW
        | nix::libc::O_NONBLOCK
        | nix::libc::O_CREAT
        | nix::libc::O_EXCL;
    let file = match openat2_file(
        directory.as_raw_fd(),
        LOCK_NAME,
        create_flags,
        JOURNAL_FILE_MODE,
        controlled_resolution(),
    ) {
        Ok(file) => {
            // The create mode is only an upper bound under umask. Normalize
            // the already-open inode rather than its public name.
            file.set_permissions(std::fs::Permissions::from_mode(JOURNAL_FILE_MODE))
                .map_err(|source| StorageError::OpenLock { source })?;
            file
        }
        Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {
            let pinned = openat2_file(
                directory.as_raw_fd(),
                LOCK_NAME,
                nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
                0,
                controlled_resolution(),
            )
            .map_err(|source| StorageError::OpenLock { source })?;
            let mode = recoverable_private_lock_mode(&pinned, &path.join("state-transition.lock"))
                .map_err(|source| StorageError::ValidateLock { source })?;
            if mode != JOURNAL_FILE_MODE {
                chmod_path_descriptor(&pinned, JOURNAL_FILE_MODE)
                    .map_err(|source| StorageError::OpenLock { source })?;
            }
            require_safe_regular_file(&pinned, &path.join("state-transition.lock"))
                .map_err(|source| StorageError::ValidateLock { source })?;

            let file = openat2_file(
                directory.as_raw_fd(),
                LOCK_NAME,
                nix::libc::O_RDWR | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK,
                0,
                controlled_resolution(),
            )
            .map_err(|source| StorageError::OpenLock { source })?;
            require_same_inode(
                inode_identity(&pinned).map_err(|source| StorageError::ValidateLock { source })?,
                inode_identity(&file).map_err(|source| StorageError::ValidateLock { source })?,
            )?;
            file
        }
        Err(source) => return Err(StorageError::OpenLock { source }),
    };
    require_safe_regular_file(&file, &path.join("state-transition.lock"))
        .map_err(|source| StorageError::ValidateLock { source })?;
    flock_exclusive(&file).map_err(|source| StorageError::AcquireLock { source })?;

    let named = openat2_file(
        directory.as_raw_fd(),
        LOCK_NAME,
        nix::libc::O_RDONLY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK,
        0,
        controlled_resolution(),
    )
    .map_err(|source| StorageError::ValidateLock { source })?;
    require_safe_regular_file(&named, &path.join("state-transition.lock"))
        .map_err(|source| StorageError::ValidateLock { source })?;
    if inode_identity(&file).map_err(|source| StorageError::ValidateLock { source })?
        != inode_identity(&named).map_err(|source| StorageError::ValidateLock { source })?
    {
        return Err(StorageError::CanonicalChanged);
    }
    // Persist exact mode and structural name even when completing recovery
    // from a previously exposed restrictive-umask inode.
    file.sync_all().map_err(|source| StorageError::OpenLock { source })?;
    directory
        .sync_all()
        .map_err(|source| StorageError::SyncJournalDirectory { source })?;
    Ok(file)
}

fn recoverable_private_lock_mode(file: &std::fs::File, path: &Path) -> io::Result<u32> {
    let metadata = file.metadata()?;
    let mode = metadata.mode() & 0o7777;
    if !metadata.file_type().is_file()
        || metadata.uid() != Uid::effective().as_raw()
        || metadata.nlink() != 1
        || mode & !JOURNAL_FILE_MODE != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "journal lock is not one safely recoverable owner-private regular file: {} (uid={}, mode={mode:04o}, links={})",
                path.display(),
                metadata.uid(),
                metadata.nlink()
            ),
        ));
    }
    Ok(mode)
}

fn flock_exclusive(file: &std::fs::File) -> io::Result<()> {
    loop {
        // SAFETY: flock operates on the live lock-file descriptor.
        if unsafe { nix::libc::flock(file.as_raw_fd(), nix::libc::LOCK_EX) } == 0 {
            return Ok(());
        }
        let source = io::Error::last_os_error();
        if source.kind() != io::ErrorKind::Interrupted {
            return Err(source);
        }
    }
}

fn valid_temporary_name(name: &[u8]) -> bool {
    let Some(tail) = name.strip_prefix(TEMPORARY_PREFIX) else {
        return false;
    };
    tail.len() == 8 + 1 + 16
        && tail[8] == b'-'
        && tail[..8]
            .iter()
            .chain(&tail[9..])
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
}

fn directory_entries(directory: &std::fs::File) -> io::Result<Vec<CString>> {
    // Duplicate because fdopendir takes ownership and closedir closes its fd.
    // SAFETY: fcntl receives one live directory descriptor.
    let duplicate = unsafe { nix::libc::fcntl(directory.as_raw_fd(), nix::libc::F_DUPFD_CLOEXEC, 0) };
    if duplicate == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fdopendir takes ownership of the fresh duplicate on success.
    let stream = unsafe { nix::libc::fdopendir(duplicate) };
    if stream.is_null() {
        let source = io::Error::last_os_error();
        // SAFETY: fdopendir failed and did not consume the duplicate.
        unsafe { nix::libc::close(duplicate) };
        return Err(source);
    }

    let result = (|| {
        let mut entries = Vec::new();
        loop {
            // SAFETY: Linux exposes thread-local errno through this pointer.
            unsafe { *nix::libc::__errno_location() = 0 };
            // SAFETY: stream remains live and is used by this thread only.
            let entry = unsafe { nix::libc::readdir(stream) };
            if entry.is_null() {
                let source = io::Error::last_os_error();
                if source.raw_os_error() == Some(0) {
                    break;
                }
                return Err(source);
            }
            // SAFETY: d_name is NUL terminated for the returned live dirent.
            let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
            if name.to_bytes() == b"." || name.to_bytes() == b".." {
                continue;
            }
            entries.push(name.to_owned());
            // Admit one entry beyond the valid maximum so cleanup can report
            // the specific stale-temporary cap even when both durable names
            // are present. Anything larger still fails before allocation can
            // grow without a bound.
            if entries.len() > MAX_STALE_TEMPORARIES + 3 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "journal directory entry bound exceeded",
                ));
            }
        }
        Ok(entries)
    })();
    // SAFETY: stream was returned by fdopendir and has not been closed.
    let close_result = unsafe { nix::libc::closedir(stream) };
    if close_result == -1 && result.is_ok() {
        return Err(io::Error::last_os_error());
    }
    result
}

fn read_bounded(file: &mut std::fs::File) -> io::Result<Vec<u8>> {
    let initial = file.metadata()?;
    require_safe_regular_file_metadata(&initial, Path::new("state-transition"), Uid::effective().as_raw())?;
    let initial_len = usize::try_from(initial.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "journal length does not fit usize"))?;
    if initial_len > MAX_CANONICAL_RECORD_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("journal exceeds {MAX_CANONICAL_RECORD_BYTES} bytes"),
        ));
    }

    let mut bytes = Vec::with_capacity(initial_len.min(MAX_CANONICAL_RECORD_BYTES));
    file.take((MAX_CANONICAL_RECORD_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_CANONICAL_RECORD_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("journal exceeds {MAX_CANONICAL_RECORD_BYTES} bytes"),
        ));
    }
    let final_metadata = file.metadata()?;
    require_safe_regular_file_metadata(
        &final_metadata,
        Path::new("state-transition"),
        Uid::effective().as_raw(),
    )?;
    if bytes.len() != initial_len || final_metadata.len() != initial.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "journal size changed while it was read",
        ));
    }
    Ok(bytes)
}

fn require_safe_regular_file(file: &std::fs::File, path: &Path) -> io::Result<()> {
    let metadata = file.metadata()?;
    require_safe_regular_file_metadata(&metadata, path, Uid::effective().as_raw())
}

fn require_safe_stale_temporary(file: &std::fs::File, path: &Path) -> io::Result<()> {
    let metadata = file.metadata()?;
    let mode = metadata.mode() & 0o7777;
    if !metadata.file_type().is_file()
        || metadata.uid() != Uid::effective().as_raw()
        || metadata.nlink() != 1
        || mode & !JOURNAL_FILE_MODE != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "stale journal temporary is not one owner-controlled regular file: {} (uid={}, mode={mode:04o}, links={})",
                path.display(),
                metadata.uid(),
                metadata.nlink()
            ),
        ));
    }
    Ok(())
}

fn require_safe_regular_file_metadata(
    metadata: &std::fs::Metadata,
    path: &Path,
    expected_owner: u32,
) -> io::Result<()> {
    let mode = metadata.mode() & 0o7777;
    if !metadata.file_type().is_file()
        || metadata.uid() != expected_owner
        || metadata.nlink() != 1
        || mode != JOURNAL_FILE_MODE
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "journal is not one owner-controlled regular 0600 file: {} (uid={}, mode={mode:04o}, links={})",
                path.display(),
                metadata.uid(),
                metadata.nlink()
            ),
        ));
    }
    Ok(())
}

fn inode_identity(file: &std::fs::File) -> io::Result<InodeIdentity> {
    let metadata = file.metadata()?;
    Ok(InodeIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

fn require_same_inode(expected: InodeIdentity, actual: InodeIdentity) -> Result<(), StorageError> {
    if expected != actual {
        return Err(StorageError::CanonicalChanged);
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum DirectoryPolicy {
    Controlled,
    ExactPrivate,
}

fn ensure_journal_directory(cast: &std::fs::File, path: &Path) -> io::Result<std::fs::File> {
    // SAFETY: the directory descriptor and fixed NUL-terminated name live for
    // the call. mkdirat never follows the final component.
    if unsafe { nix::libc::mkdirat(cast.as_raw_fd(), JOURNAL_DIRECTORY.as_ptr(), JOURNAL_DIRECTORY_MODE) } == -1 {
        let source = io::Error::last_os_error();
        if source.kind() != io::ErrorKind::AlreadyExists {
            return Err(source);
        }
    }

    let pinned = openat2_file(
        cast.as_raw_fd(),
        JOURNAL_DIRECTORY,
        nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        controlled_resolution(),
    )?;
    let mode = recoverable_private_directory_mode(&pinned, path)?;
    if mode != JOURNAL_DIRECTORY_MODE {
        chmod_path_descriptor(&pinned, JOURNAL_DIRECTORY_MODE)?;
    }
    require_directory(&pinned, path, DirectoryPolicy::ExactPrivate)?;

    let directory = openat2_file(
        cast.as_raw_fd(),
        JOURNAL_DIRECTORY,
        nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK,
        0,
        controlled_resolution(),
    )?;
    require_directory(&directory, path, DirectoryPolicy::ExactPrivate)?;
    require_same_directory(&pinned, &directory, path)?;
    // Always persist both the exact mode and the structural name. This also
    // completes a prior crash that exposed a safe umask-restricted inode
    // before its normalization was synced.
    directory.sync_all()?;
    cast.sync_all()?;
    Ok(directory)
}

fn recoverable_private_directory_mode(file: &std::fs::File, path: &Path) -> io::Result<u32> {
    let metadata = file.metadata()?;
    let mode = metadata.mode() & 0o7777;
    if !metadata.file_type().is_dir()
        || metadata.uid() != Uid::effective().as_raw()
        || mode & !JOURNAL_DIRECTORY_MODE != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "journal directory is not a safely recoverable owner-private directory: {} (uid={}, mode={mode:04o})",
                path.display(),
                metadata.uid()
            ),
        ));
    }
    Ok(mode)
}

fn open_existing_directory(
    parent: &std::fs::File,
    name: &CStr,
    path: &Path,
    policy: DirectoryPolicy,
) -> io::Result<std::fs::File> {
    let pinned = openat2_file(
        parent.as_raw_fd(),
        name,
        nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        controlled_resolution(),
    )?;
    require_directory(&pinned, path, policy)?;
    let directory = openat2_file(
        parent.as_raw_fd(),
        name,
        nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK,
        0,
        controlled_resolution(),
    )?;
    require_directory(&directory, path, policy)?;
    require_same_directory(&pinned, &directory, path)?;
    Ok(directory)
}

fn require_directory(file: &std::fs::File, path: &Path, policy: DirectoryPolicy) -> io::Result<()> {
    let metadata = file.metadata()?;
    let mode = metadata.mode() & 0o7777;
    let controlled = metadata.file_type().is_dir()
        && metadata.uid() == Uid::effective().as_raw()
        && mode & 0o7000 == 0
        && mode & 0o700 == 0o700
        && mode & 0o022 == 0;
    let exact = !matches!(policy, DirectoryPolicy::ExactPrivate) || mode == JOURNAL_DIRECTORY_MODE;
    if !controlled || !exact {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "journal parent is not an owner-controlled directory: {} (uid={}, mode={mode:04o})",
                path.display(),
                metadata.uid()
            ),
        ));
    }
    Ok(())
}

fn require_same_directory(first: &std::fs::File, second: &std::fs::File, path: &Path) -> io::Result<()> {
    let first = first.metadata()?;
    let second = second.metadata()?;
    if (first.dev(), first.ino()) != (second.dev(), second.ino()) {
        return Err(io::Error::other(format!(
            "journal directory changed while opening: {}",
            path.display()
        )));
    }
    Ok(())
}

fn open_directory_path(path: &Path) -> io::Result<std::fs::File> {
    let path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "journal root contains NUL"))?;
    openat2_file(
        nix::libc::AT_FDCWD,
        &path,
        nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        (nix::libc::RESOLVE_NO_MAGICLINKS | nix::libc::RESOLVE_NO_SYMLINKS) as u64,
    )
}

fn openat2_file(dirfd: RawFd, path: &CStr, flags: i32, mode: u32, resolve: u64) -> io::Result<std::fs::File> {
    // SAFETY: zero is valid for all public open_how fields.
    let mut how: nix::libc::open_how = unsafe { zeroed() };
    how.flags = u64::from(flags as u32);
    how.mode = u64::from(mode);
    how.resolve = resolve;
    // SAFETY: all pointers and the directory descriptor remain live. A
    // successful openat2 returns one fresh descriptor owned below.
    let descriptor = unsafe {
        nix::libc::syscall(
            nix::libc::SYS_openat2,
            dirfd,
            path.as_ptr(),
            &how,
            size_of::<nix::libc::open_how>(),
        )
    };
    if descriptor == -1 {
        return Err(io::Error::last_os_error());
    }
    let descriptor = i32::try_from(descriptor)
        .map_err(|_| io::Error::other(format!("openat2 returned invalid descriptor {descriptor}")))?;
    // SAFETY: successful openat2 returned this fresh owned descriptor.
    let descriptor = unsafe { OwnedFd::from_raw_fd(descriptor) };
    Ok(std::fs::File::from(descriptor))
}

fn controlled_resolution() -> u64 {
    (nix::libc::RESOLVE_BENEATH
        | nix::libc::RESOLVE_NO_MAGICLINKS
        | nix::libc::RESOLVE_NO_SYMLINKS
        | nix::libc::RESOLVE_NO_XDEV) as u64
}

fn chmod_path_descriptor(file: &std::fs::File, mode: u32) -> io::Result<()> {
    // Linux 5.6 cannot fchmod an O_PATH descriptor directly. Resolve only the
    // live decimal descriptor through an authenticated procfs fd directory;
    // never chmod the mutable journal pathname.
    let proc = openat2_file(
        nix::libc::AT_FDCWD,
        c"/proc",
        nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        (nix::libc::RESOLVE_NO_MAGICLINKS | nix::libc::RESOLVE_NO_SYMLINKS) as u64,
    )?;
    require_procfs(&proc, Path::new("/proc"))?;
    let process = proc_self_component(&proc)?;
    let process = openat2_file(
        proc.as_raw_fd(),
        &process,
        nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        controlled_resolution(),
    )?;
    require_procfs(&process, Path::new("/proc/<pid>"))?;
    let descriptors = openat2_file(
        process.as_raw_fd(),
        c"fd",
        nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK,
        0,
        controlled_resolution(),
    )?;
    require_procfs(&descriptors, Path::new("/proc/<pid>/fd"))?;

    let descriptor = CString::new(file.as_raw_fd().to_string()).expect("numeric descriptor contains no NUL");
    loop {
        // SAFETY: the authenticated directory, live decimal descriptor name,
        // and target O_PATH descriptor remain pinned for the call. flags=0
        // deliberately follows the procfs magic link to that exact inode.
        if unsafe { nix::libc::fchmodat(descriptors.as_raw_fd(), descriptor.as_ptr(), mode, 0) } == 0 {
            return Ok(());
        }
        let source = io::Error::last_os_error();
        if source.kind() != io::ErrorKind::Interrupted {
            return Err(source);
        }
    }
}

fn proc_self_component(proc: &std::fs::File) -> io::Result<CString> {
    const MAX_DECIMAL_PID_BYTES: usize = 16;
    let mut bytes = [0_u8; MAX_DECIMAL_PID_BYTES + 1];
    let length = loop {
        // SAFETY: proc is a live authenticated directory, `self` is a fixed
        // NUL-terminated component, and bytes is writable for its full size.
        let length = unsafe {
            nix::libc::readlinkat(
                proc.as_raw_fd(),
                c"self".as_ptr(),
                bytes.as_mut_ptr().cast(),
                bytes.len(),
            )
        };
        if length >= 0 {
            break usize::try_from(length).map_err(|_| io::Error::other("negative procfs self length"))?;
        }
        let source = io::Error::last_os_error();
        if source.kind() != io::ErrorKind::Interrupted {
            return Err(source);
        }
    };
    if length == 0
        || length > MAX_DECIMAL_PID_BYTES
        || bytes[..length].iter().any(|byte| !byte.is_ascii_digit())
        || bytes[0] == b'0'
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "authenticated procfs self link is not one bounded canonical decimal PID",
        ));
    }
    CString::new(&bytes[..length]).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "procfs PID contains NUL"))
}

fn require_procfs(file: &std::fs::File, path: &Path) -> io::Result<()> {
    // SAFETY: zeroed statfs storage is a valid output buffer and the file
    // descriptor remains live throughout fstatfs.
    let mut stat: nix::libc::statfs = unsafe { zeroed() };
    if unsafe { nix::libc::fstatfs(file.as_raw_fd(), &mut stat) } == -1 {
        return Err(io::Error::last_os_error());
    }
    if stat.f_type != PROC_SUPER_MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "refusing unauthenticated descriptor chmod through {}: expected procfs magic {PROC_SUPER_MAGIC:#x}, found {:#x}",
                path.display(),
                stat.f_type
            ),
        ));
    }
    Ok(())
}

fn renameat2(old_dir: RawFd, old: &CStr, new_dir: RawFd, new: &CStr, flags: u32) -> io::Result<()> {
    // SAFETY: both directory descriptors and names remain valid for the call.
    let result = unsafe {
        nix::libc::syscall(
            nix::libc::SYS_renameat2,
            old_dir,
            old.as_ptr(),
            new_dir,
            new.as_ptr(),
            flags,
        )
    };
    if result == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn unlinkat(directory: RawFd, name: &CStr) -> io::Result<()> {
    // SAFETY: the descriptor and single-component name remain live; flags 0
    // unlinks a non-directory entry without following its final symlink.
    if unsafe { nix::libc::unlinkat(directory, name.as_ptr(), 0) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        io::Write as _,
        os::unix::{
            ffi::OsStrExt as _,
            fs::{MetadataExt as _, PermissionsExt as _, symlink},
        },
        process::Command,
        sync::{Arc, mpsc},
        thread,
        time::{Duration, Instant},
    };

    use super::*;

    fn id() -> TransitionId {
        TransitionId::parse("0123456789abcdef0123456789abcdef").unwrap()
    }

    fn other_id() -> TransitionId {
        TransitionId::parse("11111111111111111111111111111111").unwrap()
    }

    fn identity(seed: u64) -> TreeIdentity {
        TreeIdentity {
            st_dev: seed,
            inode: seed + 1,
            statx_mount_id: seed + 2,
        }
    }

    fn new_state_record(phase: Phase) -> TransitionRecord {
        let forward = phase.forward().expect("new_state_record requires a forward phase");
        TransitionRecord {
            format: PAYLOAD_FORMAT.to_owned(),
            version: PAYLOAD_VERSION,
            generation: 7,
            transition_id: id(),
            operation: Operation::NewState,
            phase,
            rollback: None,
            candidate: Candidate {
                id: (!matches!(forward, ForwardPhase::Preparing | ForwardPhase::FreshStateAllocating)).then_some(42),
                origin: CandidateOrigin::Fresh,
                usr_identity: identity(10),
            },
            previous: Previous {
                id: Some(41),
                usr_identity: identity(20),
                origin: PreviousOrigin::ActiveState,
            },
            options: TransitionOptions {
                archive_previous: true,
                run_system_triggers: true,
                run_boot_sync: true,
            },
            quarantine_name: QuarantineName::parse("failed-0123456789abcdef").unwrap(),
        }
    }

    fn record(phase: Phase) -> TransitionRecord {
        if phase.forward().is_some() {
            new_state_record(phase)
        } else {
            valid_rollback_record(phase)
        }
    }

    fn creation_record() -> TransitionRecord {
        TransitionRecord::preparing(
            id(),
            Operation::NewState,
            None,
            identity(10),
            Previous {
                id: Some(41),
                usr_identity: identity(20),
                origin: PreviousOrigin::ActiveState,
            },
            true,
            true,
            QuarantineName::parse("failed-0123456789abcdef").unwrap(),
        )
        .unwrap()
    }

    fn archived_record(phase: Phase) -> TransitionRecord {
        assert!(phase.forward().is_some());
        let mut record = new_state_record(phase);
        record.operation = Operation::ActivateArchived;
        record.candidate.origin = CandidateOrigin::Archived;
        record.candidate.id = Some(42);
        record
    }

    fn reblit_record(phase: Phase) -> TransitionRecord {
        assert!(phase.forward().is_some());
        let mut record = new_state_record(phase);
        record.operation = Operation::ActiveReblit;
        record.candidate.origin = CandidateOrigin::ActiveReblit;
        record.candidate.id = Some(42);
        record.previous.id = Some(42);
        record.previous.origin = PreviousOrigin::ActiveReblitCorrupt;
        record.options.archive_previous = false;
        record
    }

    fn without_previous_archive(mut record: TransitionRecord, origin: PreviousOrigin) -> TransitionRecord {
        assert!(matches!(
            origin,
            PreviousOrigin::SynthesizedEmpty | PreviousOrigin::Unmanaged
        ));
        record.previous.id = None;
        record.previous.origin = origin;
        record.options.archive_previous = false;
        record
    }

    fn rollback_decided(current: &TransitionRecord) -> TransitionRecord {
        let source = current.phase.forward().expect("rollback starts from a forward phase");
        let previous_possible =
            current.options.archive_previous && source.ordinal() >= ForwardPhase::PreviousArchiveIntent.ordinal();
        let usr_possible = source.ordinal() >= ForwardPhase::UsrExchangeIntent.ordinal();
        let fresh_possible = matches!(current.operation, Operation::NewState)
            && source.ordinal() >= ForwardPhase::FreshStateAllocating.ordinal();
        let boot_possible = source == ForwardPhase::BootSyncStarted;
        let external_effects_may_remain = (current.runs_transaction_triggers()
            && source.ordinal() >= ForwardPhase::TransactionTriggersStarted.ordinal())
            || (current.options.run_system_triggers
                && source.ordinal() >= ForwardPhase::SystemTriggersStarted.ordinal())
            || boot_possible;

        let mut next = current.clone();
        next.generation += 1;
        next.phase = Phase::RollbackDecided;
        if fresh_possible && next.candidate.id.is_none() {
            next.candidate.id = Some(42);
        }
        next.rollback = Some(RollbackPlan {
            source,
            previous_archive: if previous_possible {
                RollbackAction::Pending
            } else {
                RollbackAction::NotRequired
            },
            usr_exchange: if usr_possible {
                RollbackAction::Pending
            } else {
                RollbackAction::NotRequired
            },
            candidate: CandidateRollback {
                action: RollbackAction::Pending,
                disposition: current.candidate_disposition_for(source),
            },
            fresh_db: if fresh_possible {
                RollbackAction::Pending
            } else {
                RollbackAction::NotRequired
            },
            boot: if boot_possible {
                BootRollback::PendingUnverifiable
            } else {
                BootRollback::NotRequired
            },
            external_effects_may_remain,
        });
        next
    }

    fn advance_record(current: &TransitionRecord, phase: Phase) -> TransitionRecord {
        if phase == Phase::RollbackDecided {
            return rollback_decided(current);
        }

        let mut next = current.clone();
        next.generation += 1;
        next.phase = phase;
        if (current.phase, phase) == (Phase::FreshStateAllocating, Phase::FreshStateAllocated) {
            next.candidate.id = Some(42);
        }
        if let Some(plan) = next.rollback.as_mut() {
            match (current.phase, phase) {
                (Phase::PreviousRestoreIntent, Phase::PreviousRestoredToStaging) => {
                    plan.previous_archive = RollbackAction::Applied;
                }
                (Phase::ReverseExchangeIntent, Phase::UsrRestored) => {
                    plan.usr_exchange = RollbackAction::Applied;
                }
                (Phase::CandidatePreserveIntent, Phase::CandidatePreserved) => {
                    plan.candidate.action = RollbackAction::Applied;
                }
                (Phase::FreshDbInvalidationIntent, Phase::FreshDbInvalidated) => {
                    plan.fresh_db = RollbackAction::Applied;
                }
                (Phase::BootRepairStarted, Phase::BootRepairUnverified) => {
                    plan.boot = BootRollback::Unverified;
                }
                _ => {}
            }
        }
        next
    }

    fn legal_forward_advance(current: &TransitionRecord) -> TransitionRecord {
        let current_phase = current.phase.forward().unwrap();
        let phase = next_forward_phase(current, current_phase).unwrap().into();
        advance_record(current, phase)
    }

    fn legal_rollback_advance(current: &TransitionRecord) -> TransitionRecord {
        let plan = current.rollback.as_ref().expect("rollback plan");
        let phase = next_rollback_phase(plan, current.phase).expect("nonterminal rollback successor");
        advance_record(current, phase)
    }

    fn rollback_sequence(source: &TransitionRecord) -> Vec<TransitionRecord> {
        let mut current = rollback_decided(source);
        let mut records = vec![current.clone()];
        while !current.phase.blocks_advance() {
            let next = legal_rollback_advance(&current);
            validate_advance(&current, &next).unwrap();
            records.push(next.clone());
            current = next;
        }
        records
    }

    fn valid_rollback_record(phase: Phase) -> TransitionRecord {
        let source = if matches!(
            phase,
            Phase::BootRepairRequired | Phase::BootRepairStarted | Phase::BootRepairUnverified
        ) {
            new_state_record(Phase::BootSyncStarted)
        } else {
            let mut source = new_state_record(Phase::PreviousArchiveIntent);
            source.options.run_boot_sync = false;
            source
        };
        rollback_sequence(&source)
            .into_iter()
            .find(|record| record.phase == phase)
            .expect("requested rollback phase is reachable")
    }

    fn satisfied_preparing_rollback(current: &TransitionRecord) -> TransitionRecord {
        let mut rollback = rollback_decided(current);
        rollback.rollback.as_mut().unwrap().candidate.action = RollbackAction::AlreadySatisfied;
        rollback
    }

    fn advance_to_complete(store: &TransitionJournalStore, mut current: TransitionRecord) -> TransitionRecord {
        while current.phase != Phase::Complete {
            let next = legal_forward_advance(&current);
            store.advance(&current, &next).unwrap();
            current = next;
        }
        current
    }

    fn frame_payload(payload: &[u8]) -> Vec<u8> {
        let length = u32::try_from(payload.len()).unwrap().to_be_bytes();
        let version = FRAME_VERSION.to_be_bytes();
        let checksum = checksum(&version, &length, payload);
        let mut framed = Vec::new();
        framed.extend_from_slice(MAGIC);
        framed.extend_from_slice(&version);
        framed.extend_from_slice(&length);
        framed.extend_from_slice(&checksum);
        framed.extend_from_slice(payload);
        framed
    }

    fn replace_payload(framed: &[u8], mutate: impl FnOnce(&str) -> String) -> Vec<u8> {
        let payload = std::str::from_utf8(&framed[HEADER_SIZE..]).unwrap();
        frame_payload(mutate(payload).as_bytes())
    }

    fn fixture() -> (tempfile::TempDir, TransitionJournalStore) {
        let temporary = tempfile::tempdir().unwrap();
        let cast = temporary.path().join(".cast");
        fs::create_dir(&cast).unwrap();
        fs::set_permissions(&cast, fs::Permissions::from_mode(0o700)).unwrap();
        let store = TransitionJournalStore::open(temporary.path()).unwrap();
        (temporary, store)
    }

    fn canonical(root: &Path) -> PathBuf {
        root.join(".cast/journal/state-transition")
    }

    fn stale_temporary_path(root: &Path, sequence: usize) -> PathBuf {
        root.join(".cast/journal").join(format!(
            ".state-transition.tmp-{:08x}-{sequence:016x}",
            std::process::id()
        ))
    }

    fn create_stale_temporaries(root: &Path, count: usize) {
        for sequence in 0..count {
            let path = stale_temporary_path(root, sequence);
            fs::write(&path, b"stale").unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
        }
    }

    fn assert_no_journal_temporaries(root: &Path) {
        assert!(
            fs::read_dir(root.join(".cast/journal"))
                .unwrap()
                .all(|entry| !valid_temporary_name(entry.unwrap().file_name().as_bytes()))
        );
    }

    #[test]
    fn canonical_round_trip_covers_every_phase() {
        let phases = [
            Phase::Preparing,
            Phase::FreshStateAllocating,
            Phase::FreshStateAllocated,
            Phase::CandidatePrepareStarted,
            Phase::CandidatePrepared,
            Phase::TransactionTriggersStarted,
            Phase::TransactionTriggersComplete,
            Phase::UsrExchangeIntent,
            Phase::UsrExchanged,
            Phase::RootLinksComplete,
            Phase::SystemTriggersStarted,
            Phase::SystemTriggersComplete,
            Phase::PreviousArchiveIntent,
            Phase::PreviousArchived,
            Phase::BootSyncStarted,
            Phase::BootSyncComplete,
            Phase::CommitDecided,
            Phase::CommitCleanupComplete,
            Phase::Complete,
            Phase::RollbackDecided,
            Phase::PreviousRestoreIntent,
            Phase::PreviousRestoredToStaging,
            Phase::ReverseExchangeIntent,
            Phase::UsrRestored,
            Phase::CandidatePreserveIntent,
            Phase::CandidatePreserved,
            Phase::FreshDbInvalidationIntent,
            Phase::FreshDbInvalidated,
            Phase::BootRepairRequired,
            Phase::BootRepairStarted,
            Phase::BootRepairUnverified,
            Phase::RollbackComplete,
        ];
        for phase in phases {
            let value = record(phase);
            assert_eq!(decode(&encode(&value).unwrap()).unwrap(), value);
        }
    }

    #[test]
    fn canonical_v1_full_frame_and_json_order_are_locked_by_golden_bytes() {
        const GOLDEN_JSON_WITH_NEWLINE: &[u8] =
            include_bytes!("../../../tests/fixtures/transition-journal-v1-rollback-decided.json");
        const GOLDEN_HEX_WITH_NEWLINE: &[u8] =
            include_bytes!("../../../tests/fixtures/transition-journal-v1-rollback-decided.hex");
        assert_eq!(GOLDEN_JSON_WITH_NEWLINE.last(), Some(&b'\n'));
        assert_eq!(GOLDEN_HEX_WITH_NEWLINE.last(), Some(&b'\n'));
        let golden_json = &GOLDEN_JSON_WITH_NEWLINE[..GOLDEN_JSON_WITH_NEWLINE.len() - 1];
        let mut golden_frame = Vec::with_capacity((GOLDEN_HEX_WITH_NEWLINE.len() - 1) / 2);
        let mut pairs = GOLDEN_HEX_WITH_NEWLINE[..GOLDEN_HEX_WITH_NEWLINE.len() - 1].chunks_exact(2);
        for pair in &mut pairs {
            let nibble = |byte: u8| match byte {
                b'0'..=b'9' => byte - b'0',
                b'a'..=b'f' => byte - b'a' + 10,
                _ => panic!("golden frame is not lowercase hexadecimal"),
            };
            golden_frame.push((nibble(pair[0]) << 4) | nibble(pair[1]));
        }
        assert!(pairs.remainder().is_empty());

        let value = rollback_decided(&new_state_record(Phase::BootSyncStarted));
        assert_eq!(encode(&value).unwrap(), golden_frame);
        assert_eq!(&golden_frame[HEADER_SIZE..], golden_json);
        assert_eq!(decode(&golden_frame).unwrap(), value);
    }

    #[test]
    fn exact_record_limit_and_n_plus_one_are_distinguished() {
        assert!(enforce_record_size(MAX_CANONICAL_RECORD_BYTES).is_ok());
        assert!(matches!(
            enforce_record_size(MAX_CANONICAL_RECORD_BYTES + 1),
            Err(CodecError::RecordTooLarge(size)) if size == MAX_CANONICAL_RECORD_BYTES + 1
        ));
        assert!(matches!(
            decode(&vec![0; MAX_CANONICAL_RECORD_BYTES]),
            Err(CodecError::InvalidMagic)
        ));
        assert!(matches!(
            decode(&vec![0; MAX_CANONICAL_RECORD_BYTES + 1]),
            Err(CodecError::RecordTooLarge(_))
        ));
    }

    #[test]
    fn checksum_covers_header_fields_and_payload() {
        let valid = encode(&record(Phase::Preparing)).unwrap();
        for offset in [MAGIC_END, VERSION_END, CHECKSUM_END, valid.len() - 1] {
            let mut corrupt = valid.clone();
            corrupt[offset] ^= 1;
            assert!(decode(&corrupt).is_err(), "offset {offset} unexpectedly decoded");
        }
    }

    #[test]
    fn unknown_frame_and_payload_versions_are_rejected() {
        let mut frame = encode(&record(Phase::Preparing)).unwrap();
        frame[MAGIC_END..VERSION_END].copy_from_slice(&2_u16.to_be_bytes());
        assert!(matches!(decode(&frame), Err(CodecError::UnsupportedFrameVersion(2))));

        let valid = encode(&record(Phase::Preparing)).unwrap();
        let unknown = replace_payload(&valid, |payload| payload.replacen("\"version\":1", "\"version\":2", 1));
        assert!(matches!(
            decode(&unknown),
            Err(CodecError::UnsupportedPayloadVersion(2))
        ));
    }

    #[test]
    fn unknown_phase_field_and_duplicate_field_are_rejected() {
        let valid = encode(&record(Phase::Preparing)).unwrap();
        let unknown_phase = replace_payload(&valid, |payload| {
            payload.replacen("\"phase\":\"preparing\"", "\"phase\":\"future-phase\"", 1)
        });
        assert!(matches!(decode(&unknown_phase), Err(CodecError::Json(_))));

        let unknown_field = replace_payload(&valid, |payload| payload.replacen('{', "{\"surprise\":true,", 1));
        assert!(matches!(decode(&unknown_field), Err(CodecError::Json(_))));

        let duplicate = replace_payload(&valid, |payload| {
            payload.replacen("\"generation\":7", "\"generation\":7,\"generation\":8", 1)
        });
        assert!(matches!(decode(&duplicate), Err(CodecError::Json(_))));

        let nested_unknown = replace_payload(&valid, |payload| {
            payload.replacen(
                "\"archive_previous\":true",
                "\"archive_previous\":true,\"future_option\":false",
                1,
            )
        });
        assert!(matches!(decode(&nested_unknown), Err(CodecError::Json(_))));

        let nested_duplicate = replace_payload(&valid, |payload| {
            payload.replacen(
                "\"run_boot_sync\":true",
                "\"run_boot_sync\":true,\"run_boot_sync\":false",
                1,
            )
        });
        assert!(matches!(decode(&nested_duplicate), Err(CodecError::Json(_))));
    }

    #[test]
    fn record_trailing_bytes_and_noncanonical_json_are_rejected() {
        let mut trailing = encode(&record(Phase::Preparing)).unwrap();
        trailing.push(b' ');
        assert!(matches!(decode(&trailing), Err(CodecError::LengthMismatch { .. })));

        let valid = encode(&record(Phase::Preparing)).unwrap();
        let payload = std::str::from_utf8(&valid[HEADER_SIZE..]).unwrap();
        let noncanonical = frame_payload(format!(" {payload}").as_bytes());
        assert!(matches!(decode(&noncanonical), Err(CodecError::NonCanonicalPayload)));
    }

    #[test]
    fn bounded_identifiers_and_obvious_semantic_mismatches_fail_closed() {
        for invalid in [
            "ABCDEF0123456789abcdef0123456789",
            "0123456789abcdef0123456789abcde",
            "g123456789abcdef0123456789abcdef",
        ] {
            assert!(TransitionId::parse(invalid).is_err());
        }
        for invalid in ["", ".", "..", "../escape", "Upper", "has space"] {
            assert!(matches!(
                QuarantineName::parse(invalid),
                Err(CodecError::InvalidQuarantineName)
            ));
        }
        assert!(matches!(
            QuarantineName::parse("a".repeat(MAX_QUARANTINE_NAME_BYTES + 1)),
            Err(CodecError::InvalidQuarantineName)
        ));

        let mut mismatch = record(Phase::Preparing);
        mismatch.candidate.origin = CandidateOrigin::Archived;
        assert!(matches!(
            encode(&mismatch),
            Err(CodecError::OperationOriginMismatch { .. })
        ));

        let mut archived = archived_record(Phase::Preparing);
        archived.previous.origin = PreviousOrigin::Unmanaged;
        assert!(matches!(
            encode(&archived),
            Err(CodecError::ArchiveOptionMismatch { .. })
        ));

        let mut reblit = reblit_record(Phase::Preparing);
        reblit.candidate.id = Some(99);
        assert!(matches!(encode(&reblit), Err(CodecError::ActiveReblitStateMismatch)));
    }

    #[test]
    fn preparing_constructor_derives_wire_fields_and_rejects_invalid_operation_layouts() {
        let quarantine = QuarantineName::parse("constructor-proof").unwrap();
        assert_eq!(quarantine.as_str(), "constructor-proof");
        let previous = Previous {
            id: Some(41),
            usr_identity: identity(20),
            origin: PreviousOrigin::ActiveState,
        };
        let record = TransitionRecord::preparing(
            id(),
            Operation::ActivateArchived,
            Some(42),
            identity(10),
            previous.clone(),
            false,
            true,
            quarantine.clone(),
        )
        .unwrap();
        assert_eq!(record.format, PAYLOAD_FORMAT);
        assert_eq!(record.version, PAYLOAD_VERSION);
        assert_eq!(record.generation, 1);
        assert_eq!(record.phase, Phase::Preparing);
        assert_eq!(record.rollback, None);
        assert_eq!(record.candidate.origin, CandidateOrigin::Archived);
        assert!(record.options.archive_previous);
        assert!(!record.options.run_system_triggers);
        assert!(record.options.run_boot_sync);

        assert!(matches!(
            TransitionRecord::preparing(
                id(),
                Operation::NewState,
                Some(42),
                identity(10),
                previous.clone(),
                true,
                true,
                quarantine.clone(),
            ),
            Err(CodecError::CandidateStateLayout)
        ));
        assert!(matches!(
            TransitionRecord::preparing(
                id(),
                Operation::ActivateArchived,
                None,
                identity(10),
                previous,
                true,
                true,
                quarantine,
            ),
            Err(CodecError::ExistingCandidateStateMissing)
        ));
    }

    #[test]
    fn preparing_pins_both_identities_and_operation_relationships_fail_closed() {
        let mut invalid = record(Phase::Preparing);
        invalid.previous.usr_identity.st_dev = 0;
        assert!(matches!(encode(&invalid), Err(CodecError::ZeroTreeIdentity)));

        let mut invalid = record(Phase::Preparing);
        invalid.candidate.id = Some(42);
        assert!(matches!(encode(&invalid), Err(CodecError::CandidateStateLayout)));

        let mut invalid = record(Phase::CandidatePrepared);
        invalid.candidate.id = invalid.previous.id;
        assert!(matches!(
            encode(&invalid),
            Err(CodecError::CandidatePreviousStateCollision)
        ));

        let mut invalid = record(Phase::CandidatePrepared);
        invalid.candidate.usr_identity = invalid.previous.usr_identity;
        assert!(matches!(
            encode(&invalid),
            Err(CodecError::CandidatePreviousIdentityCollision)
        ));

        let mut invalid = new_state_record(Phase::Preparing);
        invalid.previous.id = None;
        assert!(matches!(
            encode(&invalid),
            Err(CodecError::PreviousOriginStateMismatch { .. })
        ));

        let mut invalid =
            without_previous_archive(new_state_record(Phase::Preparing), PreviousOrigin::SynthesizedEmpty);
        invalid.previous.id = Some(41);
        assert!(matches!(
            encode(&invalid),
            Err(CodecError::PreviousOriginStateMismatch { .. })
        ));

        let invalid = archived_record(Phase::Preparing);
        assert_eq!(invalid.commit_disposition(), CommitDisposition::Archive);
        let invalid = reblit_record(Phase::Preparing);
        assert_eq!(invalid.commit_disposition(), CommitDisposition::Discard);
        let invalid = without_previous_archive(new_state_record(Phase::Preparing), PreviousOrigin::SynthesizedEmpty);
        assert_eq!(invalid.commit_disposition(), CommitDisposition::Discard);
        let invalid = without_previous_archive(new_state_record(Phase::Preparing), PreviousOrigin::Unmanaged);
        assert_eq!(invalid.commit_disposition(), CommitDisposition::Quarantine);
    }

    #[test]
    fn disabled_forward_phases_and_rollback_plan_placement_fail_closed() {
        let invalid = archived_record(Phase::TransactionTriggersStarted);
        assert!(matches!(encode(&invalid), Err(CodecError::DisabledPhase(_))));

        let mut invalid = record(Phase::SystemTriggersStarted);
        invalid.options.run_system_triggers = false;
        assert!(matches!(encode(&invalid), Err(CodecError::DisabledPhase(_))));

        let mut invalid = record(Phase::PreviousArchiveIntent);
        invalid.options.archive_previous = false;
        invalid.previous.origin = PreviousOrigin::SynthesizedEmpty;
        invalid.previous.id = None;
        assert!(matches!(encode(&invalid), Err(CodecError::DisabledPhase(_))));

        let mut invalid = record(Phase::BootSyncStarted);
        invalid.options.run_boot_sync = false;
        assert!(matches!(encode(&invalid), Err(CodecError::DisabledPhase(_))));

        let mut invalid = record(Phase::Preparing);
        invalid.rollback = Some(rollback_decided(&invalid).rollback.unwrap());
        assert!(matches!(encode(&invalid), Err(CodecError::RollbackPlanOnForwardPhase)));

        let mut invalid = record(Phase::RollbackComplete);
        invalid.rollback = None;
        assert!(matches!(encode(&invalid), Err(CodecError::MissingRollbackPlan)));

        let mut invalid = rollback_decided(&new_state_record(Phase::CandidatePrepared));
        invalid.rollback.as_mut().unwrap().source = ForwardPhase::CommitDecided;
        assert!(matches!(
            encode(&invalid),
            Err(CodecError::InvalidRollbackSource(ForwardPhase::CommitDecided))
        ));
    }

    #[test]
    fn all_operations_and_forward_option_paths_have_exact_successors() {
        for run_system_triggers in [false, true] {
            for archive_previous in [false, true] {
                for run_boot_sync in [false, true] {
                    let mut current = new_state_record(Phase::Preparing);
                    if !archive_previous {
                        current = without_previous_archive(current, PreviousOrigin::SynthesizedEmpty);
                    }
                    current.generation = 1;
                    current.options.run_system_triggers = run_system_triggers;
                    current.options.run_boot_sync = run_boot_sync;
                    while current.phase != Phase::Complete {
                        let next = legal_forward_advance(&current);
                        validate_advance(&current, &next).unwrap();
                        current = next;
                    }
                }
            }
        }

        for mut current in [archived_record(Phase::Preparing), reblit_record(Phase::Preparing)] {
            current.generation = 1;
            for run_system_triggers in [false, true] {
                for run_boot_sync in [false, true] {
                    let mut path = current.clone();
                    path.options.run_system_triggers = run_system_triggers;
                    path.options.run_boot_sync = run_boot_sync;
                    let mut visited = Vec::new();
                    while path.phase != Phase::Complete {
                        visited.push(path.phase);
                        let next = legal_forward_advance(&path);
                        validate_advance(&path, &next).unwrap();
                        path = next;
                    }
                    if matches!(current.operation, Operation::ActivateArchived) {
                        assert!(!visited.contains(&Phase::TransactionTriggersStarted));
                        assert!(!visited.contains(&Phase::TransactionTriggersComplete));
                    }
                }
            }
        }
    }

    #[test]
    fn rollback_is_available_until_commit_except_after_verified_boot_sync() {
        let mut current = new_state_record(Phase::Preparing);
        loop {
            let source = current.phase.forward().unwrap();
            let rollback = rollback_decided(&current);
            if rollback_allowed(&current, source) {
                validate_advance(&current, &rollback).unwrap();
                let sequence = rollback_sequence(&current);
                let terminal = sequence.last().unwrap().phase;
                let expected = if source == ForwardPhase::BootSyncStarted {
                    Phase::BootRepairUnverified
                } else {
                    Phase::RollbackComplete
                };
                assert_eq!(terminal, expected, "rollback from {source:?}");
            } else {
                assert!(matches!(
                    validate_advance(&current, &rollback),
                    Err(CodecError::InvalidRollbackSource(_)) | Err(CodecError::IllegalPhaseAdvance { .. })
                ));
            }
            if current.phase == Phase::Complete {
                break;
            }
            current = legal_forward_advance(&current);
        }

        let mut immediate_commit = without_previous_archive(
            new_state_record(Phase::RootLinksComplete),
            PreviousOrigin::SynthesizedEmpty,
        );
        immediate_commit.options.run_system_triggers = false;
        immediate_commit.options.run_boot_sync = false;
        assert_eq!(
            next_forward_phase(&immediate_commit, ForwardPhase::RootLinksComplete),
            Some(ForwardPhase::CommitDecided)
        );
        assert!(rollback_allowed(&immediate_commit, ForwardPhase::RootLinksComplete));
        validate_advance(&immediate_commit, &rollback_decided(&immediate_commit)).unwrap();

        let mut after_system = without_previous_archive(
            new_state_record(Phase::SystemTriggersComplete),
            PreviousOrigin::SynthesizedEmpty,
        );
        after_system.options.run_boot_sync = false;
        assert_eq!(
            next_forward_phase(&after_system, ForwardPhase::SystemTriggersComplete),
            Some(ForwardPhase::CommitDecided)
        );
        assert!(rollback_allowed(&after_system, ForwardPhase::SystemTriggersComplete));
        validate_advance(&after_system, &rollback_decided(&after_system)).unwrap();

        let mut after_archive = new_state_record(Phase::PreviousArchived);
        after_archive.options.run_boot_sync = false;
        assert_eq!(
            next_forward_phase(&after_archive, ForwardPhase::PreviousArchived),
            Some(ForwardPhase::CommitDecided)
        );
        assert!(rollback_allowed(&after_archive, ForwardPhase::PreviousArchived));
        validate_advance(&after_archive, &rollback_decided(&after_archive)).unwrap();
        assert!(!rollback_allowed(
            &new_state_record(Phase::BootSyncComplete),
            ForwardPhase::BootSyncComplete
        ));
    }

    #[test]
    fn conditional_advance_rejects_generation_transition_phase_and_layout_changes() {
        let current = creation_record();
        let legal = advance_record(&current, Phase::FreshStateAllocating);
        validate_advance(&current, &legal).unwrap();

        let mut invalid = legal.clone();
        invalid.generation = current.generation;
        assert!(matches!(
            validate_advance(&current, &invalid),
            Err(CodecError::GenerationMismatch { .. })
        ));

        let mut invalid = legal.clone();
        invalid.transition_id = other_id();
        assert!(matches!(
            validate_advance(&current, &invalid),
            Err(CodecError::TransitionChanged)
        ));

        let mut invalid = legal.clone();
        invalid.phase = Phase::FreshStateAllocated;
        invalid.candidate.id = Some(42);
        assert!(matches!(
            validate_advance(&current, &invalid),
            Err(CodecError::IllegalPhaseAdvance { .. })
        ));

        let mut invalid = legal.clone();
        invalid.options.run_boot_sync = false;
        assert!(matches!(
            validate_advance(&current, &invalid),
            Err(CodecError::ImmutableTransitionDataChanged)
        ));

        let mut exhausted = record(Phase::CandidatePrepared);
        exhausted.generation = u64::MAX;
        let mut next = exhausted.clone();
        next.generation = 1;
        next.phase = Phase::TransactionTriggersStarted;
        assert!(matches!(
            validate_advance(&exhausted, &next),
            Err(CodecError::GenerationExhausted)
        ));

        let mut identity_changed = legal.clone();
        identity_changed.candidate.usr_identity = identity(99);
        assert!(matches!(
            validate_advance(&current, &identity_changed),
            Err(CodecError::ImmutableTransitionDataChanged)
        ));
    }

    #[test]
    fn rollback_plan_requirements_are_derived_from_source_and_observation() {
        let preparing = new_state_record(Phase::Preparing);
        let mut rollback = rollback_decided(&preparing);
        let plan = rollback.rollback.as_ref().unwrap();
        assert_eq!(plan.previous_archive, RollbackAction::NotRequired);
        assert_eq!(plan.usr_exchange, RollbackAction::NotRequired);
        assert_eq!(plan.candidate.action, RollbackAction::Pending);
        assert_eq!(plan.fresh_db, RollbackAction::NotRequired);
        assert_eq!(plan.boot, BootRollback::NotRequired);
        encode(&rollback).unwrap();

        rollback.rollback.as_mut().unwrap().usr_exchange = RollbackAction::Pending;
        assert!(matches!(
            encode(&rollback),
            Err(CodecError::InvalidRollbackRequirement {
                action: "usr-exchange",
                ..
            })
        ));

        let mut candidate_omitted = rollback_decided(&preparing);
        candidate_omitted.rollback.as_mut().unwrap().candidate.action = RollbackAction::NotRequired;
        assert!(matches!(
            encode(&candidate_omitted),
            Err(CodecError::InvalidRollbackRequirement {
                action: "candidate",
                ..
            })
        ));

        let mut falsely_applied = rollback_decided(&preparing);
        falsely_applied.rollback.as_mut().unwrap().candidate.action = RollbackAction::Applied;
        assert!(matches!(
            encode(&falsely_applied),
            Err(CodecError::RollbackPlanPhaseMismatch {
                phase: Phase::RollbackDecided
            })
        ));

        let allocating = new_state_record(Phase::FreshStateAllocating);
        let mut absent = rollback_decided(&allocating);
        absent.candidate.id = None;
        absent.rollback.as_mut().unwrap().fresh_db = RollbackAction::AlreadySatisfied;
        encode(&absent).unwrap();

        let mut row_observed = rollback_decided(&allocating);
        assert_eq!(row_observed.candidate.id, Some(42));
        assert_eq!(
            row_observed.rollback.as_ref().unwrap().fresh_db,
            RollbackAction::Pending
        );
        encode(&row_observed).unwrap();
        row_observed.candidate.id = None;
        assert!(matches!(encode(&row_observed), Err(CodecError::CandidateStateLayout)));

        let mut row_removed_concurrently = rollback_decided(&allocating);
        row_removed_concurrently.rollback.as_mut().unwrap().candidate.action = RollbackAction::AlreadySatisfied;
        let invalidation_intent = advance_record(&row_removed_concurrently, Phase::FreshDbInvalidationIntent);
        validate_advance(&row_removed_concurrently, &invalidation_intent).unwrap();
        let mut invalidated = advance_record(&invalidation_intent, Phase::FreshDbInvalidated);
        invalidated.rollback.as_mut().unwrap().fresh_db = RollbackAction::AlreadySatisfied;
        assert_eq!(invalidated.candidate.id, Some(42));
        validate_advance(&invalidation_intent, &invalidated).unwrap();
        encode(&invalidated).unwrap();

        let source = new_state_record(Phase::PreviousArchiveIntent);
        let mut observed_safe = rollback_decided(&source);
        let plan = observed_safe.rollback.as_mut().unwrap();
        plan.previous_archive = RollbackAction::AlreadySatisfied;
        plan.usr_exchange = RollbackAction::AlreadySatisfied;
        plan.candidate.action = RollbackAction::AlreadySatisfied;
        plan.fresh_db = RollbackAction::AlreadySatisfied;
        encode(&observed_safe).unwrap();
        assert_eq!(
            next_rollback_phase(observed_safe.rollback.as_ref().unwrap(), observed_safe.phase),
            Some(Phase::RollbackComplete)
        );

        observed_safe.rollback.as_mut().unwrap().previous_archive = RollbackAction::NotRequired;
        assert!(matches!(
            encode(&observed_safe),
            Err(CodecError::InvalidRollbackRequirement {
                action: "previous-archive",
                ..
            })
        ));

        let before_exchange = rollback_decided(&new_state_record(Phase::TransactionTriggersComplete));
        assert_eq!(
            before_exchange.rollback.as_ref().unwrap().usr_exchange,
            RollbackAction::NotRequired
        );
        let at_exchange = rollback_decided(&new_state_record(Phase::UsrExchangeIntent));
        assert_eq!(
            at_exchange.rollback.as_ref().unwrap().usr_exchange,
            RollbackAction::Pending
        );
        let before_archive = rollback_decided(&new_state_record(Phase::SystemTriggersComplete));
        assert_eq!(
            before_archive.rollback.as_ref().unwrap().previous_archive,
            RollbackAction::NotRequired
        );
        let at_archive = rollback_decided(&new_state_record(Phase::PreviousArchiveIntent));
        assert_eq!(
            at_archive.rollback.as_ref().unwrap().previous_archive,
            RollbackAction::Pending
        );

        let no_archive = without_previous_archive(
            new_state_record(Phase::BootSyncStarted),
            PreviousOrigin::SynthesizedEmpty,
        );
        assert_eq!(
            rollback_decided(&no_archive)
                .rollback
                .as_ref()
                .unwrap()
                .previous_archive,
            RollbackAction::NotRequired
        );
        assert_eq!(
            rollback_decided(&reblit_record(Phase::BootSyncStarted))
                .rollback
                .as_ref()
                .unwrap()
                .fresh_db,
            RollbackAction::NotRequired
        );
    }

    #[test]
    fn rollback_candidate_disposition_and_external_effects_are_derived() {
        let cases = [
            (
                new_state_record(Phase::CandidatePrepared),
                AbortDisposition::Quarantine,
                false,
            ),
            (
                new_state_record(Phase::TransactionTriggersStarted),
                AbortDisposition::Quarantine,
                true,
            ),
            (
                reblit_record(Phase::TransactionTriggersStarted),
                AbortDisposition::Quarantine,
                true,
            ),
            (archived_record(Phase::Preparing), AbortDisposition::Rearchive, false),
            (
                archived_record(Phase::SystemTriggersStarted),
                AbortDisposition::Quarantine,
                true,
            ),
            (
                archived_record(Phase::SystemTriggersComplete),
                AbortDisposition::Rearchive,
                true,
            ),
            (
                archived_record(Phase::PreviousArchiveIntent),
                AbortDisposition::Rearchive,
                true,
            ),
        ];
        for (source, disposition, external_effects) in cases {
            let rollback = rollback_decided(&source);
            let plan = rollback.rollback.as_ref().unwrap();
            assert_eq!(plan.candidate.disposition, disposition);
            assert_eq!(plan.external_effects_may_remain, external_effects);
            encode(&rollback).unwrap();
        }

        let mut invalid = rollback_decided(&archived_record(Phase::Preparing));
        invalid.rollback.as_mut().unwrap().candidate.disposition = AbortDisposition::Quarantine;
        assert!(matches!(
            encode(&invalid),
            Err(CodecError::InvalidCandidateDisposition { .. })
        ));

        let mut invalid = rollback_decided(&new_state_record(Phase::TransactionTriggersStarted));
        invalid.rollback.as_mut().unwrap().external_effects_may_remain = false;
        assert!(matches!(
            encode(&invalid),
            Err(CodecError::InvalidExternalEffectsEvidence { .. })
        ));
    }

    #[test]
    fn rollback_recovery_order_and_status_updates_are_exact() {
        let mut source = new_state_record(Phase::PreviousArchiveIntent);
        source.options.run_boot_sync = false;
        let sequence = rollback_sequence(&source);
        assert_eq!(
            sequence.iter().map(|record| record.phase).collect::<Vec<_>>(),
            [
                Phase::RollbackDecided,
                Phase::PreviousRestoreIntent,
                Phase::PreviousRestoredToStaging,
                Phase::ReverseExchangeIntent,
                Phase::UsrRestored,
                Phase::CandidatePreserveIntent,
                Phase::CandidatePreserved,
                Phase::FreshDbInvalidationIntent,
                Phase::FreshDbInvalidated,
                Phase::RollbackComplete,
            ]
        );

        let decided = &sequence[0];
        let previous_intent = &sequence[1];
        assert_eq!(decided.rollback, previous_intent.rollback);

        let mut changed_during_intent = previous_intent.clone();
        changed_during_intent.rollback.as_mut().unwrap().candidate.action = RollbackAction::AlreadySatisfied;
        assert!(matches!(
            validate_advance(decided, &changed_during_intent),
            Err(CodecError::RollbackPlanChangedIllegally)
        ));
        assert_eq!(
            sequence[2].rollback.as_ref().unwrap().previous_archive,
            RollbackAction::Applied
        );
        assert_eq!(
            sequence[4].rollback.as_ref().unwrap().usr_exchange,
            RollbackAction::Applied
        );
        assert_eq!(
            sequence[6].rollback.as_ref().unwrap().candidate.action,
            RollbackAction::Applied
        );
        assert_eq!(sequence[8].rollback.as_ref().unwrap().fresh_db, RollbackAction::Applied);

        let mut skipped = rollback_decided(&source);
        let plan = skipped.rollback.as_mut().unwrap();
        plan.previous_archive = RollbackAction::AlreadySatisfied;
        plan.usr_exchange = RollbackAction::AlreadySatisfied;
        assert_eq!(
            next_rollback_phase(skipped.rollback.as_ref().unwrap(), skipped.phase),
            Some(Phase::CandidatePreserveIntent)
        );
        encode(&skipped).unwrap();

        let candidate_intent = advance_record(&skipped, Phase::CandidatePreserveIntent);
        validate_advance(&skipped, &candidate_intent).unwrap();
        let mut candidate_complete = advance_record(&candidate_intent, Phase::CandidatePreserved);
        candidate_complete.rollback.as_mut().unwrap().candidate.action = RollbackAction::AlreadySatisfied;
        validate_advance(&candidate_intent, &candidate_complete).unwrap();

        let mut out_of_order = candidate_intent.clone();
        out_of_order.phase = Phase::FreshDbInvalidationIntent;
        assert!(matches!(
            encode(&out_of_order),
            Err(CodecError::RollbackPlanPhaseMismatch { .. })
        ));
    }

    #[test]
    fn ambiguous_boot_repair_is_terminal_unverified_and_nondeletable() {
        let source = new_state_record(Phase::BootSyncStarted);
        let mut prematurely_unverified = rollback_decided(&source);
        prematurely_unverified.rollback.as_mut().unwrap().boot = BootRollback::Unverified;
        assert!(matches!(
            encode(&prematurely_unverified),
            Err(CodecError::RollbackPlanPhaseMismatch {
                phase: Phase::RollbackDecided
            })
        ));

        let sequence = rollback_sequence(&source);
        let terminal = sequence.last().unwrap();
        assert_eq!(terminal.phase, Phase::BootRepairUnverified);
        assert_eq!(terminal.rollback.as_ref().unwrap().boot, BootRollback::Unverified);
        let mut attempted = terminal.clone();
        attempted.generation += 1;
        assert!(matches!(
            validate_advance(terminal, &attempted),
            Err(CodecError::TerminalPhaseAdvance)
        ));

        let (_temporary, store) = fixture();
        assert!(matches!(store.delete(terminal), Err(StorageError::DeleteNonterminal)));
        assert!(!terminal.phase.deletable());
        assert!(record(Phase::RollbackComplete).phase.deletable());
    }

    #[test]
    fn shared_transition_id_is_the_only_journal_correlation_encoding() {
        let value = new_state_record(Phase::Preparing);
        let encoded = encode(&value).unwrap();
        let payload = std::str::from_utf8(&encoded[HEADER_SIZE..]).unwrap();
        assert!(payload.contains("\"transition_id\":\"0123456789abcdef0123456789abcdef\""));
        assert!(!payload.contains("transaction_id"));
        assert_eq!(decode(&encoded).unwrap().transition_id, id());
    }

    #[test]
    fn journal_directory_and_canonical_file_have_exact_private_metadata() {
        let (temporary, store) = fixture();
        store.create(&creation_record()).unwrap();
        let journal = temporary.path().join(".cast/journal");
        let journal_metadata = fs::metadata(journal).unwrap();
        let canonical_metadata = fs::metadata(canonical(temporary.path())).unwrap();
        let lock_metadata = fs::metadata(temporary.path().join(".cast/journal/state-transition.lock")).unwrap();
        assert_eq!(journal_metadata.permissions().mode() & 0o7777, 0o700);
        assert_eq!(canonical_metadata.permissions().mode() & 0o7777, 0o600);
        assert_eq!(canonical_metadata.nlink(), 1);
        assert!(canonical_metadata.file_type().is_file());
        assert_eq!(lock_metadata.permissions().mode() & 0o7777, 0o600);
        assert_eq!(lock_metadata.nlink(), 1);
    }

    #[test]
    fn restrictive_umask_structural_residue_is_normalized_by_pinned_inode() {
        let temporary = tempfile::tempdir().unwrap();
        let cast = temporary.path().join(".cast");
        let journal = cast.join("journal");
        let lock = journal.join("state-transition.lock");
        fs::create_dir(&cast).unwrap();
        fs::set_permissions(&cast, fs::Permissions::from_mode(0o700)).unwrap();
        fs::create_dir(&journal).unwrap();
        fs::write(&lock, b"").unwrap();
        fs::set_permissions(&lock, fs::Permissions::from_mode(0o000)).unwrap();
        fs::set_permissions(&journal, fs::Permissions::from_mode(0o000)).unwrap();

        let store = TransitionJournalStore::open(temporary.path()).unwrap();
        assert_eq!(fs::metadata(&journal).unwrap().permissions().mode() & 0o7777, 0o700);
        assert_eq!(fs::metadata(&lock).unwrap().permissions().mode() & 0o7777, 0o600);
        drop(store);

        // Once normalized, the ordinary exact-mode reopen path remains valid.
        TransitionJournalStore::open(temporary.path()).unwrap();
    }

    #[test]
    fn atomic_initial_update_and_delete_round_trip() {
        let (temporary, store) = fixture();
        assert!(store.load().unwrap().is_none());
        let mut first = archived_record(Phase::Preparing);
        first.generation = 1;
        store.create(&first).unwrap();
        assert_eq!(store.load().unwrap(), Some(first.clone()));

        let rollback = satisfied_preparing_rollback(&first);
        store.advance(&first, &rollback).unwrap();
        let complete = advance_record(&rollback, Phase::RollbackComplete);
        store.advance(&rollback, &complete).unwrap();
        let mut names = fs::read_dir(temporary.path().join(".cast/journal"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        names.sort();
        assert_eq!(names, ["state-transition", "state-transition.lock"]);

        assert!(store.delete(&complete).unwrap());
        assert!(!store.delete(&complete).unwrap());
        assert!(store.load().unwrap().is_none());
    }

    #[test]
    fn create_advance_and_delete_are_exactly_conditional() {
        let (_temporary, store) = fixture();
        let initial = creation_record();
        store.create(&initial).unwrap();
        assert!(matches!(
            store.create(&initial),
            Err(StorageError::CanonicalAlreadyExists)
        ));

        let mut foreign = creation_record();
        foreign.transition_id = other_id();
        let foreign_next = advance_record(&foreign, Phase::FreshStateAllocating);
        assert!(matches!(
            store.advance(&foreign, &foreign_next),
            Err(StorageError::ExpectedRecordMismatch)
        ));
        assert_eq!(store.load().unwrap(), Some(initial));
    }

    #[test]
    fn one_shared_store_serializes_competing_compare_and_swap_advances() {
        let (_temporary, store) = fixture();
        let initial = creation_record();
        let allocating = advance_record(&initial, Phase::FreshStateAllocating);
        store.create(&initial).unwrap();
        store.advance(&initial, &allocating).unwrap();

        let first = advance_record(&allocating, Phase::FreshStateAllocated);
        let mut second = first.clone();
        second.candidate.id = Some(43);
        let store = Arc::new(store);
        let held = store.operation_lock.lock().unwrap();
        let (attempted_tx, attempted_rx) = mpsc::channel();
        let mut workers = Vec::new();
        for proposal in [first.clone(), second.clone()] {
            let store = Arc::clone(&store);
            let expected = allocating.clone();
            let attempted = attempted_tx.clone();
            workers.push(thread::spawn(move || {
                attempted.send(()).unwrap();
                let result = store.advance(&expected, &proposal);
                (proposal, result)
            }));
        }
        drop(attempted_tx);
        attempted_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        attempted_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        drop(held);

        let results = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(results.iter().filter(|(_, result)| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|(_, result)| matches!(result, Err(StorageError::ExpectedRecordMismatch)))
                .count(),
            1
        );
        let winner = results
            .into_iter()
            .find_map(|(proposal, result)| result.is_ok().then_some(proposal))
            .unwrap();
        assert_eq!(store.load().unwrap(), Some(winner));
    }

    #[test]
    fn internal_lock_serializes_stale_thread_writers() {
        let (temporary, store) = fixture();
        let initial = creation_record();
        let next = advance_record(&initial, Phase::FreshStateAllocating);
        store.create(&initial).unwrap();

        let root = temporary.path().to_owned();
        let thread_initial = initial.clone();
        let thread_next = next.clone();
        let (attempted_tx, attempted_rx) = mpsc::channel();
        let (finished_tx, finished_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            attempted_tx.send(()).unwrap();
            let competing = TransitionJournalStore::open(&root).unwrap();
            let stale_rejected = matches!(
                competing.advance(&thread_initial, &thread_next),
                Err(StorageError::ExpectedRecordMismatch)
            );
            finished_tx.send(stale_rejected).unwrap();
        });

        attempted_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(matches!(
            finished_rx.recv_timeout(Duration::from_millis(150)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        store.advance(&initial, &next).unwrap();
        drop(store);
        assert!(finished_rx.recv_timeout(Duration::from_secs(5)).unwrap());
        worker.join().unwrap();

        let store = TransitionJournalStore::open(temporary.path()).unwrap();
        assert_eq!(store.load().unwrap(), Some(next));
    }

    #[test]
    fn internal_lock_prevents_stale_writer_resurrection_after_delete() {
        let (temporary, store) = fixture();
        let initial = creation_record();
        store.create(&initial).unwrap();
        let mut preterminal = initial;
        while preterminal.phase != Phase::CommitCleanupComplete {
            let next = legal_forward_advance(&preterminal);
            store.advance(&preterminal, &next).unwrap();
            preterminal = next;
        }
        let terminal = legal_forward_advance(&preterminal);

        let root = temporary.path().to_owned();
        let thread_expected = preterminal.clone();
        let thread_terminal = terminal.clone();
        let (attempted_tx, attempted_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            attempted_tx.send(()).unwrap();
            let competing = TransitionJournalStore::open(&root).unwrap();
            matches!(
                competing.advance(&thread_expected, &thread_terminal),
                Err(StorageError::CanonicalMissing)
            )
        });
        attempted_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        store.advance(&preterminal, &terminal).unwrap();
        assert!(store.delete(&terminal).unwrap());
        drop(store);
        assert!(worker.join().unwrap());

        let store = TransitionJournalStore::open(temporary.path()).unwrap();
        assert!(store.load().unwrap().is_none());
    }

    #[test]
    fn exclusive_lock_serializes_subprocess_open() {
        const CHILD: &str = "CAST_TRANSITION_JOURNAL_LOCK_CHILD";
        const ROOT: &str = "CAST_TRANSITION_JOURNAL_LOCK_ROOT";
        const STARTED: &str = "CAST_TRANSITION_JOURNAL_LOCK_STARTED";
        const ACQUIRED: &str = "CAST_TRANSITION_JOURNAL_LOCK_ACQUIRED";
        const TEST: &str = "transition_journal::tests::exclusive_lock_serializes_subprocess_open";

        if std::env::var_os(CHILD).is_some() {
            let root = PathBuf::from(std::env::var_os(ROOT).unwrap());
            let started = PathBuf::from(std::env::var_os(STARTED).unwrap());
            let acquired = PathBuf::from(std::env::var_os(ACQUIRED).unwrap());
            fs::write(started, b"started").unwrap();
            let _store = TransitionJournalStore::open(&root).unwrap();
            fs::write(acquired, b"acquired").unwrap();
            return;
        }

        let (temporary, store) = fixture();
        let started = temporary.path().join("child-started");
        let acquired = temporary.path().join("child-acquired");
        let mut child = Command::new(std::env::current_exe().unwrap())
            .arg(TEST)
            .arg("--exact")
            .arg("--nocapture")
            .env(CHILD, "1")
            .env(ROOT, temporary.path())
            .env(STARTED, &started)
            .env(ACQUIRED, &acquired)
            .spawn()
            .unwrap();

        let deadline = Instant::now() + Duration::from_secs(5);
        while !started.exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(
            started.exists(),
            "subprocess never attempted to acquire the journal lock"
        );
        thread::sleep(Duration::from_millis(150));
        assert!(!acquired.exists());
        assert!(child.try_wait().unwrap().is_none());
        drop(store);
        let status = child.wait().unwrap();
        assert!(status.success());
        assert!(acquired.exists());
    }

    #[test]
    fn stale_pid_reuse_temporaries_over_old_retry_limit_are_cleaned_boundedly() {
        let (temporary, store) = fixture();
        create_stale_temporaries(temporary.path(), 160);
        drop(store);

        let store = TransitionJournalStore::open(temporary.path()).unwrap();
        let names = fs::read_dir(temporary.path().join(".cast/journal"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        assert_eq!(names, ["state-transition.lock"]);
        let temporary_record = store.create_temporary().unwrap();
        store.cleanup_temporary(&temporary_record).unwrap();
    }

    #[test]
    fn stale_temporary_cleanup_has_a_hard_cap_and_never_partially_cleans() {
        let (temporary, store) = fixture();
        store.create(&creation_record()).unwrap();
        create_stale_temporaries(temporary.path(), MAX_STALE_TEMPORARIES + 1);
        drop(store);

        assert!(matches!(
            TransitionJournalStore::open(temporary.path()),
            Err(StorageError::TooManyStaleTemporaries)
        ));
        let stale = fs::read_dir(temporary.path().join(".cast/journal"))
            .unwrap()
            .filter(|entry| valid_temporary_name(entry.as_ref().unwrap().file_name().as_bytes()))
            .count();
        assert_eq!(stale, MAX_STALE_TEMPORARIES + 1);
    }

    #[test]
    fn unsafe_stale_temporary_is_preserved_and_never_followed() {
        let (temporary, store) = fixture();
        let target = temporary.path().join("outside-target");
        fs::write(&target, b"outside").unwrap();
        let stale = stale_temporary_path(temporary.path(), 9);
        symlink(&target, &stale).unwrap();
        drop(store);

        assert!(matches!(
            TransitionJournalStore::open(temporary.path()),
            Err(StorageError::ValidateStaleTemporary { .. })
        ));
        assert_eq!(fs::read(target).unwrap(), b"outside");
        assert!(fs::symlink_metadata(stale).unwrap().file_type().is_symlink());
    }

    #[test]
    fn crash_before_publish_keeps_old_canonical_and_cleans_temp() {
        let (temporary, store) = fixture();
        let initial = creation_record();
        let next = advance_record(&initial, Phase::FreshStateAllocating);
        store.create(&initial).unwrap();
        let mut pending = store.create_temporary().unwrap();
        pending.file.write_all(&encode(&next).unwrap()).unwrap();
        pending.file.sync_all().unwrap();
        drop(pending);
        drop(store);

        let store = TransitionJournalStore::open(temporary.path()).unwrap();
        assert_eq!(store.load().unwrap(), Some(initial));
        assert_eq!(fs::read_dir(temporary.path().join(".cast/journal")).unwrap().count(), 2);
    }

    #[test]
    fn crash_with_a_partial_temporary_never_promotes_it() {
        let (temporary, store) = fixture();
        let initial = creation_record();
        let next = advance_record(&initial, Phase::FreshStateAllocating);
        store.create(&initial).unwrap();
        let mut pending = store.create_temporary().unwrap();
        let framed = encode(&next).unwrap();
        pending.file.write_all(&framed[..framed.len() / 2]).unwrap();
        drop(pending);
        drop(store);

        let store = TransitionJournalStore::open(temporary.path()).unwrap();
        assert_eq!(store.load().unwrap(), Some(initial));
        assert_no_journal_temporaries(temporary.path());
    }

    #[test]
    fn crash_after_exchange_keeps_new_canonical_and_cleans_displaced_record() {
        let (temporary, store) = fixture();
        let initial = creation_record();
        let next = advance_record(&initial, Phase::FreshStateAllocating);
        store.create(&initial).unwrap();
        let mut pending = store.create_temporary().unwrap();
        pending.file.write_all(&encode(&next).unwrap()).unwrap();
        pending.file.sync_all().unwrap();
        renameat2(
            store.directory.as_raw_fd(),
            &pending.name,
            store.directory.as_raw_fd(),
            CANONICAL_NAME,
            nix::libc::RENAME_EXCHANGE,
        )
        .unwrap();
        drop(pending);
        drop(store);

        let store = TransitionJournalStore::open(temporary.path()).unwrap();
        assert_eq!(store.load().unwrap(), Some(next));
        assert_eq!(fs::read_dir(temporary.path().join(".cast/journal")).unwrap().count(), 2);
    }

    #[test]
    fn injected_create_publish_faults_reopen_to_absent_or_exact_new_record() {
        for (point, published) in [
            (StorageFaultPoint::TemporarySync, false),
            (StorageFaultPoint::InitialRename, false),
            (StorageFaultPoint::InitialDirectorySync, true),
        ] {
            let (temporary, store) = fixture();
            let sentinel = temporary.path().join("outside-sentinel");
            fs::write(&sentinel, b"foreign").unwrap();
            let record = creation_record();
            arm_storage_fault(point);
            assert!(store.create(&record).is_err(), "fault {point:?} unexpectedly succeeded");
            assert_storage_fault_consumed();
            drop(store);

            let reopened = TransitionJournalStore::open(temporary.path()).unwrap();
            assert_eq!(reopened.load().unwrap(), published.then_some(record));
            assert_no_journal_temporaries(temporary.path());
            assert_eq!(fs::read(&sentinel).unwrap(), b"foreign");
        }
    }

    #[test]
    fn injected_update_publish_faults_reopen_to_exact_old_or_new_record() {
        for (point, published_new) in [
            (StorageFaultPoint::TemporarySync, false),
            (StorageFaultPoint::UpdateExchange, false),
            (StorageFaultPoint::UpdateFirstDirectorySync, true),
            (StorageFaultPoint::DisplacedUnlink, true),
            (StorageFaultPoint::UpdateFinalDirectorySync, true),
        ] {
            let (temporary, store) = fixture();
            let sentinel = temporary.path().join("outside-sentinel");
            fs::write(&sentinel, b"foreign").unwrap();
            let old = creation_record();
            let new = advance_record(&old, Phase::FreshStateAllocating);
            store.create(&old).unwrap();
            arm_storage_fault(point);
            assert!(
                store.advance(&old, &new).is_err(),
                "fault {point:?} unexpectedly succeeded"
            );
            assert_storage_fault_consumed();
            drop(store);

            let reopened = TransitionJournalStore::open(temporary.path()).unwrap();
            assert_eq!(reopened.load().unwrap(), Some(if published_new { new } else { old }));
            assert_no_journal_temporaries(temporary.path());
            assert_eq!(fs::read(&sentinel).unwrap(), b"foreign");
        }
    }

    #[test]
    fn injected_delete_faults_reopen_to_exact_terminal_or_absence() {
        for (point, deleted) in [
            (StorageFaultPoint::CanonicalUnlink, false),
            (StorageFaultPoint::DeleteDirectorySync, true),
        ] {
            let (temporary, store) = fixture();
            let sentinel = temporary.path().join("outside-sentinel");
            fs::write(&sentinel, b"foreign").unwrap();
            let mut initial = archived_record(Phase::Preparing);
            initial.generation = 1;
            store.create(&initial).unwrap();
            let rollback = satisfied_preparing_rollback(&initial);
            store.advance(&initial, &rollback).unwrap();
            let terminal = advance_record(&rollback, Phase::RollbackComplete);
            store.advance(&rollback, &terminal).unwrap();

            arm_storage_fault(point);
            assert!(
                store.delete(&terminal).is_err(),
                "fault {point:?} unexpectedly succeeded"
            );
            assert_storage_fault_consumed();
            drop(store);

            let reopened = TransitionJournalStore::open(temporary.path()).unwrap();
            assert_eq!(reopened.load().unwrap(), (!deleted).then_some(terminal));
            assert_no_journal_temporaries(temporary.path());
            assert_eq!(fs::read(&sentinel).unwrap(), b"foreign");
        }
    }

    #[test]
    fn durability_checkpoints_prove_fsync_order_for_create_update_and_delete() {
        let (_temporary, store) = fixture();
        take_durability_checkpoints();
        let initial = creation_record();
        store.create(&initial).unwrap();
        assert_eq!(
            take_durability_checkpoints(),
            [
                DurabilityCheckpoint::TemporaryFullySynced,
                DurabilityCheckpoint::CanonicalPublished,
                DurabilityCheckpoint::JournalDirectorySynced,
            ]
        );

        let next = advance_record(&initial, Phase::FreshStateAllocating);
        store.advance(&initial, &next).unwrap();
        assert_eq!(
            take_durability_checkpoints(),
            [
                DurabilityCheckpoint::TemporaryFullySynced,
                DurabilityCheckpoint::CanonicalExchanged,
                DurabilityCheckpoint::JournalDirectorySynced,
                DurabilityCheckpoint::DisplacedUnlinked,
                DurabilityCheckpoint::JournalDirectorySynced,
            ]
        );

        let terminal = advance_to_complete(&store, next);
        take_durability_checkpoints();
        store.delete(&terminal).unwrap();
        assert_eq!(
            take_durability_checkpoints(),
            [
                DurabilityCheckpoint::CanonicalUnlinked,
                DurabilityCheckpoint::JournalDirectorySynced,
            ]
        );
    }

    #[test]
    fn corrupt_canonical_is_never_replaced_or_recovered_from_temporary() {
        let (temporary, store) = fixture();
        let canonical = canonical(temporary.path());
        fs::write(&canonical, b"corrupt").unwrap();
        fs::set_permissions(&canonical, fs::Permissions::from_mode(0o600)).unwrap();
        let fallback = temporary.path().join(".cast/journal/.state-transition.tmp-fallback");
        fs::write(&fallback, encode(&record(Phase::Preparing)).unwrap()).unwrap();
        fs::set_permissions(&fallback, fs::Permissions::from_mode(0o600)).unwrap();

        assert!(matches!(store.load(), Err(StorageError::Decode(_))));
        assert!(matches!(store.create(&creation_record()), Err(StorageError::Decode(_))));
        assert!(matches!(
            store.delete(&record(Phase::RollbackComplete)),
            Err(StorageError::Decode(_))
        ));
        assert_eq!(fs::read(canonical).unwrap(), b"corrupt");
        assert!(fallback.exists());
    }

    #[test]
    fn absent_canonical_never_promotes_a_valid_temporary() {
        let (temporary, store) = fixture();
        let fallback = temporary.path().join(".cast/journal/.state-transition.tmp-fallback");
        fs::write(&fallback, encode(&record(Phase::Preparing)).unwrap()).unwrap();
        fs::set_permissions(&fallback, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(store.load().unwrap().is_none());
    }

    #[test]
    fn canonical_file_reader_enforces_the_exact_n_and_n_plus_one_boundary() {
        let (temporary, store) = fixture();
        let canonical = canonical(temporary.path());
        fs::write(&canonical, vec![0; MAX_CANONICAL_RECORD_BYTES]).unwrap();
        fs::set_permissions(&canonical, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(matches!(
            store.load(),
            Err(StorageError::Decode(CodecError::InvalidMagic))
        ));

        fs::write(&canonical, vec![0; MAX_CANONICAL_RECORD_BYTES + 1]).unwrap();
        assert!(matches!(store.load(), Err(StorageError::ReadCanonical { .. })));
    }

    #[test]
    fn temporary_files_are_exclusive_unique_and_private() {
        let (temporary, store) = fixture();
        let first = store.create_temporary().unwrap();
        let second = store.create_temporary().unwrap();
        assert_ne!(first.name, second.name);
        for entry in [&first, &second] {
            let metadata = entry.file.metadata().unwrap();
            assert_eq!(metadata.permissions().mode() & 0o7777, 0o600);
            assert_eq!(metadata.nlink(), 1);
            assert!(metadata.file_type().is_file());
        }
        store.cleanup_temporary(&first).unwrap();
        store.cleanup_temporary(&second).unwrap();
        let names = fs::read_dir(temporary.path().join(".cast/journal"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        assert_eq!(names, ["state-transition.lock"]);
    }

    #[test]
    fn canonical_symlink_is_rejected_without_touching_its_target() {
        let (temporary, store) = fixture();
        let target = temporary.path().join("target");
        fs::write(&target, b"outside").unwrap();
        symlink(&target, canonical(temporary.path())).unwrap();
        assert!(matches!(store.load(), Err(StorageError::OpenCanonical { .. })));
        assert!(store.create(&creation_record()).is_err());
        assert!(store.delete(&record(Phase::RollbackComplete)).is_err());
        assert_eq!(fs::read(target).unwrap(), b"outside");
    }

    #[test]
    fn canonical_mode_and_hardlink_attacks_are_rejected() {
        let (temporary, store) = fixture();
        store.create(&creation_record()).unwrap();
        let canonical = canonical(temporary.path());
        fs::set_permissions(&canonical, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(store.load(), Err(StorageError::ValidateCanonical { .. })));

        fs::set_permissions(&canonical, fs::Permissions::from_mode(0o600)).unwrap();
        fs::hard_link(&canonical, temporary.path().join("extra-link")).unwrap();
        assert!(matches!(store.load(), Err(StorageError::ValidateCanonical { .. })));
    }

    #[test]
    fn canonical_wrong_inode_kind_is_rejected() {
        let (temporary, store) = fixture();
        fs::create_dir(canonical(temporary.path())).unwrap();
        assert!(store.load().is_err());
        assert!(store.create(&creation_record()).is_err());
        assert!(store.delete(&record(Phase::RollbackComplete)).is_err());
    }

    #[test]
    fn journal_directory_symlink_and_mode_attacks_are_rejected() {
        let temporary = tempfile::tempdir().unwrap();
        let cast = temporary.path().join(".cast");
        fs::create_dir(&cast).unwrap();
        fs::set_permissions(&cast, fs::Permissions::from_mode(0o700)).unwrap();
        let target = temporary.path().join("target");
        fs::create_dir(&target).unwrap();
        let target_mode = fs::metadata(&target).unwrap().permissions().mode() & 0o7777;
        symlink(&target, cast.join("journal")).unwrap();
        assert!(matches!(
            TransitionJournalStore::open(temporary.path()),
            Err(StorageError::OpenJournalDirectory { .. })
        ));
        assert!(
            fs::symlink_metadata(cast.join("journal"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o7777,
            target_mode
        );
        assert!(fs::read_dir(target).unwrap().next().is_none());

        fs::remove_file(cast.join("journal")).unwrap();
        fs::create_dir(cast.join("journal")).unwrap();
        fs::set_permissions(cast.join("journal"), fs::Permissions::from_mode(0o770)).unwrap();
        assert!(matches!(
            TransitionJournalStore::open(temporary.path()),
            Err(StorageError::OpenJournalDirectory { .. })
        ));
    }

    #[test]
    fn cast_directory_symlink_is_rejected() {
        let temporary = tempfile::tempdir().unwrap();
        let target = temporary.path().join("target");
        fs::create_dir(&target).unwrap();
        symlink(&target, temporary.path().join(".cast")).unwrap();
        assert!(matches!(
            TransitionJournalStore::open(temporary.path()),
            Err(StorageError::OpenCastDirectory { .. })
        ));
    }

    #[test]
    fn internal_lock_symlink_mode_and_hardlink_attacks_are_rejected() {
        let temporary = tempfile::tempdir().unwrap();
        let cast = temporary.path().join(".cast");
        let journal = cast.join("journal");
        fs::create_dir(&cast).unwrap();
        fs::set_permissions(&cast, fs::Permissions::from_mode(0o700)).unwrap();
        fs::create_dir(&journal).unwrap();
        fs::set_permissions(&journal, fs::Permissions::from_mode(0o700)).unwrap();
        let lock = journal.join("state-transition.lock");
        let target = temporary.path().join("outside-lock-target");
        fs::write(&target, b"outside").unwrap();
        symlink(&target, &lock).unwrap();
        assert!(matches!(
            TransitionJournalStore::open(temporary.path()),
            Err(StorageError::ValidateLock { .. })
        ));
        assert!(fs::symlink_metadata(&lock).unwrap().file_type().is_symlink());
        assert_eq!(fs::read(&target).unwrap(), b"outside");

        fs::remove_file(&lock).unwrap();
        fs::write(&lock, b"").unwrap();
        fs::set_permissions(&lock, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(
            TransitionJournalStore::open(temporary.path()),
            Err(StorageError::ValidateLock { .. })
        ));
        assert_eq!(fs::metadata(&lock).unwrap().permissions().mode() & 0o7777, 0o644);

        fs::set_permissions(&lock, fs::Permissions::from_mode(0o600)).unwrap();
        fs::hard_link(&lock, temporary.path().join("lock-hardlink")).unwrap();
        assert!(matches!(
            TransitionJournalStore::open(temporary.path()),
            Err(StorageError::ValidateLock { .. })
        ));
    }

    #[test]
    fn metadata_validators_reject_wrong_owner_and_inode_identity() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("file");
        fs::write(&path, b"record").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let metadata = fs::metadata(&path).unwrap();
        assert!(require_safe_regular_file_metadata(&metadata, &path, metadata.uid().wrapping_add(1)).is_err());

        let other = temporary.path().join("other");
        fs::write(&other, b"record").unwrap();
        let first = fs::File::open(path).unwrap();
        let second = fs::File::open(other).unwrap();
        assert!(matches!(
            require_same_inode(inode_identity(&first).unwrap(), inode_identity(&second).unwrap()),
            Err(StorageError::CanonicalChanged)
        ));
    }

    #[test]
    fn temporary_names_are_internal_single_bounded_components() {
        for _ in 0..256 {
            let name = temporary_name();
            let bytes = name.to_bytes();
            assert!(bytes.len() <= 255);
            assert!(!bytes.contains(&b'/'));
            assert!(bytes.starts_with(b".state-transition.tmp-"));
        }
    }
}
