use std::time::Instant;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct BootNamespaceNodeIdentity {
    pub(crate) device: u64,
    pub(crate) inode: u64,
    pub(crate) mount_id: u64,
}

impl BootNamespaceNodeIdentity {
    pub(crate) const fn new(device: u64, inode: u64, mount_id: u64) -> Self {
        Self {
            device,
            inode,
            mount_id,
        }
    }

    pub(super) const fn is_valid(self) -> bool {
        self.device != 0 && self.inode != 0 && self.mount_id != 0
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) enum BootNamespaceNodeKind {
    Directory,
    Regular,
    Symlink,
    Fifo,
    Socket,
    BlockDevice,
    CharacterDevice,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BootNamespaceObservationBoundary {
    Opening,
    Closing,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BootNamespaceLookup {
    Absent,
    Present {
        identity: BootNamespaceNodeIdentity,
        kind: BootNamespaceNodeKind,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct BootNamespaceRegularWitness {
    pub(crate) identity: BootNamespaceNodeIdentity,
    pub(crate) length: u64,
    pub(crate) digest: u128,
    pub(crate) version: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct BootNamespaceDirectoryEntryObservation {
    pub(super) name_length: usize,
    pub(super) identity: BootNamespaceNodeIdentity,
    pub(super) kind: BootNamespaceNodeKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct BootNamespaceObserverError;

pub(super) type ObserverResult<T> = Result<T, BootNamespaceObserverError>;

pub(super) trait BootNamespaceObserver {
    fn now(&mut self) -> Instant;

    fn before_allocation(&mut self, attempt: usize) -> ObserverResult<()>;

    fn root_identity(&mut self) -> ObserverResult<BootNamespaceNodeIdentity>;

    fn directory_entry_count(
        &mut self,
        directory: BootNamespaceNodeIdentity,
        boundary: BootNamespaceObservationBoundary,
    ) -> ObserverResult<usize>;

    fn directory_entry(
        &mut self,
        directory: BootNamespaceNodeIdentity,
        boundary: BootNamespaceObservationBoundary,
        index: usize,
        raw_name: &mut [u8],
    ) -> ObserverResult<BootNamespaceDirectoryEntryObservation>;

    fn lookup(
        &mut self,
        directory: BootNamespaceNodeIdentity,
        requested_name: &[u8],
        boundary: BootNamespaceObservationBoundary,
    ) -> ObserverResult<BootNamespaceLookup>;

    fn regular_witness(
        &mut self,
        identity: BootNamespaceNodeIdentity,
        boundary: BootNamespaceObservationBoundary,
    ) -> ObserverResult<BootNamespaceRegularWitness>;

    fn read_actual(
        &mut self,
        identity: BootNamespaceNodeIdentity,
        offset: u64,
        output: &mut [u8],
    ) -> ObserverResult<usize>;

    fn read_expected(&mut self, request_index: usize, offset: u64, output: &mut [u8]) -> ObserverResult<usize>;
}
