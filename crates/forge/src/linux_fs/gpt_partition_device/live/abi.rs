use std::io;

/// Linux UAPI `_IO(0x12, 104)` on every supported 64-bit Linux target.
#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
pub(super) const BLKSSZGET_REQUEST: nix::libc::c_ulong = 0x0000_1268;

/// Linux UAPI `_IOR(0x12, 114, size_t)` with an eight-byte `size_t`.
#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
pub(super) const BLKGETSIZE64_REQUEST: nix::libc::c_ulong = 0x8008_1272;

pub(super) fn require_supported_block_abi() -> io::Result<()> {
    if cfg!(all(target_os = "linux", target_pointer_width = "64")) {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "retained GPT block-device syscalls require the 64-bit Linux ioctl ABI",
        ))
    }
}

#[cfg(test)]
pub(in crate::linux_fs) fn fixture_block_ioctl_requests() -> io::Result<(u64, u64)> {
    require_supported_block_abi()?;
    #[cfg(all(target_os = "linux", target_pointer_width = "64"))]
    {
        Ok((BLKSSZGET_REQUEST, BLKGETSIZE64_REQUEST))
    }
    #[cfg(not(all(target_os = "linux", target_pointer_width = "64")))]
    {
        unreachable!("the unsupported ABI returned success")
    }
}
