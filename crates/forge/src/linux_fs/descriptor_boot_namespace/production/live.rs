//! One-shot Linux `getdents64` source for one retained directory description.
//!
//! This layer accepts no path and performs no lookup, reopen, seek, reset, or
//! mutation. The caller must transfer a readable directory description whose
//! open-file-description offset is zero and which has not been shared with a
//! concurrent enumerator. Opening and establishing that fresh authority belong
//! to the retained namespace observer, not this adapter.
//!
//! `getdents64` is synchronous. The enclosing bounded parser checks its caller
//! deadline immediately around every source call, but those checks cannot
//! preempt a kernel call that blocks past the deadline. An interrupted syscall
//! is therefore failed closed after exactly one attempt rather than retried.

use std::{os::fd::OwnedFd, time::Instant};

#[cfg(test)]
use std::io;

use super::{
    error::ProductionRawDirectoryInventoryError,
    inventory::ProductionRawDirectoryInventory,
    model::{ProductionRawDirectoryInventoryLimits, ProductionRawDirectoryInventoryUsage},
    parser::{
        parse_production_raw_directory_inventory_until, parse_production_raw_directory_inventory_with_usage_until,
    },
};

#[path = "live/abi.rs"]
mod abi;
#[path = "live/source.rs"]
mod source;
#[path = "live/syscall.rs"]
mod syscall;

use source::ProductionLinuxRawDirectorySource;
use syscall::NativeLinuxGetdents64;

impl ProductionRawDirectoryInventory {
    /// Consumes one fresh offset-zero directory description and returns only
    /// closed raw-name inventory. The descriptor is dropped on every return
    /// path and cannot be recovered from the result.
    pub(crate) fn read_fresh_linux_directory_until(
        directory: OwnedFd,
        limits: ProductionRawDirectoryInventoryLimits,
        deadline: Instant,
    ) -> Result<Self, ProductionRawDirectoryInventoryError> {
        let mut source = ProductionLinuxRawDirectorySource::new(directory, NativeLinuxGetdents64);
        parse_production_raw_directory_inventory_until(&mut source, limits, deadline)
    }

    /// The usage-bearing form preserves the parser's exact operation-wide
    /// ledger so a retained observer can charge this pass before proceeding.
    pub(crate) fn read_fresh_linux_directory_with_usage_until(
        directory: OwnedFd,
        limits: ProductionRawDirectoryInventoryLimits,
        deadline: Instant,
    ) -> Result<(Self, ProductionRawDirectoryInventoryUsage), ProductionRawDirectoryInventoryError> {
        let mut source = ProductionLinuxRawDirectorySource::new(directory, NativeLinuxGetdents64);
        parse_production_raw_directory_inventory_with_usage_until(&mut source, limits, deadline)
    }

    /// Test-only syscall seam. It deliberately withholds the descriptor from
    /// the injected closure: tests control only time and complete syscall
    /// results, never retained directory authority.
    #[cfg(test)]
    pub(crate) fn read_fresh_directory_with_injected_getdents_until<Clock, Call>(
        directory: OwnedFd,
        limits: ProductionRawDirectoryInventoryLimits,
        deadline: Instant,
        now: Clock,
        call: Call,
    ) -> Result<(Self, ProductionRawDirectoryInventoryUsage), ProductionRawDirectoryInventoryError>
    where
        Clock: FnMut() -> Instant,
        Call: FnMut(&mut [u8]) -> io::Result<usize>,
    {
        let driver = syscall::InjectedLinuxGetdents64::new(now, call);
        let mut source = ProductionLinuxRawDirectorySource::new(directory, driver);
        parse_production_raw_directory_inventory_with_usage_until(&mut source, limits, deadline)
    }
}
