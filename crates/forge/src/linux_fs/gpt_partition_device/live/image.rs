use std::{
    io,
    marker::PhantomData,
    os::fd::{AsRawFd as _, BorrowedFd},
    rc::Rc,
    time::Instant,
};

use crate::linux_fs::gpt_partition_role::GptPartitionRoleImage;

use super::{abi, syscalls};

const MAX_POSITIONAL_READ_BYTES: usize = 64 * 1024;

/// Temporary read-only image over one caller-retained descriptor.
///
/// This is operation authority, not evidence: it is deliberately not clonable,
/// owns no descriptor, and cannot outlive the caller-retained descriptor. It
/// does not borrow the observer, so the composition layer can mutably
/// re-observe that same descriptor between parser passes. Only its previously
/// authenticated `BLKGETSIZE64` length is stored.
/// The private GPT `Image::length` seam is infallible, so it cannot issue a
/// fresh fallible query or detect length drift within one parser pass. The
/// composition layer must observation-sandwich each pass and treat that
/// intra-pass limitation as unresolved rather than as continuous proof.
/// The image is thread-bound so borrowed read authority cannot be detached
/// from the namespace-scoped observer which authenticated its length.
pub(in crate::linux_fs) struct RetainedReadOnlyBlockImage<'descriptor> {
    descriptor: BorrowedFd<'descriptor>,
    authenticated_byte_length: u64,
    deadline: Instant,
    _thread_bound: PhantomData<Rc<()>>,
}

impl<'descriptor> RetainedReadOnlyBlockImage<'descriptor> {
    pub(super) fn new(
        descriptor: BorrowedFd<'descriptor>,
        authenticated_byte_length: u64,
        deadline: Instant,
    ) -> io::Result<Self> {
        abi::require_supported_block_abi()?;
        checkpoint(deadline)?;
        if authenticated_byte_length == 0 || authenticated_byte_length > i64::MAX as u64 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "authenticated block-device length is zero or exceeds signed Linux offsets",
            ));
        }
        Ok(Self {
            descriptor,
            authenticated_byte_length,
            deadline,
            _thread_bound: PhantomData,
        })
    }
}

impl GptPartitionRoleImage for RetainedReadOnlyBlockImage<'_> {
    fn length(&self) -> u64 {
        self.authenticated_byte_length
    }

    fn read(&mut self, offset: u64, output: &mut [u8]) -> io::Result<usize> {
        checkpoint(self.deadline)?;
        if output.is_empty() || offset >= self.authenticated_byte_length {
            return Ok(0);
        }
        let remaining: usize = self
            .authenticated_byte_length
            .checked_sub(offset)
            .expect("offset was checked against authenticated length")
            .min(usize::MAX as u64)
            .try_into()
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "block-device read length is not representable",
                )
            })?;
        let count = output.len().min(remaining).min(MAX_POSITIONAL_READ_BYTES);
        let offset: i64 = offset.try_into().map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "block-device read offset exceeds signed Linux offsets",
            )
        })?;
        checkpoint(self.deadline)?;
        let read = syscalls::positional_read_once(self.descriptor.as_raw_fd(), &mut output[..count], offset);
        checkpoint(self.deadline)?;
        let read = read?;
        if read > count {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "positional read returned more bytes than requested",
            ));
        }
        Ok(read)
    }
}

fn checkpoint(deadline: Instant) -> io::Result<()> {
    if Instant::now() > deadline {
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "retained block-device image exceeded its caller deadline",
        ))
    } else {
        Ok(())
    }
}

#[cfg(test)]
pub(in crate::linux_fs) fn retained_read_only_block_image_fixture_until(
    descriptor: BorrowedFd<'_>,
    authenticated_byte_length: u64,
    deadline: Instant,
) -> io::Result<RetainedReadOnlyBlockImage<'_>> {
    RetainedReadOnlyBlockImage::new(descriptor, authenticated_byte_length, deadline)
}
