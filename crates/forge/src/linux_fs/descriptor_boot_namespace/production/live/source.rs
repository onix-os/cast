use std::{
    os::fd::{AsFd as _, OwnedFd},
    time::Instant,
};

use super::super::source::{
    ProductionRawDirectorySource, ProductionRawDirectorySourceError, ProductionRawDirectorySourceResult,
};
use super::syscall::LinuxGetdents64;

/// Owns the sole directory capability for one forward-only inventory pass.
/// There is intentionally no descriptor accessor or recovery method.
pub(super) struct ProductionLinuxRawDirectorySource<Driver> {
    directory: OwnedFd,
    driver: Driver,
}

impl<Driver> ProductionLinuxRawDirectorySource<Driver> {
    pub(super) fn new(directory: OwnedFd, driver: Driver) -> Self {
        Self { directory, driver }
    }
}

impl<Driver: LinuxGetdents64> ProductionRawDirectorySource for ProductionLinuxRawDirectorySource<Driver> {
    fn now(&mut self) -> Instant {
        self.driver.now()
    }

    fn before_allocation(&mut self, _attempt: usize, _bytes: usize) -> ProductionRawDirectorySourceResult<()> {
        Ok(())
    }

    fn read_chunk(&mut self, output: &mut [u8]) -> ProductionRawDirectorySourceResult<usize> {
        self.read_once(output)
    }

    fn probe_end(&mut self, output: &mut [u8]) -> ProductionRawDirectorySourceResult<usize> {
        self.read_once(output)
    }
}

impl<Driver: LinuxGetdents64> ProductionLinuxRawDirectorySource<Driver> {
    fn read_once(&mut self, output: &mut [u8]) -> ProductionRawDirectorySourceResult<usize> {
        self.driver
            .getdents64_once(self.directory.as_fd(), output)
            .map_err(|_| ProductionRawDirectorySourceError)
    }
}
