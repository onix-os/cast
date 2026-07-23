//! Capability-required entry point for legacy compensating boot repair.

use thiserror::Error;

use crate::{
    State,
    transition_identity::{LegacyBootRepairAuthority, LegacyBootRepairAuthorityError},
};

use super::{Client, boot};

/// Run the legacy restored-state projection only while the caller retains a
/// freshly revalidated clean-journal authority. Journal-owned coordinators
/// cannot construct that authority and must fail-stop into startup recovery.
pub(super) fn synchronize(
    client: &Client,
    restored: &State,
    authority: LegacyBootRepairAuthority<'_>,
) -> Result<(), Error> {
    authority.revalidate(&client.installation, &client.state_db)?;
    before_worker();
    let synchronization = boot::synchronize(client, restored, None);
    let post_authority = authority.revalidate(&client.installation, &client.state_db);

    // As with standalone synchronization, lost authority makes a simultaneous
    // backend result unattributable and therefore takes precedence.
    post_authority?;
    synchronization?;
    Ok(())
}

#[derive(Debug, Error)]
pub(super) enum Error {
    #[error("authorize legacy compensating boot repair against exact clean transition evidence")]
    Authority(#[from] LegacyBootRepairAuthorityError),
    #[error("synchronize restored-state boot metadata during legacy compensating repair")]
    Boot(#[from] boot::Error),
}

#[cfg(test)]
std::thread_local! {
    static BEFORE_WORKER: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn arm_before_worker(hook: impl FnOnce() + 'static) {
    BEFORE_WORKER.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn before_worker() {
    BEFORE_WORKER.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn before_worker() {}

#[cfg(test)]
mod tests;
