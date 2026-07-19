use std::{
    ffi::CStr,
    io,
    mem::{MaybeUninit, size_of, zeroed},
    os::fd::{AsRawFd as _, BorrowedFd, FromRawFd as _, OwnedFd},
};

/// Performs exactly one `fcntl(F_GETFL)` attempt. `EINTR` is failed closed.
pub(super) fn descriptor_flags_once(descriptor: BorrowedFd<'_>) -> io::Result<i32> {
    // SAFETY: F_GETFL reads status flags from the live borrowed descriptor.
    let found = unsafe { nix::libc::fcntl(descriptor.as_raw_fd(), nix::libc::F_GETFL) };
    if found >= 0 {
        Ok(found)
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Performs exactly one `fstat` attempt. `EINTR` is failed closed.
pub(super) fn fstat_once(descriptor: BorrowedFd<'_>) -> io::Result<nix::libc::stat> {
    let mut status = MaybeUninit::<nix::libc::stat>::uninit();
    // SAFETY: fstat initializes the supplied storage on success and the
    // descriptor remains borrowed for the complete call.
    if unsafe { nix::libc::fstat(descriptor.as_raw_fd(), status.as_mut_ptr()) } == 0 {
        // SAFETY: successful fstat initialized every stat field.
        Ok(unsafe { status.assume_init() })
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Performs exactly one descriptor-relative `statx(AT_EMPTY_PATH)` attempt.
///
/// `STATX_MNT_ID` first appeared in Linux 5.8. Mounted boot publication has
/// an effective Linux >= 5.10 admission boundary, while generic `linux_fs`
/// remains Linux 5.6 compatible. This adapter therefore fails closed when
/// the running kernel does not return `STATX_MNT_ID`.
pub(super) fn descriptor_mount_id_once(descriptor: BorrowedFd<'_>) -> io::Result<u64> {
    // SAFETY: zero is a valid initial value for every statx field.
    let mut status: nix::libc::statx = unsafe { zeroed() };
    // SAFETY: the empty C string and output storage remain live for the one
    // syscall. AT_EMPTY_PATH binds the query to the supplied descriptor,
    // including an O_PATH descriptor, without any pathname traversal.
    let found = unsafe {
        nix::libc::syscall(
            nix::libc::SYS_statx,
            descriptor.as_raw_fd(),
            c"".as_ptr(),
            nix::libc::AT_EMPTY_PATH,
            nix::libc::STATX_MNT_ID,
            &mut status,
        )
    };
    if found != 0 {
        return Err(io::Error::last_os_error());
    }
    if status.stx_mask & nix::libc::STATX_MNT_ID == 0 {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "statx result does not contain STATX_MNT_ID",
        ));
    }
    Ok(status.stx_mnt_id)
}

/// Performs exactly one `openat2` attempt. `EINTR` is failed closed.
pub(super) fn openat2_once(
    parent: BorrowedFd<'_>,
    name: &CStr,
    flags: i32,
    mode: u32,
    resolve: u64,
) -> io::Result<std::fs::File> {
    // SAFETY: zero is valid for every public open_how field.
    let mut how: nix::libc::open_how = unsafe { zeroed() };
    how.flags = u64::from(flags as u32);
    how.mode = u64::from(mode);
    how.resolve = resolve;
    // SAFETY: the parent, C string, and open_how remain live for the one
    // syscall. A successful result is a new descriptor owned below.
    let found = unsafe {
        nix::libc::syscall(
            nix::libc::SYS_openat2,
            parent.as_raw_fd(),
            name.as_ptr(),
            &how,
            size_of::<nix::libc::open_how>(),
        )
    };
    if found < 0 {
        return Err(io::Error::last_os_error());
    }
    let raw = i32::try_from(found).map_err(|_| io::Error::other("openat2 returned an invalid descriptor"))?;
    // SAFETY: successful openat2 returned this fresh owned descriptor.
    let owned = unsafe { OwnedFd::from_raw_fd(raw) };
    Ok(std::fs::File::from(owned))
}

/// Performs exactly one positional read. `EINTR` is failed closed.
pub(super) fn pread_once(descriptor: BorrowedFd<'_>, offset: u64, output: &mut [u8]) -> io::Result<usize> {
    let offset = nix::libc::off_t::try_from(offset)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "pread offset exceeds off_t"))?;
    // SAFETY: output is writable for its complete length, the descriptor
    // remains borrowed, and pread retains neither pointer nor descriptor.
    let found = unsafe { nix::libc::pread(descriptor.as_raw_fd(), output.as_mut_ptr().cast(), output.len(), offset) };
    if found < 0 {
        return Err(io::Error::last_os_error());
    }
    usize::try_from(found).map_err(|_| io::Error::other("pread returned an oversized byte count"))
}

/// Performs exactly one `getdents64` syscall. `EINTR` is failed closed.
pub(super) fn getdents64_once(descriptor: BorrowedFd<'_>, output: &mut [u8]) -> io::Result<usize> {
    // SAFETY: output is writable for its complete length, the descriptor
    // remains borrowed, and getdents64 retains neither argument.
    let found = unsafe {
        nix::libc::syscall(
            nix::libc::SYS_getdents64,
            descriptor.as_raw_fd(),
            output.as_mut_ptr(),
            output.len(),
        )
    };
    if found < 0 {
        return Err(io::Error::last_os_error());
    }
    let found = usize::try_from(found)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "getdents64 returned a negative byte count"))?;
    if found > output.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "getdents64 returned more bytes than the supplied buffer",
        ));
    }
    Ok(found)
}
