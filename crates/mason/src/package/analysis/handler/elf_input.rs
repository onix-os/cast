use std::fs::File;

use elf::{ElfStream, endian::AnyEndian};

use crate::package::{
    analysis::BoxError,
    collect::PathInfo,
};

use super::VerifiedAnalyzerInput;

/// A complete descriptor-authenticated ELF view backed by an immutable sealed
/// copy of the exact regular file captured by the collector.
pub(in crate::package::analysis) struct VerifiedElf {
    input: VerifiedAnalyzerInput,
    stream: ElfStream<AnyEndian, File>,
}

impl VerifiedElf {
    pub(in crate::package::analysis) fn from_path_info(info: &PathInfo) -> Result<Option<Self>, BoxError> {
        let input = VerifiedAnalyzerInput::from_path_info(info, info.size)?;
        let stream = match ElfStream::open_stream(input.try_clone()?) {
            Ok(stream) => stream,
            Err(_) => return Ok(None),
        };
        Ok(Some(Self { input, stream }))
    }

    pub(in crate::package::analysis) fn input(&self) -> &VerifiedAnalyzerInput {
        &self.input
    }

    pub(in crate::package::analysis) fn stream_mut(&mut self) -> &mut ElfStream<AnyEndian, File> {
        &mut self.stream
    }
}
