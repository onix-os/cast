#[cfg(test)]
use std::cell::{Cell, RefCell};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FixtureRetainedBootPublicationParentFault {
    MkdirReportsErrorAfterApplied { component_index: usize },
    AfterCreationBeforeDurability { component_index: usize },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FixtureRetainedBootPublicationParentCheckpoint {
    DirectoryRetained { depth: usize, created: bool },
    BeforeDirectorySync { depth: usize },
    AfterDirectorySync { depth: usize },
    BeforeFilesystemSync,
    BeforeTerminalRevalidation,
}

#[cfg(test)]
thread_local! {
    static PARENT_FAULT: Cell<Option<FixtureRetainedBootPublicationParentFault>> = const { Cell::new(None) };
    static PARENT_CHECKPOINT_HOOK: RefCell<
        Option<Box<dyn FnMut(FixtureRetainedBootPublicationParentCheckpoint)>>
    > = RefCell::new(None);
}

#[cfg(test)]
pub(crate) fn arm_retained_boot_publication_parent_fault(
    point: FixtureRetainedBootPublicationParentFault,
) {
    PARENT_FAULT.with(|slot| {
        assert!(slot.replace(Some(point)).is_none(), "boot publication-parent fault already armed");
    });
}

#[cfg(test)]
pub(crate) struct FixtureRetainedBootPublicationParentHookGuard;

#[cfg(test)]
impl Drop for FixtureRetainedBootPublicationParentHookGuard {
    fn drop(&mut self) {
        PARENT_CHECKPOINT_HOOK.with(|slot| {
            slot.borrow_mut().take();
        });
    }
}

#[cfg(test)]
pub(crate) fn arm_retained_boot_publication_parent_checkpoint_hook(
    hook: impl FnMut(FixtureRetainedBootPublicationParentCheckpoint) + 'static,
) -> FixtureRetainedBootPublicationParentHookGuard {
    PARENT_CHECKPOINT_HOOK.with(|slot| {
        assert!(
            slot.borrow_mut().replace(Box::new(hook)).is_none(),
            "boot publication-parent checkpoint hook already armed"
        );
    });
    FixtureRetainedBootPublicationParentHookGuard
}

pub(super) fn emit(point: FixtureRetainedBootPublicationParentCheckpoint) {
    #[cfg(test)]
    PARENT_CHECKPOINT_HOOK.with(|slot| {
        if let Some(hook) = slot.borrow_mut().as_mut() {
            hook(point);
        }
    });
    #[cfg(not(test))]
    let _ = point;
}

pub(super) fn mkdir_report(component_index: usize, result: std::io::Result<()>) -> std::io::Result<()> {
    #[cfg(test)]
    if result.is_ok()
        && take_fault(FixtureRetainedBootPublicationParentFault::MkdirReportsErrorAfterApplied {
            component_index,
        })
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Interrupted,
            "injected mkdir error report after the directory was created",
        ));
    }
    result
}

pub(super) fn fail_after_creation(component_index: usize) -> std::io::Result<()> {
    #[cfg(test)]
    if take_fault(FixtureRetainedBootPublicationParentFault::AfterCreationBeforeDurability {
        component_index,
    }) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Interrupted,
            "injected stop after publication-parent creation",
        ));
    }
    Ok(())
}

#[cfg(test)]
fn take_fault(expected: FixtureRetainedBootPublicationParentFault) -> bool {
    PARENT_FAULT.with(|slot| {
        if slot.get() == Some(expected) {
            slot.set(None);
            true
        } else {
            false
        }
    })
}
