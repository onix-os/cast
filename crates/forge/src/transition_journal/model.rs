use std::io;

use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

use crate::boot_publication::BootPublicationReceiptPair;
use crate::state::TransitionId;

use super::codec::{CodecError, MAX_QUARANTINE_NAME_BYTES, PAYLOAD_FORMAT, PAYLOAD_VERSION};

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
    BootRepairComplete,
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

/// Canonical kernel boot identifier captured when a transition is created.
///
/// Runtime inode and mount witnesses are comparable only while this boot ID
/// and the mount-namespace identity below still match. The ID is deliberately
/// kept separate from the durable per-tree token.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub(crate) struct BootId(pub(super) String);

impl BootId {
    pub(crate) const TEXT_LENGTH: usize = 36;

    pub(crate) fn parse(value: impl Into<String>) -> Result<Self, CodecError> {
        let value = Self(value.into());
        value.validate()?;
        Ok(value)
    }

    pub(super) fn validate(&self) -> Result<(), CodecError> {
        let bytes = self.0.as_bytes();
        let canonical = bytes.len() == Self::TEXT_LENGTH
            && bytes.iter().enumerate().all(|(index, byte)| {
                if matches!(index, 8 | 13 | 18 | 23) {
                    *byte == b'-'
                } else {
                    byte.is_ascii_digit() || (b'a'..=b'f').contains(byte)
                }
            });
        let nonzero = bytes.iter().any(|byte| !matches!(*byte, b'0' | b'-'));
        if !canonical || !nonzero {
            return Err(CodecError::InvalidBootId);
        }
        Ok(())
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for BootId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(D::Error::custom)
    }
}

/// Immutable random logical identity assigned to one `/usr` tree.
///
/// The coordinator will persist this value inside the tree before it creates
/// a journal. Paths and runtime inode values may change during exchange or
/// reboot. A copied token is duplicate/corrupt evidence, not a second identity.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub(crate) struct TreeToken(pub(super) String);

impl TreeToken {
    pub(crate) const TEXT_LENGTH: usize = 32;
    const RANDOM_BYTES: usize = Self::TEXT_LENGTH / 2;

    /// Generate one canonical tree token from the kernel CSPRNG.
    ///
    /// The marker implementation must use this constructor rather than
    /// duplicating the token's random-byte or encoding contract. There is no
    /// time-, PID-, counter-, or userspace-random fallback.
    #[allow(dead_code)] // consumed by the durable tree-marker integration slice
    pub(crate) fn generate() -> io::Result<Self> {
        let mut random = [0_u8; Self::RANDOM_BYTES];
        let mut filled = 0;
        while filled < random.len() {
            // SAFETY: getrandom writes at most the supplied remaining length
            // into the live array and retains no pointer after returning.
            let result = unsafe {
                nix::libc::syscall(
                    nix::libc::SYS_getrandom,
                    random[filled..].as_mut_ptr(),
                    random.len() - filled,
                    0,
                )
            };
            if result == -1 {
                let source = io::Error::last_os_error();
                if source.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(source);
            }
            let read = usize::try_from(result)
                .map_err(|_| io::Error::other("getrandom returned a negative tree-token length"))?;
            if read == 0 || read > random.len() - filled {
                return Err(io::Error::other("getrandom returned an invalid tree-token length"));
            }
            filled += read;
        }

        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut encoded = [0_u8; Self::TEXT_LENGTH];
        for (index, byte) in random.into_iter().enumerate() {
            encoded[index * 2] = HEX[usize::from(byte >> 4)];
            encoded[index * 2 + 1] = HEX[usize::from(byte & 0x0f)];
        }
        let encoded = String::from_utf8(encoded.to_vec()).expect("lowercase hexadecimal is valid UTF-8");
        Self::parse(encoded).map_err(|_| io::Error::other("kernel randomness encoded to a noncanonical tree token"))
    }

    pub(crate) fn parse(value: impl Into<String>) -> Result<Self, CodecError> {
        let value = Self(value.into());
        value.validate()?;
        Ok(value)
    }

    pub(super) fn validate(&self) -> Result<(), CodecError> {
        let bytes = self.0.as_bytes();
        if bytes.len() != Self::TEXT_LENGTH
            || !bytes
                .iter()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
            || bytes.iter().all(|byte| *byte == b'0')
        {
            return Err(CodecError::InvalidTreeToken);
        }
        Ok(())
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for TreeToken {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(D::Error::custom)
    }
}

/// Identity of the mount namespace in which runtime witnesses were captured.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MountNamespaceIdentity {
    pub(crate) st_dev: u64,
    pub(crate) inode: u64,
}

/// Runtime epoch in which the journal's device, inode, and mount witnesses are
/// meaningful. A changed epoch requires durable-token-based reconciliation;
/// it must never reinterpret the persisted runtime values as current proof.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RuntimeEpoch {
    pub(crate) boot_id: BootId,
    pub(crate) mount_namespace: MountNamespaceIdentity,
}

/// Creation-time runtime witness for one `/usr` directory.
///
/// `mount_id` is intentionally generic: Linux 5.6 can obtain it from
/// authenticated `/proc/self/fdinfo`, before `STATX_MNT_ID` was introduced.
/// It is namespace-local and is not the durable tree identity.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RuntimeTreeIdentity {
    pub(crate) st_dev: u64,
    pub(crate) inode: u64,
    pub(crate) mount_id: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Candidate {
    pub(crate) id: Option<i32>,
    pub(crate) origin: CandidateOrigin,
    pub(crate) tree_token: TreeToken,
    pub(crate) usr_runtime_identity: RuntimeTreeIdentity,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Previous {
    pub(crate) id: Option<i32>,
    pub(crate) tree_token: TreeToken,
    pub(crate) usr_runtime_identity: RuntimeTreeIdentity,
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

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum BootRollback {
    NotRequired,
    PendingUnverifiable,
    Applied,
    AlreadySatisfied,
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
    pub(crate) creation_epoch: RuntimeEpoch,
    pub(crate) operation: Operation,
    pub(crate) phase: Phase,
    pub(crate) rollback: Option<RollbackPlan>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) boot_publication_receipts: Option<BootPublicationReceiptPair>,
    pub(crate) candidate: Candidate,
    pub(crate) previous: Previous,
    pub(crate) options: TransitionOptions,
    pub(crate) quarantine_name: QuarantineName,
}

impl TransitionRecord {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn preparing(
        transition_id: TransitionId,
        creation_epoch: RuntimeEpoch,
        operation: Operation,
        candidate_id: Option<i32>,
        candidate_tree_token: TreeToken,
        candidate_usr_runtime_identity: RuntimeTreeIdentity,
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
            creation_epoch,
            operation,
            phase: Phase::Preparing,
            rollback: None,
            boot_publication_receipts: None,
            candidate: Candidate {
                id: candidate_id,
                origin: candidate_origin,
                tree_token: candidate_tree_token,
                usr_runtime_identity: candidate_usr_runtime_identity,
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

    pub(crate) fn commit_disposition(&self) -> CommitDisposition {
        match self.previous.origin {
            PreviousOrigin::ActiveState => CommitDisposition::Archive,
            PreviousOrigin::ActiveReblitCorrupt | PreviousOrigin::SynthesizedEmpty => CommitDisposition::Discard,
            PreviousOrigin::Unmanaged => CommitDisposition::Quarantine,
        }
    }
}
