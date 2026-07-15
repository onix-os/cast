use super::*;

#[cfg(test)]
pub(crate) fn arm_quarantine_fault(point: QuarantineFaultPoint) {
    arm_quarantine_faults(point, 1);
}

#[cfg(test)]
pub(crate) fn arm_quarantine_faults(point: QuarantineFaultPoint, count: usize) {
    assert!(count > 0, "quarantine fault count must be nonzero");
    QUARANTINE_FAULT.with(|slot| {
        assert!(
            slot.replace(Some((point, count))).is_none(),
            "quarantine fault already armed"
        );
    });
}

#[cfg(test)]
pub(crate) fn arm_retained_exchange_fault(point: RetainedExchangeFaultPoint) {
    RETAINED_EXCHANGE_FAULT.with(|slot| {
        assert!(
            slot.replace(Some(point)).is_none(),
            "retained exchange fault already armed"
        );
    });
}

#[cfg(test)]
pub(crate) fn arm_retained_previous_move_fault(point: RetainedPreviousMoveFaultPoint) {
    arm_retained_previous_move_faults(&[point]);
}

#[cfg(test)]
pub(crate) fn arm_retained_previous_move_faults(points: &[RetainedPreviousMoveFaultPoint]) {
    assert!(
        !points.is_empty(),
        "retained previous-tree fault sequence must not be empty"
    );
    RETAINED_PREVIOUS_MOVE_FAULT.with(|slot| {
        let mut slot = slot.borrow_mut();
        assert!(slot.is_empty(), "retained previous-tree move fault already armed");
        slot.extend_from_slice(points);
    });
}

#[cfg(test)]
pub(crate) fn arm_before_previous_archive_slot_reopen(hook: impl FnOnce() + 'static) {
    BEFORE_PREVIOUS_ARCHIVE_SLOT_REOPEN.with(|slot| {
        assert!(
            slot.replace(Some(Box::new(hook))).is_none(),
            "previous archive slot reopen hook already armed"
        );
    });
}

#[cfg(test)]
pub(crate) fn arm_before_retained_previous_move_rename(hook: impl FnOnce() + 'static) {
    BEFORE_RETAINED_PREVIOUS_MOVE_RENAME.with(|slot| {
        assert!(
            slot.replace(Some(Box::new(hook))).is_none(),
            "retained previous-tree move hook already armed"
        );
    });
}

#[cfg(test)]
pub(crate) fn arm_before_previous_slot_retirement_rename(hook: impl FnOnce() + 'static) {
    BEFORE_PREVIOUS_SLOT_RETIREMENT_RENAME.with(|slot| {
        assert!(
            slot.replace(Some(Box::new(hook))).is_none(),
            "previous-state slot retirement hook already armed"
        );
    });
}

#[cfg(test)]
pub(crate) fn arm_before_retained_exchange_rename(hook: impl FnOnce() + 'static) {
    BEFORE_RETAINED_EXCHANGE_RENAME.with(|slot| {
        assert!(
            slot.replace(Some(Box::new(hook))).is_none(),
            "retained exchange hook already armed"
        );
    });
}

#[cfg(test)]
pub(super) fn quarantine_checkpoint(point: QuarantineFaultPoint) -> Result<(), Error> {
    QUARANTINE_FAULT.with(|slot| {
        let mut armed = slot.borrow_mut();
        match armed.as_mut() {
            Some((armed_point, remaining)) if *armed_point == point => {
                *remaining -= 1;
                if *remaining == 0 {
                    *armed = None;
                }
                Err(Error::InjectedQuarantineFault { point })
            }
            _ => Ok(()),
        }
    })
}

#[cfg(test)]
pub(super) fn retained_exchange_checkpoint(point: RetainedExchangeFaultPoint) -> Result<(), Error> {
    RETAINED_EXCHANGE_FAULT.with(|slot| {
        if slot.borrow().as_ref() == Some(&point) {
            slot.replace(None);
            Err(Error::InjectedRetainedExchangeFault { point })
        } else {
            Ok(())
        }
    })
}

#[cfg(test)]
pub(super) fn retained_previous_move_checkpoint(point: RetainedPreviousMoveFaultPoint) -> Result<(), Error> {
    RETAINED_PREVIOUS_MOVE_FAULT.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.first() == Some(&point) {
            slot.remove(0);
            Err(Error::InjectedRetainedPreviousMoveFault { point })
        } else {
            Ok(())
        }
    })
}

#[cfg(not(test))]
pub(super) fn retained_exchange_checkpoint(point: RetainedExchangeFaultPoint) -> Result<(), Error> {
    let _ = point;
    Ok(())
}

#[cfg(not(test))]
pub(super) fn retained_previous_move_checkpoint(point: RetainedPreviousMoveFaultPoint) -> Result<(), Error> {
    let _ = point;
    Ok(())
}

#[cfg(not(test))]
pub(super) fn quarantine_checkpoint(point: QuarantineFaultPoint) -> Result<(), Error> {
    let _ = point;
    Ok(())
}

#[cfg(test)]
pub(crate) fn arm_before_live_usr_mkdir(hook: impl FnOnce() + 'static) {
    BEFORE_LIVE_USR_MKDIR.with(|slot| {
        assert!(
            slot.replace(Some(Box::new(hook))).is_none(),
            "live /usr hook already armed"
        );
    });
}

#[cfg(test)]
pub(crate) fn arm_before_quarantine_slot_reopen(hook: impl FnOnce() + 'static) {
    BEFORE_QUARANTINE_SLOT_REOPEN.with(|slot| {
        assert!(
            slot.replace(Some(Box::new(hook))).is_none(),
            "quarantine slot reopen hook already armed"
        );
    });
}

#[cfg(test)]
pub(super) fn before_retained_exchange_rename() {
    BEFORE_RETAINED_EXCHANGE_RENAME.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(test)]
pub(super) fn before_retained_previous_move_rename() {
    BEFORE_RETAINED_PREVIOUS_MOVE_RENAME.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(test)]
pub(super) fn before_previous_slot_retirement_rename() {
    BEFORE_PREVIOUS_SLOT_RETIREMENT_RENAME.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
pub(super) fn before_previous_slot_retirement_rename() {}

#[cfg(not(test))]
pub(super) fn before_retained_previous_move_rename() {}

#[cfg(not(test))]
pub(super) fn before_retained_exchange_rename() {}

#[cfg(test)]
pub(super) fn before_live_usr_mkdir() {
    BEFORE_LIVE_USR_MKDIR.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
pub(super) fn before_live_usr_mkdir() {}

#[cfg(test)]
pub(super) fn before_quarantine_slot_reopen() {
    BEFORE_QUARANTINE_SLOT_REOPEN.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
pub(super) fn before_quarantine_slot_reopen() {}

#[cfg(test)]
pub(super) fn before_previous_archive_slot_reopen() {
    BEFORE_PREVIOUS_ARCHIVE_SLOT_REOPEN.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
pub(super) fn before_previous_archive_slot_reopen() {}
