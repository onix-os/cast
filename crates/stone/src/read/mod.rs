// SPDX-FileCopyrightText: 2023 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0
#![allow(dead_code)]

use std::io::{self, BufReader, Cursor, Read, Seek, SeekFrom, Write};

use thiserror::Error;

use crate::{
    StoneHeader, StoneHeaderDecodeError, StonePayload, StonePayloadAttributeRecord, StonePayloadCompression,
    StonePayloadContent, StonePayloadDecodeError, StonePayloadHeader, StonePayloadIndexRecord, StonePayloadKind,
    StonePayloadLayoutRecord, StonePayloadMetaRecord, payload,
};

use self::zstd::Zstd;

mod digest;
mod zstd;

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

#[cfg(feature = "ffi")]
pub struct StonePayloadContentReader<'a, R: Read> {
    reader: Option<ExactPlainReader<PayloadReader<digest::Reader<'a, io::Take<&'a mut R>>>>>,
    header_checksum: u64,
    stored_size: u64,
    pub is_checksum_valid: bool,
    pub buf_hint: Option<usize>,
}

#[cfg(feature = "ffi")]
impl<R: Read> Read for StonePayloadContentReader<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let Some(reader) = self.reader.as_mut() else {
            return Ok(0);
        };

        let read = reader.read(buf)?;
        if read != 0 || buf.is_empty() {
            return Ok(read);
        }

        let reader = self.reader.take().expect("content reader exists");
        let (remaining, got) = drain_raw(reader.into_inner().into_raw())?;
        if remaining != 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!(
                    "stored payload ended after {} of {} bytes",
                    self.stored_size - remaining,
                    self.stored_size
                ),
            ));
        }

        self.is_checksum_valid = got == self.header_checksum;
        if !self.is_checksum_valid {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "payload checksum mismatch: got {got:02x}, expected {:02x}",
                    self.header_checksum
                ),
            ));
        }

        Ok(0)
    }
}

enum PayloadReader<R: Read> {
    Plain(R),
    Zstd(Zstd<R>),
}

impl<R: Read> PayloadReader<R> {
    fn new(reader: R, compression: StonePayloadCompression, max_zstd_window_log: u32) -> Result<Self, StoneReadError> {
        Ok(match compression {
            StonePayloadCompression::None => PayloadReader::Plain(reader),
            StonePayloadCompression::Zstd => PayloadReader::Zstd(Zstd::new(reader, max_zstd_window_log)?),
            StonePayloadCompression::Unknown => return Err(StoneReadError::UnknownCompression),
        })
    }

    fn into_raw(self) -> RawReader<R> {
        match self {
            PayloadReader::Plain(reader) => RawReader::Plain(reader),
            PayloadReader::Zstd(reader) => RawReader::Zstd(reader.finish()),
        }
    }

    fn buf_hint(&self) -> Option<usize> {
        match self {
            PayloadReader::Plain(_) => None,
            PayloadReader::Zstd(zstd) => Some(zstd.capacity()),
        }
    }
}

impl<R: Read> Read for PayloadReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            PayloadReader::Plain(reader) => reader.read(buf),
            PayloadReader::Zstd(reader) => reader.read(buf),
        }
    }
}

enum RawReader<R: Read> {
    Plain(R),
    Zstd(BufReader<R>),
}

fn drain_raw<R: Read>(raw: RawReader<digest::Reader<'_, io::Take<&mut R>>>) -> io::Result<(u64, u64)> {
    let framed = match raw {
        RawReader::Plain(mut framed) => {
            io::copy(&mut framed, &mut io::sink())?;
            framed
        }
        RawReader::Zstd(mut buffered) => {
            io::copy(&mut buffered, &mut io::sink())?;
            buffered.into_inner()
        }
    };

    let got = framed.hasher.digest();
    let remaining = framed.into_inner().limit();
    Ok((remaining, got))
}

struct ExactPlainReader<R> {
    inner: R,
    declared: u64,
    emitted: u64,
    too_large: bool,
    too_short: bool,
}

impl<R: Read> ExactPlainReader<R> {
    fn new(inner: R, declared: u64) -> Self {
        Self {
            inner,
            declared,
            emitted: 0,
            too_large: false,
            too_short: false,
        }
    }

    fn finish_exact(&mut self) -> Result<(), StoneReadError> {
        let consumed = self.emitted;
        let mut probe = [0u8; 1];
        match self.read(&mut probe) {
            Ok(0) => Ok(()),
            Ok(_) => Err(StoneReadError::PlainPayloadUnconsumed {
                consumed,
                declared: self.declared,
            }),
            Err(error) => self.size_error().map_or_else(|| Err(error.into()), Err),
        }
    }

    fn size_error(&self) -> Option<StoneReadError> {
        if self.too_large {
            Some(StoneReadError::PlainPayloadTooLarge {
                declared: self.declared,
            })
        } else if self.too_short {
            Some(StoneReadError::PlainPayloadTruncated {
                declared: self.declared,
                actual: self.emitted,
            })
        } else {
            None
        }
    }

    fn into_inner(self) -> R {
        self.inner
    }
}

impl<R: Read> Read for ExactPlainReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        if self.emitted == self.declared {
            let mut probe = [0u8; 1];
            return match self.inner.read(&mut probe)? {
                0 => Ok(0),
                _ => {
                    self.too_large = true;
                    Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("payload expands beyond its declared {} bytes", self.declared),
                    ))
                }
            };
        }

        let remaining = self.declared - self.emitted;
        let allowed = usize::try_from(remaining).unwrap_or(usize::MAX).min(buf.len());
        let read = self.inner.read(&mut buf[..allowed])?;
        if read == 0 {
            self.too_short = true;
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!(
                    "payload ended after {} of {} declared bytes",
                    self.emitted, self.declared
                ),
            ));
        }

        self.emitted = self
            .emitted
            .checked_add(read as u64)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "plain payload byte count overflow"))?;
        Ok(read)
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
mod test {
    use std::thread;

    use xxhash_rust::xxh3::{xxh3_64, xxh3_128};

    use crate::{StoneHeaderV1, StoneHeaderV1FileType, StoneHeaderVersion, StonePayloadLayoutFile, StoneWriter};

    use super::*;

    /// Header for bash completion stone archive.
    const BASH_TEST_STONE: [u8; 32] = [
        0x0, 0x6d, 0x6f, 0x73, 0x0, 0x4, 0x0, 0x0, 0x1, 0x0, 0x0, 0x2, 0x0, 0x0, 0x3, 0x0, 0x0, 0x4, 0x0, 0x0, 0x5,
        0x0, 0x0, 0x6, 0x0, 0x0, 0x7, 0x1, 0x0, 0x0, 0x0, 0x1,
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
}
