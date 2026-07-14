// SPDX-FileCopyrightText: 2024 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use std::{fmt, os::fd::AsRawFd, sync::Arc};

use nix::fcntl::{FlockArg, flock};
use thiserror::Error;

/// An acquired file lock guaranteeing exclusive access
/// to the underlying directory.
///
/// The lock is automatically released once all instances
/// of this ref counted lock are dropped.
#[derive(Debug, Clone)]
#[allow(unused)]
pub struct Lock(Arc<std::fs::File>);

/// Acquire a lock on an already authenticated file descriptor. Path opening
/// belongs to the installation capability boundary in the parent module.
pub(super) fn acquire_file(file: std::fs::File, block_msg: impl fmt::Display) -> Result<Lock, Error> {
    match flock(file.as_raw_fd(), FlockArg::LockExclusiveNonblock) {
        Ok(_) => {}
        Err(nix::errno::Errno::EWOULDBLOCK) => {
            println!("{block_msg}");
            flock(file.as_raw_fd(), FlockArg::LockExclusive)?;
        }
        Err(e) => Err(e)?,
    }

    Ok(Lock(Arc::new(file)))
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("obtaining exclusive file lock")]
    Flock(#[from] nix::Error),
}
