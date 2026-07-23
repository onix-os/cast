use std::{mem::size_of, time::Instant};

use super::{
    error::ProductionRawDirectoryInventoryError,
    inventory::RawNameSpan,
    model::{
        HARD_MAX_RAW_DIRECTORY_ALLOCATION_ATTEMPTS, HARD_MAX_RAW_DIRECTORY_ALLOCATION_BYTES,
        HARD_MAX_RAW_DIRECTORY_NAME_BYTES, HARD_MAX_RAW_DIRECTORY_READ_BYTES, HARD_MAX_RAW_DIRECTORY_READ_CALLS,
        HARD_MAX_RAW_DIRECTORY_RECORDS, HARD_MAX_RAW_DIRECTORY_WORK, ProductionRawDirectoryInventoryLimits,
        RAW_DIRECTORY_MAXIMUM_RECORD_BYTES,
    },
    source::ProductionRawDirectorySource,
};

use super::model::ProductionRawDirectoryInventoryUsage;

pub(super) struct ProductionRawDirectoryOperation<'a, Source> {
    source: &'a mut Source,
    limits: ProductionRawDirectoryInventoryLimits,
    deadline: Instant,
    records: usize,
    name_bytes: usize,
    read_bytes: usize,
    read_calls: usize,
    eof_probes: usize,
    eof_probe_capacity_bytes: usize,
    work: usize,
    allocation_attempts: usize,
    allocation_bytes: usize,
}

impl<'a, Source: ProductionRawDirectorySource> ProductionRawDirectoryOperation<'a, Source> {
    pub(super) fn new(
        source: &'a mut Source,
        limits: ProductionRawDirectoryInventoryLimits,
        deadline: Instant,
    ) -> Result<Self, ProductionRawDirectoryInventoryError> {
        validate_limits(limits)?;
        let mut operation = Self {
            source,
            limits,
            deadline,
            records: 0,
            name_bytes: 0,
            read_bytes: 0,
            read_calls: 0,
            eof_probes: 0,
            eof_probe_capacity_bytes: 0,
            work: 0,
            allocation_attempts: 0,
            allocation_bytes: 0,
        };
        operation.checkpoint()?;
        Ok(operation)
    }

    pub(super) fn checkpoint(&mut self) -> Result<(), ProductionRawDirectoryInventoryError> {
        if self.source.now() > self.deadline {
            Err(ProductionRawDirectoryInventoryError::DeadlineExceeded {
                deadline: self.deadline,
            })
        } else {
            Ok(())
        }
    }

    pub(super) fn read_chunk(&mut self, output: &mut [u8]) -> Result<usize, ProductionRawDirectoryInventoryError> {
        let remaining = self.limits.max_read_bytes - self.read_bytes;
        debug_assert!(remaining >= RAW_DIRECTORY_MAXIMUM_RECORD_BYTES);
        let offered = output.len().min(remaining);
        self.admit_read_call()?;
        self.charge_work(1, "issuing one bounded raw-directory read")?;
        let found = self.source.read_chunk(&mut output[..offered]).map_err(|_| {
            ProductionRawDirectoryInventoryError::SourceFailed {
                action: "reading one bounded raw-directory chunk",
            }
        })?;
        self.checkpoint()?;
        if found > offered {
            return Err(ProductionRawDirectoryInventoryError::SourceProtocolViolation {
                capacity: offered,
                found,
            });
        }
        let total =
            self.read_bytes
                .checked_add(found)
                .ok_or(ProductionRawDirectoryInventoryError::ReadByteLimitExceeded {
                    limit: self.limits.max_read_bytes,
                })?;
        if total > self.limits.max_read_bytes {
            return Err(ProductionRawDirectoryInventoryError::ReadByteLimitExceeded {
                limit: self.limits.max_read_bytes,
            });
        }
        self.read_bytes = total;
        self.checkpoint()?;
        Ok(found)
    }

    pub(super) const fn remaining_read_bytes(&self) -> usize {
        self.limits.max_read_bytes - self.read_bytes
    }

    pub(super) fn probe_end(&mut self, output: &mut [u8]) -> Result<(), ProductionRawDirectoryInventoryError> {
        if self.eof_probes >= 1 {
            return Err(ProductionRawDirectoryInventoryError::EndProbeLimitExceeded { limit: 1 });
        }
        let offered = output.len().min(RAW_DIRECTORY_MAXIMUM_RECORD_BYTES);
        self.admit_read_call()?;
        self.eof_probes = 1;
        self.eof_probe_capacity_bytes = offered;
        self.charge_work(1, "issuing the bounded raw-directory terminal probe")?;
        let found = self.source.probe_end(&mut output[..offered]).map_err(|_| {
            ProductionRawDirectoryInventoryError::SourceFailed {
                action: "probing bounded raw-directory exhaustion",
            }
        })?;
        self.checkpoint()?;
        if found > offered {
            return Err(ProductionRawDirectoryInventoryError::SourceProtocolViolation {
                capacity: offered,
                found,
            });
        }
        if found != 0 {
            return Err(ProductionRawDirectoryInventoryError::ReadByteLimitExceeded {
                limit: self.limits.max_read_bytes,
            });
        }
        self.checkpoint()
    }

    fn admit_read_call(&mut self) -> Result<(), ProductionRawDirectoryInventoryError> {
        self.checkpoint()?;
        let next_call =
            self.read_calls
                .checked_add(1)
                .ok_or(ProductionRawDirectoryInventoryError::ReadCallLimitExceeded {
                    limit: self.limits.max_read_calls,
                })?;
        if next_call > self.limits.max_read_calls {
            return Err(ProductionRawDirectoryInventoryError::ReadCallLimitExceeded {
                limit: self.limits.max_read_calls,
            });
        }
        self.read_calls = next_call;
        self.checkpoint()
    }

    pub(super) fn charge_record(&mut self, record_bytes: usize) -> Result<(), ProductionRawDirectoryInventoryError> {
        let records = self
            .records
            .checked_add(1)
            .ok_or(ProductionRawDirectoryInventoryError::RecordLimitExceeded {
                limit: self.limits.max_records,
            })?;
        if records > self.limits.max_records {
            return Err(ProductionRawDirectoryInventoryError::RecordLimitExceeded {
                limit: self.limits.max_records,
            });
        }
        self.records = records;
        self.charge_work(record_bytes.saturating_add(1), "validating one raw directory record")
    }

    pub(super) fn charge_name(&mut self, found: usize) -> Result<(), ProductionRawDirectoryInventoryError> {
        let total =
            self.name_bytes
                .checked_add(found)
                .ok_or(ProductionRawDirectoryInventoryError::NameByteLimitExceeded {
                    limit: self.limits.max_name_bytes,
                })?;
        if total > self.limits.max_name_bytes {
            return Err(ProductionRawDirectoryInventoryError::NameByteLimitExceeded {
                limit: self.limits.max_name_bytes,
            });
        }
        self.name_bytes = total;
        self.checkpoint()
    }

    pub(super) fn reserve_entry(
        &mut self,
        names: &mut Vec<u8>,
        entries: &mut Vec<RawNameSpan>,
        name_bytes: usize,
    ) -> Result<(), ProductionRawDirectoryInventoryError> {
        self.reserve(names, name_bytes, name_bytes, "reserving raw directory name bytes")?;
        self.reserve(
            entries,
            1,
            size_of::<RawNameSpan>(),
            "reserving one raw directory name span",
        )
    }

    fn reserve<T>(
        &mut self,
        values: &mut Vec<T>,
        additional_items: usize,
        allocation_bytes: usize,
        action: &'static str,
    ) -> Result<(), ProductionRawDirectoryInventoryError> {
        self.checkpoint()?;
        let attempt = self.allocation_attempts.checked_add(1).ok_or(
            ProductionRawDirectoryInventoryError::AllocationAttemptLimitExceeded {
                limit: self.limits.max_allocation_attempts,
                action,
            },
        )?;
        if attempt > self.limits.max_allocation_attempts {
            return Err(ProductionRawDirectoryInventoryError::AllocationAttemptLimitExceeded {
                limit: self.limits.max_allocation_attempts,
                action,
            });
        }
        let total_bytes = self.allocation_bytes.checked_add(allocation_bytes).ok_or(
            ProductionRawDirectoryInventoryError::AllocationByteLimitExceeded {
                limit: self.limits.max_allocation_bytes,
                action,
            },
        )?;
        if total_bytes > self.limits.max_allocation_bytes {
            return Err(ProductionRawDirectoryInventoryError::AllocationByteLimitExceeded {
                limit: self.limits.max_allocation_bytes,
                action,
            });
        }
        self.source
            .before_allocation(attempt, allocation_bytes)
            .map_err(|_| ProductionRawDirectoryInventoryError::AllocationFailed { action })?;
        self.checkpoint()?;
        values
            .try_reserve_exact(additional_items)
            .map_err(|_| ProductionRawDirectoryInventoryError::AllocationFailed { action })?;
        self.allocation_attempts = attempt;
        self.allocation_bytes = total_bytes;
        self.charge_work(1, action)
    }

    fn charge_work(&mut self, amount: usize, action: &'static str) -> Result<(), ProductionRawDirectoryInventoryError> {
        self.checkpoint()?;
        let total = self
            .work
            .checked_add(amount)
            .ok_or(ProductionRawDirectoryInventoryError::WorkLimitExceeded {
                limit: self.limits.max_work,
                action,
            })?;
        if total > self.limits.max_work {
            return Err(ProductionRawDirectoryInventoryError::WorkLimitExceeded {
                limit: self.limits.max_work,
                action,
            });
        }
        self.work = total;
        self.checkpoint()
    }

    pub(super) const fn usage(&self) -> ProductionRawDirectoryInventoryUsage {
        ProductionRawDirectoryInventoryUsage {
            records: self.records,
            name_bytes: self.name_bytes,
            read_bytes: self.read_bytes,
            read_calls: self.read_calls,
            eof_probes: self.eof_probes,
            eof_probe_capacity_bytes: self.eof_probe_capacity_bytes,
            work: self.work,
            allocation_attempts: self.allocation_attempts,
            allocation_bytes: self.allocation_bytes,
        }
    }
}

fn validate_limits(limits: ProductionRawDirectoryInventoryLimits) -> Result<(), ProductionRawDirectoryInventoryError> {
    let limits_and_ceilings = [
        ("max_records", limits.max_records, HARD_MAX_RAW_DIRECTORY_RECORDS),
        (
            "max_name_bytes",
            limits.max_name_bytes,
            HARD_MAX_RAW_DIRECTORY_NAME_BYTES,
        ),
        (
            "max_read_bytes",
            limits.max_read_bytes,
            HARD_MAX_RAW_DIRECTORY_READ_BYTES,
        ),
        (
            "max_read_calls",
            limits.max_read_calls,
            HARD_MAX_RAW_DIRECTORY_READ_CALLS,
        ),
        ("max_work", limits.max_work, HARD_MAX_RAW_DIRECTORY_WORK),
        (
            "max_allocation_attempts",
            limits.max_allocation_attempts,
            HARD_MAX_RAW_DIRECTORY_ALLOCATION_ATTEMPTS,
        ),
        (
            "max_allocation_bytes",
            limits.max_allocation_bytes,
            HARD_MAX_RAW_DIRECTORY_ALLOCATION_BYTES,
        ),
    ];
    for (field, value, ceiling) in limits_and_ceilings {
        if value == 0 || value > ceiling {
            return Err(ProductionRawDirectoryInventoryError::InvalidLimit { field });
        }
    }
    Ok(())
}
