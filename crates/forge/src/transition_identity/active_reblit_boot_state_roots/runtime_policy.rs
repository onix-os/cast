//! Boot and mount-namespace epoch policy for retained state roots.

use std::path::Path;

use crate::{
    state,
    transition_journal::{RuntimeEpoch, RuntimeTreeIdentity},
};

use super::{ActiveReblitBootStateRootsError, StateRootBudget};

pub(super) fn capture_runtime_epoch(
    path: &Path,
    budget: &mut StateRootBudget,
) -> Result<RuntimeEpoch, ActiveReblitBootStateRootsError> {
    budget.step(path)?;
    let epoch = RuntimeEpoch::capture().map_err(|source| ActiveReblitBootStateRootsError::RuntimeEpoch {
        path: path.to_owned(),
        source,
    })?;
    budget.require_deadline(path)?;
    Ok(epoch)
}

pub(super) fn require_runtime_epoch(
    expected: &RuntimeEpoch,
    actual: &RuntimeEpoch,
    path: &Path,
) -> Result<(), ActiveReblitBootStateRootsError> {
    #[cfg(test)]
    if test_hooks::take_runtime_epoch_mismatch() {
        return Err(ActiveReblitBootStateRootsError::RuntimeEpochChanged { path: path.to_owned() });
    }
    if actual == expected {
        Ok(())
    } else {
        Err(ActiveReblitBootStateRootsError::RuntimeEpochChanged { path: path.to_owned() })
    }
}

pub(super) fn require_runtime_identity(
    state: state::Id,
    path: &Path,
    expected: &RuntimeTreeIdentity,
    directory: &std::fs::File,
) -> Result<(), ActiveReblitBootStateRootsError> {
    let actual = RuntimeTreeIdentity::capture_directory(directory).map_err(|source| {
        ActiveReblitBootStateRootsError::RuntimeIdentity {
            state: i32::from(state),
            path: path.to_owned(),
            source,
        }
    })?;
    if &actual == expected {
        Ok(())
    } else {
        Err(ActiveReblitBootStateRootsError::RuntimeIdentityChanged {
            state: i32::from(state),
            path: path.to_owned(),
        })
    }
}

pub(super) fn between_revalidation_passes() {
    #[cfg(test)]
    test_hooks::run_between_revalidation_passes();
}

#[cfg(test)]
pub(super) fn arm_between_revalidation_passes(hook: impl FnOnce() + 'static) {
    test_hooks::arm_between_revalidation_passes(hook);
}

#[cfg(test)]
pub(super) fn arm_runtime_epoch_mismatch() {
    test_hooks::arm_runtime_epoch_mismatch();
}

#[cfg(test)]
mod test_hooks {
    use std::cell::{Cell, RefCell};

    thread_local! {
        static BETWEEN_REVALIDATION_PASSES: RefCell<Option<Box<dyn FnOnce()>>> = const { RefCell::new(None) };
        static RUNTIME_EPOCH_MISMATCH: Cell<bool> = const { Cell::new(false) };
    }

    pub(super) fn arm_between_revalidation_passes(hook: impl FnOnce() + 'static) {
        BETWEEN_REVALIDATION_PASSES.with(|slot| {
            assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
        });
    }

    pub(super) fn run_between_revalidation_passes() {
        BETWEEN_REVALIDATION_PASSES.with(|slot| {
            if let Some(hook) = slot.borrow_mut().take() {
                hook();
            }
        });
    }

    pub(super) fn arm_runtime_epoch_mismatch() {
        RUNTIME_EPOCH_MISMATCH.with(|armed| {
            assert!(!armed.replace(true));
        });
    }

    pub(super) fn take_runtime_epoch_mismatch() -> bool {
        RUNTIME_EPOCH_MISMATCH.with(Cell::take)
    }
}
