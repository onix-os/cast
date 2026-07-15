use std::thread;

use xxhash_rust::xxh3::{xxh3_64, xxh3_128};

use crate::{StoneHeaderV1, StoneHeaderV1FileType, StoneHeaderVersion, StonePayloadLayoutFile, StoneWriter};

use super::*;

/// Header for bash completion stone archive.
const BASH_TEST_STONE: [u8; 32] = [
    0x0, 0x6d, 0x6f, 0x73, 0x0, 0x4, 0x0, 0x0, 0x1, 0x0, 0x0, 0x2, 0x0, 0x0, 0x3, 0x0, 0x0, 0x4, 0x0, 0x0, 0x5, 0x0,
    0x0, 0x6, 0x0, 0x0, 0x7, 0x1, 0x0, 0x0, 0x0, 0x1,
];

#[test]
fn read_header() {
    let stone = read_bytes(&BASH_TEST_STONE).expect("valid stone");
    assert_eq!(stone.header.version(), StoneHeaderVersion::V1);
}

#[test]
fn read_bash_completion() {
    let mut stone = read_bytes(include_bytes!(
        "../../../../tests/fixtures/bash-completion-2.11-1-1-x86_64.stone"
    ))
    .expect("valid stone");
    assert_eq!(stone.header.version(), StoneHeaderVersion::V1);

    let payloads = stone
        .payloads()
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .expect("seek payloads");

    let mut unpacked_content = vec![];

    if let Some(content) = payloads.iter().find_map(StoneDecodedPayload::content) {
        stone
            .unpack_content(content, &mut unpacked_content)
            .expect("valid content");

        for index in payloads
            .iter()
            .filter_map(StoneDecodedPayload::index)
            .flat_map(|payload| &payload.body)
        {
            let content = &unpacked_content[index.start as usize..index.end as usize];
            let digest = xxh3_128(content);
            assert_eq!(digest, index.digest);

            payloads
                .iter()
                .filter_map(StoneDecodedPayload::layout)
                .flat_map(|payload| &payload.body)
                .find(|layout| {
                    if let StonePayloadLayoutFile::Regular(digest, _) = &layout.file {
                        return *digest == index.digest;
                    }
                    false
                })
                .expect("layout exists");
        }
    }
}

fn tiny_limits() -> StoneDecodeLimits {
    StoneDecodeLimits {
        max_payloads: 2,
        max_records_per_payload: 2,
        max_record_bytes: 64,
        max_stored_payload_bytes: 256,
        max_plain_payload_bytes: 256,
        max_total_records: 4,
        max_total_record_bytes: 256,
        max_total_stored_bytes: 512,
        max_total_plain_bytes: 512,
        max_zstd_window_log: 20,
    }
}

fn archive(payloads: &[(&StonePayloadHeader, &[u8])]) -> Vec<u8> {
    let mut bytes = vec![];
    StoneHeader::V1(StoneHeaderV1 {
        num_payloads: payloads.len() as u16,
        file_type: StoneHeaderV1FileType::Binary,
    })
    .encode(&mut bytes)
    .unwrap();
    for (header, body) in payloads {
        header.encode(&mut bytes).unwrap();
        bytes.extend_from_slice(body);
    }
    bytes
}

fn raw_header(kind: StonePayloadKind, body: &[u8], records: usize) -> StonePayloadHeader {
    StonePayloadHeader {
        stored_size: body.len() as u64,
        plain_size: body.len() as u64,
        checksum: xxh3_64(body).to_be_bytes(),
        num_records: records,
        version: 1,
        kind,
        compression: StonePayloadCompression::None,
    }
}

fn compressed_header(kind: StonePayloadKind, plain: &[u8], records: usize) -> (StonePayloadHeader, Vec<u8>) {
    let stored = ::zstd::stream::encode_all(Cursor::new(plain), 1).unwrap();
    (
        StonePayloadHeader {
            stored_size: stored.len() as u64,
            plain_size: plain.len() as u64,
            checksum: xxh3_64(&stored).to_be_bytes(),
            num_records: records,
            version: 1,
            kind,
            compression: StonePayloadCompression::Zstd,
        },
        stored,
    )
}

fn decode_all(bytes: &[u8], limits: StoneDecodeLimits) -> Result<Vec<StoneDecodedPayload>, StoneReadError> {
    let mut reader = read_bytes_with_limits(bytes, limits)?;
    reader.payloads()?.collect()
}

fn index_records(count: usize) -> Vec<u8> {
    vec![0; count * 32]
}

fn index_record(start: u64, end: u64) -> Vec<u8> {
    let mut record = vec![];
    record.extend_from_slice(&start.to_be_bytes());
    record.extend_from_slice(&end.to_be_bytes());
    record.extend_from_slice(&0u128.to_be_bytes());
    record
}

fn unknown_meta_record(value: &[u8]) -> Vec<u8> {
    let mut record = vec![];
    record.extend_from_slice(&(value.len() as u32).to_be_bytes());
    record.extend_from_slice(&1u16.to_be_bytes());
    record.push(255);
    record.push(0);
    record.extend_from_slice(value);
    record
}

fn encoded_content_archive(content: &[u8]) -> Vec<u8> {
    let mut encoded = vec![];
    let mut writer = StoneWriter::new(&mut encoded, StoneHeaderV1FileType::Binary)
        .unwrap()
        .with_content(
            Cursor::new(Vec::<u8>::new()),
            Some(content.len() as u64),
            thread::available_parallelism()
                .map(|workers| workers.get())
                .unwrap_or(1) as u32,
        )
        .unwrap();
    let mut source = content;
    writer.add_content(&mut source).unwrap();
    writer.finalize().unwrap();
    encoded
}

#[test]
fn payload_count_limit_accepts_n_and_rejects_n_plus_one() {
    let body = [];
    let header = raw_header(StonePayloadKind::Unknown, &body, 0);
    let accepted = archive(&[(&header, &body), (&header, &body)]);
    decode_all(&accepted, tiny_limits()).unwrap();

    let rejected = archive(&[(&header, &body), (&header, &body), (&header, &body)]);
    assert!(matches!(
        read_bytes_with_limits(&rejected, tiny_limits()),
        Err(StoneReadError::LimitExceeded {
            resource: "payload count",
            limit: 2,
            actual: 3
        })
    ));
}

#[test]
fn record_count_limit_accepts_n_and_rejects_n_plus_one() {
    let records = [0u8; 64];
    let accepted_header = raw_header(StonePayloadKind::Index, &records, 2);
    let accepted = archive(&[(&accepted_header, &records)]);
    read_bytes_with_limits(&accepted, tiny_limits())
        .unwrap()
        .payloads()
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();

    let rejected_header = raw_header(StonePayloadKind::Index, &records, 3);
    let rejected = archive(&[(&rejected_header, &records)]);
    let error = read_bytes_with_limits(&rejected, tiny_limits())
        .unwrap()
        .payloads()
        .unwrap()
        .next()
        .unwrap()
        .unwrap_err();
    assert!(matches!(
        error,
        StoneReadError::LimitExceeded {
            resource: "records per payload",
            limit: 2,
            actual: 3
        }
    ));
}

#[test]
fn record_byte_limit_accepts_n_and_rejects_n_plus_one_before_allocation() {
    let accepted_body = unknown_meta_record(&[0; 4]);
    let accepted_header = raw_header(StonePayloadKind::Meta, &accepted_body, 1);
    let mut limits = tiny_limits();
    limits.max_record_bytes = 12;
    decode_all(&archive(&[(&accepted_header, &accepted_body)]), limits).unwrap();

    let rejected_body = unknown_meta_record(&[0; 5]);
    let rejected_header = raw_header(StonePayloadKind::Meta, &rejected_body, 1);
    let error = decode_all(&archive(&[(&rejected_header, &rejected_body)]), limits).unwrap_err();
    assert!(matches!(
        error,
        StoneReadError::PayloadDecode(StonePayloadDecodeError::LimitExceeded {
            field: "metadata primitive",
            limit: 12,
            actual: 13
        })
    ));
}

#[test]
fn stored_and_plain_payload_limits_accept_n_and_reject_n_plus_one() {
    let accepted_body = vec![0; 256];
    let accepted_header = raw_header(StonePayloadKind::Unknown, &accepted_body, 0);
    decode_all(&archive(&[(&accepted_header, &accepted_body)]), tiny_limits()).unwrap();

    let rejected_body = vec![0; 257];
    let rejected_header = raw_header(StonePayloadKind::Unknown, &rejected_body, 0);
    let error = decode_all(&archive(&[(&rejected_header, &rejected_body)]), tiny_limits()).unwrap_err();
    assert!(matches!(
        error,
        StoneReadError::LimitExceeded {
            resource: "stored payload bytes",
            limit: 256,
            actual: 257
        }
    ));

    let mut plain_header = raw_header(StonePayloadKind::Unknown, &[], 0);
    plain_header.compression = StonePayloadCompression::Zstd;
    plain_header.plain_size = 257;
    let error = decode_all(&archive(&[(&plain_header, &[])]), tiny_limits()).unwrap_err();
    assert!(matches!(
        error,
        StoneReadError::LimitExceeded {
            resource: "plain payload bytes",
            limit: 256,
            actual: 257
        }
    ));
}

#[test]
fn aggregate_stored_plain_record_and_count_limits_are_enforced_at_n_plus_one() {
    let mut limits = tiny_limits();
    limits.max_stored_payload_bytes = 512;
    limits.max_plain_payload_bytes = 512;
    limits.max_records_per_payload = 3;
    limits.max_total_records = 4;
    limits.max_total_record_bytes = 128;

    let first_body = index_records(2);
    let first_header = raw_header(StonePayloadKind::Index, &first_body, 2);
    let second_body = index_records(2);
    let second_header = raw_header(StonePayloadKind::Index, &second_body, 2);
    decode_all(
        &archive(&[(&first_header, &first_body), (&second_header, &second_body)]),
        limits,
    )
    .unwrap();

    let too_many_body = index_records(3);
    let too_many_header = raw_header(StonePayloadKind::Index, &too_many_body, 3);
    let error = decode_all(
        &archive(&[(&first_header, &first_body), (&too_many_header, &too_many_body)]),
        limits,
    )
    .unwrap_err();
    assert!(matches!(
        error,
        StoneReadError::LimitExceeded {
            resource: "aggregate record count",
            limit: 4,
            actual: 5
        }
    ));

    let mut record_bytes_header = second_header;
    record_bytes_header.plain_size += 1;
    record_bytes_header.stored_size += 1;
    let mut record_bytes_body = second_body.clone();
    record_bytes_body.push(0);
    record_bytes_header.checksum = xxh3_64(&record_bytes_body).to_be_bytes();
    let error = decode_all(
        &archive(&[(&first_header, &first_body), (&record_bytes_header, &record_bytes_body)]),
        limits,
    )
    .unwrap_err();
    assert!(matches!(
        error,
        StoneReadError::LimitExceeded {
            resource: "aggregate record bytes",
            limit: 128,
            actual: 129
        }
    ));

    let mut byte_limits = limits;
    byte_limits.max_payloads = 3;
    byte_limits.max_total_records = 6;
    byte_limits.max_total_record_bytes = 512;
    byte_limits.max_total_stored_bytes = 512;
    byte_limits.max_total_plain_bytes = 512;
    let first = vec![0; 256];
    let first_header = raw_header(StonePayloadKind::Unknown, &first, 0);
    let second = vec![0; 256];
    let second_header = raw_header(StonePayloadKind::Unknown, &second, 0);
    decode_all(
        &archive(&[(&first_header, &first), (&second_header, &second)]),
        byte_limits,
    )
    .unwrap();

    let mut plus_one = second.clone();
    plus_one.push(0);
    let plus_one_header = raw_header(StonePayloadKind::Unknown, &plus_one, 0);
    let error = decode_all(
        &archive(&[(&first_header, &first), (&plus_one_header, &plus_one)]),
        byte_limits,
    )
    .unwrap_err();
    assert!(matches!(
        error,
        StoneReadError::LimitExceeded {
            resource: "aggregate stored payload bytes",
            limit: 512,
            actual: 513
        }
    ));

    let mut plain_first = raw_header(StonePayloadKind::Unknown, &[], 0);
    plain_first.compression = StonePayloadCompression::Unknown;
    plain_first.plain_size = 256;
    let mut plain_second = plain_first;
    plain_second.plain_size = 257;
    let error = decode_all(&archive(&[(&plain_first, &[]), (&plain_second, &[])]), byte_limits).unwrap_err();
    assert!(matches!(
        error,
        StoneReadError::LimitExceeded {
            resource: "aggregate plain payload bytes",
            limit: 512,
            actual: 513
        }
    ));
}

#[test]
fn zstd_plain_size_must_match_exact_expansion() {
    let body = unknown_meta_record(b"x");
    let (header, stored) = compressed_header(StonePayloadKind::Meta, &body, 1);
    decode_all(&archive(&[(&header, &stored)]), tiny_limits()).unwrap();

    let mut too_small = header;
    too_small.plain_size -= 1;
    let error = decode_all(&archive(&[(&too_small, &stored)]), tiny_limits()).unwrap_err();
    assert!(matches!(error, StoneReadError::PlainPayloadTooLarge { declared: 8 }));

    let mut too_large = header;
    too_large.plain_size += 1;
    let error = decode_all(&archive(&[(&too_large, &stored)]), tiny_limits()).unwrap_err();
    assert!(matches!(
        error,
        StoneReadError::PlainPayloadTruncated {
            declared: 10,
            actual: 9
        }
    ));
}

#[test]
fn malformed_metadata_and_layout_lengths_are_rejected_without_panics() {
    let mut zero_dependency = vec![];
    zero_dependency.extend_from_slice(&0u32.to_be_bytes());
    zero_dependency.extend_from_slice(&8u16.to_be_bytes());
    zero_dependency.push(10);
    zero_dependency.push(0);
    let header = raw_header(StonePayloadKind::Meta, &zero_dependency, 1);
    let error = decode_all(&archive(&[(&header, &zero_dependency)]), tiny_limits()).unwrap_err();
    assert!(matches!(
        error,
        StoneReadError::PayloadDecode(StonePayloadDecodeError::LengthTooSmall {
            field: "metadata dependency/provider",
            minimum: 1,
            actual: 0
        })
    ));

    let mut bad_layout = vec![0; 16];
    bad_layout.extend_from_slice(&15u16.to_be_bytes());
    bad_layout.extend_from_slice(&0u16.to_be_bytes());
    bad_layout.push(1);
    bad_layout.extend_from_slice(&[0; 11]);
    let header = raw_header(StonePayloadKind::Layout, &bad_layout, 1);
    let error = decode_all(&archive(&[(&header, &bad_layout)]), tiny_limits()).unwrap_err();
    assert!(matches!(
        error,
        StoneReadError::PayloadDecode(StonePayloadDecodeError::InvalidLength {
            field: "regular layout digest",
            expected: 16,
            actual: 15
        })
    ));
}

#[test]
fn malformed_or_out_of_bounds_content_indices_are_rejected() {
    let backwards = index_record(2, 1);
    let index_header = raw_header(StonePayloadKind::Index, &backwards, 1);
    let error = decode_all(&archive(&[(&index_header, &backwards)]), tiny_limits()).unwrap_err();
    assert!(matches!(error, StoneReadError::InvalidIndexRange { start: 2, end: 1 }));

    let outside = index_record(0, 4);
    let index_header = raw_header(StonePayloadKind::Index, &outside, 1);
    let content = b"abc";
    let content_header = raw_header(StonePayloadKind::Content, content, 0);
    let error = decode_all(
        &archive(&[(&index_header, &outside), (&content_header, content)]),
        tiny_limits(),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        StoneReadError::IndexOutsideContent {
            end: 4,
            content_size: 3
        }
    ));
}

#[test]
fn huge_declared_attribute_length_fails_before_allocation() {
    let mut body = vec![];
    body.extend_from_slice(&u64::MAX.to_be_bytes());
    body.extend_from_slice(&0u64.to_be_bytes());
    let header = raw_header(StonePayloadKind::Attributes, &body, 1);
    let error = decode_all(&archive(&[(&header, &body)]), tiny_limits()).unwrap_err();
    assert!(matches!(
        error,
        StoneReadError::PayloadDecode(StonePayloadDecodeError::LengthOverflow {
            field: "attribute key and value"
        })
    ));
}

#[test]
fn exact_length_strings_reject_truncation() {
    let mut body = vec![];
    body.extend_from_slice(&4u32.to_be_bytes());
    body.extend_from_slice(&1u16.to_be_bytes());
    body.push(9);
    body.push(0);
    body.extend_from_slice(b"abc");
    let header = raw_header(StonePayloadKind::Meta, &body, 1);
    let error = decode_all(&archive(&[(&header, &body)]), tiny_limits()).unwrap_err();
    assert!(matches!(
        error,
        StoneReadError::PayloadDecode(StonePayloadDecodeError::Io(ref source))
            if source.kind() == io::ErrorKind::UnexpectedEof
    ));
}

#[test]
fn record_payload_trailing_bytes_are_rejected_without_panicking() {
    let mut body = index_record(0, 0);
    body.push(0);
    let header = raw_header(StonePayloadKind::Index, &body, 1);

    let error = decode_all(&archive(&[(&header, &body)]), tiny_limits()).unwrap_err();

    assert!(matches!(
        error,
        StoneReadError::PlainPayloadUnconsumed {
            consumed: 32,
            declared: 33
        }
    ));
}

#[test]
fn declared_payload_header_is_never_silently_truncated() {
    let bytes = archive(&[]);
    let mut forged = bytes;
    forged[4..6].copy_from_slice(&1u16.to_be_bytes());
    let error = decode_all(&forged, tiny_limits()).unwrap_err();
    assert!(matches!(
        error,
        StoneReadError::PayloadDecode(StonePayloadDecodeError::Io(ref source))
            if source.kind() == io::ErrorKind::UnexpectedEof
    ));
}

#[test]
fn multiple_content_payloads_are_rejected() {
    let body = b"content";
    let header = raw_header(StonePayloadKind::Content, body, 0);
    let error = decode_all(&archive(&[(&header, body), (&header, body)]), tiny_limits()).unwrap_err();
    assert!(matches!(error, StoneReadError::MultipleContent));
}

#[test]
fn unknown_payloads_are_skipped_with_exact_checksum_validation() {
    let body = b"opaque";
    let header = raw_header(StonePayloadKind::Unknown, body, 0);
    let payloads = decode_all(&archive(&[(&header, body)]), tiny_limits()).unwrap();
    assert!(matches!(payloads.as_slice(), [StoneDecodedPayload::Unknown(_)]));

    let mut unknown_compression = header;
    unknown_compression.compression = StonePayloadCompression::Unknown;
    let payloads = decode_all(&archive(&[(&unknown_compression, body)]), tiny_limits()).unwrap();
    assert!(matches!(
        payloads.as_slice(),
        [StoneDecodedPayload::UnknownCompression(_)]
    ));

    unknown_compression.checksum = 0u64.to_be_bytes();
    let error = decode_all(&archive(&[(&unknown_compression, body)]), tiny_limits()).unwrap_err();
    assert!(matches!(error, StoneReadError::PayloadChecksum { .. }));
}

#[test]
fn trailing_bytes_and_truncated_payload_are_rejected() {
    let body = b"body";
    let header = raw_header(StonePayloadKind::Unknown, body, 0);

    let mut trailing = archive(&[(&header, body)]);
    trailing.push(0);
    let error = read_bytes_with_limits(&trailing, tiny_limits())
        .unwrap()
        .payloads()
        .unwrap()
        .next()
        .unwrap()
        .unwrap_err();
    assert!(matches!(error, StoneReadError::TrailingOrChangedArchive { .. }));

    let mut truncated_header = header;
    truncated_header.stored_size += 1;
    truncated_header.plain_size += 1;
    let truncated = archive(&[(&truncated_header, body)]);
    let error = read_bytes_with_limits(&truncated, tiny_limits())
        .unwrap()
        .payloads()
        .unwrap()
        .next()
        .unwrap()
        .unwrap_err();
    assert!(matches!(error, StoneReadError::PayloadBodyOutOfBounds { .. }));

    let mut empty_with_trailing = archive(&[]);
    empty_with_trailing.push(0);
    let error = read_bytes_with_limits(&empty_with_trailing, tiny_limits())
        .err()
        .unwrap();
    assert!(matches!(error, StoneReadError::TrailingOrChangedArchive { .. }));
}

struct EndOnlyReader {
    end: u64,
}

impl Read for EndOnlyReader {
    fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
        panic!("oversized archive must be rejected before it is read")
    }
}

impl Seek for EndOnlyReader {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        match position {
            SeekFrom::End(0) => Ok(self.end),
            _ => panic!("oversized archive must be rejected before an offset seek"),
        }
    }
}

#[test]
fn huge_sparse_archive_and_limit_arithmetic_fail_before_offset_seeks() {
    let limits = tiny_limits();
    let maximum = max_archive_bytes(&limits).unwrap();
    let error = read_with_limits(EndOnlyReader { end: maximum + 1 }, limits)
        .err()
        .unwrap();
    assert!(matches!(
        error,
        StoneReadError::LimitExceeded {
            resource: "archive bytes",
            limit,
            actual
        } if limit == maximum && actual == maximum + 1
    ));

    let unrepresentable = StoneDecodeLimits {
        max_payloads: u64::MAX,
        max_records_per_payload: u64::MAX,
        max_record_bytes: u64::MAX,
        max_stored_payload_bytes: u64::MAX,
        max_plain_payload_bytes: u64::MAX,
        max_total_records: u64::MAX,
        max_total_record_bytes: u64::MAX,
        max_total_stored_bytes: u64::MAX,
        max_total_plain_bytes: u64::MAX,
        max_zstd_window_log: 31,
    };
    let error = read_bytes_with_limits(&[], unrepresentable).err().unwrap();
    assert!(matches!(error, StoneReadError::ArithmeticOverflow(_)));

    let unsafe_offsets = StoneDecodeLimits {
        max_payloads: 0,
        max_records_per_payload: 0,
        max_record_bytes: 0,
        max_stored_payload_bytes: i64::MAX as u64,
        max_plain_payload_bytes: 0,
        max_total_records: 0,
        max_total_record_bytes: 0,
        max_total_stored_bytes: i64::MAX as u64,
        max_total_plain_bytes: 0,
        max_zstd_window_log: 20,
    };
    let error = read_bytes_with_limits(&[], unsafe_offsets).err().unwrap();
    assert!(matches!(
        error,
        StoneReadError::LimitExceeded {
            resource: "configured maximum archive bytes",
            limit,
            actual
        } if limit == i64::MAX as u64 && actual > limit
    ));
}

#[test]
fn content_output_never_exceeds_declared_plain_size() {
    let content = b"N plus one";
    let encoded = encoded_content_archive(content);

    // Patch the content payload's declared plain size down by one.
    let mut reader = read_bytes(&encoded).unwrap();
    let payloads = reader.payloads().unwrap().collect::<Result<Vec<_>, _>>().unwrap();
    let content_payload = payloads.iter().find_map(StoneDecodedPayload::content).unwrap();

    let mut exact_output = vec![];
    reader.unpack_content(content_payload, &mut exact_output).unwrap();
    assert_eq!(exact_output, content);

    let mut forged = content_payload.clone();
    forged.header.plain_size -= 1;
    let mut output = vec![];
    let error = reader.unpack_content(&forged, &mut output).unwrap_err();
    assert!(matches!(error, StoneReadError::PlainPayloadTooLarge { .. }));
    assert_eq!(output.len() as u64, forged.header.plain_size);

    let mut forged = content_payload.clone();
    forged.header.plain_size += 1;
    let mut output = vec![];
    let error = reader.unpack_content(&forged, &mut output).unwrap_err();
    assert!(matches!(
        error,
        StoneReadError::PlainPayloadTruncated {
            declared,
            actual
        } if declared == content.len() as u64 + 1 && actual == content.len() as u64
    ));
    assert_eq!(output, content);

    let raw_content_header = raw_header(StonePayloadKind::Content, content, 0);
    let raw_content = archive(&[(&raw_content_header, content)]);
    let mut limits = tiny_limits();
    limits.max_plain_payload_bytes = content.len() as u64;
    decode_all(&raw_content, limits).unwrap();
    limits.max_plain_payload_bytes -= 1;
    let error = decode_all(&raw_content, limits).unwrap_err();
    assert!(matches!(
        error,
        StoneReadError::LimitExceeded {
            resource: "plain payload bytes",
            limit,
            actual
        } if limit + 1 == actual && actual == content.len() as u64
    ));
}

#[cfg(feature = "ffi")]
#[test]
fn ffi_content_stream_is_bounded_and_validates_checksum_before_eof() {
    let content = b"streamed content";
    let encoded = encoded_content_archive(content);
    let mut reader = read_bytes(&encoded).unwrap();
    let payloads = reader.payloads().unwrap().collect::<Result<Vec<_>, _>>().unwrap();
    let content_payload = payloads.iter().find_map(StoneDecodedPayload::content).unwrap();

    let mut stream = reader.read_content(content_payload).unwrap();
    let mut output = vec![];
    stream.read_to_end(&mut output).unwrap();
    assert_eq!(output, content);
    assert!(stream.is_checksum_valid);
    drop(stream);

    let mut forged = content_payload.clone();
    forged.header.plain_size -= 1;
    let mut stream = reader.read_content(&forged).unwrap();
    let mut output = vec![];
    let error = stream.read_to_end(&mut output).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert_eq!(output.len() as u64, forged.header.plain_size);
    assert!(!stream.is_checksum_valid);
    drop(stream);

    let mut forged = content_payload.clone();
    forged.header.checksum = 0u64.to_be_bytes();
    let mut stream = reader.read_content(&forged).unwrap();
    let mut output = vec![];
    let error = stream.read_to_end(&mut output).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert_eq!(output, content);
    assert!(!stream.is_checksum_valid);
}
