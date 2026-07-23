use std::{
    io,
    os::fd::{AsRawFd as _, BorrowedFd},
    time::Instant,
};

pub(super) trait LinuxGetdents64 {
    fn now(&mut self) -> Instant;

    /// Performs exactly one complete call. Implementations must not retry
    /// `EINTR`, split a kernel result, or report more than `output.len()`.
    fn getdents64_once(&mut self, directory: BorrowedFd<'_>, output: &mut [u8]) -> io::Result<usize>;
}

#[derive(Clone, Copy, Debug)]
pub(super) struct NativeLinuxGetdents64;

impl LinuxGetdents64 for NativeLinuxGetdents64 {
    fn now(&mut self) -> Instant {
        Instant::now()
    }

    fn getdents64_once(&mut self, directory: BorrowedFd<'_>, output: &mut [u8]) -> io::Result<usize> {
        // SAFETY: `directory` remains borrowed for the call, `output` is a
        // writable allocation of the supplied length, and SYS_getdents64 does
        // not retain either argument. The raw syscall is intentionally issued
        // once: `-1/EINTR` is returned to the caller without retry.
        let found = unsafe {
            nix::libc::syscall(
                nix::libc::SYS_getdents64,
                directory.as_raw_fd(),
                output.as_mut_ptr(),
                output.len(),
            )
        };
        if found < 0 {
            return Err(io::Error::last_os_error());
        }
        let found = usize::try_from(found)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "negative getdents64 byte count"))?;
        if found > output.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "getdents64 returned more bytes than the supplied buffer",
            ));
        }
        Ok(found)
    }
}

#[cfg(test)]
pub(super) struct InjectedLinuxGetdents64<Clock, Call> {
    now: Clock,
    call: Call,
}

#[cfg(test)]
impl<Clock, Call> InjectedLinuxGetdents64<Clock, Call> {
    pub(super) fn new(now: Clock, call: Call) -> Self {
        Self { now, call }
    }
}

#[cfg(test)]
impl<Clock, Call> LinuxGetdents64 for InjectedLinuxGetdents64<Clock, Call>
where
    Clock: FnMut() -> Instant,
    Call: FnMut(&mut [u8]) -> io::Result<usize>,
{
    fn now(&mut self) -> Instant {
        (self.now)()
    }

    fn getdents64_once(&mut self, _directory: BorrowedFd<'_>, output: &mut [u8]) -> io::Result<usize> {
        (self.call)(output)
    }
}
