use gluon_config::EvaluationIdentityValidationError;
use thiserror::Error;

use crate::{build_policy::layers::BuildPolicyOperation, spec::SourceUrlValidationError};

use super::super::{BuildLockValidationError, CompilerExecutableRole};

#[derive(Debug, Error)]
pub enum DerivationValidationError {
    #[error("schema_version: unsupported schema {found}; expected {expected}")]
    UnsupportedSchema { found: u32, expected: u32 },
    #[error("{field}: value must not be empty")]
    Empty { field: String },
    #[error("{field}: value contains an embedded NUL byte")]
    EmbeddedNul { field: String },
    #[error("{field}: environment name must match [A-Za-z_][A-Za-z0-9_]*")]
    InvalidEnvironmentName { field: String },
    #[error("{field}: process-data limit exceeded ({actual} {unit}; maximum {limit})")]
    LimitExceeded {
        field: String,
        actual: usize,
        limit: usize,
        unit: &'static str,
    },
    #[error("{field}: package name {value:?} must use only ASCII letters, digits, '+', '-', '.', or '_'")]
    InvalidPackageName { field: String, value: String },
    #[error("{field}: value {value:?} must be one normalized filename component")]
    InvalidArtifactComponent { field: String, value: String },
    #[error("package.version: version must start with an integer (found {value:?})")]
    InvalidPackageVersion { value: String },
    #[error("{field}: invalid evaluation identity: {source}")]
    InvalidEvaluationIdentity {
        field: String,
        #[source]
        source: EvaluationIdentityValidationError,
    },
    #[error("{field}: logical name {value:?} must be normalized and relative")]
    InvalidLogicalName { field: String, value: String },
    #[error("provenance.recipe.explicit_inputs_sha256 {recipe:?} does not match source_lock_digest {source_lock:?}")]
    RecipeSourceLockDigestMismatch { recipe: String, source_lock: String },
    #[error(
        "provenance.profiles[{duplicate_index}].logical_name: duplicate profile fragment {logical_name:?}; first declared at provenance.profiles[{first_index}]"
    )]
    DuplicateProfileLogicalName {
        logical_name: String,
        first_index: usize,
        duplicate_index: usize,
    },
    #[error("build_lock.profile.fingerprint: expected v2 profile aggregate {expected:?}, found {found:?}")]
    ProfileAggregateMismatch { expected: String, found: String },
    #[error("build_lock.policy.name: expected provenance policy {expected:?}, found {found:?}")]
    PolicyNameMismatch { expected: String, found: String },
    #[error("build_lock.policy.fingerprint: expected policy root aggregate {expected:?}, found {found:?}")]
    PolicyAggregateMismatch { expected: String, found: String },
    #[error(
        "provenance.policy.layers[{duplicate_index}].name: duplicate policy layer {name:?}; first declared at provenance.policy.layers[{first_index}]"
    )]
    DuplicatePolicyLayer {
        name: String,
        first_index: usize,
        duplicate_index: usize,
    },
    #[error("{field}: module origin {value:?} must be a normalized relative path")]
    InvalidPolicyOrigin { field: String, value: String },
    #[error("{field}: invalid {operation:?} transition: {reason}")]
    InvalidPolicyTransition {
        field: String,
        operation: BuildPolicyOperation,
        reason: &'static str,
    },
    #[error("provenance.policy.layers: ordered transitions never create a policy state")]
    MissingPolicyState,
    #[error(
        "provenance.policy.root.explicit_inputs_sha256: expected policy composition digest {expected:?}, found {found:?}"
    )]
    PolicyCompositionDigestMismatch { expected: String, found: String },
    #[error("package.build_release: value must be greater than zero")]
    ZeroBuildRelease,
    #[error("package.source_release: value must be greater than zero")]
    ZeroSourceRelease,
    #[error("execution.jobs: value must be greater than zero")]
    ZeroJobs,
    #[error("execution.root_materialization: package-manager state is not permitted in a frozen derivation")]
    PackageManagerRootMaterialization,
    #[error("execution.credentials: a frozen derivation must select isolated credentials")]
    UnspecifiedExecutionCredentials,
    #[error("execution.network: enabled networking is not permitted in a frozen derivation")]
    NetworkEnabled,
    #[error("package.architecture: unsupported Stone artifact architecture {value:?}; expected one of {supported}")]
    UnsupportedArtifactArchitecture { value: String, supported: String },
    #[error(
        "package.architecture: artifact architecture {artifact} does not match build_lock.target_platform.architecture {target}"
    )]
    ArtifactTargetArchitectureMismatch { artifact: String, target: String },
    #[error("sources: duplicate source order {order}")]
    DuplicateSourceOrder { order: u32 },
    #[error("sources[{index}].order: expected canonical order {index}, found {order}")]
    UnexpectedSourceOrder { index: usize, order: u32 },
    #[error(
        "sources[{index}].{field}: duplicate materialization destination {value:?}; first declared at sources[{first_index}].{first_field}"
    )]
    DuplicateSourceDestination {
        index: usize,
        field: &'static str,
        value: String,
        first_index: usize,
        first_field: &'static str,
    },
    #[error("jobs[{job}].phases: duplicate phase name `{name}`")]
    DuplicatePhase { job: usize, name: String },
    #[error("outputs: duplicate output name `{name}`")]
    DuplicateOutput { name: String },
    #[error("outputs: duplicate emitted package name {package}")]
    DuplicateOutputPackage { package: String },
    #[error("outputs: frozen plan must declare logical root output `out`")]
    MissingRootOutput,
    #[error("outputs[{index}].package_name: root output must emit package {expected:?}, found {found:?}")]
    RootOutputPackageMismatch {
        index: usize,
        expected: String,
        found: String,
    },
    #[error("outputs[{index}].include_in_manifest: root output must be present in the binary build manifest")]
    RootOutputExcludedFromManifest { index: usize },
    #[error("analysis.handlers: duplicate analyzer `{name}`")]
    DuplicateAnalyzer { name: String },
    #[error("analysis.handlers: required analyzer `{name}` is missing")]
    MissingAnalyzer { name: String },
    #[error("analysis.handlers: analyzer `{name}` must be last")]
    AnalyzerMustBeLast { name: String },
    #[error("{field}: required analyzer tool is missing")]
    MissingAnalyzerTool { field: String },
    #[error("{field}: analyzer tool is unreachable from the frozen handler/options graph")]
    UnexpectedAnalyzerTool { field: String },
    #[error("{field}: requirement {value:?} is not an executable capability")]
    ExecutableRequirementNotRunnable { field: String, value: String },
    #[error("{field}: executable requirement {value:?} must be one normalized filename component")]
    InvalidExecutableRequirement { field: String, value: String },
    #[error("{field}: expected canonical executable path {expected:?}, found {found:?}")]
    ExecutablePathMismatch {
        field: String,
        expected: String,
        found: String,
    },
    #[error("{field}: package-bound executable path {value:?} must not use the binary provider namespaces")]
    AmbiguousPackageExecutable { field: String, value: String },
    #[error("{field}: executable provider request {request:?} is absent from build_lock.requests")]
    UnlockedExecutable { field: String, request: String },
    #[error("{field}: locked request {request:?} is missing typed input origin {expected}")]
    MissingExecutableInputOrigin {
        field: String,
        request: String,
        expected: String,
    },
    #[error("toolchain_commands.compilers: expected {expected} compiler commands, found {found}")]
    CompilerCommandCount { found: usize, expected: usize },
    #[error(
        "toolchain_commands.compilers[{index}].role: expected {expected:?} in canonical role order, found {found:?}"
    )]
    UnexpectedCompilerCommandRole {
        index: usize,
        expected: CompilerExecutableRole,
        found: CompilerExecutableRole,
    },
    #[error(
        "toolchain_commands cache selection does not match execution.compiler_cache={enabled} (ccache={ccache}, sccache={sccache})"
    )]
    CompilerCacheCommandMismatch { enabled: bool, ccache: bool, sccache: bool },
    #[error("{field}: unknown locked output `{package}:{output}`")]
    UnknownOutputReference {
        field: String,
        package: String,
        output: String,
    },
    #[error("{field}: unknown planned output `{output}`")]
    UnknownPlannedOutput { field: String, output: String },
    #[error("{field}: invalid typed package relation target {value:?}")]
    InvalidRelation {
        field: String,
        value: String,
        #[source]
        source: stone::relation::ParseError,
    },
    #[error("{field}: invalid regular expression {value:?}")]
    InvalidRegex {
        field: String,
        value: String,
        #[source]
        source: regex::Error,
    },
    #[error("{field}: invalid collection glob {value:?}")]
    InvalidGlob {
        field: String,
        value: String,
        #[source]
        source: glob::PatternError,
    },
    #[error("{field}: planned output dependency cycle: {}", cycle.join(" -> "))]
    PlannedOutputCycle { field: String, cycle: Vec<String> },
    #[error("{field}: path must be a normalized, non-root absolute path, found {value:?}")]
    UnsafeAbsolutePath { field: String, value: String },
    #[error("{field}: path {value:?} must remain within {root_field} {root:?}")]
    PathOutsideRoot {
        field: String,
        value: String,
        root_field: String,
        root: String,
    },
    #[error("layout.hostname: invalid sandbox hostname {value:?}")]
    InvalidSandboxHostname { value: String },
    #[error("{field}: path {value:?} overlaps {other_field} {other:?}")]
    OverlappingLayoutPath {
        field: String,
        value: String,
        other_field: String,
        other: String,
    },
    #[error("jobs[{job}].pgo_stage: unsupported frozen PGO stage {stage:?}")]
    UnsupportedPgoStage { job: usize, stage: String },
    #[error(
        "jobs[{job}]: pgo_stage and pgo_dir must either both be set or both be absent (stage={stage:?}, directory={directory:?})"
    )]
    PgoStageDirectoryMismatch {
        job: usize,
        stage: Option<String>,
        directory: Option<String>,
    },
    #[error("jobs[{job}].phases[{phase}].name: unsupported frozen phase {name:?}")]
    UnsupportedPhase { job: usize, phase: usize, name: String },
    #[error("jobs[{job}] contains overlapping archive extraction destinations")]
    OverlappingArchiveDestinations { job: usize },
    #[error(
        "jobs[{job}] archive destination {destination:?} overlaps sources[{source_index}] Git directory {directory:?}"
    )]
    ArchiveDestinationOverlapsGitSource {
        job: usize,
        destination: String,
        source_index: usize,
        directory: String,
    },
    #[error("{field}: built-in archive extraction is permitted only in the prepare phase body")]
    ArchiveStepOutsidePrepare { field: String },
    #[error("{field}.source: {source_index} does not identify a locked archive")]
    InvalidArchiveStepSource { field: String, source_index: u32 },
    #[error("{field}.destination: unsafe normalized relative archive destination {destination:?}")]
    UnsafeArchiveStepDestination { field: String, destination: String },
    #[error("{field}.strip_components: found {found}, limit {limit}")]
    ArchiveStripComponentsLimit { field: String, found: u32, limit: u32 },
    #[error("sources[{index}].url: invalid source URL: {source}")]
    InvalidSourceUrl {
        index: usize,
        #[source]
        source: SourceUrlValidationError,
    },
    #[error("sources[{index}].commit: expected exactly 40 lowercase ASCII hexadecimal characters, found `{value}`")]
    InvalidGitCommit { index: usize, value: String },
    #[error(
        "sources[{index}].materialization_sha256: expected exactly 64 lowercase ASCII hexadecimal characters, found `{value}`"
    )]
    InvalidGitMaterializationSha256 { index: usize, value: String },
    #[error("sources[{index}].sha256: expected exactly 64 lowercase ASCII hexadecimal characters, found `{value}`")]
    InvalidArchiveSha256 { index: usize, value: String },
    #[error("sources[{index}].{field}: unsafe relative materialization path {value:?}")]
    UnsafeSourceDestination {
        index: usize,
        field: &'static str,
        value: String,
    },
    #[error(transparent)]
    BuildLock(#[from] BuildLockValidationError),
}
