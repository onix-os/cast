/// Possible errors returned by functions in this module.
#[derive(Debug, Error)]
pub enum Error {
    /// An error occurred while handling a Git repository.
    #[error("{0}")]
    Git(#[from] gitwrap::Error),
    /// A cache entry belongs to a different canonical source URL.
    #[error("cached Git mirror at {cache:?} does not belong to the requested source URL")]
    OriginMismatch { cache: PathBuf },
    /// A previous in-place fetch did not reach its durable commit point.
    #[error("cached Git mirror at {cache:?} has an incomplete fetch marker")]
    IncompleteCache { cache: PathBuf },
    /// Another process currently owns the cache mutation boundary.
    #[error("cached Git mirror at {cache:?} is busy in another process")]
    CacheBusy { cache: PathBuf },
    /// Submodules require their own explicit, locked source model.
    #[error("Git commit {commit} contains submodules, which are not supported as implicit sources")]
    UnsupportedSubmodules { commit: String },
    /// A frozen source has no expected normalized-tree identity.
    #[error("Git source {index} at commit {commit} has no locked materialization digest")]
    MissingMaterializationDigest { index: usize, commit: String },
    /// A caller attempted to export over an existing path of any type.
    #[error("refusing to export Git source over existing destination {0:?}")]
    DestinationExists(PathBuf),
    /// A build-visible checkout destination had no containing directory.
    #[error("Git checkout destination has no parent: {0:?}")]
    MissingDestinationParent(PathBuf),
    /// A private staging directory could not be created beside the final path.
    #[error("create private Git checkout staging directory in {parent:?}")]
    CreateStaging {
        parent: PathBuf,
        #[source]
        source: io::Error,
    },
    /// The verified staging tree could not be installed atomically.
    #[error("atomically install verified Git checkout from {source_path:?} at {destination:?}")]
    Install {
        source_path: PathBuf,
        destination: PathBuf,
        #[source]
        source: io::Error,
    },
    /// The normalized checkout differs from the bytes admitted by lock refresh.
    #[error("Git source {index} at commit {commit} materialized as {found}, but sources.lock.glu requires {expected}")]
    MaterializationDigestMismatch {
        index: usize,
        commit: String,
        expected: String,
        found: String,
    },
    /// Canonical tree normalization or hashing failed.
    #[error("normalize and hash Git materialization at {root:?}")]
    Materialization {
        root: PathBuf,
        #[source]
        source: materialization::Error,
    },
    /// Post-publication verification failed and the rejected inode was moved
    /// out of the build-visible destination without following it.
    #[error("rejected installed Git materialization at {destination:?}; quarantined at {quarantine:?}")]
    RejectedInstalledMaterialization {
        destination: PathBuf,
        quarantine: PathBuf,
        #[source]
        source: materialization::Error,
    },
    /// Verification failed and moving the rejected public name into a private
    /// no-replace quarantine also failed.
    #[error("failed to quarantine rejected Git materialization at {destination:?}: {cleanup}")]
    RejectedInstallCleanup {
        destination: PathBuf,
        #[source]
        verification: Box<materialization::Error>,
        cleanup: io::Error,
    },
    /// A generic I/O error occurred.
    #[error("{0}")]
    Io(#[from] io::Error),
}
