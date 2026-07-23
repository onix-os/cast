pub(super) const HARD_MAX_ASSESSMENT_BYTES: u64 = 16 * 1024 * 1024 * 1024;
pub(super) const HARD_MAX_ASSESSMENT_READ_CALLS: usize = 8 * 1024 * 1024;

/// Receipt-derived scalar identity for one canonical leaf below a retained
/// boot-publication parent.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RetainedBootLeafAssessmentRequest<'a> {
    canonical_leaf: &'a str,
    expected_length: u64,
    expected_xxh3: u128,
    expected_sha256: [u8; 32],
}

impl<'a> RetainedBootLeafAssessmentRequest<'a> {
    pub(crate) const fn new(
        canonical_leaf: &'a str,
        expected_length: u64,
        expected_xxh3: u128,
        expected_sha256: [u8; 32],
    ) -> Self {
        Self {
            canonical_leaf,
            expected_length,
            expected_xxh3,
            expected_sha256,
        }
    }

    pub(crate) const fn canonical_leaf(self) -> &'a str {
        self.canonical_leaf
    }

    pub(crate) const fn expected_length(self) -> u64 {
        self.expected_length
    }

    pub(crate) const fn expected_xxh3(self) -> u128 {
        self.expected_xxh3
    }

    pub(crate) const fn expected_sha256(self) -> [u8; 32] {
        self.expected_sha256
    }
}

/// Independent resource ceilings for one read-only leaf assessment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RetainedBootLeafAssessmentLimits {
    pub(crate) max_read_bytes: u64,
    pub(crate) max_read_calls: usize,
}

impl Default for RetainedBootLeafAssessmentLimits {
    fn default() -> Self {
        Self {
            max_read_bytes: HARD_MAX_ASSESSMENT_BYTES,
            max_read_calls: HARD_MAX_ASSESSMENT_READ_CALLS,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RetainedBootLeafAssessmentState {
    Absent,
    Exact,
    Different,
}

#[derive(Debug, Eq, PartialEq)]
struct ExactFileIdentity {
    device: u64,
    inode: u64,
    mount_id: u64,
}

/// Opaque scalar evidence from one descriptor-rooted bounded observation.
///
/// Its fields and constructor stay private to the assessor. In particular,
/// receipt scalars alone cannot manufacture an `Exact` result or a file
/// identity. This is bounded observation evidence, not mutation authority and
/// not a claim that the namespace remains unchanged after the call.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct ValidatedRetainedBootLeafAssessment {
    state: RetainedBootLeafAssessmentState,
    assessment_root_device: u64,
    assessment_root_inode: u64,
    assessment_root_mount_id: u64,
    parent_components: Box<[Box<str>]>,
    retained_parent: Option<ExactFileIdentity>,
    canonical_leaf: Box<str>,
    expected_length: u64,
    expected_xxh3: u128,
    expected_sha256: [u8; 32],
    exact_file: Option<ExactFileIdentity>,
}

impl ValidatedRetainedBootLeafAssessment {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        state: RetainedBootLeafAssessmentState,
        assessment_root: (u64, u64, u64),
        parent_components: Box<[Box<str>]>,
        retained_parent_identity: Option<(u64, u64, u64)>,
        canonical_leaf: Box<str>,
        expected_length: u64,
        expected_xxh3: u128,
        expected_sha256: [u8; 32],
        exact_file_identity: Option<(u64, u64, u64)>,
    ) -> Self {
        debug_assert_eq!(state == RetainedBootLeafAssessmentState::Exact, exact_file_identity.is_some());
        let retained_parent = retained_parent_identity.map(file_identity_from_tuple);
        let exact_file = exact_file_identity.map(file_identity_from_tuple);
        Self {
            state,
            assessment_root_device: assessment_root.0,
            assessment_root_inode: assessment_root.1,
            assessment_root_mount_id: assessment_root.2,
            parent_components,
            retained_parent,
            canonical_leaf,
            expected_length,
            expected_xxh3,
            expected_sha256,
            exact_file,
        }
    }

    pub(crate) const fn state(&self) -> RetainedBootLeafAssessmentState {
        self.state
    }

    pub(crate) const fn assessment_root_device(&self) -> u64 {
        self.assessment_root_device
    }

    pub(crate) const fn assessment_root_inode(&self) -> u64 {
        self.assessment_root_inode
    }

    pub(crate) const fn assessment_root_mount_id(&self) -> u64 {
        self.assessment_root_mount_id
    }

    pub(crate) fn parent_components(&self) -> impl ExactSizeIterator<Item = &str> {
        self.parent_components.iter().map(Box::as_ref)
    }

    pub(crate) const fn retained_parent_device(&self) -> Option<u64> {
        match &self.retained_parent {
            Some(identity) => Some(identity.device),
            None => None,
        }
    }

    pub(crate) const fn retained_parent_inode(&self) -> Option<u64> {
        match &self.retained_parent {
            Some(identity) => Some(identity.inode),
            None => None,
        }
    }

    pub(crate) const fn retained_parent_mount_id(&self) -> Option<u64> {
        match &self.retained_parent {
            Some(identity) => Some(identity.mount_id),
            None => None,
        }
    }

    pub(crate) fn canonical_leaf(&self) -> &str {
        &self.canonical_leaf
    }

    pub(crate) const fn expected_length(&self) -> u64 {
        self.expected_length
    }

    pub(crate) const fn expected_xxh3(&self) -> u128 {
        self.expected_xxh3
    }

    pub(crate) const fn expected_sha256(&self) -> [u8; 32] {
        self.expected_sha256
    }

    pub(crate) const fn exact_file_device(&self) -> Option<u64> {
        match &self.exact_file {
            Some(identity) => Some(identity.device),
            None => None,
        }
    }

    pub(crate) const fn exact_file_inode(&self) -> Option<u64> {
        match &self.exact_file {
            Some(identity) => Some(identity.inode),
            None => None,
        }
    }

    pub(crate) const fn exact_file_mount_id(&self) -> Option<u64> {
        match &self.exact_file {
            Some(identity) => Some(identity.mount_id),
            None => None,
        }
    }
}

fn file_identity_from_tuple((device, inode, mount_id): (u64, u64, u64)) -> ExactFileIdentity {
    ExactFileIdentity {
        device,
        inode,
        mount_id,
    }
}
