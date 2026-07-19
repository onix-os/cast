use std::time::{Duration, Instant};

use super::{support::*, *};

fn one_entry_chunk() -> Vec<Vec<u8>> {
    vec![raw_chunk(&[b"entry"])]
}

#[test]
fn zero_and_above_ceiling_limits_fail_before_source_observation() {
    let defaults = ProductionRawDirectoryInventoryLimits::default();
    let invalid = [
        ProductionRawDirectoryInventoryLimits {
            max_records: 0,
            ..defaults
        },
        ProductionRawDirectoryInventoryLimits {
            max_name_bytes: 0,
            ..defaults
        },
        ProductionRawDirectoryInventoryLimits {
            max_read_bytes: 0,
            ..defaults
        },
        ProductionRawDirectoryInventoryLimits {
            max_read_calls: 0,
            ..defaults
        },
        ProductionRawDirectoryInventoryLimits {
            max_work: 0,
            ..defaults
        },
        ProductionRawDirectoryInventoryLimits {
            max_allocation_attempts: 0,
            ..defaults
        },
        ProductionRawDirectoryInventoryLimits {
            max_allocation_bytes: 0,
            ..defaults
        },
        ProductionRawDirectoryInventoryLimits {
            max_records: defaults.max_records + 1,
            ..defaults
        },
        ProductionRawDirectoryInventoryLimits {
            max_name_bytes: defaults.max_name_bytes + 1,
            ..defaults
        },
        ProductionRawDirectoryInventoryLimits {
            max_read_bytes: defaults.max_read_bytes + 1,
            ..defaults
        },
        ProductionRawDirectoryInventoryLimits {
            max_read_calls: defaults.max_read_calls + 1,
            ..defaults
        },
        ProductionRawDirectoryInventoryLimits {
            max_work: defaults.max_work + 1,
            ..defaults
        },
        ProductionRawDirectoryInventoryLimits {
            max_allocation_attempts: defaults.max_allocation_attempts + 1,
            ..defaults
        },
        ProductionRawDirectoryInventoryLimits {
            max_allocation_bytes: defaults.max_allocation_bytes + 1,
            ..defaults
        },
    ];
    for limits in invalid {
        let (mut source, deadline) = FixtureRawDirectorySource::stable(one_entry_chunk());
        assert!(matches!(
            parse_production_raw_directory_inventory_until(&mut source, limits, deadline).unwrap_err(),
            ProductionRawDirectoryInventoryError::InvalidLimit { .. }
        ));
        assert_eq!(source.now_calls(), 0);
        assert_eq!(source.read_calls(), 0);
        assert_eq!(source.allocation_calls(), 0);
    }
}

#[test]
fn record_and_name_ledgers_accept_exact_n_and_reject_n_minus_one() {
    let chunks = vec![raw_chunk(&[b".", b"first", b"second"])];
    let (_, usage) = parse_with_usage(chunks.clone(), ProductionRawDirectoryInventoryLimits::default()).unwrap();
    assert_eq!(usage.records, 3);
    assert_eq!(usage.name_bytes, 1 + 5 + 6);

    let exact = ProductionRawDirectoryInventoryLimits {
        max_records: usage.records,
        max_name_bytes: usage.name_bytes,
        ..ProductionRawDirectoryInventoryLimits::default()
    };
    parse(chunks.clone(), exact).unwrap();

    assert!(matches!(
        parse(
            chunks.clone(),
            ProductionRawDirectoryInventoryLimits {
                max_records: usage.records - 1,
                ..exact
            }
        )
        .unwrap_err(),
        ProductionRawDirectoryInventoryError::RecordLimitExceeded { .. }
    ));
    assert!(matches!(
        parse(
            chunks,
            ProductionRawDirectoryInventoryLimits {
                max_name_bytes: usage.name_bytes - 1,
                ..exact
            }
        )
        .unwrap_err(),
        ProductionRawDirectoryInventoryError::NameByteLimitExceeded { .. }
    ));
}

#[test]
fn read_byte_and_call_ledgers_include_the_terminal_eof_call() {
    let maximum_name = [b'x'; 255];
    let chunks = vec![raw_record(&maximum_name, 1, u8::MAX)];
    assert_eq!(chunks[0].len(), MAXIMUM_RECORD_BYTES);

    let exact = ProductionRawDirectoryInventoryLimits {
        max_read_bytes: chunks[0].len(),
        max_read_calls: 2,
        ..ProductionRawDirectoryInventoryLimits::default()
    };
    let (source, deadline) = FixtureRawDirectorySource::stable(chunks.clone());
    let mut exact_source = source;
    let (_, usage) =
        parse_production_raw_directory_inventory_with_usage_until(&mut exact_source, exact, deadline).unwrap();
    assert_eq!(usage.read_bytes, chunks[0].len());
    assert_eq!(usage.read_calls, 2);
    assert_eq!(usage.eof_probes, 1);
    assert_eq!(usage.eof_probe_capacity_bytes, MAXIMUM_RECORD_BYTES);
    assert_eq!(exact_source.data_offers(), &[MAXIMUM_RECORD_BYTES]);
    assert_eq!(exact_source.data_read_calls(), 1);
    assert_eq!(exact_source.eof_probe_calls(), 1);

    let (source, deadline) = FixtureRawDirectorySource::stable(chunks.clone());
    let mut too_small = source;
    assert!(matches!(
        parse_production_raw_directory_inventory_until(
            &mut too_small,
            ProductionRawDirectoryInventoryLimits {
                max_read_bytes: MAXIMUM_RECORD_BYTES - 1,
                ..exact
            },
            deadline,
        )
        .unwrap_err(),
        ProductionRawDirectoryInventoryError::ReadByteLimitExceeded { .. }
    ));
    assert_eq!(too_small.data_read_calls(), 0);
    assert_eq!(too_small.eof_probe_calls(), 1);
    assert!(too_small.data_offers().is_empty());

    let (source, deadline) = FixtureRawDirectorySource::stable(vec![chunks[0].clone(), chunks[0].clone()]);
    let mut exhausted = source;
    assert!(matches!(
        parse_production_raw_directory_inventory_until(
            &mut exhausted,
            ProductionRawDirectoryInventoryLimits {
                max_read_bytes: MAXIMUM_RECORD_BYTES,
                ..exact
            },
            deadline,
        )
        .unwrap_err(),
        ProductionRawDirectoryInventoryError::ReadByteLimitExceeded { .. }
    ));
    assert_eq!(exhausted.data_read_calls(), 1);
    assert_eq!(exhausted.eof_probe_calls(), 1);
    assert_eq!(exhausted.data_offers(), &[MAXIMUM_RECORD_BYTES]);

    let (source, deadline) = FixtureRawDirectorySource::stable(chunks);
    let mut call_limited = source;
    assert!(matches!(
        parse_production_raw_directory_inventory_until(
            &mut call_limited,
            ProductionRawDirectoryInventoryLimits {
                max_read_calls: 1,
                ..exact
            },
            deadline,
        )
        .unwrap_err(),
        ProductionRawDirectoryInventoryError::ReadCallLimitExceeded { .. }
    ));
    assert_eq!(call_limited.data_read_calls(), 1);
    assert_eq!(call_limited.eof_probe_calls(), 0);
}

#[test]
fn work_ledger_accepts_exact_n_and_rejects_n_minus_one() {
    let chunks = one_entry_chunk();
    let (_, usage) = parse_with_usage(chunks.clone(), ProductionRawDirectoryInventoryLimits::default()).unwrap();
    assert!(usage.work > 1);
    parse(
        chunks.clone(),
        ProductionRawDirectoryInventoryLimits {
            max_work: usage.work,
            ..ProductionRawDirectoryInventoryLimits::default()
        },
    )
    .unwrap();
    assert!(matches!(
        parse(
            chunks,
            ProductionRawDirectoryInventoryLimits {
                max_work: usage.work - 1,
                ..ProductionRawDirectoryInventoryLimits::default()
            }
        )
        .unwrap_err(),
        ProductionRawDirectoryInventoryError::WorkLimitExceeded { .. }
    ));
}

#[test]
fn allocation_ledgers_and_injected_failure_are_fail_closed() {
    let chunks = one_entry_chunk();
    let (_, usage) = parse_with_usage(chunks.clone(), ProductionRawDirectoryInventoryLimits::default()).unwrap();
    assert_eq!(usage.allocation_attempts, 2);
    assert!(usage.allocation_bytes > b"entry".len());

    let exact = ProductionRawDirectoryInventoryLimits {
        max_allocation_attempts: usage.allocation_attempts,
        max_allocation_bytes: usage.allocation_bytes,
        ..ProductionRawDirectoryInventoryLimits::default()
    };
    parse(chunks.clone(), exact).unwrap();
    assert!(matches!(
        parse(
            chunks.clone(),
            ProductionRawDirectoryInventoryLimits {
                max_allocation_attempts: usage.allocation_attempts - 1,
                ..exact
            }
        )
        .unwrap_err(),
        ProductionRawDirectoryInventoryError::AllocationAttemptLimitExceeded { .. }
    ));
    assert!(matches!(
        parse(
            chunks,
            ProductionRawDirectoryInventoryLimits {
                max_allocation_bytes: usage.allocation_bytes - 1,
                ..exact
            }
        )
        .unwrap_err(),
        ProductionRawDirectoryInventoryError::AllocationByteLimitExceeded { .. }
    ));

    let (source, deadline) = FixtureRawDirectorySource::stable(one_entry_chunk());
    let mut failed = source.fail_allocation_at(2);
    assert!(matches!(
        parse_production_raw_directory_inventory_until(
            &mut failed,
            ProductionRawDirectoryInventoryLimits::default(),
            deadline,
        )
        .unwrap_err(),
        ProductionRawDirectoryInventoryError::AllocationFailed { .. }
    ));
    assert_eq!(failed.allocation_calls(), 2);
}

#[test]
fn deadline_equality_is_admitted_and_expiry_is_checked_around_source_calls() {
    let now = Instant::now();
    let mut equal = FixtureRawDirectorySource::new(Vec::new(), now);
    let inventory = parse_production_raw_directory_inventory_until(
        &mut equal,
        ProductionRawDirectoryInventoryLimits::default(),
        now,
    )
    .unwrap();
    assert!(inventory.is_empty());

    let mut expired = FixtureRawDirectorySource::new(Vec::new(), now + Duration::from_nanos(1));
    assert!(matches!(
        parse_production_raw_directory_inventory_until(
            &mut expired,
            ProductionRawDirectoryInventoryLimits::default(),
            now,
        )
        .unwrap_err(),
        ProductionRawDirectoryInventoryError::DeadlineExceeded { .. }
    ));
    assert_eq!(expired.read_calls(), 0);

    let deadline = now + Duration::from_secs(1);
    let mut late_read =
        FixtureRawDirectorySource::new(Vec::new(), now).expire_after_now_call(5, deadline + Duration::from_nanos(1));
    assert!(matches!(
        parse_production_raw_directory_inventory_until(
            &mut late_read,
            ProductionRawDirectoryInventoryLimits::default(),
            deadline,
        )
        .unwrap_err(),
        ProductionRawDirectoryInventoryError::DeadlineExceeded { .. }
    ));
    assert_eq!(late_read.read_calls(), 1);
}
