use std::io::{Read, Write};

use super::{Record, RecordReader, StonePayloadDecodeError, StonePayloadEncodeError};
use crate::ext::{ReadExt, WriteExt};

#[derive(Debug, Clone)]
pub struct StonePayloadAttributeRecord {
    pub key: Vec<u8>,
    pub value: Vec<u8>,
}

impl Record for StonePayloadAttributeRecord {
    fn decode<R: Read>(reader: &mut RecordReader<R>) -> Result<Self, StonePayloadDecodeError> {
        let key_length = reader.read_u64()?;
        let value_length = reader.read_u64()?;

        let variable_length = key_length
            .checked_add(value_length)
            .ok_or(StonePayloadDecodeError::LengthOverflow {
                field: "attribute key and value",
            })?;
        reader.ensure_additional("attribute key and value", variable_length)?;

        let key = reader.read_sized_vec("attribute key", key_length)?;
        let value = reader.read_sized_vec("attribute value", value_length)?;

        Ok(Self { key, value })
    }

    fn encode<W: Write>(&self, writer: &mut W) -> Result<(), StonePayloadEncodeError> {
        writer.write_u64(self.key.len() as u64)?;
        writer.write_u64(self.value.len() as u64)?;
        writer.write_all(&self.key)?;
        writer.write_all(&self.value)?;

        Ok(())
    }

    fn size(&self) -> usize {
        8 + 8 + self.key.len() + self.value.len()
    }
}
