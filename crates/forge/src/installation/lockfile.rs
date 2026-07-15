// SPDX-FileCopyrightText: 2024 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use std::{
    fmt,
    os::fd::AsRawFd,
    sync::Arc,
    time::{Duration, Instant},
};

use nix::fcntl::{FlockArg, flock};
use thiserror::Error;

/// An acquired file lock guaranteeing shared or exclusive access to the
/// underlying directory, according to the acquisition function used.
///
/// The lock is automatically released once all instances
/// of this ref counted lock are dropped.
#[derive(Debug, Clone)]
pub struct Lock(Arc<std::fs::File>);

/// Acquire a lock on an already authenticated file descriptor. Path opening
/// belongs to the installation capability boundary in the parent module.
pub(super) fn acquire_file(file: std::fs::File, block_msg: impl fmt::Display) -> Result<Lock, Error> {
    match flock(file.as_raw_fd(), FlockArg::LockExclusiveNonblock) {
        Ok(()) => {}
        Err(nix::errno::Errno::EWOULDBLOCK) => {
            println!("{block_msg}");
            flock(file.as_raw_fd(), FlockArg::LockExclusive)?;
        }
        Err(source) => return Err(source.into()),
    }

    Ok(Lock(Arc::new(file)))
}

/// Acquire a shared lock on an existing authenticated file descriptor without
/// ever entering an unbounded kernel wait. A contended lock is retried with a
/// short sleep until the caller's complete acquisition budget expires.
pub(super) fn acquire_shared_file(
    file: std::fs::File,
    block_msg: impl fmt::Display,
    timeout: Duration,
) -> Result<Lock, Error> {
    const RETRY_INTERVAL: Duration = Duration::from_millis(10);

    let started = Instant::now();
    let deadline = started.checked_add(timeout).ok_or(Error::Timeout { timeout })?;
    let mut announced_contention = false;
    loop {
        match flock(file.as_raw_fd(), FlockArg::LockSharedNonblock) {
            Ok(()) => return Ok(Lock(Arc::new(file))),
            Err(nix::errno::Errno::EINTR) => continue,
            Err(nix::errno::Errno::EWOULDBLOCK) => {
                if !announced_contention {
                    println!("{block_msg}");
                    announced_contention = true;
                }
                let now = Instant::now();
                if now >= deadline {
                    return Err(Error::Timeout { timeout });
                }
                std::thread::sleep(RETRY_INTERVAL.min(deadline.saturating_duration_since(now)));
            }
            Err(source) => return Err(source.into()),
        }
    }
}

impl Lock {
    pub(super) fn file(&self) -> &std::fs::File {
        &self.0
    }
}

#[cfg(test)]
pub(super) fn try_acquire_exclusive_file(file: std::fs::File) -> Result<Option<Lock>, Error> {
    match flock(file.as_raw_fd(), FlockArg::LockExclusiveNonblock) {
        Ok(()) => Ok(Some(Lock(Arc::new(file)))),
        Err(nix::errno::Errno::EWOULDBLOCK) => Ok(None),
        Err(source) => Err(source.into()),
    }
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("obtaining file lock")]
    Flock(#[from] nix::Error),
    #[error("timed out after {timeout:?} acquiring a shared file lock")]
    Timeout { timeout: Duration },
}
