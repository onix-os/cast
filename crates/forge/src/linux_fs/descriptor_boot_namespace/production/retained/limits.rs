use std::{mem::size_of, time::Instant};

use super::super::super::model::HARD_MAX_REQUESTS;
use super::super::model::*;
use super::error::RetainedBootNamespaceAssessmentError;

const HARD_MAX_LIVE_OBSERVATION_IO_ATTEMPTS: usize = 64 * 1024 * 1024;
const HARD_MAX_LIVE_INVENTORY_PASSES: usize = 262_144;
const HARD_MAX_LIVE_CONTENT_READ_BYTES: u64 = 32 * 1024 * 1024 * 1024;
const HARD_MAX_LIVE_CONTENT_READ_CALLS: usize = 16 * 1024 * 1024;
const HARD_MAX_LIVE_EXPECTED_HASH_BYTES: u64 = 16 * 1024 * 1024 * 1024;
const HARD_MAX_LIVE_EXPECTED_HASH_CHUNKS: usize = 16 * 1024 * 1024;
// Physical expected descriptors can be streamed once while binding their
// declared digest and once again during an equal-length comparison. Each pass
// reserves one additional offered byte per possible terminal EOF probe.
const HARD_MAX_LIVE_EXPECTED_SOURCE_READ_BYTES: u64 = 2 * (16 * 1024 * 1024 * 1024 + HARD_MAX_REQUESTS as u64);
// Every physical expected-source pread is also admitted by the stricter
// operation-wide I/O-attempt ledger, so this independent subset ceiling never
// needs to exceed that global ceiling.
const HARD_MAX_LIVE_EXPECTED_SOURCE_READ_CALLS: usize = HARD_MAX_LIVE_OBSERVATION_IO_ATTEMPTS;
const HARD_MAX_LIVE_RETAINED_NODES: usize = 32;
const HARD_MAX_LIVE_DESCRIPTOR_SLOTS: usize = 128;
const HARD_MAX_LIVE_ALLOCATION_ATTEMPTS: usize = 1_000_000;
const HARD_MAX_LIVE_ALLOCATION_BYTES: usize = 256 * 1024 * 1024;

/// Operation-wide limits for the syscall-backed observer.
///
/// Raw parser fields are aggregate across every opening and closing directory
/// pass, not reset per directory. They include the parser's separately
/// reported terminal EOF-probe capacity.
///
/// `max_observation_io_attempts` admits exactly the adapter's one-shot
/// `fcntl(F_GETFL)`, `fcntl(F_GETFD)`, `fcntl(F_GET_SEALS)`, `fstat`, `statx`,
/// `openat2`, `pread`, and `getdents64` attempts. It deliberately excludes
/// mandatory descriptor closes, clock reads, allocator/runtime internals, and
/// injected protocol hooks.
///
/// `max_expected_source_read_bytes` charges the complete offered capacity, not
/// only bytes returned, before each physical expected-source `pread`. Together
/// with `max_expected_source_read_calls`, it covers both pre-binding and
/// comparison passes while excluding in-memory generated-source copies.
///
/// `max_descriptor_slots` is conservative pre-open admission capacity. A slot
/// is reserved before an open attempt and released if that attempt fails, so
/// `peak_descriptor_slots` is not an exact peak of kernel-owned descriptors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RetainedBootNamespaceAssessmentLimits {
    pub(crate) max_observation_io_attempts: usize,
    pub(crate) max_inventory_passes: usize,
    pub(crate) max_raw_records: usize,
    pub(crate) max_raw_name_bytes: usize,
    pub(crate) max_raw_read_admission_bytes: usize,
    pub(crate) max_raw_read_calls: usize,
    pub(crate) max_raw_work: usize,
    pub(crate) max_raw_allocation_attempts: usize,
    pub(crate) max_raw_allocation_bytes: usize,
    pub(crate) max_content_read_bytes: u64,
    pub(crate) max_content_read_calls: usize,
    pub(crate) max_expected_hash_bytes: u64,
    pub(crate) max_expected_hash_chunks: usize,
    pub(crate) max_expected_source_read_bytes: u64,
    pub(crate) max_expected_source_read_calls: usize,
    pub(crate) max_retained_nodes: usize,
    pub(crate) max_descriptor_slots: usize,
    pub(crate) max_allocation_attempts: usize,
    pub(crate) max_allocation_bytes: usize,
}

impl Default for RetainedBootNamespaceAssessmentLimits {
    fn default() -> Self {
        Self {
            max_observation_io_attempts: HARD_MAX_LIVE_OBSERVATION_IO_ATTEMPTS,
            max_inventory_passes: HARD_MAX_LIVE_INVENTORY_PASSES,
            max_raw_records: HARD_MAX_RAW_DIRECTORY_RECORDS,
            max_raw_name_bytes: HARD_MAX_RAW_DIRECTORY_NAME_BYTES,
            max_raw_read_admission_bytes: HARD_MAX_RAW_DIRECTORY_READ_BYTES,
            max_raw_read_calls: HARD_MAX_RAW_DIRECTORY_READ_CALLS,
            max_raw_work: HARD_MAX_RAW_DIRECTORY_WORK,
            max_raw_allocation_attempts: HARD_MAX_RAW_DIRECTORY_ALLOCATION_ATTEMPTS,
            max_raw_allocation_bytes: HARD_MAX_RAW_DIRECTORY_ALLOCATION_BYTES,
            max_content_read_bytes: HARD_MAX_LIVE_CONTENT_READ_BYTES,
            max_content_read_calls: HARD_MAX_LIVE_CONTENT_READ_CALLS,
            max_expected_hash_bytes: HARD_MAX_LIVE_EXPECTED_HASH_BYTES,
            max_expected_hash_chunks: HARD_MAX_LIVE_EXPECTED_HASH_CHUNKS,
            max_expected_source_read_bytes: HARD_MAX_LIVE_EXPECTED_SOURCE_READ_BYTES,
            max_expected_source_read_calls: HARD_MAX_LIVE_EXPECTED_SOURCE_READ_CALLS,
            max_retained_nodes: HARD_MAX_LIVE_RETAINED_NODES,
            max_descriptor_slots: HARD_MAX_LIVE_DESCRIPTOR_SLOTS,
            max_allocation_attempts: HARD_MAX_LIVE_ALLOCATION_ATTEMPTS,
            max_allocation_bytes: HARD_MAX_LIVE_ALLOCATION_BYTES,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct FixtureRetainedBootNamespaceUsage {
    pub(crate) observation_io_attempts: usize,
    pub(crate) inventory_passes: usize,
    pub(crate) raw_records: usize,
    pub(crate) raw_name_bytes: usize,
    pub(crate) raw_read_admission_bytes: usize,
    pub(crate) raw_read_calls: usize,
    pub(crate) raw_eof_probes: usize,
    pub(crate) raw_work: usize,
    pub(crate) raw_allocation_attempts: usize,
    pub(crate) raw_allocation_bytes: usize,
    pub(crate) content_read_bytes: u64,
    pub(crate) content_read_calls: usize,
    pub(crate) expected_hash_bytes: u64,
    pub(crate) expected_hash_chunks: usize,
    pub(crate) expected_source_read_bytes: u64,
    pub(crate) expected_source_read_calls: usize,
    pub(crate) peak_retained_nodes: usize,
    pub(crate) peak_descriptor_slots: usize,
    pub(crate) allocation_attempts: usize,
    pub(crate) allocation_bytes: usize,
}

pub(super) struct LiveLedger {
    limits: RetainedBootNamespaceAssessmentLimits,
    deadline: Instant,
    usage: FixtureRetainedBootNamespaceUsage,
    retained_nodes: usize,
    descriptor_slots: usize,
}

impl LiveLedger {
    pub(super) fn new(
        limits: RetainedBootNamespaceAssessmentLimits,
        deadline: Instant,
    ) -> Result<Self, RetainedBootNamespaceAssessmentError> {
        validate_limits(limits)?;
        let ledger = Self {
            limits,
            deadline,
            usage: FixtureRetainedBootNamespaceUsage::default(),
            retained_nodes: 0,
            descriptor_slots: 0,
        };
        ledger.checkpoint()?;
        Ok(ledger)
    }

    pub(super) const fn deadline(&self) -> Instant {
        self.deadline
    }

    pub(super) fn checkpoint(&self) -> Result<(), RetainedBootNamespaceAssessmentError> {
        if Instant::now() > self.deadline {
            Err(RetainedBootNamespaceAssessmentError::DeadlineExceeded {
                deadline: self.deadline,
            })
        } else {
            Ok(())
        }
    }

    pub(super) fn admit_observation_io_attempt(
        &mut self,
        action: &'static str,
    ) -> Result<(), RetainedBootNamespaceAssessmentError> {
        self.checkpoint()?;
        charge_usize(
            &mut self.usage.observation_io_attempts,
            1,
            self.limits.max_observation_io_attempts,
            "observation I/O attempts",
            action,
        )?;
        self.checkpoint()
    }

    pub(super) fn complete_observation_io_attempt(&self) -> Result<(), RetainedBootNamespaceAssessmentError> {
        self.checkpoint()
    }

    pub(super) fn admit_inventory_pass(&mut self) -> Result<(), RetainedBootNamespaceAssessmentError> {
        charge_usize(
            &mut self.usage.inventory_passes,
            1,
            self.limits.max_inventory_passes,
            "inventory passes",
            "starting one fresh raw-directory inventory",
        )
    }

    pub(super) fn parser_limits(
        &self,
    ) -> Result<ProductionRawDirectoryInventoryLimits, RetainedBootNamespaceAssessmentError> {
        let records = remaining(
            self.limits.max_raw_records,
            self.usage.raw_records,
            "raw records",
            "admitting one raw-directory inventory",
        )?
        .min(HARD_MAX_RAW_DIRECTORY_RECORDS);
        let names = remaining(
            self.limits.max_raw_name_bytes,
            self.usage.raw_name_bytes,
            "raw name bytes",
            "admitting one raw-directory inventory",
        )?
        .min(HARD_MAX_RAW_DIRECTORY_NAME_BYTES);
        let read_remaining = remaining(
            self.limits.max_raw_read_admission_bytes,
            self.usage.raw_read_admission_bytes,
            "raw read admission bytes",
            "admitting one raw-directory inventory and its EOF probe",
        )?;
        let reserved_for_probe = RAW_DIRECTORY_MAXIMUM_RECORD_BYTES;
        let reads = read_remaining
            .saturating_sub(reserved_for_probe)
            .min(HARD_MAX_RAW_DIRECTORY_READ_BYTES);
        if reads < RAW_DIRECTORY_MAXIMUM_RECORD_BYTES {
            return Err(RetainedBootNamespaceAssessmentError::LiveBudgetExceeded {
                field: "raw read admission bytes",
                limit: self.limits.max_raw_read_admission_bytes as u64,
                action: "reserving one maximum-record terminal EOF probe",
            });
        }
        let calls = remaining(
            self.limits.max_raw_read_calls,
            self.usage.raw_read_calls,
            "raw read calls",
            "admitting one raw-directory inventory",
        )?
        .min(HARD_MAX_RAW_DIRECTORY_READ_CALLS)
        .min(remaining(
            self.limits.max_observation_io_attempts,
            self.usage.observation_io_attempts,
            "observation I/O attempts",
            "admitting one raw-directory inventory I/O attempt",
        )?);
        let work = remaining(
            self.limits.max_raw_work,
            self.usage.raw_work,
            "raw work",
            "admitting one raw-directory inventory",
        )?
        .min(HARD_MAX_RAW_DIRECTORY_WORK);
        let allocations = remaining(
            self.limits.max_raw_allocation_attempts,
            self.usage.raw_allocation_attempts,
            "raw allocation attempts",
            "admitting one raw-directory inventory",
        )?
        .min(HARD_MAX_RAW_DIRECTORY_ALLOCATION_ATTEMPTS);
        let allocation_bytes = remaining(
            self.limits.max_raw_allocation_bytes,
            self.usage.raw_allocation_bytes,
            "raw allocation bytes",
            "admitting one raw-directory inventory",
        )?
        .min(HARD_MAX_RAW_DIRECTORY_ALLOCATION_BYTES);
        Ok(ProductionRawDirectoryInventoryLimits {
            max_records: records,
            max_name_bytes: names,
            max_read_bytes: reads,
            max_read_calls: calls,
            max_work: work,
            max_allocation_attempts: allocations,
            max_allocation_bytes: allocation_bytes,
        })
    }

    pub(super) fn charge_raw_usage(
        &mut self,
        found: ProductionRawDirectoryInventoryUsage,
    ) -> Result<(), RetainedBootNamespaceAssessmentError> {
        let admission = found.read_bytes.checked_add(found.eof_probe_capacity_bytes).ok_or(
            RetainedBootNamespaceAssessmentError::LiveBudgetExceeded {
                field: "raw read admission bytes",
                limit: self.limits.max_raw_read_admission_bytes as u64,
                action: "aggregating raw-directory parser usage",
            },
        )?;
        charge_usize(
            &mut self.usage.raw_records,
            found.records,
            self.limits.max_raw_records,
            "raw records",
            "aggregating raw-directory parser usage",
        )?;
        charge_usize(
            &mut self.usage.raw_name_bytes,
            found.name_bytes,
            self.limits.max_raw_name_bytes,
            "raw name bytes",
            "aggregating raw-directory parser usage",
        )?;
        charge_usize(
            &mut self.usage.raw_read_admission_bytes,
            admission,
            self.limits.max_raw_read_admission_bytes,
            "raw read admission bytes",
            "aggregating raw-directory parser usage",
        )?;
        charge_usize(
            &mut self.usage.raw_read_calls,
            found.read_calls,
            self.limits.max_raw_read_calls,
            "raw read calls",
            "aggregating raw-directory parser usage",
        )?;
        charge_usize(
            &mut self.usage.raw_eof_probes,
            found.eof_probes,
            self.limits.max_inventory_passes,
            "raw EOF probes",
            "aggregating raw-directory parser usage",
        )?;
        charge_usize(
            &mut self.usage.raw_work,
            found.work,
            self.limits.max_raw_work,
            "raw work",
            "aggregating raw-directory parser usage",
        )?;
        charge_usize(
            &mut self.usage.raw_allocation_attempts,
            found.allocation_attempts,
            self.limits.max_raw_allocation_attempts,
            "raw allocation attempts",
            "aggregating raw-directory parser usage",
        )?;
        charge_usize(
            &mut self.usage.raw_allocation_bytes,
            found.allocation_bytes,
            self.limits.max_raw_allocation_bytes,
            "raw allocation bytes",
            "aggregating raw-directory parser usage",
        )?;
        self.checkpoint()
    }

    pub(super) fn charge_content_read(
        &mut self,
        offered: usize,
        action: &'static str,
    ) -> Result<(), RetainedBootNamespaceAssessmentError> {
        let offered = u64::try_from(offered).map_err(|_| RetainedBootNamespaceAssessmentError::LiveBudgetExceeded {
            field: "content read bytes",
            limit: self.limits.max_content_read_bytes,
            action,
        })?;
        charge_u64(
            &mut self.usage.content_read_bytes,
            offered,
            self.limits.max_content_read_bytes,
            "content read bytes",
            action,
        )?;
        charge_usize(
            &mut self.usage.content_read_calls,
            1,
            self.limits.max_content_read_calls,
            "content read calls",
            action,
        )
    }

    pub(super) fn charge_expected_hash_chunk(
        &mut self,
        bytes: usize,
    ) -> Result<(), RetainedBootNamespaceAssessmentError> {
        let bytes = u64::try_from(bytes).map_err(|_| RetainedBootNamespaceAssessmentError::LiveBudgetExceeded {
            field: "expected hash bytes",
            limit: self.limits.max_expected_hash_bytes,
            action: "hash-binding expected bytes",
        })?;
        charge_u64(
            &mut self.usage.expected_hash_bytes,
            bytes,
            self.limits.max_expected_hash_bytes,
            "expected hash bytes",
            "hash-binding expected bytes",
        )?;
        charge_usize(
            &mut self.usage.expected_hash_chunks,
            1,
            self.limits.max_expected_hash_chunks,
            "expected hash chunks",
            "hash-binding expected bytes",
        )?;
        self.checkpoint()
    }

    /// Admit the offered capacity for one physical expected-source `pread`.
    ///
    /// Charging before the syscall bounds short reads and failures as attempts;
    /// in-memory generated-source copies do not use this ledger.
    pub(super) fn charge_expected_source_read(
        &mut self,
        offered: usize,
        action: &'static str,
    ) -> Result<(), RetainedBootNamespaceAssessmentError> {
        let offered = u64::try_from(offered).map_err(|_| RetainedBootNamespaceAssessmentError::LiveBudgetExceeded {
            field: "expected source read bytes",
            limit: self.limits.max_expected_source_read_bytes,
            action,
        })?;
        charge_u64(
            &mut self.usage.expected_source_read_bytes,
            offered,
            self.limits.max_expected_source_read_bytes,
            "expected source read bytes",
            action,
        )?;
        charge_usize(
            &mut self.usage.expected_source_read_calls,
            1,
            self.limits.max_expected_source_read_calls,
            "expected source read calls",
            action,
        )?;
        self.checkpoint()
    }

    pub(super) fn acquire_node(&mut self) -> Result<(), RetainedBootNamespaceAssessmentError> {
        let next =
            self.retained_nodes
                .checked_add(1)
                .ok_or(RetainedBootNamespaceAssessmentError::LiveBudgetExceeded {
                    field: "retained nodes",
                    limit: self.limits.max_retained_nodes as u64,
                    action: "retaining one namespace node",
                })?;
        if next > self.limits.max_retained_nodes {
            return Err(RetainedBootNamespaceAssessmentError::LiveBudgetExceeded {
                field: "retained nodes",
                limit: self.limits.max_retained_nodes as u64,
                action: "retaining one namespace node",
            });
        }
        self.retained_nodes = next;
        self.usage.peak_retained_nodes = self.usage.peak_retained_nodes.max(next);
        Ok(())
    }

    pub(super) fn release_node(&mut self) {
        debug_assert!(self.retained_nodes > 0, "releasing an unaccounted retained node");
        self.retained_nodes = self.retained_nodes.saturating_sub(1);
    }

    pub(super) fn reserve_descriptor_slot(
        &mut self,
        action: &'static str,
    ) -> Result<(), RetainedBootNamespaceAssessmentError> {
        let next =
            self.descriptor_slots
                .checked_add(1)
                .ok_or(RetainedBootNamespaceAssessmentError::LiveBudgetExceeded {
                    field: "descriptor slots",
                    limit: self.limits.max_descriptor_slots as u64,
                    action,
                })?;
        if next > self.limits.max_descriptor_slots {
            return Err(RetainedBootNamespaceAssessmentError::LiveBudgetExceeded {
                field: "descriptor slots",
                limit: self.limits.max_descriptor_slots as u64,
                action,
            });
        }
        self.descriptor_slots = next;
        self.usage.peak_descriptor_slots = self.usage.peak_descriptor_slots.max(next);
        Ok(())
    }

    pub(super) fn release_descriptor_slot(&mut self) {
        debug_assert!(
            self.descriptor_slots > 0,
            "releasing an unaccounted descriptor admission slot"
        );
        self.descriptor_slots = self.descriptor_slots.saturating_sub(1);
    }

    pub(super) const fn has_live_descriptor_slots(&self) -> bool {
        self.descriptor_slots != 0
    }

    #[cfg(test)]
    pub(super) const fn fixture_descriptor_slots(&self) -> usize {
        self.descriptor_slots
    }

    pub(super) fn reserve<T>(
        &mut self,
        values: &mut Vec<T>,
        additional: usize,
        action: &'static str,
    ) -> Result<(), RetainedBootNamespaceAssessmentError> {
        if additional == 0 {
            return self.checkpoint();
        }
        let bytes =
            additional
                .checked_mul(size_of::<T>())
                .ok_or(RetainedBootNamespaceAssessmentError::LiveBudgetExceeded {
                    field: "allocation bytes",
                    limit: self.limits.max_allocation_bytes as u64,
                    action,
                })?;
        charge_usize(
            &mut self.usage.allocation_attempts,
            1,
            self.limits.max_allocation_attempts,
            "allocation attempts",
            action,
        )?;
        charge_usize(
            &mut self.usage.allocation_bytes,
            bytes,
            self.limits.max_allocation_bytes,
            "allocation bytes",
            action,
        )?;
        values
            .try_reserve_exact(additional)
            .map_err(|source| RetainedBootNamespaceAssessmentError::Allocation { action, source })?;
        self.checkpoint()
    }

    pub(super) const fn usage(&self) -> FixtureRetainedBootNamespaceUsage {
        self.usage
    }
}

fn validate_limits(limits: RetainedBootNamespaceAssessmentLimits) -> Result<(), RetainedBootNamespaceAssessmentError> {
    let usize_values = [
        (
            "max_observation_io_attempts",
            limits.max_observation_io_attempts,
            HARD_MAX_LIVE_OBSERVATION_IO_ATTEMPTS,
        ),
        (
            "max_inventory_passes",
            limits.max_inventory_passes,
            HARD_MAX_LIVE_INVENTORY_PASSES,
        ),
        (
            "max_raw_records",
            limits.max_raw_records,
            HARD_MAX_RAW_DIRECTORY_RECORDS,
        ),
        (
            "max_raw_name_bytes",
            limits.max_raw_name_bytes,
            HARD_MAX_RAW_DIRECTORY_NAME_BYTES,
        ),
        (
            "max_raw_read_admission_bytes",
            limits.max_raw_read_admission_bytes,
            HARD_MAX_RAW_DIRECTORY_READ_BYTES,
        ),
        (
            "max_raw_read_calls",
            limits.max_raw_read_calls,
            HARD_MAX_RAW_DIRECTORY_READ_CALLS,
        ),
        ("max_raw_work", limits.max_raw_work, HARD_MAX_RAW_DIRECTORY_WORK),
        (
            "max_raw_allocation_attempts",
            limits.max_raw_allocation_attempts,
            HARD_MAX_RAW_DIRECTORY_ALLOCATION_ATTEMPTS,
        ),
        (
            "max_raw_allocation_bytes",
            limits.max_raw_allocation_bytes,
            HARD_MAX_RAW_DIRECTORY_ALLOCATION_BYTES,
        ),
        (
            "max_content_read_calls",
            limits.max_content_read_calls,
            HARD_MAX_LIVE_CONTENT_READ_CALLS,
        ),
        (
            "max_expected_hash_chunks",
            limits.max_expected_hash_chunks,
            HARD_MAX_LIVE_EXPECTED_HASH_CHUNKS,
        ),
        (
            "max_expected_source_read_calls",
            limits.max_expected_source_read_calls,
            HARD_MAX_LIVE_EXPECTED_SOURCE_READ_CALLS,
        ),
        (
            "max_retained_nodes",
            limits.max_retained_nodes,
            HARD_MAX_LIVE_RETAINED_NODES,
        ),
        (
            "max_descriptor_slots",
            limits.max_descriptor_slots,
            HARD_MAX_LIVE_DESCRIPTOR_SLOTS,
        ),
        (
            "max_allocation_attempts",
            limits.max_allocation_attempts,
            HARD_MAX_LIVE_ALLOCATION_ATTEMPTS,
        ),
        (
            "max_allocation_bytes",
            limits.max_allocation_bytes,
            HARD_MAX_LIVE_ALLOCATION_BYTES,
        ),
    ];
    for (field, value, ceiling) in usize_values {
        if value == 0 || value > ceiling {
            return Err(RetainedBootNamespaceAssessmentError::InvalidLiveLimit { field });
        }
    }
    let u64_values = [
        (
            "max_content_read_bytes",
            limits.max_content_read_bytes,
            HARD_MAX_LIVE_CONTENT_READ_BYTES,
        ),
        (
            "max_expected_hash_bytes",
            limits.max_expected_hash_bytes,
            HARD_MAX_LIVE_EXPECTED_HASH_BYTES,
        ),
        (
            "max_expected_source_read_bytes",
            limits.max_expected_source_read_bytes,
            HARD_MAX_LIVE_EXPECTED_SOURCE_READ_BYTES,
        ),
    ];
    for (field, value, ceiling) in u64_values {
        if value == 0 || value > ceiling {
            return Err(RetainedBootNamespaceAssessmentError::InvalidLiveLimit { field });
        }
    }
    Ok(())
}

fn remaining(
    limit: usize,
    used: usize,
    field: &'static str,
    action: &'static str,
) -> Result<usize, RetainedBootNamespaceAssessmentError> {
    let found = limit.saturating_sub(used);
    if found == 0 {
        Err(RetainedBootNamespaceAssessmentError::LiveBudgetExceeded {
            field,
            limit: limit as u64,
            action,
        })
    } else {
        Ok(found)
    }
}

fn charge_usize(
    used: &mut usize,
    amount: usize,
    limit: usize,
    field: &'static str,
    action: &'static str,
) -> Result<(), RetainedBootNamespaceAssessmentError> {
    let next = used
        .checked_add(amount)
        .ok_or(RetainedBootNamespaceAssessmentError::LiveBudgetExceeded {
            field,
            limit: limit as u64,
            action,
        })?;
    if next > limit {
        return Err(RetainedBootNamespaceAssessmentError::LiveBudgetExceeded {
            field,
            limit: limit as u64,
            action,
        });
    }
    *used = next;
    Ok(())
}

fn charge_u64(
    used: &mut u64,
    amount: u64,
    limit: u64,
    field: &'static str,
    action: &'static str,
) -> Result<(), RetainedBootNamespaceAssessmentError> {
    let next = used
        .checked_add(amount)
        .ok_or(RetainedBootNamespaceAssessmentError::LiveBudgetExceeded { field, limit, action })?;
    if next > limit {
        return Err(RetainedBootNamespaceAssessmentError::LiveBudgetExceeded { field, limit, action });
    }
    *used = next;
    Ok(())
}
