use std::{ffi::CStr, fs::File, io, os::fd::AsRawFd as _};

use crate::linux_fs::{renameat2_exchange_once, renameat2_noreplace_once};

pub(super) fn exchange_once(
    parent: &File,
    canonical_name: &CStr,
    sidecar_name: &CStr,
) -> io::Result<()> {
    let result = renameat2_exchange_once(parent, canonical_name, parent, sidecar_name);
    #[cfg(test)]
    if result.is_ok() && take_exchange_error_after_applied() {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "injected exchange error report after the boot-file exchange applied",
        ));
    }
    result
}

pub(super) fn unlink_once(parent: &File, name: &CStr) -> io::Result<()> {
    // SAFETY: the retained parent and validated component remain live for one
    // non-retried unlink. The caller reconciles the name after every report.
    if unsafe { nix::libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), 0) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

pub(super) fn detach_once(parent: &File, canonical_name: &CStr, private_name: &CStr) -> io::Result<()> {
    let result = renameat2_noreplace_once(parent, canonical_name, parent, private_name);
    #[cfg(test)]
    if result.is_ok() && take_detach_error_after_applied() {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "injected detach error report after the stale boot-file move applied",
        ));
    }
    result
}

#[cfg(test)]
thread_local! {
    static EXCHANGE_ERROR_AFTER_APPLIED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static DETACH_ERROR_AFTER_APPLIED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static STOP_BEFORE_EXCHANGE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static STOP_AFTER_STALE_DETACH: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static STOP_AFTER_SIDECAR_UNLINK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
pub(crate) fn arm_boot_file_exchange_error_after_applied() {
    EXCHANGE_ERROR_AFTER_APPLIED.with(|slot| assert!(!slot.replace(true), "exchange fault already armed"));
}

#[cfg(test)]
pub(crate) fn arm_stale_boot_file_detach_error_after_applied() {
    DETACH_ERROR_AFTER_APPLIED.with(|slot| assert!(!slot.replace(true), "detach fault already armed"));
}

#[cfg(test)]
pub(crate) fn arm_boot_file_replacement_stop_before_exchange() {
    STOP_BEFORE_EXCHANGE.with(|slot| assert!(!slot.replace(true), "pre-exchange stop already armed"));
}

#[cfg(test)]
pub(crate) fn arm_stale_boot_file_stop_after_detach() {
    STOP_AFTER_STALE_DETACH.with(|slot| assert!(!slot.replace(true), "post-detach stop already armed"));
}

#[cfg(test)]
pub(crate) fn arm_boot_file_sidecar_stop_after_unlink() {
    STOP_AFTER_SIDECAR_UNLINK.with(|slot| assert!(!slot.replace(true), "post-unlink stop already armed"));
}

pub(super) fn stop_before_exchange() -> bool {
    #[cfg(test)]
    if STOP_BEFORE_EXCHANGE.with(|slot| slot.replace(false)) {
        return true;
    }
    false
}

pub(super) fn stop_after_stale_detach() -> bool {
    #[cfg(test)]
    if STOP_AFTER_STALE_DETACH.with(|slot| slot.replace(false)) {
        return true;
    }
    false
}

pub(super) fn stop_after_sidecar_unlink() -> bool {
    #[cfg(test)]
    if STOP_AFTER_SIDECAR_UNLINK.with(|slot| slot.replace(false)) {
        return true;
    }
    false
}

#[cfg(test)]
fn take_exchange_error_after_applied() -> bool {
    EXCHANGE_ERROR_AFTER_APPLIED.with(|slot| slot.replace(false))
}

#[cfg(test)]
fn take_detach_error_after_applied() -> bool {
    DETACH_ERROR_AFTER_APPLIED.with(|slot| slot.replace(false))
}
