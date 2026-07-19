use std::{io, mem::size_of, time::Instant};

use super::constants;

/// Minimal random-access image seam visible only inside `linux_fs`.
///
/// The future retained block-device adapter can implement this trait without
/// making a descriptor, path, or read capability part of returned evidence.
pub(in crate::linux_fs) trait Image {
    fn length(&self) -> u64;
    fn read(&mut self, offset: u64, output: &mut [u8]) -> io::Result<usize>;
}

pub(super) struct SliceImage<'a> {
    bytes: &'a [u8],
}

impl<'a> SliceImage<'a> {
    pub(super) const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes }
    }
}

impl Image for SliceImage<'_> {
    fn length(&self) -> u64 {
        self.bytes.len().try_into().unwrap_or(u64::MAX)
    }

    fn read(&mut self, offset: u64, output: &mut [u8]) -> io::Result<usize> {
        let offset: usize = offset
            .try_into()
            .map_err(|_| io::Error::new(io::ErrorKind::UnexpectedEof, "GPT image offset is not representable"))?;
        let Some(remaining) = self.bytes.get(offset..) else {
            return Ok(0);
        };
        let count = remaining.len().min(output.len());
        output[..count].copy_from_slice(&remaining[..count]);
        Ok(count)
    }
}

#[cfg(test)]
pub(super) struct ChunkedSliceImage<'a> {
    bytes: &'a [u8],
    max_chunk: usize,
    stop_after: Option<usize>,
    delivered: usize,
}

#[cfg(test)]
impl<'a> ChunkedSliceImage<'a> {
    pub(super) const fn new(bytes: &'a [u8], max_chunk: usize, stop_after: Option<usize>) -> Self {
        Self {
            bytes,
            max_chunk,
            stop_after,
            delivered: 0,
        }
    }
}

#[cfg(test)]
impl Image for ChunkedSliceImage<'_> {
    fn length(&self) -> u64 {
        self.bytes.len().try_into().unwrap_or(u64::MAX)
    }

    fn read(&mut self, offset: u64, output: &mut [u8]) -> io::Result<usize> {
        if self.stop_after.is_some_and(|limit| self.delivered >= limit) {
            return Ok(0);
        }
        let offset: usize = offset.try_into().map_err(|_| {
            io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "fixture image offset is not representable",
            )
        })?;
        let Some(remaining) = self.bytes.get(offset..) else {
            return Ok(0);
        };
        let until_stop = self
            .stop_after
            .map_or(usize::MAX, |limit| limit.saturating_sub(self.delivered));
        let count = remaining.len().min(output.len()).min(self.max_chunk).min(until_stop);
        output[..count].copy_from_slice(&remaining[..count]);
        self.delivered = self.delivered.saturating_add(count);
        Ok(count)
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Limits {
    max_read_bytes: usize,
    max_read_calls: usize,
    max_work: usize,
    max_allocation_bytes: usize,
}

impl Limits {
    pub(super) const fn production() -> Self {
        Self {
            max_read_bytes: constants::MAX_READ_BYTES,
            max_read_calls: constants::MAX_READ_CALLS,
            max_work: constants::MAX_WORK,
            max_allocation_bytes: constants::MAX_ALLOCATION_BYTES,
        }
    }
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct FixtureLimits {
    pub(crate) max_read_bytes: usize,
    pub(crate) max_read_calls: usize,
    pub(crate) max_work: usize,
    pub(crate) max_allocation_bytes: usize,
}

#[cfg(test)]
impl Default for FixtureLimits {
    fn default() -> Self {
        let production = Limits::production();
        Self {
            max_read_bytes: production.max_read_bytes,
            max_read_calls: production.max_read_calls,
            max_work: production.max_work,
            max_allocation_bytes: production.max_allocation_bytes,
        }
    }
}

#[cfg(test)]
impl From<FixtureLimits> for Limits {
    fn from(value: FixtureLimits) -> Self {
        Self {
            max_read_bytes: value.max_read_bytes,
            max_read_calls: value.max_read_calls,
            max_work: value.max_work,
            max_allocation_bytes: value.max_allocation_bytes,
        }
    }
}

pub(super) struct Operation<'a> {
    deadline: Instant,
    clock: Option<&'a mut dyn FnMut() -> Instant>,
    read_bytes: usize,
    read_calls: usize,
    work: usize,
    allocations: usize,
    limits: Limits,
}

impl<'a> Operation<'a> {
    pub(super) fn new_with_clock(
        limits: Limits,
        deadline: Instant,
        clock: Option<&'a mut dyn FnMut() -> Instant>,
    ) -> io::Result<Self> {
        if limits.max_read_bytes == 0
            || limits.max_read_calls == 0
            || limits.max_work == 0
            || limits.max_allocation_bytes == 0
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "GPT parser limits must all be nonzero",
            ));
        }
        let production = Limits::production();
        if limits.max_read_bytes > production.max_read_bytes
            || limits.max_read_calls > production.max_read_calls
            || limits.max_work > production.max_work
            || limits.max_allocation_bytes > production.max_allocation_bytes
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "GPT parser limits exceed the hard production ceilings",
            ));
        }
        let mut operation = Self {
            deadline,
            clock,
            read_bytes: 0,
            read_calls: 0,
            work: 0,
            allocations: 0,
            limits,
        };
        operation.checkpoint()?;
        Ok(operation)
    }

    pub(super) fn checkpoint(&mut self) -> io::Result<()> {
        let now = self.clock.as_mut().map_or_else(Instant::now, |clock| clock());
        if now > self.deadline {
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "GPT image authentication exceeded its deadline",
            ))
        } else {
            Ok(())
        }
    }

    pub(super) fn charge_work(&mut self, amount: usize, action: &'static str) -> io::Result<()> {
        self.checkpoint()?;
        self.work = self
            .work
            .checked_add(amount)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "GPT parser work accounting overflowed"))?;
        if self.work > self.limits.max_work {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("GPT parser exceeded its work limit while {action}"),
            ));
        }
        self.checkpoint()
    }

    pub(super) fn allocate_zeroed(&mut self, bytes: usize, action: &'static str) -> io::Result<Vec<u8>> {
        self.checkpoint()?;
        self.allocations = self
            .allocations
            .checked_add(bytes)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "GPT allocation accounting overflowed"))?;
        if self.allocations > self.limits.max_allocation_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("GPT parser exceeded its allocation limit while {action}"),
            ));
        }
        let mut output = Vec::new();
        output
            .try_reserve_exact(bytes)
            .map_err(|source| io::Error::other(format!("could not allocate {action}: {source}")))?;
        output.resize(bytes, 0);
        self.checkpoint()?;
        Ok(output)
    }

    pub(super) fn reserve_items<T>(&mut self, count: usize, action: &'static str) -> io::Result<Vec<T>> {
        self.checkpoint()?;
        let bytes = count
            .checked_mul(size_of::<T>())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "GPT item allocation size overflowed"))?;
        self.allocations = self
            .allocations
            .checked_add(bytes)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "GPT allocation accounting overflowed"))?;
        if self.allocations > self.limits.max_allocation_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("GPT parser exceeded its allocation limit while {action}"),
            ));
        }
        let mut output = Vec::new();
        output
            .try_reserve_exact(count)
            .map_err(|source| io::Error::other(format!("could not allocate {action}: {source}")))?;
        self.checkpoint()?;
        Ok(output)
    }

    pub(super) fn read_exact(
        &mut self,
        source: &mut impl Image,
        offset: u64,
        output: &mut [u8],
        action: &'static str,
    ) -> io::Result<()> {
        self.checkpoint()?;
        self.read_bytes = self
            .read_bytes
            .checked_add(output.len())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "GPT read accounting overflowed"))?;
        if self.read_bytes > self.limits.max_read_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("GPT parser exceeded its read-byte limit while {action}"),
            ));
        }
        let mut completed = 0usize;
        while completed < output.len() {
            self.checkpoint()?;
            self.read_calls = self
                .read_calls
                .checked_add(1)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "GPT read-call accounting overflowed"))?;
            if self.read_calls > self.limits.max_read_calls {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("GPT parser exceeded its read-call limit while {action}"),
                ));
            }
            let completed_u64: u64 = completed
                .try_into()
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "GPT read progress is not representable"))?;
            let current = offset
                .checked_add(completed_u64)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "GPT read offset overflowed"))?;
            let chunk_end = completed
                .checked_add(constants::READ_CHUNK_BYTES.min(output.len() - completed))
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "GPT read chunk overflowed"))?;
            let read = source.read(current, &mut output[completed..chunk_end])?;
            if read == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    format!("GPT image ended while {action}"),
                ));
            }
            if read > chunk_end - completed {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "GPT image source returned an impossible read length",
                ));
            }
            completed += read;
            self.checkpoint()?;
        }
        Ok(())
    }
}
