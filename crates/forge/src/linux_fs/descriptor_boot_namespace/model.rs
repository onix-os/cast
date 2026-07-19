pub(super) const HARD_MAX_REQUESTS: usize = 8_336;
pub(super) const HARD_MAX_COMPONENTS_PER_REQUEST: usize = 16;
pub(super) const HARD_MAX_PATH_BYTES: usize = 4_095;
pub(super) const HARD_MAX_TOTAL_PATH_BYTES: usize = 8 * 1024 * 1024;
pub(super) const HARD_MAX_COMPONENT_BYTES: usize = 255;
pub(super) const HARD_MAX_DIRECTORY_ENTRIES: usize = 65_536;
pub(super) const HARD_MAX_TOTAL_ENTRIES: usize = 262_144;
pub(super) const HARD_MAX_NAME_BYTES: usize = 255;
pub(super) const HARD_MAX_TOTAL_NAME_BYTES: usize = 8 * 1024 * 1024;
pub(super) const HARD_MAX_READ_BYTES: u64 = 20 * 1024 * 1024 * 1024 + 2 * HARD_MAX_REQUESTS as u64;
pub(super) const HARD_MAX_WORK: usize = 64_000_000;
pub(super) const HARD_MAX_DESCRIPTORS: usize = 32;
pub(super) const HARD_MAX_ALLOCATIONS: usize = 1_000_000;

/// Closed classification of one requested destination.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum BootNamespaceDestinationState {
    Absent,
    Exact,
    Different,
}

/// Canonical request metadata plus the expected stream's scalar identity.
///
/// The stream itself belongs to the injected observer. This value therefore
/// owns no file, descriptor, reader, path object, reopen closure, or mutation
/// capability.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct BootNamespaceRequest<'a> {
    relative_path: &'a str,
    expected_length: u64,
    expected_digest: u128,
}

impl<'a> BootNamespaceRequest<'a> {
    pub(crate) const fn new(relative_path: &'a str, expected_length: u64, expected_digest: u128) -> Self {
        Self {
            relative_path,
            expected_length,
            expected_digest,
        }
    }

    pub(super) const fn relative_path(self) -> &'a str {
        self.relative_path
    }

    pub(super) const fn expected_length(self) -> u64 {
        self.expected_length
    }

    pub(super) const fn expected_digest(self) -> u128 {
        self.expected_digest
    }
}

/// Independent operation limits for one collision-domain assessment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct BootNamespaceAssessmentLimits {
    pub(crate) max_requests: usize,
    pub(crate) max_components_per_request: usize,
    pub(crate) max_path_bytes: usize,
    pub(crate) max_total_path_bytes: usize,
    pub(crate) max_component_bytes: usize,
    pub(crate) max_directory_entries: usize,
    pub(crate) max_total_entries: usize,
    pub(crate) max_name_bytes: usize,
    pub(crate) max_total_name_bytes: usize,
    pub(crate) max_read_bytes: u64,
    pub(crate) max_work: usize,
    pub(crate) max_descriptors: usize,
    pub(crate) max_allocations: usize,
}

impl Default for BootNamespaceAssessmentLimits {
    fn default() -> Self {
        Self {
            max_requests: HARD_MAX_REQUESTS,
            max_components_per_request: HARD_MAX_COMPONENTS_PER_REQUEST,
            max_path_bytes: HARD_MAX_PATH_BYTES,
            max_total_path_bytes: HARD_MAX_TOTAL_PATH_BYTES,
            max_component_bytes: HARD_MAX_COMPONENT_BYTES,
            max_directory_entries: HARD_MAX_DIRECTORY_ENTRIES,
            max_total_entries: HARD_MAX_TOTAL_ENTRIES,
            max_name_bytes: HARD_MAX_NAME_BYTES,
            max_total_name_bytes: HARD_MAX_TOTAL_NAME_BYTES,
            max_read_bytes: HARD_MAX_READ_BYTES,
            max_work: HARD_MAX_WORK,
            max_descriptors: HARD_MAX_DESCRIPTORS,
            max_allocations: HARD_MAX_ALLOCATIONS,
        }
    }
}

/// Scalar-only ordered result. States remain in exact request order.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct ValidatedBootNamespaceAssessment {
    states: Vec<BootNamespaceDestinationState>,
}

impl ValidatedBootNamespaceAssessment {
    pub(super) fn new(states: Vec<BootNamespaceDestinationState>) -> Self {
        Self { states }
    }

    pub(crate) fn states(&self) -> &[BootNamespaceDestinationState] {
        &self.states
    }
}

/// Scalar accounting exposed only to injected tests for exact N/N-1 limits.
#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct FixtureBootNamespaceUsage {
    pub(crate) work: usize,
    pub(crate) allocations: usize,
    pub(crate) entries: usize,
    pub(crate) path_bytes: usize,
    pub(crate) name_bytes: usize,
    pub(crate) read_bytes: u64,
    pub(crate) peak_descriptors: usize,
}
