use std::{io, time::Instant};

use crate::linux_fs::gpt_partition_role::{
    FixtureGptPartitionRoleLimits, GptPartitionRole, authenticate_gpt_partition_role_chunked_fixture_until,
    authenticate_gpt_partition_role_fixture_until, authenticate_gpt_partition_role_fixture_with_clock_until,
};

use super::support::{ESP_UUID, Fixture, live_deadline};

fn authenticate_with(
    fixture: &Fixture,
    limits: FixtureGptPartitionRoleLimits,
) -> io::Result<crate::linux_fs::gpt_partition_role::ValidatedGptPartitionRole> {
    authenticate_gpt_partition_role_fixture_until(
        &fixture.bytes,
        fixture.block_size,
        1,
        ESP_UUID,
        GptPartitionRole::Esp,
        limits,
        live_deadline(),
    )
}

#[test]
fn expired_deadline_and_zero_limits_fail_before_image_parsing() {
    let fixture = Fixture::esp(512);
    let error = authenticate_gpt_partition_role_fixture_until(
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

    let deadline = live_deadline();
    let mut exactly_at_deadline = || deadline;
    authenticate_gpt_partition_role_fixture_with_clock_until(
        &fixture.bytes,
        512,
        1,
        ESP_UUID,
        GptPartitionRole::Esp,
        FixtureGptPartitionRoleLimits::default(),
        deadline,
        &mut exactly_at_deadline,
    )
    .unwrap();
    let late = deadline.checked_add(std::time::Duration::from_nanos(1)).unwrap();
    let mut one_nanosecond_late = || late;
    let error = authenticate_gpt_partition_role_fixture_with_clock_until(
        &fixture.bytes,
        512,
        1,
        ESP_UUID,
        GptPartitionRole::Esp,
        FixtureGptPartitionRoleLimits::default(),
        deadline,
        &mut one_nanosecond_late,
    )
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);

    for field in 0..4 {
        let mut limits = FixtureGptPartitionRoleLimits::default();
        match field {
            0 => limits.max_read_bytes = 0,
            1 => limits.max_read_calls = 0,
            2 => limits.max_work = 0,
            _ => limits.max_allocation_bytes = 0,
        }
        let error = authenticate_with(&fixture, limits).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }
}

#[test]
fn read_byte_and_call_limits_fail_closed_at_exact_boundaries() {
    let fixture = Fixture::esp(512);
    let exact_read_bytes = 512 + 2 * 512 + 2 * fixture.array_bytes();
    let mut limits = FixtureGptPartitionRoleLimits {
        max_read_bytes: exact_read_bytes,
        max_read_calls: 5,
        ..FixtureGptPartitionRoleLimits::default()
    };
    authenticate_with(&fixture, limits).unwrap();

    limits.max_read_bytes -= 1;
    assert_eq!(
        authenticate_with(&fixture, limits).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    limits.max_read_bytes += 1;
    limits.max_read_calls -= 1;
    assert_eq!(
        authenticate_with(&fixture, limits).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
}

#[test]
fn work_and_allocation_limits_reject_underprovisioned_operations() {
    let fixture = Fixture::esp(512);
    let mut work = FixtureGptPartitionRoleLimits::default();
    work.max_work = 1;
    assert_eq!(
        authenticate_with(&fixture, work).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );

    let mut allocation = FixtureGptPartitionRoleLimits::default();
    allocation.max_allocation_bytes = 511;
    assert_eq!(
        authenticate_with(&fixture, allocation).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
}

#[test]
fn maximum_entry_count_and_array_size_are_admitted_within_global_bounds() {
    let fixture = Fixture::with_entry_count(512, 4_096);
    let selected = fixture.authenticate().unwrap();
    assert_eq!(selected.partition_uuid(), ESP_UUID);
    assert_eq!(fixture.array_bytes(), 512 * 1024);
}

#[test]
fn image_truncation_and_too_small_images_fail_without_fallback() {
    let fixture = Fixture::esp(512);
    for length in [0, 512, 5 * 512, fixture.bytes.len() - 512] {
        let error = crate::linux_fs::gpt_partition_role::authenticate_gpt_partition_role_image_until(
            &fixture.bytes[..length],
            512,
            1,
            ESP_UUID,
            GptPartitionRole::Esp,
            live_deadline(),
        )
        .unwrap_err();
        assert!(matches!(
            error.kind(),
            io::ErrorKind::InvalidData | io::ErrorKind::UnexpectedEof
        ));
    }

    let selected = authenticate_gpt_partition_role_chunked_fixture_until(
        &fixture.bytes,
        512,
        1,
        ESP_UUID,
        GptPartitionRole::Esp,
        1_024,
        None,
        live_deadline(),
    )
    .unwrap();
    assert_eq!(selected.partition_uuid(), ESP_UUID);

    let error = authenticate_gpt_partition_role_chunked_fixture_until(
        &fixture.bytes,
        512,
        1,
        ESP_UUID,
        GptPartitionRole::Esp,
        1_024,
        Some(600),
        live_deadline(),
    )
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
}
