// SPDX-FileCopyrightText: 2024 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0
use std::io::{self, Read, Result, Write};

pub trait ReadExt: Read {
    fn read_u8(&mut self) -> Result<u8> {
        let bytes = self.read_array_::<1>()?;
        Ok(bytes[0])
    }

    fn read_u16(&mut self) -> Result<u16> {
        let bytes = self.read_array_()?;
        Ok(u16::from_be_bytes(bytes))
    }

    fn read_u32(&mut self) -> Result<u32> {
        let bytes = self.read_array_()?;
        Ok(u32::from_be_bytes(bytes))
    }

    fn read_u64(&mut self) -> Result<u64> {
        let bytes = self.read_array_()?;
        Ok(u64::from_be_bytes(bytes))
    }

    fn read_u128(&mut self) -> Result<u128> {
        let bytes = self.read_array_()?;
        Ok(u128::from_be_bytes(bytes))
    }

    // Name has trailing underscore to avoid conflict with unstable method from std
    fn read_array_<const N: usize>(&mut self) -> Result<[u8; N]> {
        let mut bytes = [0u8; N];
        self.read_exact(&mut bytes)?;
        Ok(bytes)
    }

    fn read_vec(&mut self, length: usize) -> Result<Vec<u8>> {
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(length)
            .map_err(|error| io::Error::other(format!("failed to allocate {length} bytes: {error}")))?;
        bytes.resize(length, 0);
        self.read_exact(&mut bytes)?;
        Ok(bytes)
    }

    fn read_string(&mut self, length: u64) -> Result<String> {
        let length = usize::try_from(length)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "string length does not fit in memory"))?;
        let bytes = self.read_vec(length)?;
        String::from_utf8(bytes).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
    }
}

impl<T: Read> ReadExt for T {}

pub trait WriteExt: Write {
    fn write_u8(&mut self, item: u8) -> Result<()> {
        self.write_array([item])
    }

    fn write_u16(&mut self, item: u16) -> Result<()> {
        self.write_array(item.to_be_bytes())
    }

    fn write_u32(&mut self, item: u32) -> Result<()> {
        self.write_array(item.to_be_bytes())
    }

    fn write_u64(&mut self, item: u64) -> Result<()> {
        self.write_array(item.to_be_bytes())
    }

    fn write_u128(&mut self, item: u128) -> Result<()> {
        self.write_array(item.to_be_bytes())
    }

    fn write_array<const N: usize>(&mut self, bytes: [u8; N]) -> Result<()> {
        self.write_all(&bytes)?;
        Ok(())
    }
}

impl<T: Write> WriteExt for T {}
