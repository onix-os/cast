#[cfg(test)]
use std::cell::RefCell;

#[cfg(test)]
thread_local! {
    static BEFORE_TERMINAL_REBIND: RefCell<Option<Box<dyn FnOnce()>>> = RefCell::new(None);
}

#[cfg(test)]
pub(crate) struct FixtureRetainedBootLeafAssessmentHookGuard;

#[cfg(test)]
impl Drop for FixtureRetainedBootLeafAssessmentHookGuard {
    fn drop(&mut self) {
        BEFORE_TERMINAL_REBIND.with(|slot| {
            slot.borrow_mut().take();
        });
    }
}

#[cfg(test)]
pub(crate) fn arm_retained_boot_leaf_assessment_terminal_rebind_hook(
    hook: impl FnOnce() + 'static,
) -> FixtureRetainedBootLeafAssessmentHookGuard {
    BEFORE_TERMINAL_REBIND.with(|slot| {
        assert!(
            slot.borrow_mut().replace(Box::new(hook)).is_none(),
            "boot-leaf assessment terminal-rebind hook already armed"
        );
    });
    FixtureRetainedBootLeafAssessmentHookGuard
}

pub(super) fn before_terminal_rebind() {
    #[cfg(test)]
    {
        let hook = BEFORE_TERMINAL_REBIND.with(|slot| slot.borrow_mut().take());
        if let Some(hook) = hook {
            hook();
        }
    }
}
