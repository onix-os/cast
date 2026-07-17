//! Sealed content-derived evidence for the delegated execution matrix.
//!
//! The harness-free binary can ask Mason to run the matrix, but it cannot
//! construct, inspect, or serialize this evidence. Only the fixture loop below
//! can finish the selection-aware builder, and only a complete required matrix
//! can publish the CI proof.

use std::collections::BTreeMap;

use serde::Serialize;
use sha2::{Digest, Sha256};

use super::{ExecutionFixtureSelection, Publication, REQUIRED_EXECUTION_FIXTURES, WriteOutcome};

#[path = "execution_evidence/ledger.rs"]
mod ledger;
#[path = "execution_evidence/proof.rs"]
mod proof;

const EXPECTED_EXECUTIONS: u64 = 32;
const EXPECTED_BUNDLE_VALIDATIONS: u64 = 48;
const EXPECTED_STONES: u64 = 104;
const EXPECTED_MANIFESTS: u64 = 32;
const EXPECTED_ARTIFACTS: u64 = 136;
const MAX_CANONICAL_PLAN_BYTES: usize = 16 * 1024 * 1024;

/// Opaque result of the contentful execution path.
///
/// `CapabilityUnavailable` is only constructible before the first fixture has
/// executed. It deliberately carries no evidence and can never publish proof.
pub(crate) enum DelegatedExecutionOutcome {
    Completed(ExecutionMatrixEvidence),
    CapabilityUnavailable,
}

impl DelegatedExecutionOutcome {
    /// Publish only when the caller already classified this invocation as the
    /// required all-fixture lane. Every other successful invocation discards
    /// its sealed local observations without manufacturing a matrix receipt.
    #[cfg(feature = "delegated-fixture-test-support")]
    pub(crate) fn publish_if_required(self, proof_required: bool) {
        match (self, proof_required) {
            (Self::Completed(evidence), true) => evidence.publish_required(),
            (Self::Completed(_), false) => {}
            (Self::CapabilityUnavailable, false) => {}
            (Self::CapabilityUnavailable, true) => {
                panic!("required all-fixture execution lost capability without producing evidence")
            }
        }
    }
}

enum MatrixScope {
    Complete(MatrixTotals),
    Single,
}

pub(crate) struct ExecutionMatrixEvidence {
    scope: MatrixScope,
    fixtures: Vec<FixtureEvidence>,
}

impl ExecutionMatrixEvidence {
    #[cfg(feature = "delegated-fixture-test-support")]
    fn publish_required(self) {
        let MatrixScope::Complete(totals) = &self.scope else {
            panic!("a single-fixture execution cannot publish all-fixture proof");
        };
        proof::publish(&self.fixtures, totals);
    }
}

pub(super) struct ExecutionEvidenceBuilder {
    expected: Vec<&'static str>,
    complete_matrix: bool,
    fixtures: Vec<FixtureEvidence>,
}

impl ExecutionEvidenceBuilder {
    pub(super) fn new(selection: ExecutionFixtureSelection) -> Self {
        let (expected, complete_matrix) = match selection {
            ExecutionFixtureSelection::All => (REQUIRED_EXECUTION_FIXTURES.to_vec(), true),
            ExecutionFixtureSelection::One(name) => (vec![name], false),
        };
        Self {
            fixtures: Vec::with_capacity(expected.len()),
            expected,
            complete_matrix,
        }
    }

    pub(super) fn push(&mut self, inputs: FixtureEvidenceInputs<'_>) {
        let expected = self
            .expected
            .get(self.fixtures.len())
            .unwrap_or_else(|| panic!("execution evidence received surplus fixture {:?}", inputs.name));
        assert_eq!(
            inputs.name, *expected,
            "execution evidence fixture order drifted from its selected canonical matrix"
        );
        self.fixtures.push(FixtureEvidence::capture(inputs));
    }

    pub(super) fn capability_unavailable(self) -> DelegatedExecutionOutcome {
        assert!(
            self.fixtures.is_empty(),
            "capability loss after partial execution cannot be reclassified as a skip"
        );
        DelegatedExecutionOutcome::CapabilityUnavailable
    }

    pub(super) fn finish(self) -> DelegatedExecutionOutcome {
        let actual = self
            .fixtures
            .iter()
            .map(|fixture| fixture.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            actual, self.expected,
            "execution evidence did not cover its exact selection"
        );
        let scope = if self.complete_matrix {
            MatrixScope::Complete(MatrixTotals::required(&self.fixtures))
        } else {
            assert_eq!(
                self.fixtures.len(),
                1,
                "single selection produced a non-single evidence set"
            );
            MatrixScope::Single
        };
        DelegatedExecutionOutcome::Completed(ExecutionMatrixEvidence {
            scope,
            fixtures: self.fixtures,
        })
    }
}

pub(super) struct FixtureEvidenceInputs<'a> {
    pub(super) name: &'a str,
    pub(super) first_plan: &'a [u8],
    pub(super) first_derivation_id: &'a str,
    pub(super) repeat_plan: &'a [u8],
    pub(super) repeat_derivation_id: &'a str,
    pub(super) first_build_lock: &'a [u8],
    pub(super) first_build_lock_outcome: Option<WriteOutcome>,
    pub(super) repeat_build_lock: &'a [u8],
    pub(super) repeat_build_lock_outcome: Option<WriteOutcome>,
    pub(super) first_publication: Publication,
    pub(super) repeat_publication: Publication,
    pub(super) published_after_first: &'a BTreeMap<String, Vec<u8>>,
    pub(super) staged_after_repeat: &'a BTreeMap<String, Vec<u8>>,
    pub(super) published_after_repeat: &'a BTreeMap<String, Vec<u8>>,
}

#[derive(Serialize)]
struct FixtureEvidence {
    name: String,
    plans: PlanObservations,
    build_locks: BuildLockObservations,
    publications: PublicationObservations,
    artifacts: ArtifactInventory,
    bundle_observations: Vec<BundleObservation>,
}

impl FixtureEvidence {
    fn capture(inputs: FixtureEvidenceInputs<'_>) -> Self {
        assert_eq!(
            inputs.first_plan, inputs.repeat_plan,
            "{}: repeated plan bytes changed while capturing evidence",
            inputs.name
        );
        let first_plan = PlanObservation::capture(inputs.first_plan, inputs.first_derivation_id);
        let repeat_plan = PlanObservation::capture(inputs.repeat_plan, inputs.repeat_derivation_id);
        assert_eq!(
            first_plan, repeat_plan,
            "{}: repeated plan identity changed while capturing evidence",
            inputs.name
        );

        assert_eq!(
            inputs.first_build_lock, inputs.repeat_build_lock,
            "{}: repeated build.lock.glu bytes changed while capturing evidence",
            inputs.name
        );
        assert_eq!(
            inputs.first_build_lock_outcome,
            Some(WriteOutcome::Written),
            "{}: first proof lock observation was not freshly written",
            inputs.name
        );
        assert_eq!(
            inputs.repeat_build_lock_outcome, None,
            "{}: repeated proof lock observation rewrote build.lock.glu",
            inputs.name
        );
        let first_lock = BuildLockObservation::capture("written", inputs.first_build_lock);
        let repeat_lock = BuildLockObservation::capture("unchanged", inputs.repeat_build_lock);
        assert_eq!(first_lock.byte_count, repeat_lock.byte_count);
        assert_eq!(first_lock.sha256, repeat_lock.sha256);

        assert_eq!(
            inputs.published_after_first, inputs.staged_after_repeat,
            "{}: repeated staged bundle changed emitted bytes",
            inputs.name
        );
        assert_eq!(
            inputs.published_after_first, inputs.published_after_repeat,
            "{}: repeated execution changed the published bundle",
            inputs.name
        );

        let artifacts = ledger::capture_inventory(inputs.name, inputs.published_after_first);
        let bundle_observations = [
            ("published-after-first", inputs.published_after_first),
            ("staged-after-repeat", inputs.staged_after_repeat),
            ("published-after-repeat", inputs.published_after_repeat),
        ]
        .into_iter()
        .map(|(point, bundle)| ledger::observe_bundle(point, bundle))
        .collect::<Vec<_>>();
        for observation in &bundle_observations {
            assert_eq!(observation.artifact_count, artifacts.artifact_count);
            assert_eq!(observation.total_bytes, artifacts.total_bytes);
            assert_eq!(observation.ledger_sha256, artifacts.ledger_sha256);
        }
        assert_eq!(
            inputs.first_publication,
            Publication::Published,
            "{}: first proof publication was not new",
            inputs.name
        );
        assert_eq!(
            inputs.repeat_publication,
            Publication::Reused,
            "{}: repeated proof publication was not reused",
            inputs.name
        );

        Self {
            name: inputs.name.to_owned(),
            plans: PlanObservations {
                first: first_plan,
                repeat: repeat_plan,
            },
            build_locks: BuildLockObservations {
                first: first_lock,
                repeat: repeat_lock,
            },
            publications: PublicationObservations {
                first: "published",
                repeat: "reused",
            },
            artifacts,
            bundle_observations,
        }
    }
}

#[derive(Debug, PartialEq, Eq, Serialize)]
struct PlanObservation {
    byte_count: u64,
    sha256: String,
    derivation_id: String,
}

impl PlanObservation {
    fn capture(bytes: &[u8], derivation_id: &str) -> Self {
        assert!(!bytes.is_empty(), "canonical plan evidence is empty");
        assert!(
            bytes.len() <= MAX_CANONICAL_PLAN_BYTES,
            "canonical plan evidence exceeds its {MAX_CANONICAL_PLAN_BYTES}-byte boundary"
        );
        require_sha256(derivation_id, "derivation ID");
        let sha256 = digest(bytes);
        assert_eq!(sha256, derivation_id, "derivation ID is not the canonical plan SHA-256");
        Self {
            byte_count: u64::try_from(bytes.len()).expect("canonical plan size exceeds u64"),
            sha256,
            derivation_id: derivation_id.to_owned(),
        }
    }
}

#[derive(Serialize)]
struct PlanObservations {
    first: PlanObservation,
    repeat: PlanObservation,
}

#[derive(Serialize)]
struct BuildLockObservation {
    write_outcome: &'static str,
    byte_count: u64,
    sha256: String,
}

impl BuildLockObservation {
    fn capture(write_outcome: &'static str, bytes: &[u8]) -> Self {
        assert!(!bytes.is_empty(), "build lock evidence is empty");
        assert!(
            bytes.len() <= gluon_config::Limits::default().max_source_bytes,
            "build lock evidence exceeds the evaluator source boundary"
        );
        Self {
            write_outcome,
            byte_count: u64::try_from(bytes.len()).expect("build lock size exceeds u64"),
            sha256: digest(bytes),
        }
    }
}

#[derive(Serialize)]
struct BuildLockObservations {
    first: BuildLockObservation,
    repeat: BuildLockObservation,
}

#[derive(Serialize)]
struct PublicationObservations {
    first: &'static str,
    repeat: &'static str,
}

#[derive(Serialize)]
struct ArtifactInventory {
    stone_count: u64,
    manifest_count: u64,
    artifact_count: u64,
    total_bytes: u64,
    ledger_sha256: String,
    entries: Vec<ArtifactEvidence>,
}

#[derive(Serialize)]
struct ArtifactEvidence {
    name: String,
    kind: &'static str,
    byte_count: u64,
    sha256: String,
}

#[derive(Serialize)]
struct BundleObservation {
    point: &'static str,
    artifact_count: u64,
    total_bytes: u64,
    ledger_sha256: String,
}

#[derive(Serialize)]
struct MatrixTotals {
    fixture_count: u64,
    execution_count: u64,
    bundle_validation_count: u64,
    stone_count: u64,
    manifest_count: u64,
    artifact_count: u64,
    artifact_bytes: u64,
}

impl MatrixTotals {
    fn required(fixtures: &[FixtureEvidence]) -> Self {
        assert_eq!(
            fixtures.iter().map(|fixture| fixture.name.as_str()).collect::<Vec<_>>(),
            REQUIRED_EXECUTION_FIXTURES,
            "required evidence fixtures are not the exact canonical matrix"
        );
        let fixture_count = u64::try_from(fixtures.len()).expect("fixture count exceeds u64");
        let stone_count = checked_sum(
            fixtures.iter().map(|fixture| fixture.artifacts.stone_count),
            "Stone count",
        );
        let manifest_count = checked_sum(
            fixtures.iter().map(|fixture| fixture.artifacts.manifest_count),
            "manifest count",
        );
        let artifact_count = checked_sum(
            fixtures.iter().map(|fixture| fixture.artifacts.artifact_count),
            "artifact count",
        );
        let artifact_bytes = checked_sum(
            fixtures.iter().map(|fixture| fixture.artifacts.total_bytes),
            "artifact byte count",
        );
        let execution_count = fixture_count.checked_mul(2).expect("execution count overflowed");
        let bundle_validation_count = fixture_count
            .checked_mul(3)
            .expect("bundle validation count overflowed");
        assert_eq!(fixture_count, u64::try_from(REQUIRED_EXECUTION_FIXTURES.len()).unwrap());
        assert_eq!(execution_count, EXPECTED_EXECUTIONS);
        assert_eq!(bundle_validation_count, EXPECTED_BUNDLE_VALIDATIONS);
        assert_eq!(stone_count, EXPECTED_STONES);
        assert_eq!(manifest_count, EXPECTED_MANIFESTS);
        assert_eq!(artifact_count, EXPECTED_ARTIFACTS);
        assert!(artifact_bytes > 0, "required matrix artifact byte total is empty");
        Self {
            fixture_count,
            execution_count,
            bundle_validation_count,
            stone_count,
            manifest_count,
            artifact_count,
            artifact_bytes,
        }
    }
}

fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn require_sha256(value: &str, field: &str) {
    assert_eq!(value.len(), 64, "{field} must contain one SHA-256 digest");
    assert!(
        value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "{field} must be canonical lowercase hexadecimal"
    );
}

fn checked_sum(mut values: impl Iterator<Item = u64>, label: &str) -> u64 {
    values
        .try_fold(0_u64, u64::checked_add)
        .unwrap_or_else(|| panic!("{label} overflowed"))
}

#[cfg(test)]
#[path = "execution_evidence/tests.rs"]
mod tests;
