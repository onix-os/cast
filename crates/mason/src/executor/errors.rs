#[derive(Debug, Error)]
pub enum StepExecutionError {
    #[error("could not spawn frozen build step: {source}")]
    Spawn {
        #[source]
        source: io::Error,
    },
    #[error("could not configure frozen build-step output pipes: {source}")]
    PipeSetup {
        #[source]
        source: io::Error,
    },
    #[error("could not start frozen build-step {stream} reader: {source}")]
    ReaderThreadSpawn {
        stream: OutputStream,
        #[source]
        source: io::Error,
    },
    #[error("could not install frozen build-step SIGINT forwarding: {source}")]
    SignalForward {
        #[source]
        source: nix::Error,
    },
    #[error("frozen build step exceeded its operational wall limit of {limit:?}")]
    Timeout { limit: Duration },
    #[error("could not wait for frozen build step: {source}")]
    Wait {
        #[source]
        source: io::Error,
    },
    #[error("frozen build-step {stream} produced {observed} bytes, exceeding its {limit}-byte ceiling")]
    OutputLimit {
        stream: OutputStream,
        limit: u64,
        observed: u64,
    },
    #[error(
        "frozen build-step stdout and stderr produced {observed} bytes, exceeding their combined {limit}-byte ceiling"
    )]
    TotalOutputLimit { limit: u64, observed: u64 },
    #[error("could not read frozen build-step {stream}: {source}")]
    OutputRead {
        stream: OutputStream,
        #[source]
        source: io::Error,
    },
    #[error("could not stream frozen build-step {stream}: {source}")]
    OutputWrite {
        stream: OutputStream,
        #[source]
        source: io::Error,
    },
    #[error("frozen build-step output budget lock was poisoned")]
    OutputBudgetPoisoned,
    #[error("frozen build-step log multiplexer lock was poisoned")]
    LogMuxPoisoned,
    #[error("frozen build-step output reader reported a failure without preserving it")]
    ReaderAlertLost,
    #[error("frozen build-step {stream} reader panicked")]
    ReaderThreadPanicked { stream: OutputStream },
    #[error("frozen build-step cleanup `{operation}` failed: {source}")]
    Cleanup {
        operation: &'static str,
        #[source]
        source: io::Error,
    },
    #[error("{failure}; frozen build-step cleanup `{operation}` also failed: {source}")]
    CleanupAfterFailure {
        failure: Box<StepExecutionError>,
        operation: &'static str,
        #[source]
        source: io::Error,
    },
}

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    InvalidPlan(#[from] stone_recipe::derivation::DerivationValidationError),
    #[error("plan requires executor ABI {found}, but this Cast provides {expected}")]
    IncompatibleExecutor { expected: &'static str, found: String },
    #[error("plan was created by Cast {found}, but executor is {expected}")]
    IncompatibleCast { expected: String, found: String },
    #[error("plan requires Cast implementation {found}, but executor provides {expected}")]
    IncompatibleCastSemantics { expected: String, found: String },
    #[error("plan executor identity is {found}, but this Cast requires {expected}")]
    IncompatibleExecutorFingerprint { expected: String, found: String },
    #[error("frozen plan requires build host `{required}`, but Cast is running on `{actual}`")]
    IncompatibleBuildHost { required: String, actual: String },
    #[error("frozen executor must run as PID 1 in its dedicated PID namespace, got PID {0}")]
    PidNamespaceInitRequired(i32),
    #[error("frozen execution requests {requested} CPUs, but this executor can represent at most {representable}")]
    UnrepresentableCpuAffinity { requested: u32, representable: usize },
    #[error(
        "frozen execution requests {requested} CPUs, but the current allowed affinity provides only {available} representable CPUs"
    )]
    InsufficientCpuAffinity { requested: u32, available: usize },
    #[error("kernel applied {actual} CPUs to frozen execution; expected exactly {expected}")]
    CpuAffinityCardinalityMismatch { expected: u32, actual: usize },
    #[error("kernel applied CPU affinity {actual:?}; expected deterministic affinity {expected:?}")]
    CpuAffinityMaskMismatch { expected: Vec<usize>, actual: Vec<usize> },
    #[error("unsupported frozen PGO stage {0}")]
    UnsupportedPgoStage(String),
    #[error("unsupported frozen phase {0}")]
    UnsupportedPhase(String),
    #[error("frozen archive source index {0} is invalid")]
    InvalidArchiveSource(u32),
    #[error("retain frozen built executable {path:?}")]
    BuiltExecutable {
        path: String,
        #[source]
        source: io::Error,
    },
    #[error("archive extraction")]
    Archive(#[from] crate::archive::Error),
    #[error(transparent)]
    StepExecution(#[from] StepExecutionError),
    #[error("build step failed with status code {0}")]
    Code(i32),
    #[error("build step stopped by signal {}", .0.as_str())]
    Signal(Signal),
    #[error("build step stopped by an unknown signal")]
    UnknownSignal,
    #[error("container")]
    Container(#[from] ::container::Error),
    #[error("nix")]
    Nix(#[from] nix::Error),
    #[error("io")]
    Io(#[from] io::Error),
}
