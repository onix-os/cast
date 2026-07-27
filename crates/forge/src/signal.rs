// SPDX-FileCopyrightText: 2024 AerynOS Developers

//! Signal handling

use nix::sys::signal::{SaFlags, SigAction, SigHandler, SigSet, sigaction};
use thiserror::Error;
#[cfg(not(test))]
use zbus::message;

pub use nix::sys::signal::Signal;

#[cfg(not(test))]
use crate::runtime;

/// Ignore the provided signals until [`Guard`] is dropped
pub fn ignore(signals: impl IntoIterator<Item = Signal>) -> Result<Guard, Error> {
    Ok(Guard(
        signals
            .into_iter()
            .map(|signal| {
                let action = unsafe {
                    sigaction(
                        signal,
                        &SigAction::new(SigHandler::SigIgn, SaFlags::empty(), SigSet::empty()),
                    )
                }
                .map_err(Error::Ignore)?;

                Ok(PrevHandler { signal, action })
            })
            .collect::<Result<_, Error>>()?,
    ))
}

// https://www.freedesktop.org/wiki/Software/systemd/inhibit/
pub fn inhibit(what: Vec<&str>, who: String, why: String, mode: String) -> Result<Inhibitor, Error> {
    #[cfg(test)]
    {
        // Unit tests exercise complete transaction paths without depending on
        // a host logind service. Production builds still acquire and retain
        // the real inhibitor body below.
        let _ = (what, who, why, mode);
        Ok(Inhibitor {})
    }

    #[cfg(not(test))]
    {
        // Escape hatch for environments with no logind on the system bus.
        //
        // The inhibitor exists to stop a login session from suspending or
        // rebooting mid-transaction. Where nothing can issue such a request —
        // a minimal initramfs, a single-purpose guest, a container without a
        // session manager — there is nothing to inhibit, and requiring the
        // service turns "no logind" into "no state mutation at all".
        //
        // This is what blocks the crash matrix: its guest runs forge under a
        // busybox init with no session manager, so every state-mutating cell
        // failed at the inhibitor rather than at anything it was testing
        // (`plans/future_impl.md` §2.1). Deliberately opt-in and named so it
        // cannot be reached by accident.
        if std::env::var_os("CAST_ALLOW_UNINHIBITED_TRANSACTION").is_some() {
            let _ = (what, who, why, mode);
            return Ok(Inhibitor { _body: None });
        }

        runtime::block_on(async {
            let conn = zbus::Connection::system().await?;
            let msg = conn
                .call_method(
                    Some("org.freedesktop.login1"),
                    "/org/freedesktop/login1",
                    Some("org.freedesktop.login1.Manager"),
                    "Inhibit",
                    &(what.join(":"), who, why, mode),
                )
                .await?;
            Ok(Inhibitor {
                _body: Some(msg.body()),
            })
        })
    }
}

/// Retains a logind inhibitor for the duration of a transaction.
pub struct Inhibitor {
    /// `None` when the uninhibited escape hatch was taken; there is no logind
    /// reply to retain in that case.
    #[cfg(not(test))]
    _body: Option<message::Body>,
}

/// A guard which restores the previous signal
/// handlers when dropped
pub struct Guard(Vec<PrevHandler>);

impl Drop for Guard {
    fn drop(&mut self) {
        for PrevHandler { signal, action } in &self.0 {
            unsafe {
                let _ = sigaction(*signal, action);
            };
        }
    }
}

struct PrevHandler {
    signal: Signal,
    action: SigAction,
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("ignore signal")]
    Ignore(#[source] nix::Error),
    #[error("failed to connect to dbus")]
    Zbus(#[from] zbus::Error),
}
