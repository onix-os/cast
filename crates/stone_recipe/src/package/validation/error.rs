use crate::{
    package::{BuilderEnvironmentSpec, DependencySpec},
    spec::UpstreamValidationError,
};
use stone::relation::ParseError;
use thiserror::Error;

/// Failure to validate a concrete package-v3 declaration.
#[derive(Debug, Error)]
pub enum PackageConversionError {
    #[error("meta.pname: package name `{name}` must use only ASCII letters, digits, '+', '-', '.', or '_'")]
    InvalidPackageName { name: String },
    #[error("meta.version: version must start with an integer (found `{version}`)")]
    VersionMustStartWithDigit { version: String },
    #[error("meta.version: version `{version}` must be one normalized filename component")]
    InvalidVersionComponent { version: String },
    #[error("meta.release: release must be greater than zero (found `{release}`)")]
    ReleaseMustBePositive { release: i64 },
    #[error("meta.homepage: invalid URL `{value}`")]
    InvalidHomepage {
        value: String,
        #[source]
        source: url::ParseError,
    },
    #[error("meta.homepage: URL `{value}` must be absolute HTTP or HTTPS with a host")]
    UnsupportedHomepage { value: String },
    #[error("meta.homepage: URL must not contain embedded credentials")]
    HomepageCredentials,
    #[error("{field}: value {value:?} {requirement}")]
    InvalidText {
        field: String,
        value: String,
        requirement: &'static str,
    },
    #[error("{field}: duplicate value {value:?}; first declared at {first_field}")]
    DuplicateValue {
        field: String,
        value: String,
        first_field: String,
    },
    #[error("{field}: invalid glob {value:?}")]
    InvalidGlob {
        field: String,
        value: String,
        #[source]
        source: glob::PatternError,
    },
    #[error("{field}: invalid regular expression {value:?}")]
    InvalidRegex {
        field: String,
        value: String,
        #[source]
        source: Box<regex::Error>,
    },
    #[error("{field}: {actual} {unit} exceeds the package limit of {limit}")]
    LimitExceeded {
        field: String,
        actual: usize,
        limit: usize,
        unit: &'static str,
    },
    #[error(
        "options.networking: frozen builds must declare fetched content as locked sources; network access during execution is unsupported"
    )]
    FrozenBuildNetworkingUnsupported,
    #[error("{field}: {source}")]
    InvalidSource {
        field: String,
        #[source]
        source: UpstreamValidationError,
    },
    #[error("{field}: materialization destination `{value}` duplicates `{first_field}`")]
    DuplicateSourceMaterialization {
        field: String,
        value: String,
        first_field: String,
    },
    #[error("{field}: invalid dependency: {source}")]
    InvalidDependency {
        field: String,
        #[source]
        source: ParseError,
    },
    #[error("{field}: invalid provider: {source}")]
    InvalidProvider {
        field: String,
        #[source]
        source: ParseError,
    },
    #[error("outputs: package must declare exactly one `out` output")]
    MissingRootOutput,
    #[error("outputs[{index}].name: duplicate output name `{name}`")]
    DuplicateOutput { index: usize, name: String },
    #[error("outputs[{index}].name: invalid output name `{name}`")]
    InvalidOutputName { index: usize, name: String },
    #[error("profiles[{index}].name: profile name `{name}` must be a normalized safe relative path")]
    InvalidProfileName { index: usize, name: String },
    #[error(
        "profiles[{duplicate_index}].name: duplicate profile name `{name}`; first declared at profiles[{first_index}].name"
    )]
    DuplicateProfileName {
        first_index: usize,
        duplicate_index: usize,
        name: String,
    },
    #[error("{field}: output `{output}` does not exist in package `{package}`")]
    MissingOutputReference {
        field: String,
        package: String,
        output: String,
    },
    #[error("{field}: package output dependency cycle: {cycle}")]
    OutputDependencyCycle { field: String, cycle: String },
    #[error("{field}: duplicate builder environment marker `{environment:?}`")]
    DuplicateBuilderEnvironment {
        field: String,
        environment: BuilderEnvironmentSpec,
    },
    #[error("{field}: hook is not supported by the selected builder")]
    UnsupportedBuilderHook { field: String },
    #[error("{field}: program path `{value}` must be a normalized non-root absolute path")]
    InvalidProgramPath { field: String, value: String },
    #[error("{field}: {requirement:?} is not an executable program capability")]
    UnsupportedProgramRequirement { field: String, requirement: DependencySpec },
    #[error("{field}: {requirement:?} is not a normalized executable capability")]
    InvalidProgramRequirement { field: String, requirement: DependencySpec },
    #[error("{field}: package/output program path `{value}` is ambiguous under the canonical binary directories")]
    AmbiguousPackageProgramPath { field: String, value: String },
    #[error("{field}: program path `{actual}` does not match the canonical path `{expected}` for {requirement:?}")]
    ProgramRequirementPathMismatch {
        field: String,
        requirement: DependencySpec,
        expected: String,
        actual: String,
    },
}

impl PackageConversionError {
    pub fn field(&self) -> &str {
        match self {
            Self::InvalidPackageName { .. } => "meta.pname",
            Self::VersionMustStartWithDigit { .. } => "meta.version",
            Self::InvalidVersionComponent { .. } => "meta.version",
            Self::ReleaseMustBePositive { .. } => "meta.release",
            Self::InvalidHomepage { .. } | Self::UnsupportedHomepage { .. } | Self::HomepageCredentials => {
                "meta.homepage"
            }
            Self::FrozenBuildNetworkingUnsupported => "options.networking",
            Self::InvalidSource { field, .. }
            | Self::DuplicateSourceMaterialization { field, .. }
            | Self::InvalidText { field, .. }
            | Self::DuplicateValue { field, .. }
            | Self::InvalidGlob { field, .. }
            | Self::InvalidRegex { field, .. }
            | Self::LimitExceeded { field, .. }
            | Self::InvalidDependency { field, .. }
            | Self::InvalidProvider { field, .. }
            | Self::MissingOutputReference { field, .. }
            | Self::OutputDependencyCycle { field, .. }
            | Self::DuplicateBuilderEnvironment { field, .. }
            | Self::UnsupportedBuilderHook { field }
            | Self::InvalidProgramPath { field, .. }
            | Self::UnsupportedProgramRequirement { field, .. }
            | Self::InvalidProgramRequirement { field, .. }
            | Self::AmbiguousPackageProgramPath { field, .. }
            | Self::ProgramRequirementPathMismatch { field, .. } => field,
            Self::MissingRootOutput => "outputs",
            Self::DuplicateOutput { .. } | Self::InvalidOutputName { .. } => "outputs",
            Self::InvalidProfileName { .. } | Self::DuplicateProfileName { .. } => "profiles",
        }
    }
}
