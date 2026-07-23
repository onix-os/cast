use std::{io, mem::MaybeUninit, os::fd::RawFd};

use super::abi;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct RawBlockDeviceStat {
    pub(super) containing_device: u64,
    pub(super) inode: u64,
    pub(super) mode: u32,
    pub(super) raw_device: u64,
}

pub(super) trait ObservationSyscalls {
    fn fstat_once(&mut self, descriptor: RawFd) -> io::Result<RawBlockDeviceStat>;
    fn fcntl_getfl_once(&mut self, descriptor: RawFd) -> io::Result<i32>;
    fn block_logical_size_once(&mut self, descriptor: RawFd) -> io::Result<u32>;
    fn block_byte_length_once(&mut self, descriptor: RawFd) -> io::Result<u64>;
}

pub(super) struct LinuxObservationSyscalls;

impl ObservationSyscalls for LinuxObservationSyscalls {
    fn fstat_once(&mut self, descriptor: RawFd) -> io::Result<RawBlockDeviceStat> {
        abi::require_supported_block_abi()?;
        #[cfg(all(target_os = "linux", target_pointer_width = "64"))]
        {
            let mut status = MaybeUninit::<nix::libc::stat>::uninit();
            // SAFETY: `status` points to writable storage for one `stat`, and
            // the caller retains `descriptor` for this one-shot syscall.
            if unsafe { nix::libc::fstat(descriptor, status.as_mut_ptr()) } != 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: successful `fstat` initialized every field of `status`.
            let status = unsafe { status.assume_init() };
            Ok(RawBlockDeviceStat {
                containing_device: status.st_dev,
                inode: status.st_ino,
                mode: status.st_mode,
                raw_device: status.st_rdev,
            })
        }
        #[cfg(not(all(target_os = "linux", target_pointer_width = "64")))]
        {
            let _ = descriptor;
            unreachable!("the unsupported ABI returned success")
        }
    }

    fn fcntl_getfl_once(&mut self, descriptor: RawFd) -> io::Result<i32> {
        abi::require_supported_block_abi()?;
        #[cfg(all(target_os = "linux", target_pointer_width = "64"))]
        {
            // SAFETY: `F_GETFL` takes no variadic argument and only observes
            // status flags of the caller-retained descriptor.
            let flags = unsafe { nix::libc::fcntl(descriptor, nix::libc::F_GETFL) };
            if flags < 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(flags)
            }
        }
        #[cfg(not(all(target_os = "linux", target_pointer_width = "64")))]
        {
            let _ = descriptor;
            unreachable!("the unsupported ABI returned success")
        }
    }

    fn block_logical_size_once(&mut self, descriptor: RawFd) -> io::Result<u32> {
        abi::require_supported_block_abi()?;
        #[cfg(all(target_os = "linux", target_pointer_width = "64"))]
        {
            let mut logical_block_size = 0_i32;
            // SAFETY: the exact 64-bit Linux request writes one C `int` to the
            // live output pointer and does not mutate the retained device.
            let result = unsafe {
                nix::libc::ioctl(
                    descriptor,
                    abi::BLKSSZGET_REQUEST,
                    std::ptr::from_mut(&mut logical_block_size),
                )
            };
            if result != 0 {
                return Err(io::Error::last_os_error());
            }
            logical_block_size.try_into().map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "block device returned a nonpositive logical block size",
                )
            })
        }
        #[cfg(not(all(target_os = "linux", target_pointer_width = "64")))]
        {
            let _ = descriptor;
            unreachable!("the unsupported ABI returned success")
        }
    }

    fn block_byte_length_once(&mut self, descriptor: RawFd) -> io::Result<u64> {
        abi::require_supported_block_abi()?;
        #[cfg(all(target_os = "linux", target_pointer_width = "64"))]
        {
            let mut byte_length = 0_u64;
            // SAFETY: the exact 64-bit Linux request writes one eight-byte
            // `size_t` value to the live output pointer and is read-only.
            let result = unsafe {
                nix::libc::ioctl(
                    descriptor,
                    abi::BLKGETSIZE64_REQUEST,
                    std::ptr::from_mut(&mut byte_length),
                )
            };
            if result != 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(byte_length)
            }
        }
        #[cfg(not(all(target_os = "linux", target_pointer_width = "64")))]
        {
            let _ = descriptor;
            unreachable!("the unsupported ABI returned success")
        }
    }
}

pub(super) fn positional_read_once(descriptor: RawFd, output: &mut [u8], offset: i64) -> io::Result<usize> {
    abi::require_supported_block_abi()?;
    #[cfg(all(target_os = "linux", target_pointer_width = "64"))]
    {
        // SAFETY: `output` is writable for exactly `output.len()` bytes,
        // `offset` is nonnegative, and the caller retains the descriptor for
        // this single read-only positional syscall.
        let read = unsafe { nix::libc::pread(descriptor, output.as_mut_ptr().cast(), output.len(), offset) };
        if read < 0 {
            Err(io::Error::last_os_error())
        } else {
            usize::try_from(read)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "positional read count is not representable"))
        }
    }
    #[cfg(not(all(target_os = "linux", target_pointer_width = "64")))]
    {
        let _ = (descriptor, output, offset);
        unreachable!("the unsupported ABI returned success")
    }
}
