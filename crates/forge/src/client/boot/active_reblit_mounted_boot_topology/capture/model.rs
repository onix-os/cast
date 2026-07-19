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

#[cfg(test)]
use crate::linux_fs::descriptor_boot_filesystem::{
    BootFilesystemAuthenticationError, BootFilesystemObservationPhase, FIXTURE_MSDOS_SUPER_MAGIC,
    FixtureBootFilesystemIdentity, FixtureBootFilesystemLimits, FixtureBootFilesystemObservations,
    ValidatedBootFilesystemDescriptorEvidence, validate_fixture_boot_filesystem_authentication,
};

use super::super::{ActiveReblitMountedBootTopology, BoundActiveReblitMountedBootTopology};

/// Exactly one descriptor-retained target capability.
pub(super) struct PreparedMountedBootTarget {
    pub(super) attachment: PreparedTaskRootedAttachment,
    pub(super) sysfs: PreparedSysfsPartitionIdentity,
    pub(super) boot_filesystem_source: BootFilesystemEvidenceSource,
}

pub(super) enum BootFilesystemEvidenceSource {
    Production,
    #[cfg(test)]
    Fixture(FixtureBootFilesystemEvidenceFeed),
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

/// Test-owned scalar filesystem observations for exactly one retained target.
///
/// The feed contains no descriptor, path, reader, or mutation authority. Tests
/// can replace its next stable observation between coordinator passes without
/// mounting a FAT filesystem.
#[cfg(test)]
#[derive(Clone)]
pub(in crate::client) struct FixtureBootFilesystemEvidenceFeed {
    observations: Rc<Cell<FixtureBootFilesystemObservations>>,
    reads: Rc<Cell<usize>>,
}

#[cfg(test)]
impl FixtureBootFilesystemEvidenceFeed {
    pub(in crate::client) fn stable_msdos(device: u64, inode: u64) -> Self {
        Self {
            observations: Rc::new(Cell::new(stable_boot_filesystem_observations(
                device,
                inode,
                FIXTURE_MSDOS_SUPER_MAGIC,
            ))),
            reads: Rc::new(Cell::new(0)),
        }
    }

    pub(in crate::client) fn replace_stable(&self, device: u64, inode: u64, magic: nix::libc::c_long) {
        self.observations
            .set(stable_boot_filesystem_observations(device, inode, magic));
    }

    pub(in crate::client) fn read_count(&self) -> usize {
        self.reads.get()
    }

    pub(super) fn authenticate_until(
        &self,
        expected_device: u64,
        expected_inode: u64,
        deadline: Instant,
    ) -> Result<ValidatedBootFilesystemDescriptorEvidence, BootFilesystemAuthenticationError> {
        self.reads.set(self.reads.get().saturating_add(1));
        let mut clock = Instant::now;
        let mut hook = |_phase: BootFilesystemObservationPhase| Ok(());
        validate_fixture_boot_filesystem_authentication(
            self.observations.get(),
            expected_device,
            expected_inode,
            FixtureBootFilesystemLimits::default(),
            deadline,
            &mut clock,
            &mut hook,
        )
        .map(|(evidence, _usage)| evidence)
    }
}

#[cfg(test)]
fn stable_boot_filesystem_observations(
    device: u64,
    inode: u64,
    magic: nix::libc::c_long,
) -> FixtureBootFilesystemObservations {
    let identity = FixtureBootFilesystemIdentity {
        device,
        inode,
        kind: nix::libc::S_IFDIR,
    };
    FixtureBootFilesystemObservations {
        opening_identity: identity,
        opening_magic: magic,
        closing_magic: magic,
        closing_identity: identity,
    }
}

/// Fixture evidence follows the exact declarative target shape.
#[cfg(test)]
#[derive(Clone)]
pub(in crate::client) enum FixtureBootFilesystemEvidenceFeeds {
    BootAliasesEsp {
        esp: FixtureBootFilesystemEvidenceFeed,
    },
    DistinctXbootldr {
        esp: FixtureBootFilesystemEvidenceFeed,
        xbootldr: FixtureBootFilesystemEvidenceFeed,
    },
}

#[cfg(test)]
impl FixtureBootFilesystemEvidenceFeeds {
    pub(in crate::client) fn aliases_esp(esp: FixtureBootFilesystemEvidenceFeed) -> Self {
        Self::BootAliasesEsp { esp }
    }

    pub(in crate::client) fn distinct(
        esp: FixtureBootFilesystemEvidenceFeed,
        xbootldr: FixtureBootFilesystemEvidenceFeed,
    ) -> Self {
        Self::DistinctXbootldr { esp, xbootldr }
    }

    pub(super) fn source_for(&self, role: super::super::BootTargetRole) -> Option<BootFilesystemEvidenceSource> {
        use super::super::BootTargetRole;

        match (self, role) {
            (Self::BootAliasesEsp { esp }, BootTargetRole::Esp)
            | (Self::DistinctXbootldr { esp, .. }, BootTargetRole::Esp) => {
                Some(BootFilesystemEvidenceSource::Fixture(esp.clone()))
            }
            (Self::DistinctXbootldr { xbootldr, .. }, BootTargetRole::Xbootldr) => {
                Some(BootFilesystemEvidenceSource::Fixture(xbootldr.clone()))
            }
            (Self::BootAliasesEsp { .. }, BootTargetRole::Xbootldr) => None,
        }
    }
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
