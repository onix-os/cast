use std::time::{Duration, Instant};

use xxhash_rust::xxh3::xxh3_128;

use super::*;

pub(super) const ROOT: BootNamespaceNodeIdentity = BootNamespaceNodeIdentity::new(7, 100, 40);
pub(super) const FILE_A: BootNamespaceNodeIdentity = BootNamespaceNodeIdentity::new(7, 101, 40);
pub(super) const FILE_B: BootNamespaceNodeIdentity = BootNamespaceNodeIdentity::new(7, 102, 40);
pub(super) const DIRECTORY_A: BootNamespaceNodeIdentity = BootNamespaceNodeIdentity::new(7, 103, 40);
pub(super) const NESTED_FILE: BootNamespaceNodeIdentity = BootNamespaceNodeIdentity::new(7, 104, 40);
pub(super) const DIRECTORY_B: BootNamespaceNodeIdentity = BootNamespaceNodeIdentity::new(7, 105, 40);

pub(super) fn digest(content: &[u8]) -> u128 {
    xxh3_128(content)
}

pub(super) fn request<'a>(relative_path: &'a str, expected: &[u8]) -> BootNamespaceRequest<'a> {
    BootNamespaceRequest::new(relative_path, expected.len() as u64, digest(expected))
}

pub(super) fn witness(identity: BootNamespaceNodeIdentity, content: &[u8]) -> BootNamespaceRegularWitness {
    BootNamespaceRegularWitness {
        identity,
        length: content.len() as u64,
        digest: digest(content),
        version: 1,
    }
}

pub(super) fn entry(
    raw_name: impl Into<Vec<u8>>,
    identity: BootNamespaceNodeIdentity,
    kind: BootNamespaceNodeKind,
) -> FixtureDirectoryEntry {
    FixtureDirectoryEntry::new(raw_name, identity, kind)
}

pub(super) fn present(identity: BootNamespaceNodeIdentity, kind: BootNamespaceNodeKind) -> BootNamespaceLookup {
    BootNamespaceLookup::Present { identity, kind }
}

pub(super) fn empty_fixture(expected: Vec<FixtureExpectedStream>) -> FixtureBootNamespace {
    FixtureBootNamespace::new(
        ROOT,
        vec![FixtureDirectory::stable(ROOT, Vec::new())],
        Vec::new(),
        expected,
        Instant::now(),
    )
}

pub(super) fn one_file_fixture(raw_name: &[u8], actual: &[u8], expected: &[u8]) -> FixtureBootNamespace {
    FixtureBootNamespace::new(
        ROOT,
        vec![FixtureDirectory::stable(
            ROOT,
            vec![entry(raw_name, FILE_A, BootNamespaceNodeKind::Regular)],
        )],
        vec![FixtureRegularFile::stable(FILE_A, witness(FILE_A, actual), actual)],
        vec![FixtureExpectedStream::new(expected)],
        Instant::now(),
    )
}

pub(super) fn nested_file_fixture(actual: &[u8], expected: &[u8]) -> FixtureBootNamespace {
    FixtureBootNamespace::new(
        ROOT,
        vec![
            FixtureDirectory::stable(
                ROOT,
                vec![entry(b"loader".to_vec(), DIRECTORY_A, BootNamespaceNodeKind::Directory)],
            ),
            FixtureDirectory::stable(
                DIRECTORY_A,
                vec![entry(
                    b"entry.conf".to_vec(),
                    NESTED_FILE,
                    BootNamespaceNodeKind::Regular,
                )],
            ),
        ],
        vec![FixtureRegularFile::stable(
            NESTED_FILE,
            witness(NESTED_FILE, actual),
            actual,
        )],
        vec![FixtureExpectedStream::new(expected)],
        Instant::now(),
    )
}

pub(super) fn assess<'a>(
    requests: &'a [BootNamespaceRequest<'a>],
    fixture: &mut FixtureBootNamespace,
) -> Result<(ValidatedBootNamespaceAssessment, FixtureBootNamespaceUsage), BootNamespaceAssessmentError> {
    assess_with_limits(requests, BootNamespaceAssessmentLimits::default(), fixture)
}

pub(super) fn assess_with_limits<'a>(
    requests: &'a [BootNamespaceRequest<'a>],
    limits: BootNamespaceAssessmentLimits,
    fixture: &mut FixtureBootNamespace,
) -> Result<(ValidatedBootNamespaceAssessment, FixtureBootNamespaceUsage), BootNamespaceAssessmentError> {
    assess_fixture_boot_namespace_until(requests, limits, Instant::now() + Duration::from_secs(60), fixture)
}
