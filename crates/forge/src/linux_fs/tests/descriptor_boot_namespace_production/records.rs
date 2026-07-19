use std::mem::size_of;

use super::{support::*, *};

#[test]
fn valid_inventory_filters_dot_entries_and_ignores_kernel_identity_hints() {
    assert!(RECORD_ALIGNMENT_BYTES == 4 || RECORD_ALIGNMENT_BYTES == 8);
    assert_eq!(RECORD_ALIGNMENT_BYTES, size_of::<usize>());
    let chunk = [
        raw_record(b".", 0, 10),
        raw_record(b"..", u64::MAX, 4),
        raw_record(b"alpha", 0, 10),
        raw_record(&[b'n', 0xff, b'x'], u64::MAX, u8::MAX),
    ]
    .concat();
    let inventory = parse(vec![chunk], ProductionRawDirectoryInventoryLimits::default()).unwrap();

    assert_eq!(inventory.len(), 2);
    assert!(!inventory.is_empty());
    assert_eq!(inventory.raw_name(0), Some(b"alpha".as_slice()));
    assert_eq!(inventory.raw_name(1), Some([b'n', 0xff, b'x'].as_slice()));
    assert_eq!(inventory.raw_name(2), None);
}

#[test]
fn complete_syscall_chunks_preserve_raw_record_order() {
    let inventory = parse(
        vec![raw_chunk(&[b"zeta", b"eta"]), raw_chunk(&[b"theta"])],
        ProductionRawDirectoryInventoryLimits::default(),
    )
    .unwrap();

    assert_eq!(inventory.raw_name(0), Some(b"zeta".as_slice()));
    assert_eq!(inventory.raw_name(1), Some(b"eta".as_slice()));
    assert_eq!(inventory.raw_name(2), Some(b"theta".as_slice()));
}

#[test]
fn truncated_headers_and_records_split_across_chunks_are_rejected() {
    let error = parse(
        vec![vec![0u8; RAW_NAME_OFFSET - 1]],
        ProductionRawDirectoryInventoryLimits::default(),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        ProductionRawDirectoryInventoryError::TruncatedRecordHeader { .. }
    ));

    let record = raw_record(b"split", 1, 8);
    let error = parse(
        vec![record[..record.len() - 1].to_vec(), record[record.len() - 1..].to_vec()],
        ProductionRawDirectoryInventoryLimits::default(),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        ProductionRawDirectoryInventoryError::RecordOverrun { .. }
    ));
}

#[test]
fn too_small_and_unaligned_record_lengths_are_rejected() {
    let mut too_small = raw_record(b"x", 1, 8);
    too_small[16..18].copy_from_slice(&19u16.to_ne_bytes());
    let error = parse(vec![too_small], ProductionRawDirectoryInventoryLimits::default()).unwrap_err();
    assert!(matches!(
        error,
        ProductionRawDirectoryInventoryError::RecordLengthTooSmall { .. }
    ));

    let mut unaligned = raw_record(b"x", 1, 8);
    unaligned[16..18].copy_from_slice(&21u16.to_ne_bytes());
    let error = parse(vec![unaligned], ProductionRawDirectoryInventoryLimits::default()).unwrap_err();
    assert!(matches!(
        error,
        ProductionRawDirectoryInventoryError::RecordLengthUnaligned { .. }
    ));
}

#[test]
fn missing_empty_overlong_and_slash_names_are_rejected() {
    let mut missing_nul = raw_record(b"name", 1, 8);
    missing_nul[RAW_NAME_OFFSET..].fill(b'x');
    assert!(matches!(
        parse(vec![missing_nul], ProductionRawDirectoryInventoryLimits::default()).unwrap_err(),
        ProductionRawDirectoryInventoryError::MissingNameTerminator { .. }
    ));

    let empty = raw_record(b"", 1, 8);
    assert!(matches!(
        parse(vec![empty], ProductionRawDirectoryInventoryLimits::default()).unwrap_err(),
        ProductionRawDirectoryInventoryError::EmptyName { .. }
    ));

    let overlong = raw_record(&vec![b'x'; 256], 1, 8);
    assert!(matches!(
        parse(vec![overlong], ProductionRawDirectoryInventoryLimits::default()).unwrap_err(),
        ProductionRawDirectoryInventoryError::NameTooLong { .. }
    ));

    let slash = raw_record(b"unsafe/name", 1, 8);
    assert!(matches!(
        parse(vec![slash], ProductionRawDirectoryInventoryLimits::default()).unwrap_err(),
        ProductionRawDirectoryInventoryError::NameContainsSlash { .. }
    ));
}

#[test]
fn source_failure_and_impossible_return_count_are_typed() {
    let (source, deadline) = FixtureRawDirectorySource::stable(vec![raw_chunk(&[b"entry"])]);
    let mut failed = source.fail_read_at(1);
    assert!(matches!(
        parse_production_raw_directory_inventory_until(
            &mut failed,
            ProductionRawDirectoryInventoryLimits::default(),
            deadline,
        )
        .unwrap_err(),
        ProductionRawDirectoryInventoryError::SourceFailed { .. }
    ));

    let (source, deadline) = FixtureRawDirectorySource::stable(Vec::new());
    let mut impossible = source.report_count(32 * 1024 + 1);
    assert_eq!(
        parse_production_raw_directory_inventory_until(
            &mut impossible,
            ProductionRawDirectoryInventoryLimits::default(),
            deadline,
        )
        .unwrap_err(),
        ProductionRawDirectoryInventoryError::SourceProtocolViolation {
            capacity: 32 * 1024,
            found: 32 * 1024 + 1,
        }
    );
}
