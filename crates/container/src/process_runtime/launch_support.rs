use std::{io, os::fd::RawFd, ptr::NonNull};

use nix::{errno::Errno, unistd::close};

use super::super::{
    CLONE_STACK_BYTES, Error, MAX_CHILD_ERROR_BYTES, MAX_CONTROL_EINTR_RETRIES, MAX_ERROR_SOURCE_DEPTH,
};

pub(crate) fn format_error(error: impl std::error::Error) -> String {
    let mut output = String::new();
    let mut current: Option<&dyn std::error::Error> = Some(&error);
    for depth in 0..MAX_ERROR_SOURCE_DEPTH {
        let Some(source) = current else {
            break;
        };
        if depth != 0 && !push_bounded_error_text(&mut output, ": ") {
            break;
        }
        let rendered = {
            let mut writer = BoundedErrorWriter { output: &mut output };
            std::fmt::write(&mut writer, format_args!("{source}"))
        };
        if rendered.is_err() {
            break;
        }
        current = source.source();
    }
    if current.is_some() {
        let _ = push_bounded_error_text(&mut output, " [truncated]");
    }
    output
}

struct BoundedErrorWriter<'a> {
    output: &'a mut String,
}

impl std::fmt::Write for BoundedErrorWriter<'_> {
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        if push_bounded_error_text(self.output, text) {
            Ok(())
        } else {
            Err(std::fmt::Error)
        }
    }
}

fn push_bounded_error_text(output: &mut String, text: &str) -> bool {
    let remaining = MAX_CHILD_ERROR_BYTES.saturating_sub(output.len());
    if text.len() <= remaining {
        output.push_str(text);
        return true;
    }
    let mut end = remaining.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    output.push_str(&text[..end]);
    false
}

/// Per-run clone stack with a protected low-address guard page. Linux clone
/// starts at the high end of the supplied slice and grows downward, so stack
/// exhaustion faults instead of corrupting an adjacent allocator object.
pub(crate) struct CloneStack {
    mapping: NonNull<nix::libc::c_void>,
    mapping_len: usize,
    page_size: usize,
}

impl CloneStack {
    pub(crate) fn new() -> io::Result<Self> {
        // SAFETY: sysconf has no pointer arguments.
        let page_size = unsafe { nix::libc::sysconf(nix::libc::_SC_PAGESIZE) };
        let page_size = usize::try_from(page_size)
            .ok()
            .filter(|size| size.is_power_of_two())
            .ok_or_else(|| io::Error::other("kernel returned an invalid page size"))?;
        let mapping_len = CLONE_STACK_BYTES
            .checked_add(page_size)
            .ok_or_else(|| io::Error::other("clone stack mapping length overflow"))?;
        // SAFETY: anonymous mapping uses no input pointer or file descriptor.
        let mapping = unsafe {
            nix::libc::mmap(
                std::ptr::null_mut(),
                mapping_len,
                nix::libc::PROT_NONE,
                nix::libc::MAP_PRIVATE | nix::libc::MAP_ANONYMOUS | nix::libc::MAP_STACK,
                -1,
                0,
            )
        };
        if mapping == nix::libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        let Some(mapping) = NonNull::new(mapping) else {
            // A null mapping is legal only on hosts that permit mapping page
            // zero, but NonNull is part of this type's safety invariant.
            // SAFETY: mmap returned this exact live mapping and length.
            unsafe {
                nix::libc::munmap(mapping, mapping_len);
            }
            return Err(io::Error::other("clone stack mapping unexpectedly starts at null"));
        };
        // SAFETY: the mapping is page aligned and the selected range excludes
        // exactly the first guard page.
        let usable = unsafe { mapping.as_ptr().cast::<u8>().add(page_size).cast() };
        if unsafe { nix::libc::mprotect(usable, CLONE_STACK_BYTES, nix::libc::PROT_READ | nix::libc::PROT_WRITE) } == -1
        {
            let source = io::Error::last_os_error();
            // SAFETY: mapping and length came from the successful mmap above.
            unsafe {
                nix::libc::munmap(mapping.as_ptr(), mapping_len);
            }
            return Err(source);
        }
        Ok(Self {
            mapping,
            mapping_len,
            page_size,
        })
    }

    pub(crate) fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: the usable range was made read-write, is wholly owned by this
        // value, and remains mapped until Drop after the child is reaped.
        unsafe {
            std::slice::from_raw_parts_mut(
                self.mapping.as_ptr().cast::<u8>().add(self.page_size),
                CLONE_STACK_BYTES,
            )
        }
    }

    #[cfg(test)]
    fn guard_address(&self) -> usize {
        self.mapping.as_ptr() as usize
    }

    #[cfg(test)]
    fn usable_address(&self) -> usize {
        self.guard_address() + self.page_size
    }
}

impl Drop for CloneStack {
    fn drop(&mut self) {
        // SAFETY: this value owns the complete live mapping.
        unsafe {
            nix::libc::munmap(self.mapping.as_ptr(), self.mapping_len);
        }
    }
}

/// Parent-owned bidirectional synchronization socket. `SOCK_SEQPACKET` keeps
/// the release byte and one bounded diagnostic as distinct atomic messages;
/// `MSG_NOSIGNAL` ensures child death can never terminate the supervisor.
pub(crate) struct SyncSocket {
    pub(crate) supervisor: Option<RawFd>,
    pub(crate) child: Option<RawFd>,
}

impl SyncSocket {
    pub(crate) fn new() -> Result<Self, Error> {
        let mut endpoints = [-1_i32; 2];
        // SAFETY: endpoints is a writable two-element output array and all
        // socket domain/type/protocol values are valid Linux constants.
        if unsafe {
            nix::libc::socketpair(
                nix::libc::AF_UNIX,
                nix::libc::SOCK_SEQPACKET | nix::libc::SOCK_CLOEXEC,
                0,
                endpoints.as_mut_ptr(),
            )
        } == -1
        {
            return Err(Error::Nix { source: Errno::last() });
        }
        let supervisor = match rehome_sync_fd(endpoints[0]) {
            Ok(fd) => fd,
            Err(source) => {
                let _ = close(endpoints[1]);
                return Err(Error::Nix { source });
            }
        };
        let child = match rehome_sync_fd(endpoints[1]) {
            Ok(fd) => fd,
            Err(source) => {
                let _ = close(supervisor);
                return Err(Error::Nix { source });
            }
        };
        Ok(Self {
            supervisor: Some(supervisor),
            child: Some(child),
        })
    }

    pub(crate) fn raw(&self) -> (RawFd, RawFd) {
        (self.supervisor_fd(), self.child_fd())
    }

    pub(crate) fn supervisor_fd(&self) -> RawFd {
        self.supervisor.unwrap_or(-1)
    }

    pub(crate) fn child_fd(&self) -> RawFd {
        self.child.unwrap_or(-1)
    }

    pub(crate) fn close_child_endpoint(&mut self) -> Result<(), nix::Error> {
        let Some(fd) = self.child.take() else {
            return Err(Errno::EBADF);
        };
        close_sync_endpoint(fd)
    }
}

pub(crate) fn close_sync_endpoint(fd: RawFd) -> Result<(), Errno> {
    match close(fd) {
        Ok(()) | Err(Errno::EINTR) => Ok(()),
        Err(source) => Err(source),
    }
}

pub(crate) fn send_packet_no_signal(fd: RawFd, bytes: &[u8]) -> Result<usize, Errno> {
    let mut interrupted = 0;
    loop {
        // SAFETY: bytes remains readable for its declared length and send does
        // not retain the pointer. MSG_NOSIGNAL converts peer closure to EPIPE;
        // MSG_DONTWAIT prevents a compromised child-side producer from ever
        // turning the supervisor's control path into an unbounded wait.
        let sent = unsafe {
            nix::libc::send(
                fd,
                bytes.as_ptr().cast(),
                bytes.len(),
                nix::libc::MSG_NOSIGNAL | nix::libc::MSG_DONTWAIT,
            )
        };
        if sent >= 0 {
            return usize::try_from(sent).map_err(|_| Errno::EOVERFLOW);
        }
        let source = Errno::last();
        if source == Errno::EINTR && interrupted < MAX_CONTROL_EINTR_RETRIES {
            interrupted += 1;
            continue;
        }
        return Err(source);
    }
}

fn rehome_sync_fd(fd: RawFd) -> Result<RawFd, Errno> {
    if fd >= 3 {
        return Ok(fd);
    }
    // SAFETY: the source descriptor is live and success returns a new
    // close-on-exec descriptor numbered at least three.
    let duplicated = unsafe { nix::libc::fcntl(fd, nix::libc::F_DUPFD_CLOEXEC, 3) };
    if duplicated == -1 {
        let source = Errno::last();
        let _ = close(fd);
        return Err(source);
    }
    if let Err(source) = close(fd) {
        let _ = close(duplicated);
        return Err(source);
    }
    Ok(duplicated)
}

pub(crate) fn set_fd_nonblocking(fd: RawFd) -> Result<(), Errno> {
    // SAFETY: fd is live for both fcntl calls.
    let flags = unsafe { nix::libc::fcntl(fd, nix::libc::F_GETFL) };
    if flags == -1 {
        return Err(Errno::last());
    }
    // SAFETY: F_SETFL updates only status flags on the same live descriptor.
    if unsafe { nix::libc::fcntl(fd, nix::libc::F_SETFL, flags | nix::libc::O_NONBLOCK) } == -1 {
        return Err(Errno::last());
    }
    Ok(())
}

impl Drop for SyncSocket {
    fn drop(&mut self) {
        if let Some(fd) = self.supervisor.take() {
            let _ = close(fd);
        }
        if let Some(fd) = self.child.take() {
            let _ = close(fd);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fmt;

    use super::{CloneStack, format_error};
    use crate::{CLONE_STACK_BYTES, MAX_CHILD_ERROR_BYTES};

    #[test]
    fn error_transport_format_is_bounded_even_for_cyclic_and_huge_sources() {
        #[derive(Debug)]
        struct CyclicHugeError;
        impl fmt::Display for CyclicHugeError {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                for _ in 0..(MAX_CHILD_ERROR_BYTES * 4) {
                    formatter.write_str("x")?;
                }
                Ok(())
            }
        }
        impl std::error::Error for CyclicHugeError {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                Some(self)
            }
        }

        let rendered = format_error(CyclicHugeError);
        assert_eq!(rendered.len(), MAX_CHILD_ERROR_BYTES);
        assert!(rendered.bytes().all(|byte| byte == b'x'));
    }

    #[test]
    fn clone_stack_has_a_non_accessible_guard_and_read_write_usable_mapping() {
        fn permissions(address: usize) -> Option<String> {
            let maps = fs_err::read_to_string("/proc/self/maps").ok()?;
            maps.lines().find_map(|line| {
                let mut fields = line.split_whitespace();
                let mut range = fields.next()?.split('-');
                let start = usize::from_str_radix(range.next()?, 16).ok()?;
                let end = usize::from_str_radix(range.next()?, 16).ok()?;
                let permissions = fields.next()?;
                (start <= address && address < end).then(|| permissions.to_owned())
            })
        }

        let mut stack = CloneStack::new().unwrap();
        let guard = stack.guard_address();
        let usable = stack.usable_address();
        assert_eq!(permissions(guard).as_deref(), Some("---p"));
        assert_eq!(permissions(usable).as_deref(), Some("rw-p"));
        let slice = stack.as_mut_slice();
        assert_eq!(slice.len(), CLONE_STACK_BYTES);
        assert_eq!(slice.as_ptr() as usize, usable);
        slice[0] = 1;
        slice[CLONE_STACK_BYTES - 1] = 2;
    }
}
