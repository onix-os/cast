use super::super::boot_file_publication::{
    AttachmentIdentity, RetainedBootFilePublicationRequest,
    destination::FileIdentity,
};

/// Receipt-correlated namespace owner used only to derive collision-resistant
/// private names. This inert value grants no filesystem authority by itself.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RetainedBootFileMutationFingerprint([u8; 32]);

impl RetainedBootFileMutationFingerprint {
    pub(crate) const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub(crate) const fn as_bytes(self) -> [u8; 32] {
        self.0
    }
}

/// One exact predecessor-to-successor replacement request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RetainedBootFileReplacementRequest<'a> {
    installed: RetainedBootFilePublicationRequest<'a>,
    replacement: RetainedBootFilePublicationRequest<'a>,
    owner: RetainedBootFileMutationFingerprint,
}

impl<'a> RetainedBootFileReplacementRequest<'a> {
    pub(crate) const fn new(
        installed: RetainedBootFilePublicationRequest<'a>,
        replacement: RetainedBootFilePublicationRequest<'a>,
        owner: RetainedBootFileMutationFingerprint,
    ) -> Self {
        Self {
            installed,
            replacement,
            owner,
        }
    }

    pub(crate) const fn installed(self) -> RetainedBootFilePublicationRequest<'a> {
        self.installed
    }

    pub(crate) const fn replacement(self) -> RetainedBootFilePublicationRequest<'a> {
        self.replacement
    }

    pub(crate) const fn owner(self) -> RetainedBootFileMutationFingerprint {
        self.owner
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ExactContent {
    pub(super) length: u64,
    pub(super) xxh3: u128,
    pub(super) sha256: [u8; 32],
}

impl ExactContent {
    pub(super) const fn from_request(request: RetainedBootFilePublicationRequest<'_>) -> Self {
        Self {
            length: request.expected_length(),
            xxh3: request.expected_xxh3(),
            sha256: request.expected_sha256(),
        }
    }

    pub(super) fn request<'leaf>(self, leaf: &'leaf str) -> RetainedBootFilePublicationRequest<'leaf> {
        RetainedBootFilePublicationRequest::new(leaf, self.length, self.xxh3, self.sha256)
    }
}

/// Non-cloneable exact authority for an applied successor/rollback-sidecar pair.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct ValidatedRetainedBootFileReplacement {
    pub(super) destination: AttachmentIdentity,
    pub(super) canonical_leaf: Box<str>,
    pub(super) sidecar_leaf: Box<str>,
    pub(super) installed: ExactContent,
    pub(super) replacement: ExactContent,
    pub(super) installed_file: FileIdentity,
    pub(super) replacement_file: FileIdentity,
    pub(super) owner: RetainedBootFileMutationFingerprint,
}

impl ValidatedRetainedBootFileReplacement {
    pub(crate) fn canonical_leaf(&self) -> &str {
        &self.canonical_leaf
    }

    pub(crate) fn sidecar_leaf(&self) -> &str {
        &self.sidecar_leaf
    }

    pub(crate) const fn installed_file_inode(&self) -> u64 {
        self.installed_file.inode
    }

    pub(crate) const fn replacement_file_inode(&self) -> u64 {
        self.replacement_file.inode
    }
}

/// Non-cloneable exact authority after rollback restored the predecessor.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct ValidatedRetainedBootFileRestoration {
    pub(super) replacement: ValidatedRetainedBootFileReplacement,
}

impl ValidatedRetainedBootFileRestoration {
    pub(crate) fn canonical_leaf(&self) -> &str {
        self.replacement.canonical_leaf()
    }

    pub(crate) fn sidecar_leaf(&self) -> &str {
        self.replacement.sidecar_leaf()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RetainedBootFileSidecarCleanupOutcome {
    RemovedInstalledRollback,
    RemovedDisplacedReplacement,
}

/// Receipt-correlated request to remove one predecessor-only canonical leaf.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RetainedBootFileStaleCleanupRequest<'a> {
    stale: RetainedBootFilePublicationRequest<'a>,
    owner: RetainedBootFileMutationFingerprint,
}

impl<'a> RetainedBootFileStaleCleanupRequest<'a> {
    pub(crate) const fn new(
        stale: RetainedBootFilePublicationRequest<'a>,
        owner: RetainedBootFileMutationFingerprint,
    ) -> Self {
        Self { stale, owner }
    }

    pub(crate) const fn stale(self) -> RetainedBootFilePublicationRequest<'a> {
        self.stale
    }

    pub(crate) const fn owner(self) -> RetainedBootFileMutationFingerprint {
        self.owner
    }
}

/// Non-cloneable fresh exact authority for one predecessor-only leaf.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct AuthenticatedRetainedBootFileStaleCleanup {
    pub(super) destination: AttachmentIdentity,
    pub(super) canonical_leaf: Box<str>,
    pub(super) private_leaf: Box<str>,
    pub(super) content: ExactContent,
    pub(super) file: FileIdentity,
    pub(super) location: StaleFileLocation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum StaleFileLocation {
    Canonical,
    Detached,
}

impl AuthenticatedRetainedBootFileStaleCleanup {
    pub(crate) fn canonical_leaf(&self) -> &str {
        &self.canonical_leaf
    }

    pub(crate) fn private_leaf(&self) -> &str {
        &self.private_leaf
    }

    pub(crate) const fn file_inode(&self) -> u64 {
        self.file.inode
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RetainedBootFileStaleCleanupOutcome {
    RemovedPredecessorOutput,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum RetainedBootFileAppliedSidecarCleanupState {
    Pending(ValidatedRetainedBootFileReplacement),
    AlreadyClean,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum RetainedBootFileRestoredSidecarCleanupState {
    Pending(ValidatedRetainedBootFileRestoration),
    AlreadyClean,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum RetainedBootFileStaleCleanupState {
    Canonical(AuthenticatedRetainedBootFileStaleCleanup),
    Detached(AuthenticatedRetainedBootFileStaleCleanup),
    AlreadyClean,
}
