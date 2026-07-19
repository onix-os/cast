use std::{cell::Cell, io, time::Instant};

use crate::linux_fs::gpt_partition_role::{
    FixtureGptPartitionRoleLimits, GptPartitionRole, GptPartitionRoleImage,
    authenticate_gpt_partition_role_two_image_fixture_until,
    authenticate_gpt_partition_role_two_sources_fixture_with_clock_until,
};

use super::support::{ESP_UUID, Fixture, live_deadline};

// Exact cumulative ledger use for the canonical 512-byte, 128-entry fixture:
// both authentication passes, complete snapshot comparison, and SHA-256.
const EXACT_READ_BYTES: usize = 68_608;
const EXACT_READ_CALLS: usize = 10;
const EXACT_WORK: usize = 186_998;
const EXACT_ALLOCATION_BYTES: usize = 78_848;

struct SignallingImage<'a> {
    bytes: &'a [u8],
    started: &'a Cell<bool>,
    signal_reads: bool,
}

impl GptPartitionRoleImage for SignallingImage<'_> {
    fn length(&self) -> u64 {
        self.bytes.len().try_into().unwrap()
    }

    fn read(&mut self, offset: u64, output: &mut [u8]) -> io::Result<usize> {
        if self.signal_reads {
            self.started.set(true);
        }
        let offset: usize = offset.try_into().unwrap();
        let Some(remaining) = self.bytes.get(offset..) else {
            return Ok(0);
        };
        let count = remaining.len().min(output.len());
        output[..count].copy_from_slice(&remaining[..count]);
        Ok(count)
    }
}

#[test]
fn two_pass_snapshot_authentication_shares_one_absolute_deadline() {
    let fixture = Fixture::esp(512);
    let error = authenticate_gpt_partition_role_two_image_fixture_until(
        &fixture.bytes,
        &fixture.bytes,
        512,
        1,
        ESP_UUID,
        GptPartitionRole::Esp,
        FixtureGptPartitionRoleLimits::default(),
        Instant::now() - std::time::Duration::from_nanos(1),
    )
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
}

#[test]
fn deadline_expiry_during_the_second_source_fails_before_returning_evidence() {
    let fixture = Fixture::esp(512);
    let second_started = Cell::new(false);
    let mut first = SignallingImage {
        bytes: &fixture.bytes,
        started: &second_started,
        signal_reads: false,
    };
    let mut second = SignallingImage {
        bytes: &fixture.bytes,
        started: &second_started,
        signal_reads: true,
    };
    let deadline = live_deadline();
    let late = deadline.checked_add(std::time::Duration::from_nanos(1)).unwrap();
    let mut clock = || {
        if second_started.get() { late } else { deadline }
    };
    let error = authenticate_gpt_partition_role_two_sources_fixture_with_clock_until(
        &mut first,
        &mut second,
        512,
        1,
        ESP_UUID,
        GptPartitionRole::Esp,
        FixtureGptPartitionRoleLimits::default(),
        deadline,
        &mut clock,
    )
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert!(second_started.get());
}

#[test]
fn fixture_limits_cannot_raise_any_hard_production_ceiling() {
    let fixture = Fixture::esp(512);
    for field in 0..4 {
        let mut limits = FixtureGptPartitionRoleLimits::default();
        match field {
            0 => limits.max_read_bytes = usize::MAX,
            1 => limits.max_read_calls = usize::MAX,
            2 => limits.max_work = usize::MAX,
            _ => limits.max_allocation_bytes = usize::MAX,
        }
        let error = authenticate_gpt_partition_role_two_image_fixture_until(
            &fixture.bytes,
            &fixture.bytes,
            512,
            1,
            ESP_UUID,
            GptPartitionRole::Esp,
            limits,
            live_deadline(),
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }
}

#[test]
fn complete_stable_ledger_accepts_exact_n_and_rejects_every_n_minus_one() {
    let fixture = Fixture::esp(512);
    let exact = FixtureGptPartitionRoleLimits {
        max_read_bytes: EXACT_READ_BYTES,
        max_read_calls: EXACT_READ_CALLS,
        max_work: EXACT_WORK,
        max_allocation_bytes: EXACT_ALLOCATION_BYTES,
    };
    authenticate_gpt_partition_role_two_image_fixture_until(
        &fixture.bytes,
        &fixture.bytes,
        512,
        1,
        ESP_UUID,
        GptPartitionRole::Esp,
        exact,
        live_deadline(),
    )
    .unwrap();

    for field in 0..4 {
        let mut below = exact;
        match field {
            0 => below.max_read_bytes -= 1,
            1 => below.max_read_calls -= 1,
            2 => below.max_work -= 1,
            _ => below.max_allocation_bytes -= 1,
        }
        let error = authenticate_gpt_partition_role_two_image_fixture_until(
            &fixture.bytes,
            &fixture.bytes,
            512,
            1,
            ESP_UUID,
            GptPartitionRole::Esp,
            below,
            live_deadline(),
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }
}
