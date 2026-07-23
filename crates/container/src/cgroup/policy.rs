/// Hard aggregate controls written to a newly created cgroup v2 leaf.
///
/// Values are emitted as canonical base-10 integers. This type intentionally
/// has no `max` variant: its purpose is to represent actual hard ceilings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CgroupLimits {
    pids_max: u64,
    memory_max: u64,
    memory_swap_max: u64,
    cpu_quota_micros: u64,
    cpu_period_micros: u64,
}

impl CgroupLimits {
    pub fn new(
        pids_max: u64,
        memory_max: u64,
        memory_swap_max: u64,
        cpu_quota_micros: u64,
        cpu_period_micros: u64,
    ) -> Result<Self> {
        for (field, value) in [
            ("pids.max", pids_max),
            ("memory.max", memory_max),
            ("cpu.max quota", cpu_quota_micros),
            ("cpu.max period", cpu_period_micros),
        ] {
            if value == 0 {
                return Err(CgroupError::ZeroLimit { field });
            }
        }
        if pids_max > MAX_PIDS {
            return Err(CgroupError::InvalidPidsMax {
                value: pids_max,
                maximum: MAX_PIDS,
            });
        }
        if !(MIN_CPU_BANDWIDTH_MICROS..=MAX_CPU_QUOTA_MICROS).contains(&cpu_quota_micros) {
            return Err(CgroupError::InvalidCpuQuota {
                value: cpu_quota_micros,
                minimum: MIN_CPU_BANDWIDTH_MICROS,
                maximum: MAX_CPU_QUOTA_MICROS,
            });
        }
        if !(MIN_CPU_BANDWIDTH_MICROS..=MAX_CPU_PERIOD_MICROS).contains(&cpu_period_micros) {
            return Err(CgroupError::InvalidCpuPeriod {
                value: cpu_period_micros,
                minimum: MIN_CPU_BANDWIDTH_MICROS,
                maximum: MAX_CPU_PERIOD_MICROS,
            });
        }
        let page_size = system_page_size()?;
        for (field, value) in [("memory.max", memory_max), ("memory.swap.max", memory_swap_max)] {
            if value % page_size != 0 {
                return Err(CgroupError::UnalignedMemoryLimit {
                    field,
                    value,
                    page_size,
                });
            }
        }

        Ok(Self {
            pids_max,
            memory_max,
            memory_swap_max,
            cpu_quota_micros,
            cpu_period_micros,
        })
    }

    pub const fn pids_max(self) -> u64 {
        self.pids_max
    }

    pub const fn memory_max(self) -> u64 {
        self.memory_max
    }

    pub const fn memory_swap_max(self) -> u64 {
        self.memory_swap_max
    }

    pub const fn cpu_quota_micros(self) -> u64 {
        self.cpu_quota_micros
    }

    pub const fn cpu_period_micros(self) -> u64 {
        self.cpu_period_micros
    }
}

/// Finite policy used while waiting for a killed cgroup to become empty.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DrainPolicy {
    timeout: Duration,
    poll_interval: Duration,
}

impl DrainPolicy {
    pub fn new(timeout: Duration, poll_interval: Duration) -> Result<Self> {
        if timeout.is_zero() || poll_interval.is_zero() {
            return Err(CgroupError::InvalidDrainPolicy);
        }
        Ok(Self { timeout, poll_interval })
    }

    pub const fn timeout(self) -> Duration {
        self.timeout
    }

    pub const fn poll_interval(self) -> Duration {
        self.poll_interval
    }
}

impl Default for DrainPolicy {
    fn default() -> Self {
        Self {
            timeout: DEFAULT_DRAIN_TIMEOUT,
            poll_interval: DEFAULT_DRAIN_POLL_INTERVAL,
        }
    }
}

/// Parsed `cgroup.events` state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CgroupEvents {
    populated: bool,
    frozen: bool,
}

impl CgroupEvents {
    pub const fn populated(self) -> bool {
        self.populated
    }

    pub const fn frozen(self) -> bool {
        self.frozen
    }
}
