use thiserror::Error;

/// Semantic policy error with a stable field path.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum BuildPolicyConversionError {
    #[error("{field}: collection has {count} items, limit is {limit}")]
    CollectionLimit { field: String, count: usize, limit: usize },
    #[error("{field}: string has {bytes} bytes, limit is {limit}")]
    StringBytesLimit { field: String, bytes: usize, limit: usize },
    #[error("policy collections contain {count} items in total, limit is {limit}")]
    TotalCollectionItemsLimit { count: usize, limit: usize },
    #[error("policy strings contain {bytes} bytes in total, limit is {limit}")]
    TotalStringBytesLimit { bytes: usize, limit: usize },
    #[error("{field}: text has at least {nodes} nodes, limit is {limit}")]
    TextNodeLimit { field: String, nodes: usize, limit: usize },
    #[error("{field}: text depth is {depth}, limit is {limit}")]
    TextDepthLimit { field: String, depth: usize, limit: usize },
    #[error("{field}: literal has {bytes} bytes, limit is {limit}")]
    TextLiteralBytesLimit { field: String, bytes: usize, limit: usize },
    #[error("{field}: text literals contain {bytes} bytes in total, limit is {limit}")]
    TextTotalLiteralBytesLimit { field: String, bytes: usize, limit: usize },
    #[error("policy text contains {nodes} nodes in total, limit is {limit}")]
    TotalTextNodesLimit { nodes: usize, limit: usize },
    #[error("policy text literals contain {bytes} bytes in total, limit is {limit}")]
    TotalTextLiteralBytesLimit { bytes: usize, limit: usize },
    #[error("{field}: unable to reserve bounded capacity for {count} items")]
    Capacity { field: String, count: usize },
    #[error("{field}: value must not be empty")]
    Empty { field: String },
    #[error("{field}: duplicate value `{value}`")]
    Duplicate { field: String, value: String },
    #[error("{field}: required value `{value}` is missing")]
    MissingRequired { field: String, value: String },
    #[error("{field}: value `{value}` must be last")]
    MustBeLast { field: String, value: String },
    #[error("{field}: PGO finish must declare at least one input")]
    EmptyPgoInputs { field: String },
    #[error("{field}: unknown reference `{value}`")]
    UnknownReference { field: String, value: String },
    #[error("{field}: default choice `{value}` does not exist")]
    InvalidDefault { field: String, value: String },
    #[error("{field}: flag `{value}` cannot be both enabled and disabled")]
    ConflictingTuningFlag { field: String, value: String },
    #[error("{field}: guest path `{value}` must be absolute and normalized")]
    InvalidGuestPath { field: String, value: String },
    #[error("{field}: invalid sandbox hostname `{value}`")]
    InvalidHostname { field: String, value: String },
    #[error("{field}: target name `{value}` must be a normalized safe relative path")]
    InvalidTargetName { field: String, value: String },
    #[error("{field}: unsupported artifact architecture `{value}`; expected one of {supported}")]
    UnsupportedArtifactArchitecture {
        field: String,
        value: String,
        supported: String,
    },
    #[error("{field}: guest path `{value}` is outside `{guest_root}`")]
    GuestPathOutsideRoot {
        field: String,
        value: String,
        guest_root: String,
    },
    #[error("{field}: guest path `{value}` overlaps {other_field} `{other}`")]
    OverlappingGuestPath {
        field: String,
        value: String,
        other_field: String,
        other: String,
    },
    #[error("{field}: platform component must be explicit, found `{value}`")]
    InvalidPlatformComponent { field: String, value: String },
    #[error("{field}: architecture `{value}` does not match `{expected}`")]
    ArchitectureMismatch {
        field: String,
        value: String,
        expected: String,
    },
    #[error("{field}: analyzer tool must be a binary or system-binary capability")]
    AnalyzerToolMustBeExecutable { field: String },
    #[error("{field}: analyzer executable `{value}` must be one normalized filename component")]
    InvalidAnalyzerExecutable { field: String, value: String },
    #[error("{field}: executable path `{value}` must be a normalized, non-root absolute path")]
    InvalidProgramPath { field: String, value: String },
    #[error("{field}: executable requirement `{value}` is invalid")]
    InvalidProgramRequirement { field: String, value: String },
    #[error("{field}: expected executable path `{expected}` for its provider, found `{found}`")]
    ProgramPathMismatch {
        field: String,
        expected: String,
        found: String,
    },
    #[error("{field}: package-bound executable path `{value}` must not use the binary provider namespaces")]
    AmbiguousPackageProgram { field: String, value: String },
    #[error("{field}: command argument contains an embedded NUL byte")]
    InvalidCommandArgument { field: String },
}
