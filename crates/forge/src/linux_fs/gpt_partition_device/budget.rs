use std::{io, time::Instant};

pub(super) const MAX_OBSERVATION_CALLS: usize = 2;
pub(super) const MAX_WORK_UNITS: usize = 45;

#[derive(Clone, Copy, Debug)]
pub(super) struct Limits {
    observation_calls: usize,
    work_units: usize,
}

impl Limits {
    pub(super) const fn production() -> Self {
        Self {
            observation_calls: MAX_OBSERVATION_CALLS,
            work_units: MAX_WORK_UNITS,
        }
    }

    #[cfg(test)]
    pub(super) const fn fixture(observation_calls: usize, work_units: usize) -> Self {
        Self {
            observation_calls,
            work_units,
        }
    }
}

pub(super) struct Operation<'a> {
    remaining_observations: usize,
    remaining_work: usize,
    deadline: Instant,
    clock: Option<&'a mut dyn FnMut() -> Instant>,
}

impl<'a> Operation<'a> {
    pub(super) fn new(limits: Limits, deadline: Instant) -> io::Result<Self> {
        Self::new_with_clock(limits, deadline, None)
    }

    pub(super) fn new_with_clock(
        limits: Limits,
        deadline: Instant,
        clock: Option<&'a mut dyn FnMut() -> Instant>,
    ) -> io::Result<Self> {
        if limits.observation_calls == 0
            || limits.observation_calls > MAX_OBSERVATION_CALLS
            || limits.work_units == 0
            || limits.work_units > MAX_WORK_UNITS
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "GPT-device fixture limits exceed production ceilings",
            ));
        }
        let mut operation = Self {
            remaining_observations: limits.observation_calls,
            remaining_work: limits.work_units,
            deadline,
            clock,
        };
        operation.checkpoint()?;
        Ok(operation)
    }

    pub(super) fn reserve_observation(&mut self) -> io::Result<()> {
        self.checkpoint()?;
        self.remaining_observations = self
            .remaining_observations
            .checked_sub(1)
            .ok_or_else(|| io::Error::other("GPT-device observation-call budget exhausted"))?;
        self.checkpoint()
    }

    pub(super) fn charge_work(&mut self, units: usize) -> io::Result<()> {
        self.checkpoint()?;
        self.remaining_work = self
            .remaining_work
            .checked_sub(units)
            .ok_or_else(|| io::Error::other("GPT-device validation-work budget exhausted"))?;
        self.checkpoint()
    }

    pub(super) fn finish(&mut self) -> io::Result<()> {
        self.checkpoint()
    }

    pub(super) fn checkpoint(&mut self) -> io::Result<()> {
        let now = match self.clock.as_mut() {
            Some(clock) => clock(),
            None => Instant::now(),
        };
        if now > self.deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "GPT-device reconciliation exceeded its deadline",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
#[derive(Clone, Copy, Debug)]
pub(in crate::linux_fs) struct FixtureLimits {
    pub(in crate::linux_fs) observation_calls: usize,
    pub(in crate::linux_fs) work_units: usize,
}

#[cfg(test)]
impl From<FixtureLimits> for Limits {
    fn from(value: FixtureLimits) -> Self {
        Self::fixture(value.observation_calls, value.work_units)
    }
}
