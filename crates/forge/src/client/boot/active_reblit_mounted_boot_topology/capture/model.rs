use std::{io, marker::PhantomData, rc::Rc, time::Instant};

#[cfg(test)]
use std::cell::{Cell, RefCell};

use crate::{
    Installation,
    client::active_reblit_boot_topology_intent::PreparedActiveReblitBootTopologyIntent,
    linux_fs::{
        mount_namespace::{AuthenticatedMountInfoSnapshot, PreparedMountNamespaceAnchor, PreparedTaskRootedAttachment},
        sysfs_identity::PreparedSysfsPartitionIdentity,
    },
};

use super::super::{ActiveReblitMountedBootTopology, BoundActiveReblitMountedBootTopology};

/// Exactly one descriptor-retained target capability.
pub(super) struct PreparedMountedBootTarget {
    pub(super) attachment: PreparedTaskRootedAttachment,
    pub(super) sysfs: PreparedSysfsPartitionIdentity,
}

/// Closed retained shape: alias intent physically stores only the ESP target.
pub(super) enum PreparedMountedBootTargets {
    BootAliasesEsp {
        esp: PreparedMountedBootTarget,
    },
    DistinctXbootldr {
        esp: PreparedMountedBootTarget,
        xbootldr: PreparedMountedBootTarget,
    },
}

pub(super) enum MountInfoSource {
    Production,
    #[cfg(test)]
    Fixture(FixtureMountInfoFeed),
}

/// Test-owned in-memory mountinfo input.  It never contains a path or reader.
#[cfg(test)]
#[derive(Clone)]
pub(in crate::client) struct FixtureMountInfoFeed {
    bytes: Rc<RefCell<Vec<u8>>>,
    reads: Rc<Cell<usize>>,
}

#[cfg(test)]
impl FixtureMountInfoFeed {
    pub(in crate::client) fn new(bytes: impl Into<Vec<u8>>) -> Self {
        Self {
            bytes: Rc::new(RefCell::new(bytes.into())),
            reads: Rc::new(Cell::new(0)),
        }
    }

    pub(in crate::client) fn replace(&self, bytes: impl Into<Vec<u8>>) {
        *self.bytes.borrow_mut() = bytes.into();
    }

    pub(in crate::client) fn read_count(&self) -> usize {
        self.reads.get()
    }
}

impl MountInfoSource {
    pub(super) fn read_until(
        &self,
        anchor: &PreparedMountNamespaceAnchor,
        deadline: Instant,
    ) -> io::Result<AuthenticatedMountInfoSnapshot> {
        match self {
            Self::Production => anchor.read_current_thread_mountinfo_until(deadline),
            #[cfg(test)]
            Self::Fixture(feed) => {
                use crate::linux_fs::mount_namespace::FixtureMountInfoSnapshotLimits;

                feed.reads.set(feed.reads.get().saturating_add(1));
                let bytes = feed.bytes.borrow();
                let mut hook = |_| Ok(());
                anchor.read_fixture_mountinfo_bytes_with(
                    &bytes,
                    FixtureMountInfoSnapshotLimits::default(),
                    deadline,
                    &mut hook,
                )
            }
        }
    }
}

/// Non-cloneable, thread-bound authority retained across every observation.
pub(in crate::client) struct PreparedActiveReblitMountedBootTopology {
    pub(super) intent: PreparedActiveReblitBootTopologyIntent,
    pub(super) anchor: PreparedMountNamespaceAnchor,
    pub(super) targets: PreparedMountedBootTargets,
    pub(super) mountinfo_source: MountInfoSource,
    pub(super) facts: ActiveReblitMountedBootTopology,
}

impl std::fmt::Debug for PreparedActiveReblitMountedBootTopology {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreparedActiveReblitMountedBootTopology")
            .field("topology", &self.facts.bound())
            .field("evidence", &"retained; revalidation required")
            .finish_non_exhaustive()
    }
}

/// Scalar-only view after Pass1, Pass2, and Terminal all matched bootstrap.
pub(in crate::client) struct RevalidatedActiveReblitMountedBootTopology<'a> {
    pub(super) prepared: &'a PreparedActiveReblitMountedBootTopology,
    pub(super) _installation: &'a Installation,
    pub(super) deadline: Instant,
    pub(super) _same_thread: PhantomData<Rc<()>>,
}

impl std::fmt::Debug for RevalidatedActiveReblitMountedBootTopology<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RevalidatedActiveReblitMountedBootTopology")
            .field("topology", &self.topology())
            .finish_non_exhaustive()
    }
}

impl RevalidatedActiveReblitMountedBootTopology<'_> {
    pub(in crate::client) fn topology(&self) -> BoundActiveReblitMountedBootTopology<'_> {
        self.prepared.facts.bound()
    }

    pub(in crate::client) fn deadline(&self) -> Instant {
        self.deadline
    }
}
