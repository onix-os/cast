use super::*;

#[cfg(feature = "ffi")]
pub struct StonePayloadContentReader<'a, R: Read> {
    pub(super) reader: Option<ExactPlainReader<PayloadReader<digest::Reader<'a, io::Take<&'a mut R>>>>>,
    pub(super) header_checksum: u64,
    pub(super) stored_size: u64,
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

pub(super) enum PayloadReader<R: Read> {
    Plain(R),
    Zstd(Zstd<R>),
}

impl<R: Read> PayloadReader<R> {
    pub(super) fn new(
        reader: R,
        compression: StonePayloadCompression,
        max_zstd_window_log: u32,
    ) -> Result<Self, StoneReadError> {
        Ok(match compression {
            StonePayloadCompression::None => PayloadReader::Plain(reader),
            StonePayloadCompression::Zstd => PayloadReader::Zstd(Zstd::new(reader, max_zstd_window_log)?),
            StonePayloadCompression::Unknown => return Err(StoneReadError::UnknownCompression),
        })
    }

    pub(super) fn into_raw(self) -> RawReader<R> {
        match self {
            PayloadReader::Plain(reader) => RawReader::Plain(reader),
            PayloadReader::Zstd(reader) => RawReader::Zstd(reader.finish()),
        }
    }

    pub(super) fn buf_hint(&self) -> Option<usize> {
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

pub(super) enum RawReader<R: Read> {
    Plain(R),
    Zstd(BufReader<R>),
}

pub(super) fn drain_raw<R: Read>(raw: RawReader<digest::Reader<'_, io::Take<&mut R>>>) -> io::Result<(u64, u64)> {
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

pub(super) struct ExactPlainReader<R> {
    inner: R,
    declared: u64,
    emitted: u64,
    too_large: bool,
    too_short: bool,
}

impl<R: Read> ExactPlainReader<R> {
    pub(super) fn new(inner: R, declared: u64) -> Self {
        Self {
            inner,
            declared,
            emitted: 0,
            too_large: false,
            too_short: false,
        }
    }

    pub(super) fn finish_exact(&mut self) -> Result<(), StoneReadError> {
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

    pub(super) fn size_error(&self) -> Option<StoneReadError> {
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

    pub(super) fn into_inner(self) -> R {
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
