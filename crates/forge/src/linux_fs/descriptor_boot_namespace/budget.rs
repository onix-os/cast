use std::time::Instant;

use super::{
    error::BootNamespaceAssessmentError,
    model::{
        BootNamespaceAssessmentLimits, HARD_MAX_ALLOCATIONS, HARD_MAX_COMPONENT_BYTES, HARD_MAX_COMPONENTS_PER_REQUEST,
        HARD_MAX_DESCRIPTORS, HARD_MAX_DIRECTORY_ENTRIES, HARD_MAX_NAME_BYTES, HARD_MAX_PATH_BYTES,
        HARD_MAX_READ_BYTES, HARD_MAX_REQUESTS, HARD_MAX_TOTAL_ENTRIES, HARD_MAX_TOTAL_NAME_BYTES,
        HARD_MAX_TOTAL_PATH_BYTES, HARD_MAX_WORK,
    },
    observer::BootNamespaceObserver,
};

#[cfg(test)]
use super::model::FixtureBootNamespaceUsage;

pub(super) const RAW_NAME_BUFFER_BYTES: usize = 255;
pub(super) const STREAM_BUFFER_BYTES: usize = 4 * 1024;
const SORT_WORK_PER_ELEMENT_LEVEL: usize = 4;

pub(super) struct Operation<'a, Observer> {
    observer: &'a mut Observer,
    limits: BootNamespaceAssessmentLimits,
    deadline: Instant,
    work: usize,
    allocations: usize,
    entries: usize,
    path_bytes: usize,
    name_bytes: usize,
    read_bytes: u64,
    descriptors: usize,
    peak_descriptors: usize,
}

impl<'a, Observer: BootNamespaceObserver> Operation<'a, Observer> {
    pub(super) fn new(
        observer: &'a mut Observer,
        limits: BootNamespaceAssessmentLimits,
        deadline: Instant,
    ) -> Result<Self, BootNamespaceAssessmentError> {
        validate_limits(limits)?;
        let mut operation = Self {
            observer,
            limits,
            deadline,
            work: 0,
            allocations: 0,
            entries: 0,
            path_bytes: 0,
            name_bytes: 0,
            read_bytes: 0,
            descriptors: 0,
            peak_descriptors: 0,
        };
        operation.checkpoint()?;
        Ok(operation)
    }

    pub(super) const fn limits(&self) -> BootNamespaceAssessmentLimits {
        self.limits
    }

    pub(super) fn checkpoint(&mut self) -> Result<(), BootNamespaceAssessmentError> {
        if self.observer.now() > self.deadline {
            Err(BootNamespaceAssessmentError::DeadlineExceeded {
                deadline: self.deadline,
            })
        } else {
            Ok(())
        }
    }

    pub(super) fn charge_work(
        &mut self,
        amount: usize,
        action: &'static str,
    ) -> Result<(), BootNamespaceAssessmentError> {
        self.checkpoint()?;
        let next = self
            .work
            .checked_add(amount)
            .ok_or(BootNamespaceAssessmentError::WorkLimitExceeded {
                limit: self.limits.max_work,
                action,
            })?;
        if next > self.limits.max_work {
            return Err(BootNamespaceAssessmentError::WorkLimitExceeded {
                limit: self.limits.max_work,
                action,
            });
        }
        self.work = next;
        self.checkpoint()
    }

    pub(super) fn reserve<T>(
        &mut self,
        values: &mut Vec<T>,
        additional: usize,
        action: &'static str,
    ) -> Result<(), BootNamespaceAssessmentError> {
        self.checkpoint()?;
        if additional == 0 {
            return Ok(());
        }
        let attempt = self
            .allocations
            .checked_add(1)
            .ok_or(BootNamespaceAssessmentError::AllocationLimitExceeded {
                limit: self.limits.max_allocations,
                action,
            })?;
        if attempt > self.limits.max_allocations {
            return Err(BootNamespaceAssessmentError::AllocationLimitExceeded {
                limit: self.limits.max_allocations,
                action,
            });
        }
        self.observer
            .before_allocation(attempt)
            .map_err(|_| BootNamespaceAssessmentError::AllocationFailed { action })?;
        self.allocations = attempt;
        values
            .try_reserve_exact(additional)
            .map_err(|_| BootNamespaceAssessmentError::AllocationFailed { action })?;
        self.charge_work(1, action)
    }

    pub(super) fn observe<T>(
        &mut self,
        action: &'static str,
        observe: impl FnOnce(&mut Observer) -> super::observer::ObserverResult<T>,
    ) -> Result<T, BootNamespaceAssessmentError> {
        self.charge_work(1, action)?;
        let observed =
            observe(self.observer).map_err(|_| BootNamespaceAssessmentError::ObservationFailed { action })?;
        self.checkpoint()?;
        Ok(observed)
    }

    pub(super) fn observe_retained<T>(
        &mut self,
        action: &'static str,
        observe: impl FnOnce(&mut Observer) -> super::observer::ObserverResult<T>,
        release_after_late_deadline: impl FnOnce(&mut Observer, &T),
    ) -> Result<T, BootNamespaceAssessmentError> {
        self.charge_work(1, action)?;
        let observed =
            observe(self.observer).map_err(|_| BootNamespaceAssessmentError::ObservationFailed { action })?;
        if let Err(error) = self.checkpoint() {
            release_after_late_deadline(self.observer, &observed);
            return Err(error);
        }
        Ok(observed)
    }

    pub(super) fn charge_entries(&mut self, found: usize) -> Result<(), BootNamespaceAssessmentError> {
        if found > self.limits.max_directory_entries {
            return Err(BootNamespaceAssessmentError::DirectoryEntryLimitExceeded {
                limit: self.limits.max_directory_entries,
                found,
            });
        }
        let total = self
            .entries
            .checked_add(found)
            .ok_or(BootNamespaceAssessmentError::TotalEntryLimitExceeded {
                limit: self.limits.max_total_entries,
            })?;
        if total > self.limits.max_total_entries {
            return Err(BootNamespaceAssessmentError::TotalEntryLimitExceeded {
                limit: self.limits.max_total_entries,
            });
        }
        self.entries = total;
        self.charge_work(found, "accounting a bounded directory inventory")
    }

    pub(super) fn charge_request_path(&mut self, found: usize) -> Result<(), BootNamespaceAssessmentError> {
        let total = self.path_bytes.checked_add(found).ok_or(
            BootNamespaceAssessmentError::TotalRequestPathBytesLimitExceeded {
                limit: self.limits.max_total_path_bytes,
                found: usize::MAX,
            },
        )?;
        if total > self.limits.max_total_path_bytes {
            return Err(BootNamespaceAssessmentError::TotalRequestPathBytesLimitExceeded {
                limit: self.limits.max_total_path_bytes,
                found: total,
            });
        }
        self.path_bytes = total;
        self.charge_work(found.max(1), "scanning bounded requested-path bytes")
    }

    pub(super) fn charge_name(&mut self, found: usize) -> Result<(), BootNamespaceAssessmentError> {
        if found > self.limits.max_name_bytes || found > RAW_NAME_BUFFER_BYTES {
            return Err(BootNamespaceAssessmentError::RawNameLimitExceeded {
                limit: self.limits.max_name_bytes.min(RAW_NAME_BUFFER_BYTES),
                found,
            });
        }
        let total =
            self.name_bytes
                .checked_add(found)
                .ok_or(BootNamespaceAssessmentError::TotalNameBytesLimitExceeded {
                    limit: self.limits.max_total_name_bytes,
                })?;
        if total > self.limits.max_total_name_bytes {
            return Err(BootNamespaceAssessmentError::TotalNameBytesLimitExceeded {
                limit: self.limits.max_total_name_bytes,
            });
        }
        self.name_bytes = total;
        self.charge_work(found.max(1), "accounting bounded raw-name bytes")
    }

    pub(super) fn charge_read(&mut self, read: usize) -> Result<(), BootNamespaceAssessmentError> {
        let read = u64::try_from(read).map_err(|_| BootNamespaceAssessmentError::ReadLimitExceeded {
            limit: self.limits.max_read_bytes,
        })?;
        let total = self
            .read_bytes
            .checked_add(read)
            .ok_or(BootNamespaceAssessmentError::ReadLimitExceeded {
                limit: self.limits.max_read_bytes,
            })?;
        if total > self.limits.max_read_bytes {
            return Err(BootNamespaceAssessmentError::ReadLimitExceeded {
                limit: self.limits.max_read_bytes,
            });
        }
        self.read_bytes = total;
        self.charge_work(1, "streaming bounded content bytes")
    }

    pub(super) fn bounded_read_window(&mut self, requested: usize) -> Result<usize, BootNamespaceAssessmentError> {
        self.checkpoint()?;
        let remaining = self.limits.max_read_bytes - self.read_bytes;
        if remaining == 0 {
            return Err(BootNamespaceAssessmentError::ReadLimitExceeded {
                limit: self.limits.max_read_bytes,
            });
        }
        Ok(requested.min(usize::try_from(remaining).unwrap_or(usize::MAX)))
    }

    pub(super) fn charge_unstable_sort(
        &mut self,
        elements: usize,
        action: &'static str,
    ) -> Result<(), BootNamespaceAssessmentError> {
        if elements < 2 {
            return self.checkpoint();
        }
        let levels = usize::BITS as usize - (elements - 1).leading_zeros() as usize;
        let work = elements
            .checked_mul(levels)
            .and_then(|work| work.checked_mul(SORT_WORK_PER_ELEMENT_LEVEL))
            .ok_or(BootNamespaceAssessmentError::WorkLimitExceeded {
                limit: self.limits.max_work,
                action,
            })?;
        self.charge_work(work, action)
    }

    pub(super) fn acquire_descriptor(
        &mut self,
        request_index: usize,
        component_index: usize,
    ) -> Result<(), BootNamespaceAssessmentError> {
        let next = self
            .descriptors
            .checked_add(1)
            .ok_or(BootNamespaceAssessmentError::DescriptorLimitExceeded {
                limit: self.limits.max_descriptors,
                request_index,
                component_index,
            })?;
        if next > self.limits.max_descriptors {
            return Err(BootNamespaceAssessmentError::DescriptorLimitExceeded {
                limit: self.limits.max_descriptors,
                request_index,
                component_index,
            });
        }
        self.charge_work(1, "reserving one bounded namespace identity slot")?;
        self.descriptors = next;
        self.peak_descriptors = self.peak_descriptors.max(next);
        Ok(())
    }

    pub(super) fn release_descriptor(&mut self, identity: super::observer::BootNamespaceNodeIdentity) {
        debug_assert!(self.descriptors > 0, "releasing an unaccounted namespace descriptor");
        self.descriptors = self.descriptors.saturating_sub(1);
        self.observer.release_node(identity);
    }

    pub(super) fn cancel_descriptor_reservation(&mut self) {
        debug_assert!(
            self.descriptors > 0,
            "cancelling an absent namespace descriptor reservation"
        );
        self.descriptors = self.descriptors.saturating_sub(1);
    }

    #[cfg(test)]
    pub(super) const fn usage(&self) -> FixtureBootNamespaceUsage {
        FixtureBootNamespaceUsage {
            work: self.work,
            allocations: self.allocations,
            entries: self.entries,
            path_bytes: self.path_bytes,
            name_bytes: self.name_bytes,
            read_bytes: self.read_bytes,
            peak_descriptors: self.peak_descriptors,
        }
    }
}

fn validate_limits(limits: BootNamespaceAssessmentLimits) -> Result<(), BootNamespaceAssessmentError> {
    let values = [
        ("max_requests", limits.max_requests, HARD_MAX_REQUESTS),
        (
            "max_components_per_request",
            limits.max_components_per_request,
            HARD_MAX_COMPONENTS_PER_REQUEST,
        ),
        ("max_path_bytes", limits.max_path_bytes, HARD_MAX_PATH_BYTES),
        (
            "max_total_path_bytes",
            limits.max_total_path_bytes,
            HARD_MAX_TOTAL_PATH_BYTES,
        ),
        (
            "max_component_bytes",
            limits.max_component_bytes,
            HARD_MAX_COMPONENT_BYTES,
        ),
        (
            "max_directory_entries",
            limits.max_directory_entries,
            HARD_MAX_DIRECTORY_ENTRIES,
        ),
        ("max_total_entries", limits.max_total_entries, HARD_MAX_TOTAL_ENTRIES),
        ("max_name_bytes", limits.max_name_bytes, HARD_MAX_NAME_BYTES),
        (
            "max_total_name_bytes",
            limits.max_total_name_bytes,
            HARD_MAX_TOTAL_NAME_BYTES,
        ),
        ("max_work", limits.max_work, HARD_MAX_WORK),
        ("max_descriptors", limits.max_descriptors, HARD_MAX_DESCRIPTORS),
        ("max_allocations", limits.max_allocations, HARD_MAX_ALLOCATIONS),
    ];
    for (field, value, ceiling) in values {
        if value == 0 || value > ceiling {
            return Err(BootNamespaceAssessmentError::InvalidLimit { field });
        }
    }
    if limits.max_read_bytes == 0 || limits.max_read_bytes > HARD_MAX_READ_BYTES {
        return Err(BootNamespaceAssessmentError::InvalidLimit {
            field: "max_read_bytes",
        });
    }
    Ok(())
}
