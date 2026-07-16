//! Test-only fault and race hooks for retained wrapper mechanics.

use super::legacy_lifecycle::{RetainedStagingWrapperRotationFaultPoint, StagingWrapperRotationError};

#[cfg(test)]
std::thread_local! {
    static FAULT: std::cell::RefCell<Vec<RetainedStagingWrapperRotationFaultPoint>> =
        const { std::cell::RefCell::new(Vec::new()) };
    static BEFORE_EXCHANGE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn arm_staging_wrapper_rotation_faults(
    points: impl IntoIterator<Item = RetainedStagingWrapperRotationFaultPoint>,
) {
    let mut points = points.into_iter().collect::<Vec<_>>();
    points.reverse();
    FAULT.with(|fault| *fault.borrow_mut() = points);
}

#[cfg(test)]
pub(crate) fn arm_before_staging_wrapper_exchange(hook: impl FnOnce() + 'static) {
    BEFORE_EXCHANGE.with(|armed| *armed.borrow_mut() = Some(Box::new(hook)));
}

pub(super) fn before_exchange() {
    #[cfg(test)]
    BEFORE_EXCHANGE.with(|armed| {
        if let Some(hook) = armed.borrow_mut().take() {
            hook();
        }
    });
}

pub(super) fn checkpoint(point: RetainedStagingWrapperRotationFaultPoint) -> Result<(), StagingWrapperRotationError> {
    #[cfg(test)]
    if FAULT.with(|fault| fault.borrow_mut().last().copied()) == Some(point) {
        FAULT.with(|fault| {
            fault.borrow_mut().pop();
        });
        return Err(StagingWrapperRotationError::InjectedFault { point });
    }
    let _ = point;
    Ok(())
}
