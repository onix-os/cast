//! Direction-neutral, exactly-once raw exchange of two retained `/usr` names.
//!
//! This adapter owns only the single syscall attempt and its test result
//! injection. It does not authorize an exchange, infer direction, validate
//! namespace evidence, reconcile the result, sync either parent, or retry.
//! Callers must provide all of those guarantees around this boundary.

use std::{fs::File, path::Path};

#[cfg(test)]
use std::io;

use crate::linux_fs::renameat2_exchange_once;

use super::{Error, LIVE_USR_NAME, fault_injection::begin_retained_exchange_syscall_attempt, retained_exchange_io};

#[cfg(test)]
use super::RetainedExchangeSyscallFault;

/// Attempt one symmetric exchange of the fixed `usr` child beneath two
/// retained parents.
///
/// Parent ordering carries no forward/reverse meaning. The diagnostic path is
/// used only to retain the existing structured error context.
pub(crate) fn exchange_retained_usr_once(
    first_parent: &File,
    second_parent: &File,
    diagnostic_usr_path: &Path,
) -> Result<(), Error> {
    // Never retry this syscall: an EINTR or injected error may describe an
    // exchange which the kernel already completed. The caller must reconcile
    // both retained parent namespaces before interpreting this raw result.
    #[cfg(test)]
    let injected = begin_retained_exchange_syscall_attempt();
    #[cfg(not(test))]
    let _injected = begin_retained_exchange_syscall_attempt();
    #[cfg(test)]
    let apply = !matches!(
        injected,
        Some(RetainedExchangeSyscallFault::ErrorWithoutApply | RetainedExchangeSyscallFault::SuccessWithoutApply)
    );
    #[cfg(not(test))]
    let apply = true;
    let kernel_result = apply.then(|| {
        renameat2_exchange_once(first_parent, LIVE_USR_NAME, second_parent, LIVE_USR_NAME)
            .map_err(|source| retained_exchange_io("exchange staged and live /usr", diagnostic_usr_path, source))
    });
    #[cfg(test)]
    let syscall_result = match (injected, kernel_result) {
        (Some(RetainedExchangeSyscallFault::ErrorWithoutApply), None) => Err(retained_exchange_io(
            "injected /usr exchange error without application",
            diagnostic_usr_path,
            io::Error::from_raw_os_error(nix::libc::EIO),
        )),
        (Some(RetainedExchangeSyscallFault::SuccessWithoutApply), None) => Ok(()),
        (Some(RetainedExchangeSyscallFault::ErrorAfterApply), Some(Ok(()))) => Err(retained_exchange_io(
            "injected /usr exchange error after application",
            diagnostic_usr_path,
            io::Error::from_raw_os_error(nix::libc::EINTR),
        )),
        (_, Some(result)) => result,
        _ => unreachable!("test exchange injection has a complete result matrix"),
    };
    #[cfg(not(test))]
    let syscall_result = kernel_result.expect("production always invokes the one-shot exchange");
    syscall_result
}
