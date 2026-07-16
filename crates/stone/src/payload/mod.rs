mod attribute;
mod content;
mod index;
pub mod layout;
pub mod meta;

use std::io::{self, Read, Write};

use thiserror::Error;

use crate::ext::{ReadExt, WriteExt};

pub use self::attribute::StonePayloadAttributeRecord;
pub use self::content::StonePayloadContent;
pub use self::index::StonePayloadIndexRecord;
pub use self::layout::{StonePayloadLayoutFile, StonePayloadLayoutFileType, StonePayloadLayoutRecord};
pub use self::meta::{
    StonePayloadMetaDependency, StonePayloadMetaPrimitive, StonePayloadMetaRecord, StonePayloadMetaTag,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::Display)]
#[strum(serialize_all = "kebab-case")]
#[repr(u8)]
pub enum StonePayloadKind {
    // The Metadata store
    Meta = 1,
    // File store, i.e. hash indexed
    Content = 2,
    // Map Files to Disk with basic UNIX permissions + types
    Layout = 3,
    // For indexing the deduplicated store
    Index = 4,
    // Attribute storage
    Attributes = 5,

    Unknown = 255,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::Display)]
#[strum(serialize_all = "kebab-case")]
#[repr(u8)]
pub enum StonePayloadCompression {
    // Payload has no compression
    None = 1,
    // Payload uses ZSTD compression
    Zstd = 2,

    Unknown = 255,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct StonePayloadHeader {
    pub stored_size: u64,
    pub plain_size: u64,
    pub checksum: [u8; 8],
    pub num_records: usize,
    pub version: u16,
    pub kind: StonePayloadKind,
    pub compression: StonePayloadCompression,
}

impl StonePayloadHeader {
    /// Size of a payload header in the version-1 wire format.
    pub const SIZE: usize = 32;

    pub fn decode<R: Read>(mut reader: R) -> Result<Self, StonePayloadDecodeError> {
        let stored_size = reader.read_u64()?;
        let plain_size = reader.read_u64()?;
        let checksum = reader.read_array_()?;
        let encoded_num_records = reader.read_u32()?;
        let num_records = usize::try_from(encoded_num_records).map_err(|_| StonePayloadDecodeError::LimitExceeded {
            field: "payload record count",
            limit: usize::MAX as u64,
            actual: u64::from(encoded_num_records),
        })?;
        let version = reader.read_u16()?;

        let kind = match reader.read_u8()? {
            1 => StonePayloadKind::Meta,
            2 => StonePayloadKind::Content,
            3 => StonePayloadKind::Layout,
            4 => StonePayloadKind::Index,
            5 => StonePayloadKind::Attributes,
            _ => StonePayloadKind::Unknown,
        };

        let compression = match reader.read_u8()? {
            1 => StonePayloadCompression::None,
            2 => StonePayloadCompression::Zstd,
            _ => StonePayloadCompression::Unknown,
        };

        Ok(Self {
            stored_size,
            plain_size,
            checksum,
            num_records,
            version,
            kind,
            compression,
        })
    }

    pub fn encode<W: Write>(&self, writer: &mut W) -> Result<(), StonePayloadEncodeError> {
        writer.write_u64(self.stored_size)?;
        writer.write_u64(self.plain_size)?;
        writer.write_array(self.checksum)?;
        writer.write_u32(self.num_records as u32)?;
        writer.write_u16(self.version)?;
        writer.write_u8(self.kind as u8)?;
        writer.write_u8(self.compression as u8)?;

        Ok(())
    }
}

pub(crate) trait Record: Sized {
    fn decode<R: Read>(reader: &mut RecordReader<R>) -> Result<Self, StonePayloadDecodeError>;
    fn encode<W: Write>(&self, writer: &mut W) -> Result<(), StonePayloadEncodeError>;
    fn size(&self) -> usize;
}

pub(crate) struct RecordReader<R> {
    inner: R,
    limit: u64,
    consumed: u64,
}

impl<R: Read> RecordReader<R> {
    fn new(inner: R, limit: u64) -> Self {
        Self {
            inner,
            limit,
            consumed: 0,
        }
    }

    pub(crate) fn ensure_additional(&self, field: &'static str, length: u64) -> Result<(), StonePayloadDecodeError> {
        let requested = self
            .consumed
            .checked_add(length)
            .ok_or(StonePayloadDecodeError::LengthOverflow { field })?;

        if requested > self.limit {
            return Err(StonePayloadDecodeError::LimitExceeded {
                field,
                limit: self.limit,
                actual: requested,
            });
        }

        Ok(())
    }

    pub(crate) fn read_sized_vec(
        &mut self,
        field: &'static str,
        length: u64,
    ) -> Result<Vec<u8>, StonePayloadDecodeError> {
        self.ensure_additional(field, length)?;
        let length = usize::try_from(length).map_err(|_| StonePayloadDecodeError::LimitExceeded {
            field,
            limit: usize::MAX as u64,
            actual: length,
        })?;
        Ok(self.read_vec(length)?)
    }

    pub(crate) fn read_sized_string(
        &mut self,
        field: &'static str,
        length: u64,
    ) -> Result<String, StonePayloadDecodeError> {
        self.ensure_additional(field, length)?;
        Ok(self.read_string(length)?)
    }
}

impl<R: Read> Read for RecordReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        let remaining = self.limit.saturating_sub(self.consumed);
        if remaining == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("record exceeds the configured {} byte limit", self.limit),
            ));
        }

        let allowed = usize::try_from(remaining).unwrap_or(usize::MAX).min(buf.len());
        let read = self.inner.read(&mut buf[..allowed])?;
        self.consumed = self
            .consumed
            .checked_add(read as u64)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "record byte count overflow"))?;
        Ok(read)
    }
}

pub(crate) fn decode_records<T: Record, R: Read>(
    mut reader: R,
    num_records: usize,
    max_record_bytes: u64,
) -> Result<Vec<T>, StonePayloadDecodeError> {
    let mut records = Vec::new();
    records
        .try_reserve_exact(num_records)
        .map_err(|error| StonePayloadDecodeError::Allocation {
            field: "payload records",
            requested: num_records as u64,
            error: error.to_string(),
        })?;

    for _ in 0..num_records {
        let mut record = RecordReader::new(&mut reader, max_record_bytes);
        records.push(T::decode(&mut record)?);
    }

    Ok(records)
}

pub(crate) fn encode_records<T: Record, W: Write>(
    writer: &mut W,
    records: &[T],
) -> Result<(), StonePayloadEncodeError> {
    for record in records {
        record.encode(writer)?;
    }
    Ok(())
}

pub(crate) fn records_total_size<T: Record>(records: &[T]) -> usize {
    records.iter().map(T::size).sum()
}

#[derive(Debug, Clone)]
pub struct StonePayload<T> {
    pub header: StonePayloadHeader,
    pub body: T,
}

#[derive(Debug, Error)]
pub enum StonePayloadDecodeError {
    #[error("{field} length {actual} exceeds limit {limit}")]
    LimitExceeded {
        field: &'static str,
        limit: u64,
        actual: u64,
    },
    #[error("invalid {field} length {actual}; expected {expected}")]
    InvalidLength {
        field: &'static str,
        expected: u64,
        actual: u64,
    },
    #[error("invalid {field} length {actual}; expected at least {minimum}")]
    LengthTooSmall {
        field: &'static str,
        minimum: u64,
        actual: u64,
    },
    #[error("{field} length arithmetic overflow")]
    LengthOverflow { field: &'static str },
    #[error("failed to reserve {requested} {field}: {error}")]
    Allocation {
        field: &'static str,
        requested: u64,
        error: String,
    },
    #[error("io")]
    Io(#[from] io::Error),
}

#[derive(Debug, Error)]
pub enum StonePayloadEncodeError {
    #[error("io")]
    Io(#[from] io::Error),
}
