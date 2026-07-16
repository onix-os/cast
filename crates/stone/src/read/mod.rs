#![allow(dead_code)]

use std::io::{self, BufReader, Cursor, Read, Seek, SeekFrom, Write};

use thiserror::Error;

use crate::{
    StoneHeader, StoneHeaderDecodeError, StonePayload, StonePayloadAttributeRecord, StonePayloadCompression,
    StonePayloadContent, StonePayloadDecodeError, StonePayloadHeader, StonePayloadIndexRecord, StonePayloadKind,
    StonePayloadLayoutRecord, StonePayloadMetaRecord, payload,
};

use self::content_reader::{ExactPlainReader, PayloadReader, drain_raw};
use self::zstd::Zstd;

mod content_reader;
mod digest;
mod zstd;

#[cfg(feature = "ffi")]
pub use self::content_reader::StonePayloadContentReader;

const MIB: u64 = 1024 * 1024;
const GIB: u64 = 1024 * MIB;

/// Resource ceilings applied while decoding an untrusted `.stone` archive.
///
/// The defaults are intentionally generous enough for large binary packages,
/// but every declaration is checked before allocation, seeking, or
/// decompression. Applications processing smaller artifacts should lower these
/// values and use [`read_with_limits`] or [`read_bytes_with_limits`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoneDecodeLimits {
    pub max_payloads: u64,
    pub max_records_per_payload: u64,
    pub max_record_bytes: u64,
    pub max_stored_payload_bytes: u64,
    pub max_plain_payload_bytes: u64,
    pub max_total_records: u64,
    pub max_total_record_bytes: u64,
    pub max_total_stored_bytes: u64,
    pub max_total_plain_bytes: u64,
    /// Maximum zstd window-log accepted from a frame (30 = 1 GiB).
    pub max_zstd_window_log: u32,
}

impl Default for StoneDecodeLimits {
    fn default() -> Self {
        Self {
            max_payloads: 1_024,
            max_records_per_payload: 4_000_000,
            max_record_bytes: 64 * MIB,
            max_stored_payload_bytes: 8 * GIB,
            max_plain_payload_bytes: 32 * GIB,
            max_total_records: 8_000_000,
            max_total_record_bytes: 48 * GIB,
            max_total_stored_bytes: 16 * GIB,
            max_total_plain_bytes: 64 * GIB,
            max_zstd_window_log: 30,
        }
    }
}

/// Read a `.stone` archive with secure default resource ceilings.
pub fn read<R: Read + Seek>(reader: R) -> Result<StoneReader<R>, StoneReadError> {
    read_with_limits(reader, StoneDecodeLimits::default())
}

/// Read a `.stone` archive with caller-provided resource ceilings.
pub fn read_with_limits<R: Read + Seek>(
    mut reader: R,
    limits: StoneDecodeLimits,
) -> Result<StoneReader<R>, StoneReadError> {
    validate_limits(&limits)?;

    let archive_end = reader.seek(SeekFrom::End(0))?;
    let max_archive_bytes = max_archive_bytes(&limits)?;
    enforce_limit("configured maximum archive bytes", i64::MAX as u64, max_archive_bytes)?;
    enforce_limit("archive bytes", max_archive_bytes, archive_end)?;

    reader.seek(SeekFrom::Start(0))?;
    let header = StoneHeader::decode(&mut reader).map_err(StoneReadError::HeaderDecode)?;
    enforce_limit("payload count", limits.max_payloads, u64::from(header.num_payloads()))?;
    if header.num_payloads() == 0 {
        verify_archive_end(&mut reader, archive_end)?;
    }

    Ok(StoneReader {
        header,
        reader,
        hasher: digest::Hasher::new(),
        limits,
        archive_end,

        #[cfg(feature = "ffi")]
        next_payload: 0,
        #[cfg(feature = "ffi")]
        next_state: DecodeState::default(),
        #[cfg(feature = "ffi")]
        next_failed: false,
    })
}

/// Read archive bytes with secure default resource ceilings.
pub fn read_bytes(bytes: &[u8]) -> Result<StoneReader<Cursor<&[u8]>>, StoneReadError> {
    read_bytes_with_limits(bytes, StoneDecodeLimits::default())
}

/// Read archive bytes with caller-provided resource ceilings.
pub fn read_bytes_with_limits(
    bytes: &[u8],
    limits: StoneDecodeLimits,
) -> Result<StoneReader<Cursor<&[u8]>>, StoneReadError> {
    read_with_limits(Cursor::new(bytes), limits)
}

fn validate_limits(limits: &StoneDecodeLimits) -> Result<(), StoneReadError> {
    if !(10..=31).contains(&limits.max_zstd_window_log) {
        return Err(StoneReadError::InvalidLimits(
            "max_zstd_window_log must be between 10 and 31",
        ));
    }

    for (resource, per_payload, total) in [
        (
            "stored payload bytes",
            limits.max_stored_payload_bytes,
            limits.max_total_stored_bytes,
        ),
        (
            "plain payload bytes",
            limits.max_plain_payload_bytes,
            limits.max_total_plain_bytes,
        ),
    ] {
        if per_payload > total {
            return Err(StoneReadError::InvalidLimitRelationship {
                resource,
                per_payload,
                total,
            });
        }
    }

    if limits.max_record_bytes > limits.max_total_record_bytes {
        return Err(StoneReadError::InvalidLimitRelationship {
            resource: "record bytes",
            per_payload: limits.max_record_bytes,
            total: limits.max_total_record_bytes,
        });
    }

    if limits.max_records_per_payload > limits.max_total_records {
        return Err(StoneReadError::InvalidLimitRelationship {
            resource: "record count",
            per_payload: limits.max_records_per_payload,
            total: limits.max_total_records,
        });
    }

    Ok(())
}

fn max_archive_bytes(limits: &StoneDecodeLimits) -> Result<u64, StoneReadError> {
    let payload_headers = limits
        .max_payloads
        .checked_mul(StonePayloadHeader::SIZE as u64)
        .ok_or(StoneReadError::ArithmeticOverflow("maximum payload-header bytes"))?;
    (StoneHeader::SIZE as u64)
        .checked_add(payload_headers)
        .and_then(|bytes| bytes.checked_add(limits.max_total_stored_bytes))
        .ok_or(StoneReadError::ArithmeticOverflow("maximum archive bytes"))
}

pub struct StoneReader<R> {
    pub header: StoneHeader,
    reader: R,
    hasher: digest::Hasher,
    limits: StoneDecodeLimits,
    archive_end: u64,

    #[cfg(feature = "ffi")]
    next_payload: u16,
    #[cfg(feature = "ffi")]
    next_state: DecodeState,
    #[cfg(feature = "ffi")]
    next_failed: bool,
}

impl<R: Read + Seek> StoneReader<R> {
    pub fn limits(&self) -> &StoneDecodeLimits {
        &self.limits
    }

    pub fn payloads(
        &mut self,
    ) -> Result<impl Iterator<Item = Result<StoneDecodedPayload, StoneReadError>> + '_, StoneReadError> {
        self.reader.seek(SeekFrom::Start(StoneHeader::SIZE as u64))?;

        if self.header.num_payloads() == 0 {
            verify_archive_end(&mut self.reader, self.archive_end)?;
        }

        #[cfg(feature = "ffi")]
        {
            // Iteration and the stateful FFI cursor must not decode the same
            // payload stream independently.
            self.next_payload = self.header.num_payloads();
        }

        Ok(StonePayloadIterator {
            reader: &mut self.reader,
            hasher: &mut self.hasher,
            limits: self.limits,
            archive_end: self.archive_end,
            remaining: self.header.num_payloads(),
            state: DecodeState::default(),
            failed: false,
        })
    }

    pub fn unpack_content<W>(
        &mut self,
        content: &StonePayload<StonePayloadContent>,
        writer: &mut W,
    ) -> Result<(), StoneReadError>
    where
        W: Write,
    {
        self.validate_content_reference(content)?;
        self.reader.seek(SeekFrom::Start(content.body.offset))?;
        self.hasher.reset();

        let hashed = digest::Reader::new((&mut self.reader).take(content.header.stored_size), &mut self.hasher);
        let framed = PayloadReader::new(hashed, content.header.compression, self.limits.max_zstd_window_log)?;
        let mut plain = ExactPlainReader::new(framed, content.header.plain_size);

        let copy_result = io::copy(&mut plain, writer);
        if let Some(error) = plain.size_error() {
            return Err(error);
        }
        copy_result?;
        plain.finish_exact()?;

        let (remaining, got) = drain_raw(plain.into_inner().into_raw())?;
        if remaining != 0 {
            return Err(StoneReadError::StoredPayloadTruncated {
                declared: content.header.stored_size,
                actual: content.header.stored_size - remaining,
            });
        }
        validate_checksum_value(got, &content.header)?;

        Ok(())
    }

    fn validate_content_reference(&self, content: &StonePayload<StonePayloadContent>) -> Result<(), StoneReadError> {
        if content.header.kind != StonePayloadKind::Content {
            return Err(StoneReadError::InvalidContentReference("payload kind is not content"));
        }
        if content.header.version != 1 {
            return Err(StoneReadError::UnsupportedPayloadVersion(content.header.version));
        }
        if content.header.compression == StonePayloadCompression::Unknown {
            return Err(StoneReadError::UnknownCompression);
        }
        enforce_limit(
            "stored payload bytes",
            self.limits.max_stored_payload_bytes,
            content.header.stored_size,
        )?;
        enforce_limit(
            "plain payload bytes",
            self.limits.max_plain_payload_bytes,
            content.header.plain_size,
        )?;

        let body_end = content
            .body
            .offset
            .checked_add(content.header.stored_size)
            .ok_or(StoneReadError::ArithmeticOverflow("content body end"))?;
        if content.body.offset < StoneHeader::SIZE as u64 || body_end > self.archive_end {
            return Err(StoneReadError::PayloadBodyOutOfBounds {
                offset: content.body.offset,
                stored_size: content.header.stored_size,
                archive_size: self.archive_end,
            });
        }

        Ok(())
    }
}

struct StonePayloadIterator<'a, R> {
    reader: &'a mut R,
    hasher: &'a mut digest::Hasher,
    limits: StoneDecodeLimits,
    archive_end: u64,
    remaining: u16,
    state: DecodeState,
    failed: bool,
}

impl<R: Read + Seek> Iterator for StonePayloadIterator<'_, R> {
    type Item = Result<StoneDecodedPayload, StoneReadError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed || self.remaining == 0 {
            return None;
        }

        let decoded = StoneDecodedPayload::decode(
            &mut self.reader,
            self.hasher,
            &self.limits,
            self.archive_end,
            &mut self.state,
        );
        self.remaining -= 1;

        let result = decoded.and_then(|payload| {
            if self.remaining == 0 {
                verify_archive_end(&mut self.reader, self.archive_end)?;
            }
            Ok(payload)
        });

        if result.is_err() {
            self.failed = true;
        }
        Some(result)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        if self.failed {
            return (0, Some(0));
        }
        let remaining = usize::from(self.remaining);
        (remaining, Some(remaining))
    }
}

#[cfg(feature = "ffi")]
impl<R: Read + Seek> StoneReader<R> {
    pub fn next_payload(&mut self) -> Result<Option<StoneDecodedPayload>, StoneReadError> {
        if self.next_failed {
            return Err(StoneReadError::DecoderPoisoned);
        }
        if self.next_payload >= self.header.num_payloads() {
            return Ok(None);
        }

        let payload = match StoneDecodedPayload::decode(
            &mut self.reader,
            &mut self.hasher,
            &self.limits,
            self.archive_end,
            &mut self.next_state,
        ) {
            Ok(payload) => payload,
            Err(error) => {
                self.next_failed = true;
                return Err(error);
            }
        };
        self.next_payload += 1;

        if self.next_payload == self.header.num_payloads()
            && let Err(error) = verify_archive_end(&mut self.reader, self.archive_end)
        {
            self.next_failed = true;
            return Err(error);
        }

        Ok(Some(payload))
    }

    pub fn read_content<'a>(
        &'a mut self,
        content: &StonePayload<StonePayloadContent>,
    ) -> Result<StonePayloadContentReader<'a, R>, StoneReadError> {
        self.validate_content_reference(content)?;
        self.reader.seek(SeekFrom::Start(content.body.offset))?;
        self.hasher.reset();

        let hashed = digest::Reader::new((&mut self.reader).take(content.header.stored_size), &mut self.hasher);
        let framed = PayloadReader::new(hashed, content.header.compression, self.limits.max_zstd_window_log)?;
        let buf_hint = framed.buf_hint();

        Ok(StonePayloadContentReader {
            reader: Some(ExactPlainReader::new(framed, content.header.plain_size)),
            header_checksum: u64::from_be_bytes(content.header.checksum),
            stored_size: content.header.stored_size,
            is_checksum_valid: false,
            buf_hint,
        })
    }
}

#[derive(Debug, Default)]
struct DecodeState {
    records: u64,
    record_bytes: u64,
    stored_bytes: u64,
    plain_bytes: u64,
    content_seen: bool,
    content_plain_size: Option<u64>,
    max_index_end: u64,
}

impl DecodeState {
    fn admit(&mut self, header: &StonePayloadHeader, limits: &StoneDecodeLimits) -> Result<(), StoneReadError> {
        if header.kind != StonePayloadKind::Unknown && header.version != 1 {
            return Err(StoneReadError::UnsupportedPayloadVersion(header.version));
        }

        enforce_limit(
            "records per payload",
            limits.max_records_per_payload,
            header.num_records as u64,
        )?;
        enforce_limit(
            "stored payload bytes",
            limits.max_stored_payload_bytes,
            header.stored_size,
        )?;
        enforce_limit("plain payload bytes", limits.max_plain_payload_bytes, header.plain_size)?;

        self.records = checked_total(
            "aggregate record count",
            self.records,
            header.num_records as u64,
            limits.max_total_records,
        )?;
        self.stored_bytes = checked_total(
            "aggregate stored payload bytes",
            self.stored_bytes,
            header.stored_size,
            limits.max_total_stored_bytes,
        )?;
        self.plain_bytes = checked_total(
            "aggregate plain payload bytes",
            self.plain_bytes,
            header.plain_size,
            limits.max_total_plain_bytes,
        )?;

        if is_record_payload(header.kind) {
            self.record_bytes = checked_total(
                "aggregate record bytes",
                self.record_bytes,
                header.plain_size,
                limits.max_total_record_bytes,
            )?;
            validate_minimum_record_bytes(header)?;
        }

        if header.kind == StonePayloadKind::Content {
            if self.content_seen {
                return Err(StoneReadError::MultipleContent);
            }
            self.content_seen = true;
            self.content_plain_size = Some(header.plain_size);
            if self.max_index_end > header.plain_size {
                return Err(StoneReadError::IndexOutsideContent {
                    end: self.max_index_end,
                    content_size: header.plain_size,
                });
            }
        }

        if header.compression == StonePayloadCompression::None && header.stored_size != header.plain_size {
            return Err(StoneReadError::PlainPayloadSizeMismatch {
                stored: header.stored_size,
                plain: header.plain_size,
            });
        }

        Ok(())
    }

    fn admit_indices(&mut self, records: &[StonePayloadIndexRecord]) -> Result<(), StoneReadError> {
        for record in records {
            if record.end < record.start {
                return Err(StoneReadError::InvalidIndexRange {
                    start: record.start,
                    end: record.end,
                });
            }
            self.max_index_end = self.max_index_end.max(record.end);
            if let Some(content_size) = self.content_plain_size
                && record.end > content_size
            {
                return Err(StoneReadError::IndexOutsideContent {
                    end: record.end,
                    content_size,
                });
            }
        }
        Ok(())
    }
}

fn validate_minimum_record_bytes(header: &StonePayloadHeader) -> Result<(), StoneReadError> {
    let minimum_per_record = match header.kind {
        StonePayloadKind::Meta => 8,
        StonePayloadKind::Layout => 32,
        StonePayloadKind::Index => 32,
        StonePayloadKind::Attributes => 16,
        StonePayloadKind::Content | StonePayloadKind::Unknown => return Ok(()),
    };
    let minimum = (header.num_records as u64)
        .checked_mul(minimum_per_record)
        .ok_or(StoneReadError::ArithmeticOverflow("minimum record bytes"))?;
    if minimum > header.plain_size {
        return Err(StoneReadError::ImpossibleRecordDeclaration {
            records: header.num_records as u64,
            minimum_bytes: minimum,
            plain_size: header.plain_size,
        });
    }
    Ok(())
}

fn is_record_payload(kind: StonePayloadKind) -> bool {
    matches!(
        kind,
        StonePayloadKind::Meta | StonePayloadKind::Layout | StonePayloadKind::Index | StonePayloadKind::Attributes
    )
}

#[derive(Debug)]
pub enum StoneDecodedPayload {
    Meta(StonePayload<Vec<StonePayloadMetaRecord>>),
    Attributes(StonePayload<Vec<StonePayloadAttributeRecord>>),
    Layout(StonePayload<Vec<StonePayloadLayoutRecord>>),
    Index(StonePayload<Vec<StonePayloadIndexRecord>>),
    Content(StonePayload<StonePayloadContent>),

    /// Payload type not known / supported by this decoder.
    Unknown(StonePayload<()>),
    /// Payload compression type not known / supported by this decoder.
    UnknownCompression(StonePayload<()>),
}

impl StoneDecodedPayload {
    pub fn header(&self) -> &StonePayloadHeader {
        match self {
            StoneDecodedPayload::Meta(payload) => &payload.header,
            StoneDecodedPayload::Attributes(payload) => &payload.header,
            StoneDecodedPayload::Layout(payload) => &payload.header,
            StoneDecodedPayload::Index(payload) => &payload.header,
            StoneDecodedPayload::Content(payload) => &payload.header,
            StoneDecodedPayload::Unknown(payload) => &payload.header,
            StoneDecodedPayload::UnknownCompression(payload) => &payload.header,
        }
    }

    fn decode<R: Read + Seek>(
        reader: &mut R,
        hasher: &mut digest::Hasher,
        limits: &StoneDecodeLimits,
        archive_end: u64,
        state: &mut DecodeState,
    ) -> Result<Self, StoneReadError> {
        let header = StonePayloadHeader::decode(&mut *reader).map_err(StoneReadError::PayloadDecode)?;
        state.admit(&header, limits)?;

        let body_offset = reader.stream_position()?;
        let body_end = body_offset
            .checked_add(header.stored_size)
            .ok_or(StoneReadError::ArithmeticOverflow("payload body end"))?;
        if body_end > archive_end {
            return Err(StoneReadError::PayloadBodyOutOfBounds {
                offset: body_offset,
                stored_size: header.stored_size,
                archive_size: archive_end,
            });
        }

        if header.compression == StonePayloadCompression::Unknown {
            read_raw_payload(reader, hasher, &header)?;
            return Ok(StoneDecodedPayload::UnknownCompression(StonePayload {
                header,
                body: (),
            }));
        }

        let payload = match header.kind {
            StonePayloadKind::Meta => StoneDecodedPayload::Meta(StonePayload {
                header,
                body: decode_record_body::<StonePayloadMetaRecord, _>(reader, hasher, &header, limits)?,
            }),
            StonePayloadKind::Layout => StoneDecodedPayload::Layout(StonePayload {
                header,
                body: decode_record_body::<StonePayloadLayoutRecord, _>(reader, hasher, &header, limits)?,
            }),
            StonePayloadKind::Index => StoneDecodedPayload::Index(StonePayload {
                header,
                body: {
                    let records = decode_record_body::<StonePayloadIndexRecord, _>(reader, hasher, &header, limits)?;
                    state.admit_indices(&records)?;
                    records
                },
            }),
            StonePayloadKind::Attributes => StoneDecodedPayload::Attributes(StonePayload {
                header,
                body: decode_record_body::<StonePayloadAttributeRecord, _>(reader, hasher, &header, limits)?,
            }),
            StonePayloadKind::Content => {
                read_raw_payload(reader, hasher, &header)?;
                StoneDecodedPayload::Content(StonePayload {
                    header,
                    body: StonePayloadContent { offset: body_offset },
                })
            }
            StonePayloadKind::Unknown => {
                read_raw_payload(reader, hasher, &header)?;
                StoneDecodedPayload::Unknown(StonePayload { header, body: () })
            }
        };

        Ok(payload)
    }

    pub fn meta(&self) -> Option<&StonePayload<Vec<StonePayloadMetaRecord>>> {
        if let Self::Meta(meta) = self { Some(meta) } else { None }
    }

    pub fn attributes(&self) -> Option<&StonePayload<Vec<StonePayloadAttributeRecord>>> {
        if let Self::Attributes(attributes) = self {
            Some(attributes)
        } else {
            None
        }
    }

    pub fn layout(&self) -> Option<&StonePayload<Vec<StonePayloadLayoutRecord>>> {
        if let Self::Layout(layouts) = self {
            Some(layouts)
        } else {
            None
        }
    }

    pub fn index(&self) -> Option<&StonePayload<Vec<StonePayloadIndexRecord>>> {
        if let Self::Index(indices) = self {
            Some(indices)
        } else {
            None
        }
    }

    pub fn content(&self) -> Option<&StonePayload<StonePayloadContent>> {
        if let Self::Content(content) = self {
            Some(content)
        } else {
            None
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            StoneDecodedPayload::Meta(_) => "Meta",
            StoneDecodedPayload::Attributes(_) => "Attributes",
            StoneDecodedPayload::Layout(_) => "Layout",
            StoneDecodedPayload::Index(_) => "Index",
            StoneDecodedPayload::Content(_) => "Content",
            StoneDecodedPayload::Unknown(_) => "Unknown payload type",
            StoneDecodedPayload::UnknownCompression(payload) => match payload.header.kind {
                StonePayloadKind::Unknown => "Unknown payload type & compression",
                StonePayloadKind::Meta => "Meta - unknown compression",
                StonePayloadKind::Content => "Content - unknown compression",
                StonePayloadKind::Layout => "Layout - unknown compression",
                StonePayloadKind::Index => "Index - unknown compression",
                StonePayloadKind::Attributes => "Attributes - unknown compression",
            },
        }
    }
}

fn decode_record_body<T: payload::Record, R: Read>(
    reader: &mut R,
    hasher: &mut digest::Hasher,
    header: &StonePayloadHeader,
    limits: &StoneDecodeLimits,
) -> Result<Vec<T>, StoneReadError> {
    hasher.reset();
    let hashed = digest::Reader::new(reader.take(header.stored_size), hasher);
    let framed = PayloadReader::new(hashed, header.compression, limits.max_zstd_window_log)?;
    let mut plain = ExactPlainReader::new(framed, header.plain_size);

    let records = payload::decode_records(&mut plain, header.num_records, limits.max_record_bytes);
    if let Some(error) = plain.size_error() {
        return Err(error);
    }
    let records = records.map_err(StoneReadError::PayloadDecode)?;
    plain.finish_exact()?;

    let (remaining, got) = drain_raw(plain.into_inner().into_raw())?;
    if remaining != 0 {
        return Err(StoneReadError::StoredPayloadTruncated {
            declared: header.stored_size,
            actual: header.stored_size - remaining,
        });
    }
    validate_checksum_value(got, header)?;

    Ok(records)
}

fn read_raw_payload<R: Read>(
    reader: &mut R,
    hasher: &mut digest::Hasher,
    header: &StonePayloadHeader,
) -> Result<(), StoneReadError> {
    hasher.reset();
    let mut hashed = digest::Reader::new(reader.take(header.stored_size), hasher);
    let actual = io::copy(&mut hashed, &mut io::sink())?;
    if actual != header.stored_size {
        return Err(StoneReadError::StoredPayloadTruncated {
            declared: header.stored_size,
            actual,
        });
    }
    validate_checksum(hasher, header)
}

fn validate_checksum(hasher: &digest::Hasher, header: &StonePayloadHeader) -> Result<(), StoneReadError> {
    validate_checksum_value(hasher.digest(), header)
}

fn validate_checksum_value(got: u64, header: &StonePayloadHeader) -> Result<(), StoneReadError> {
    let expected = u64::from_be_bytes(header.checksum);

    if got != expected {
        Err(StoneReadError::PayloadChecksum { got, expected })
    } else {
        Ok(())
    }
}

fn enforce_limit(resource: &'static str, limit: u64, actual: u64) -> Result<(), StoneReadError> {
    if actual > limit {
        Err(StoneReadError::LimitExceeded {
            resource,
            limit,
            actual,
        })
    } else {
        Ok(())
    }
}

fn checked_total(resource: &'static str, current: u64, additional: u64, limit: u64) -> Result<u64, StoneReadError> {
    let actual = current
        .checked_add(additional)
        .ok_or(StoneReadError::ArithmeticOverflow(resource))?;
    enforce_limit(resource, limit, actual)?;
    Ok(actual)
}

fn verify_archive_end<R: Seek>(reader: &mut R, expected_end: u64) -> Result<(), StoneReadError> {
    let payload_end = reader.stream_position()?;
    let current_end = reader.seek(SeekFrom::End(0))?;
    if payload_end != expected_end || current_end != expected_end {
        return Err(StoneReadError::TrailingOrChangedArchive {
            payload_end,
            initial_end: expected_end,
            current_end,
        });
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum StoneReadError {
    #[error("decoder cannot continue after a previous payload error")]
    DecoderPoisoned,
    #[error("multiple content payloads are not allowed")]
    MultipleContent,
    #[error("unknown payload compression")]
    UnknownCompression,
    #[error("unsupported payload version {0}")]
    UnsupportedPayloadVersion(u16),
    #[error("invalid decode limits: {0}")]
    InvalidLimits(&'static str),
    #[error("invalid {resource} limits: per-payload value {per_payload} exceeds aggregate value {total}")]
    InvalidLimitRelationship {
        resource: &'static str,
        per_payload: u64,
        total: u64,
    },
    #[error("{resource} {actual} exceeds limit {limit}")]
    LimitExceeded {
        resource: &'static str,
        limit: u64,
        actual: u64,
    },
    #[error("arithmetic overflow while calculating {0}")]
    ArithmeticOverflow(&'static str),
    #[error("payload body at {offset} with stored size {stored_size} lies outside {archive_size} archive bytes")]
    PayloadBodyOutOfBounds {
        offset: u64,
        stored_size: u64,
        archive_size: u64,
    },
    #[error("stored payload ended after {actual} of {declared} declared bytes")]
    StoredPayloadTruncated { declared: u64, actual: u64 },
    #[error("plain payload ended after {actual} of {declared} declared bytes")]
    PlainPayloadTruncated { declared: u64, actual: u64 },
    #[error("plain payload expands beyond its declared {declared} bytes")]
    PlainPayloadTooLarge { declared: u64 },
    #[error("record decoder consumed {consumed} of {declared} plain payload bytes")]
    PlainPayloadUnconsumed { consumed: u64, declared: u64 },
    #[error("uncompressed payload stored size {stored} does not equal plain size {plain}")]
    PlainPayloadSizeMismatch { stored: u64, plain: u64 },
    #[error("{records} records need at least {minimum_bytes} bytes, exceeding declared plain size {plain_size}")]
    ImpossibleRecordDeclaration {
        records: u64,
        minimum_bytes: u64,
        plain_size: u64,
    },
    #[error("invalid content reference: {0}")]
    InvalidContentReference(&'static str),
    #[error("invalid content index range: end {end} precedes start {start}")]
    InvalidIndexRange { start: u64, end: u64 },
    #[error("content index end {end} exceeds content size {content_size}")]
    IndexOutsideContent { end: u64, content_size: u64 },
    #[error(
        "archive ended payloads at {payload_end}, initially ended at {initial_end}, and currently ends at {current_end}"
    )]
    TrailingOrChangedArchive {
        payload_end: u64,
        initial_end: u64,
        current_end: u64,
    },
    #[error("header decode")]
    HeaderDecode(#[from] StoneHeaderDecodeError),
    #[error("payload decode")]
    PayloadDecode(#[from] StonePayloadDecodeError),
    #[error("payload checksum mismatch: got {got:02x}, expected {expected:02x}")]
    PayloadChecksum { got: u64, expected: u64 },
    #[error("io")]
    Io(#[from] io::Error),
}

#[cfg(test)]
#[path = "tests.rs"]
mod test;
