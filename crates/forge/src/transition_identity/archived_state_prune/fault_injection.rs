#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ArchivedStatePruneFaultPoint {
    AfterQuarantineMove,
    AfterChildUnlink,
    BeforeChangedParentSync,
    AfterChangedParentSync,
    AfterPrivateDirectoryUnlink,
}

#[cfg(test)]
std::thread_local! {
    static FAULT: std::cell::Cell<Option<ArchivedStatePruneFaultPoint>> = const { std::cell::Cell::new(None) };
    static BEFORE_WRAPPER_MOVE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static BEFORE_CHILD_UNLINK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn arm_archived_state_prune_fault(point: ArchivedStatePruneFaultPoint) {
    FAULT.with(|slot| assert!(slot.replace(Some(point)).is_none(), "state-prune fault already armed"));
}

#[cfg(test)]
pub(crate) fn arm_before_archived_state_prune_wrapper_move(hook: impl FnOnce() + 'static) {
    BEFORE_WRAPPER_MOVE.with(|slot| {
        assert!(
            slot.borrow_mut().replace(Box::new(hook)).is_none(),
            "state-prune wrapper-move hook already armed"
        );
    });
}

#[cfg(test)]
pub(crate) fn arm_before_archived_state_prune_child_unlink(hook: impl FnOnce() + 'static) {
    BEFORE_CHILD_UNLINK.with(|slot| {
        assert!(
            slot.borrow_mut().replace(Box::new(hook)).is_none(),
            "state-prune child-unlink hook already armed"
        );
    });
}

pub(super) fn checkpoint(_point: ArchivedStatePruneFaultPoint) -> Result<(), super::ArchivedStatePruneError> {
    #[cfg(test)]
    if FAULT.with(|slot| slot.get()) == Some(_point) {
        FAULT.with(|slot| slot.set(None));
        return Err(super::ArchivedStatePruneError::InjectedFault { point: _point });
    }
    Ok(())
}

pub(super) fn before_wrapper_move() {
    #[cfg(test)]
    BEFORE_WRAPPER_MOVE.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

pub(super) fn before_child_unlink() {
    #[cfg(test)]
    BEFORE_CHILD_UNLINK.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}
