use std::io;

use super::super::super::observer::{BootNamespaceNodeIdentity, BootNamespaceObservationBoundary};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FixtureRetainedBootNamespaceProtocolEvent {
    RootRetained {
        identity: BootNamespaceNodeIdentity,
    },
    FreshInventoryOpened {
        boundary: BootNamespaceObservationBoundary,
    },
    InventoryParsed {
        boundary: BootNamespaceObservationBoundary,
        entries: usize,
    },
    RawEntryObserved {
        boundary: BootNamespaceObservationBoundary,
        index: usize,
    },
    LookupObserved {
        boundary: BootNamespaceObservationBoundary,
        request_index: usize,
        component_index: usize,
        present: bool,
    },
    RegularHashComplete {
        boundary: BootNamespaceObservationBoundary,
        request_index: usize,
    },
    ActualRead {
        request_index: usize,
        offset: u64,
        offered: usize,
    },
    NodeReleased {
        identity: BootNamespaceNodeIdentity,
    },
    Complete,
}

pub(super) trait RetainedBootNamespaceHook {
    fn emit(&mut self, event: FixtureRetainedBootNamespaceProtocolEvent) -> io::Result<()>;
}

pub(super) struct NoopRetainedBootNamespaceHook;

impl RetainedBootNamespaceHook for NoopRetainedBootNamespaceHook {
    fn emit(&mut self, _event: FixtureRetainedBootNamespaceProtocolEvent) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
pub(super) struct FixtureHook<'a, Hook>(pub(super) &'a mut Hook);

#[cfg(test)]
impl<Hook> RetainedBootNamespaceHook for FixtureHook<'_, Hook>
where
    Hook: FnMut(FixtureRetainedBootNamespaceProtocolEvent) -> io::Result<()>,
{
    fn emit(&mut self, event: FixtureRetainedBootNamespaceProtocolEvent) -> io::Result<()> {
        (self.0)(event)
    }
}
