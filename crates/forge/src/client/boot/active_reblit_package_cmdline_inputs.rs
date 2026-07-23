//! Authenticated semantic package command-line inputs for boot rendering.
//!
//! Package command-line fragments begin as sealed Stone snapshots.  This
//! module binds each relevant plan coordinate to one exact non-cloneable Stone
//! owner, reads it with explicit offsets under one caller-owned deadline,
//! verifies its digest and length, and retains only normalized scalar text.
//! It performs no destination discovery, rendering, or mutation.

use std::{
    collections::TryReserveError,
    ffi::{OsStr, OsString},
    io,
    path::{Path, PathBuf},
    time::Instant,
};

use thiserror::Error;

use crate::state;

use super::{
    active_reblit_boot_inputs::{BoundActiveReblitBootAsset, PreparedActiveReblitStoneBootInputs},
    active_reblit_boot_projection::{BootAssetRole, MAX_BOOT_PLAN_ASSETS},
};

#[path = "active_reblit_package_cmdline_inputs/binding.rs"]
mod binding;
#[path = "active_reblit_package_cmdline_inputs/normalization.rs"]
mod normalization;

const KIB: usize = 1024;
const MIB: usize = 1024 * KIB;
const MAX_PACKAGE_CMDLINE_SOURCE_BYTES: usize = 64 * KIB;
const MAX_PACKAGE_CMDLINE_TOTAL_BYTES: usize = 16 * MIB;
const MAX_PACKAGE_CMDLINE_WORK: usize = 1_000_000;
const MAX_PACKAGE_CMDLINE_INTERRUPTED_RETRIES: usize = 1_024;
const SORT_WORK_PER_ELEMENT_LEVEL: usize = 4;

const PACKAGE_CMDLINE_POLICY: PackageCmdlinePolicy = PackageCmdlinePolicy {
    max_entries: MAX_BOOT_PLAN_ASSETS,
    max_source_bytes: MAX_PACKAGE_CMDLINE_SOURCE_BYTES,
    max_total_bytes: MAX_PACKAGE_CMDLINE_TOTAL_BYTES,
    max_work: MAX_PACKAGE_CMDLINE_WORK,
};

/// Semantic scope of one package-owned command-line fragment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) enum BoundActiveReblitPackageCmdlineScope<'a> {
    Global,
    Kernel { version: &'a str },
}

/// Scalar view of one authenticated package command-line fragment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) struct BoundActiveReblitPackageCmdline<'a> {
    state_id: state::Id,
    scope: BoundActiveReblitPackageCmdlineScope<'a>,
    filename: &'a OsStr,
    snippet: &'a str,
    binding_index: u16,
    digest: u128,
    length: u64,
}

impl BoundActiveReblitPackageCmdline<'_> {
    pub(in crate::client) const fn state_id(&self) -> state::Id {
        self.state_id
    }

    pub(in crate::client) const fn scope(&self) -> BoundActiveReblitPackageCmdlineScope<'_> {
        self.scope
    }

    pub(in crate::client) const fn version(&self) -> Option<&str> {
        match self.scope {
            BoundActiveReblitPackageCmdlineScope::Global => None,
            BoundActiveReblitPackageCmdlineScope::Kernel { version } => Some(version),
        }
    }

    pub(in crate::client) const fn filename(&self) -> &OsStr {
        self.filename
    }

    pub(in crate::client) const fn snippet(&self) -> &str {
        self.snippet
    }

    pub(in crate::client) const fn binding_index(&self) -> u16 {
        self.binding_index
    }

    pub(in crate::client) const fn digest(&self) -> u128 {
        self.digest
    }

    pub(in crate::client) const fn length(&self) -> u64 {
        self.length
    }
}

#[derive(Debug, Eq, PartialEq)]
enum PackageCmdlineScope {
    Global,
    Kernel { version: Box<str> },
}

impl PackageCmdlineScope {
    fn bound(&self) -> BoundActiveReblitPackageCmdlineScope<'_> {
        match self {
            Self::Global => BoundActiveReblitPackageCmdlineScope::Global,
            Self::Kernel { version } => BoundActiveReblitPackageCmdlineScope::Kernel { version },
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
struct PreparedActiveReblitPackageCmdline {
    state_id: state::Id,
    state_position: u16,
    scope: PackageCmdlineScope,
    logical_path: PathBuf,
    filename: OsString,
    snippet: Box<str>,
    binding_index: u16,
    digest: u128,
    length: u64,
}

impl PreparedActiveReblitPackageCmdline {
    fn bound(&self) -> BoundActiveReblitPackageCmdline<'_> {
        BoundActiveReblitPackageCmdline {
            state_id: self.state_id,
            scope: self.scope.bound(),
            filename: &self.filename,
            snippet: &self.snippet,
            binding_index: self.binding_index,
            digest: self.digest,
            length: self.length,
        }
    }
}

/// Normalized package fragments tied to one exact sealed Stone owner.
///
/// This value is intentionally not `Clone`.  Its private owner reference makes
/// it impossible to rebind the retained coordinates to a separately prepared
/// Stone input set, even when that set happens to describe equal bytes.
pub(in crate::client) struct PreparedActiveReblitPackageCmdlineInputs<'stone> {
    source_owner: &'stone PreparedActiveReblitStoneBootInputs,
    projected_state_ids: Box<[state::Id]>,
    entries: Box<[PreparedActiveReblitPackageCmdline]>,
    total_source_bytes: usize,
    preparation_work: usize,
}

impl std::fmt::Debug for PreparedActiveReblitPackageCmdlineInputs<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreparedActiveReblitPackageCmdlineInputs")
            .field("projected_state_ids", &self.projected_state_ids)
            .field("entry_count", &self.entries.len())
            .field("total_source_bytes", &self.total_source_bytes)
            .field("source_binding", &"retained")
            .finish()
    }
}

impl<'stone> PreparedActiveReblitPackageCmdlineInputs<'stone> {
    /// Authenticate and normalize every package-owned command-line fragment
    /// before the exact caller deadline.
    pub(in crate::client) fn prepare_until(
        stone: &'stone PreparedActiveReblitStoneBootInputs,
        deadline: Instant,
    ) -> Result<Self, ActiveReblitPackageCmdlineInputsError> {
        binding::prepare_with_policy_until(stone, PackageCmdlinePolicy::production(), deadline)
    }

    /// Re-read every retained coordinate from the same sealed owner and
    /// require its exact bytes and normalized meaning to remain unchanged.
    pub(in crate::client) fn revalidate_until(
        &self,
        deadline: Instant,
    ) -> Result<(), ActiveReblitPackageCmdlineInputsError> {
        binding::revalidate_with_policy_until(self, PackageCmdlinePolicy::production(), deadline)
    }

    pub(in crate::client) fn projected_state_ids(&self) -> &[state::Id] {
        &self.projected_state_ids
    }

    pub(in crate::client) fn entries(&self) -> impl ExactSizeIterator<Item = BoundActiveReblitPackageCmdline<'_>> {
        self.entries.iter().map(PreparedActiveReblitPackageCmdline::bound)
    }

    pub(in crate::client) const fn total_source_bytes(&self) -> usize {
        self.total_source_bytes
    }

    pub(in crate::client) const fn preparation_work(&self) -> usize {
        self.preparation_work
    }
}

#[derive(Clone, Copy)]
struct PackageCmdlinePolicy {
    max_entries: usize,
    max_source_bytes: usize,
    max_total_bytes: usize,
    max_work: usize,
}

impl PackageCmdlinePolicy {
    const fn production() -> Self {
        PACKAGE_CMDLINE_POLICY
    }
}

struct PackageCmdlineBudget {
    policy: PackageCmdlinePolicy,
    deadline: Instant,
    work: usize,
    source_bytes: usize,
}

impl PackageCmdlineBudget {
    fn new(policy: PackageCmdlinePolicy, deadline: Instant) -> Result<Self, ActiveReblitPackageCmdlineInputsError> {
        let budget = Self {
            policy,
            deadline,
            work: 0,
            source_bytes: 0,
        };
        budget.require_deadline("coordinator entry")?;
        Ok(budget)
    }

    fn step(&mut self, checkpoint: &'static str) -> Result<(), ActiveReblitPackageCmdlineInputsError> {
        self.reserve_work(1, checkpoint)
    }

    fn reserve_sort_work(&mut self, entries: usize) -> Result<(), ActiveReblitPackageCmdlineInputsError> {
        self.reserve_work(conservative_sort_work(entries), "canonical sort")
    }

    fn reserve_work(
        &mut self,
        amount: usize,
        checkpoint: &'static str,
    ) -> Result<(), ActiveReblitPackageCmdlineInputsError> {
        self.require_deadline(checkpoint)?;
        let actual = self.work.checked_add(amount).unwrap_or(usize::MAX);
        if actual > self.policy.max_work {
            return Err(ActiveReblitPackageCmdlineInputsError::WorkLimit {
                limit: self.policy.max_work,
                actual,
            });
        }
        self.work = actual;
        Ok(())
    }

    fn admit_source(
        &mut self,
        binding_index: usize,
        length: u64,
    ) -> Result<usize, ActiveReblitPackageCmdlineInputsError> {
        self.step("source byte admission")?;
        let actual = usize::try_from(length).unwrap_or(usize::MAX);
        if actual > self.policy.max_source_bytes {
            return Err(ActiveReblitPackageCmdlineInputsError::SourceByteLimit {
                binding_index,
                limit: self.policy.max_source_bytes,
                actual,
            });
        }
        let total = self.source_bytes.checked_add(actual).unwrap_or(usize::MAX);
        if total > self.policy.max_total_bytes {
            return Err(ActiveReblitPackageCmdlineInputsError::TotalByteLimit {
                limit: self.policy.max_total_bytes,
                actual: total,
            });
        }
        self.source_bytes = total;
        Ok(actual)
    }

    fn require_deadline(&self, checkpoint: &'static str) -> Result<(), ActiveReblitPackageCmdlineInputsError> {
        if Instant::now() > self.deadline {
            Err(ActiveReblitPackageCmdlineInputsError::DeadlineExceeded { checkpoint })
        } else {
            Ok(())
        }
    }
}

fn conservative_sort_work(entries: usize) -> usize {
    if entries < 2 {
        return 0;
    }
    let levels = usize::BITS as usize - entries.saturating_sub(1).leading_zeros() as usize;
    entries
        .saturating_mul(levels)
        .saturating_mul(SORT_WORK_PER_ELEMENT_LEVEL)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) enum ActiveReblitPackageCmdlineContentReason {
    NonAsciiOrUnsupportedControl,
    NormalizedControl,
}

#[derive(Debug, Error)]
pub(in crate::client) enum ActiveReblitPackageCmdlineInputsError {
    #[error("package command-line input deadline expired at {checkpoint}")]
    DeadlineExceeded { checkpoint: &'static str },
    #[error("package command-line input work {actual} exceeds limit {limit}")]
    WorkLimit { limit: usize, actual: usize },
    #[error("package command-line input count {actual} exceeds limit {limit}")]
    EntryCountLimit { limit: usize, actual: usize },
    #[error("package command-line binding index {actual} exceeds limit {limit}")]
    BindingIndexLimit { limit: usize, actual: usize },
    #[error("package command-line binding {binding_index} state position {actual} exceeds limit {limit}")]
    StatePositionLimit {
        binding_index: usize,
        limit: usize,
        actual: usize,
    },
    #[error("package command-line binding {binding_index} references state {state} outside the retained projection")]
    AssetStateOutsideProjection { binding_index: usize, state: i32 },
    #[error("package command-line binding {binding_index} has an invalid role or logical coordinate")]
    InvalidCoordinate { binding_index: usize },
    #[error("package command-line binding {binding_index} exceeds {limit} bytes (got {actual})")]
    SourceByteLimit {
        binding_index: usize,
        limit: usize,
        actual: usize,
    },
    #[error("package command-line inputs exceed {limit} aggregate bytes (got {actual})")]
    TotalByteLimit { limit: usize, actual: usize },
    #[error("read sealed package command-line binding {binding_index}")]
    ReadSource {
        binding_index: usize,
        #[source]
        source: io::Error,
    },
    #[error(
        "sealed package command-line binding {binding_index} digest mismatch: expected {expected:032x}, got {actual:032x}"
    )]
    DigestMismatch {
        binding_index: usize,
        expected: u128,
        actual: u128,
    },
    #[error("package command-line binding {binding_index} contains invalid semantic text: {reason:?}")]
    InvalidContent {
        binding_index: usize,
        reason: ActiveReblitPackageCmdlineContentReason,
    },
    #[error("package command-line state projection changed after preparation")]
    StateProjectionChanged,
    #[error("sealed package command-line binding {binding_index} changed after preparation")]
    SourceChanged { binding_index: usize },
    #[error("allocate {resource} while preparing package command-line inputs")]
    Allocation {
        resource: &'static str,
        #[source]
        source: TryReserveError,
    },
}

#[cfg(test)]
#[path = "active_reblit_package_cmdline_inputs_tests.rs"]
mod tests;
