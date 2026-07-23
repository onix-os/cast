use std::{
    mem::size_of,
    time::{Duration, Instant},
};

use super::*;

pub(super) const RECORD_ALIGNMENT_BYTES: usize = size_of::<usize>();
pub(super) const RAW_NAME_OFFSET: usize = 19;
pub(super) const MAXIMUM_RECORD_BYTES: usize =
    (RAW_NAME_OFFSET + 255 + 1).div_ceil(RECORD_ALIGNMENT_BYTES) * RECORD_ALIGNMENT_BYTES;

#[derive(Debug)]
pub(super) struct FixtureRawDirectorySource {
    chunks: Vec<Vec<u8>>,
    next_chunk: usize,
    now: Instant,
    expired_now: Instant,
    expire_after_now_call: Option<usize>,
    now_calls: usize,
    read_calls: usize,
    data_read_calls: usize,
    eof_probe_calls: usize,
    data_offers: Vec<usize>,
    allocation_calls: usize,
    fail_read_at: Option<usize>,
    fail_allocation_at: Option<usize>,
    reported_count: Option<usize>,
}

impl FixtureRawDirectorySource {
    pub(super) fn new(chunks: Vec<Vec<u8>>, now: Instant) -> Self {
        Self {
            chunks,
            next_chunk: 0,
            now,
            expired_now: now,
            expire_after_now_call: None,
            now_calls: 0,
            read_calls: 0,
            data_read_calls: 0,
            eof_probe_calls: 0,
            data_offers: Vec::new(),
            allocation_calls: 0,
            fail_read_at: None,
            fail_allocation_at: None,
            reported_count: None,
        }
    }

    pub(super) fn stable(chunks: Vec<Vec<u8>>) -> (Self, Instant) {
        let now = Instant::now();
        (Self::new(chunks, now), now + Duration::from_secs(30))
    }

    pub(super) fn fail_read_at(mut self, call: usize) -> Self {
        self.fail_read_at = Some(call);
        self
    }

    pub(super) fn fail_allocation_at(mut self, call: usize) -> Self {
        self.fail_allocation_at = Some(call);
        self
    }

    pub(super) fn report_count(mut self, count: usize) -> Self {
        self.reported_count = Some(count);
        self
    }

    pub(super) fn expire_after_now_call(mut self, call: usize, expired_now: Instant) -> Self {
        self.expire_after_now_call = Some(call);
        self.expired_now = expired_now;
        self
    }

    pub(super) const fn now_calls(&self) -> usize {
        self.now_calls
    }

    pub(super) const fn read_calls(&self) -> usize {
        self.read_calls
    }

    pub(super) const fn allocation_calls(&self) -> usize {
        self.allocation_calls
    }

    pub(super) const fn data_read_calls(&self) -> usize {
        self.data_read_calls
    }

    pub(super) const fn eof_probe_calls(&self) -> usize {
        self.eof_probe_calls
    }

    pub(super) fn data_offers(&self) -> &[usize] {
        &self.data_offers
    }

    fn admit_read(&mut self) -> Result<(), ProductionRawDirectorySourceError> {
        self.read_calls = self.read_calls.saturating_add(1);
        if self.fail_read_at == Some(self.read_calls) {
            Err(ProductionRawDirectorySourceError)
        } else {
            Ok(())
        }
    }
}

impl ProductionRawDirectorySource for FixtureRawDirectorySource {
    fn now(&mut self) -> Instant {
        self.now_calls = self.now_calls.saturating_add(1);
        if self.expire_after_now_call.is_some_and(|call| self.now_calls > call) {
            self.expired_now
        } else {
            self.now
        }
    }

    fn before_allocation(&mut self, _attempt: usize, _bytes: usize) -> Result<(), ProductionRawDirectorySourceError> {
        self.allocation_calls = self.allocation_calls.saturating_add(1);
        if self.fail_allocation_at == Some(self.allocation_calls) {
            Err(ProductionRawDirectorySourceError)
        } else {
            Ok(())
        }
    }

    fn read_chunk(&mut self, output: &mut [u8]) -> Result<usize, ProductionRawDirectorySourceError> {
        self.admit_read()?;
        self.data_read_calls = self.data_read_calls.saturating_add(1);
        self.data_offers.push(output.len());
        if let Some(found) = self.reported_count.take() {
            return Ok(found);
        }
        let Some(chunk) = self.chunks.get(self.next_chunk) else {
            return Ok(0);
        };
        if chunk.len() > output.len() {
            return Err(ProductionRawDirectorySourceError);
        }
        output[..chunk.len()].copy_from_slice(chunk);
        self.next_chunk += 1;
        Ok(chunk.len())
    }

    fn probe_end(&mut self, output: &mut [u8]) -> Result<usize, ProductionRawDirectorySourceError> {
        self.admit_read()?;
        self.eof_probe_calls = self.eof_probe_calls.saturating_add(1);
        let Some(chunk) = self.chunks.get(self.next_chunk) else {
            return Ok(0);
        };
        if chunk.len() > output.len() {
            return Err(ProductionRawDirectorySourceError);
        }
        output[..chunk.len()].copy_from_slice(chunk);
        self.next_chunk += 1;
        Ok(chunk.len())
    }
}

pub(super) fn raw_record(name: &[u8], inode: u64, node_type_hint: u8) -> Vec<u8> {
    let unaligned = RAW_NAME_OFFSET + name.len() + 1;
    let record_length = unaligned.div_ceil(RECORD_ALIGNMENT_BYTES) * RECORD_ALIGNMENT_BYTES;
    let mut record = vec![0xa5; record_length];
    record[0..8].copy_from_slice(&inode.to_ne_bytes());
    record[8..16].copy_from_slice(&i64::MAX.to_ne_bytes());
    record[16..18].copy_from_slice(
        &u16::try_from(record_length)
            .expect("test raw record length fits in u16")
            .to_ne_bytes(),
    );
    record[18] = node_type_hint;
    record[RAW_NAME_OFFSET..RAW_NAME_OFFSET + name.len()].copy_from_slice(name);
    record[RAW_NAME_OFFSET + name.len()] = 0;
    record
}

pub(super) fn raw_chunk(names: &[&[u8]]) -> Vec<u8> {
    let mut chunk = Vec::new();
    for (index, name) in names.iter().enumerate() {
        chunk.extend_from_slice(&raw_record(name, index as u64 + 1, u8::MAX));
    }
    chunk
}

pub(super) fn parse(
    chunks: Vec<Vec<u8>>,
    limits: ProductionRawDirectoryInventoryLimits,
) -> Result<ProductionRawDirectoryInventory, ProductionRawDirectoryInventoryError> {
    let (mut source, deadline) = FixtureRawDirectorySource::stable(chunks);
    parse_production_raw_directory_inventory_until(&mut source, limits, deadline)
}

pub(super) fn parse_with_usage(
    chunks: Vec<Vec<u8>>,
    limits: ProductionRawDirectoryInventoryLimits,
) -> Result<(ProductionRawDirectoryInventory, ProductionRawDirectoryInventoryUsage), ProductionRawDirectoryInventoryError>
{
    let (mut source, deadline) = FixtureRawDirectorySource::stable(chunks);
    parse_production_raw_directory_inventory_with_usage_until(&mut source, limits, deadline)
}
