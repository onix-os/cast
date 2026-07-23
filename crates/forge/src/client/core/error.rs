/// Client-relevant error mapping type
#[derive(Debug, Error)]
pub enum Error {
    #[error("root must have an active state")]
    NoActiveState,
    #[error("{operation} at {path:?} while proving the live active-state selection")]
    LiveActiveStateProof {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("active-state snapshot changed since installation discovery: expected {expected:?}, found {actual:?}")]
    ActiveStateSnapshotChanged {
        expected: Option<state::Id>,
        actual: Option<state::Id>,
    },
    #[error("state {0} already active")]
    StateAlreadyActive(state::Id),
    #[error("state {0} doesn't exist")]
    StateDoesntExist(state::Id),
    #[error("open merged-/usr root ABI directory {root:?}")]
    OpenRootAbiDirectory {
        root: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("stat merged-/usr root ABI directory {root:?}")]
    StatRootAbiDirectory {
        root: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("merged-/usr root ABI directory was replaced while linking: {0:?}")]
    RootAbiDirectoryReplaced(PathBuf),
    #[error("inspect merged-/usr root ABI entry {path:?}")]
    InspectRootAbiEntry {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("read merged-/usr root ABI symlink {path:?}")]
    ReadRootAbiLink {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("merged-/usr root ABI symlink {path:?} exceeds {limit} target bytes")]
    RootAbiLinkTargetTooLong { path: PathBuf, limit: usize },
    #[error(
        "legacy merged-/usr staging entry {path:?} must be absent, found {actual_type} with target {symlink_target:?}"
    )]
    RootAbiStagingConflict {
        path: PathBuf,
        actual_type: &'static str,
        symlink_target: Option<OsString>,
    },
    #[error("merged-/usr root ABI link {path:?} must target {target:?}, found {actual_type}")]
    RootAbiLinkTypeConflict {
        path: PathBuf,
        target: String,
        actual_type: &'static str,
    },
    #[error("merged-/usr root ABI link {path:?} must target {expected:?}, found {actual:?}")]
    RootAbiLinkTargetConflict {
        path: PathBuf,
        expected: String,
        actual: OsString,
    },
    #[error("merged-/usr root ABI link {path:?} targeting {target:?} is missing after publication")]
    RootAbiLinkMissing { path: PathBuf, target: String },
    #[error("merged-/usr root ABI link appeared after absence was retained: {0:?}")]
    RootAbiLinkAppeared(PathBuf),
    #[error("merged-/usr root ABI link was replaced across its durability boundary: {0:?}")]
    RootAbiLinkReplaced(PathBuf),
    #[error("create absent merged-/usr root ABI link {path:?} targeting {target:?}")]
    CreateRootAbiLink {
        path: PathBuf,
        target: String,
        #[source]
        source: io::Error,
    },
    #[error("sync merged-/usr root ABI directory {root:?}")]
    SyncRootAbiDirectory {
        root: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(
        "state transition for candidate {candidate} failed before /usr was exchanged; the candidate was preserved outside the active root and arbitrary trigger side effects may remain: {primary}"
    )]
    StatefulCandidatePreserved {
        candidate: state::Id,
        previous: Option<state::Id>,
        #[source]
        primary: Box<Error>,
    },
    #[error(
        "state transition for candidate {candidate} failed; /usr was restored to {previous:?}, the failed candidate was preserved outside the active root, and arbitrary trigger side effects may remain: {primary}"
    )]
    StatefulTransitionUsrRestored {
        candidate: state::Id,
        previous: Option<state::Id>,
        #[source]
        primary: Box<Error>,
    },
    #[error(
        "boot synchronization for failed candidate {candidate} had already started; synchronization for restored state {previous:?} returned without a verifiable proof that candidate boot metadata was removed"
    )]
    StatefulBootRepairUnverified {
        candidate: state::Id,
        previous: Option<state::Id>,
    },
    #[error(
        "failed-candidate preservation failed and its one bounded in-process retry also failed (first={first}; retry={retry})"
    )]
    StatefulCandidatePreservationRetryFailed { first: Box<Error>, retry: Box<Error> },
    #[error(
        "state transition for candidate {candidate} failed with primary error {primary}; recovery for previous state {previous:?} was incomplete (previous_archive_cleanup={previous_archive_cleanup:?}, restore_previous={restore_previous:?}, reverse_exchange={reverse_exchange:?}, preserve_candidate={preserve_candidate:?}, invalidate_candidate={invalidate_candidate:?}, repair_boot={repair_boot:?})"
    )]
    StatefulTransitionRecoveryFailed {
        candidate: state::Id,
        previous: Option<state::Id>,
        #[source]
        primary: Box<Error>,
        previous_archive_cleanup: Option<Box<Error>>,
        restore_previous: Option<Box<Error>>,
        reverse_exchange: Option<Box<Error>>,
        preserve_candidate: Option<Box<Error>>,
        invalidate_candidate: Option<Box<Error>>,
        repair_boot: Option<Box<Error>>,
    },
    #[error("No metadata found for package {0:?}")]
    MissingMetadata(package::Id),
    #[error("package {package} has invalid /usr-relative Stone layout target {target:?}: {reason}")]
    InvalidStoneLayoutTarget {
        package: package::Id,
        target: String,
        reason: &'static str,
    },
    #[error("ephemeral/public materialization destination overlaps the installation root")]
    EphemeralInstallationRoot,
    #[error("ephemeral postblit requested {requested:?}, but this client is bound to {configured:?}")]
    EphemeralDestinationMismatch { configured: PathBuf, requested: PathBuf },
    #[error(
        "initial materialization target {path:?} must be an exact empty owner-controlled ACL-free directory (uid={owner}, mode={mode:04o})"
    )]
    UnsafeInitialMaterializationTarget { path: PathBuf, owner: u32, mode: u32 },
    #[error("retained initial materialization target changed at {path:?}")]
    InitialMaterializationTargetChanged { path: PathBuf },
    #[error(
        "initial materialization parent {path:?} must be an owner-controlled ACL-free directory (uid={owner}, mode={mode:04o})"
    )]
    UnsafeInitialMaterializationParent { path: PathBuf, owner: u32, mode: u32 },
    #[error("Operation not allowed with ephemeral client")]
    EphemeralProhibitedOperation,
    #[error("frozen-root materialization requires a dedicated frozen client")]
    FrozenRootRequiresFrozenClient,
    #[error("frozen clients require an installation opened with Installation::open_frozen")]
    FrozenInstallationRequired,
    #[error("system and ephemeral clients require Installation::open on a writable system root")]
    SystemInstallationRequired,
    #[error("operation is not available on a dedicated frozen client")]
    FrozenClientProhibitedOperation,
    #[error("duplicate package ID in frozen closure: {0}")]
    DuplicateFrozenPackage(package::Id),
    #[error("frozen package closure must not be empty")]
    EmptyFrozenPackageClosure,
    #[error("layout query returned package outside the frozen closure: {0}")]
    UnexpectedFrozenLayoutPackage(package::Id),
    #[error("package {package} has a frozen-root layout path exceeding {limit} bytes (got {actual})")]
    FrozenLayoutPathTooLong {
        package: package::Id,
        limit: usize,
        actual: usize,
    },
    #[error("package {package} has a frozen-root layout path exceeding {limit} components (got {actual})")]
    FrozenLayoutPathTooDeep {
        package: package::Id,
        limit: usize,
        actual: usize,
    },
    #[error("package {package} has a frozen-root symlink target exceeding {limit} bytes (got {actual})")]
    FrozenLayoutSymlinkTargetTooLong {
        package: package::Id,
        limit: usize,
        actual: usize,
    },
    #[error("package {package} has an invalid frozen-root layout path: {path:?}")]
    InvalidFrozenLayoutPath { package: package::Id, path: String },
    #[error("package {package} has an invalid or unenforceable frozen-root mode {mode:#o} at {path:?}")]
    InvalidFrozenLayoutMode {
        package: package::Id,
        path: String,
        mode: u32,
    },
    #[error("package {package} has an unsupported frozen-root inode at {path:?}")]
    UnsupportedFrozenLayout { package: package::Id, path: String },
    #[error("package {package} requests unsupported frozen-root ownership {uid}:{gid} at {path:?}")]
    UnsupportedFrozenOwnership {
        package: package::Id,
        path: String,
        uid: u32,
        gid: u32,
    },
    #[error("frozen-root path collision at {path:?}: packages {first} and {second}")]
    FrozenPathCollision {
        path: String,
        first: package::Id,
        second: package::Id,
    },
    #[error(
        "package {package} declares frozen-root path {path:?} beneath directory symlink {redirect:?}; explicit descendants under directory symlinks are forbidden"
    )]
    FrozenDirectorySymlinkDescendant {
        package: package::Id,
        path: Box<str>,
        redirect: Box<str>,
    },
    #[error("frozen executable closure package count exceeds {limit} (got {actual})")]
    FrozenExecutablePackageLimit { limit: usize, actual: usize },
    #[error("frozen executable closure package IDs exceed {limit} aggregate bytes (got {actual})")]
    FrozenExecutableClosureIdByteLimit { limit: usize, actual: usize },
    #[error("frozen executable binding count exceeds {limit} (got {actual})")]
    FrozenExecutableBindingLimit { limit: usize, actual: usize },
    #[error("frozen executable path exceeds {limit} bytes (got {actual})")]
    FrozenExecutablePathByteLimit { limit: usize, actual: usize },
    #[error("frozen executable path exceeds {limit} components (got {actual})")]
    FrozenExecutablePathDepthLimit { limit: usize, actual: usize },
    #[error("frozen executable path is not UTF-8 ({bytes} bytes)")]
    FrozenExecutablePathEncoding { bytes: usize },
    #[error(
        "frozen executable bindings exceed {limit} aggregate path bytes at provider {package} path {path:?} (got {actual})"
    )]
    FrozenExecutableBindingByteLimit {
        package: package::Id,
        path: PathBuf,
        limit: usize,
        actual: usize,
    },
    #[error("frozen closure layout count exceeds {limit} (got {actual})")]
    FrozenExecutableLayoutLimit { limit: usize, actual: usize },
    #[error("frozen closure stored layout strings exceed {limit} aggregate bytes (got {actual})")]
    FrozenLayoutStorageByteLimit { limit: usize, actual: usize },
    #[error(
        "frozen executable closure layouts exceed {limit} aggregate bytes at provider {package} path {path:?} (got {actual})"
    )]
    FrozenExecutableLayoutByteLimit {
        package: package::Id,
        path: PathBuf,
        limit: usize,
        actual: usize,
    },
    #[error("frozen directory discovery exceeds {limit} paths (got {actual})")]
    FrozenExecutableDirectoryLimit { limit: usize, actual: usize },
    #[error("frozen directory discovery exceeds {limit} aggregate path bytes (got {actual})")]
    FrozenExecutableDirectoryByteLimit { limit: usize, actual: usize },
    #[error(
        "frozen executable graph from provider {package} at {path:?} exceeds the retained-file limit {limit} (got {actual})"
    )]
    FrozenExecutablePinnedFileLimit {
        package: package::Id,
        path: PathBuf,
        limit: usize,
        actual: usize,
    },
    #[error("frozen executable provider {package} at {path:?} is outside the materialized closure")]
    FrozenExecutableProviderOutsideClosure { package: package::Id, path: PathBuf },
    #[error("frozen executable provider {package} has invalid path {path:?}")]
    InvalidFrozenExecutablePath { package: package::Id, path: PathBuf },
    #[error("frozen executable provider {package} has duplicate layout entries at {path:?}")]
    DuplicateFrozenExecutableLayout { package: package::Id, path: PathBuf },
    #[error("frozen executable provider {package} has no regular layout entry at {path:?}")]
    MissingFrozenExecutableLayout { package: package::Id, path: PathBuf },
    #[error(
        "frozen executable provider {package} binding {binding:?} resolves to {target:?}, which has no provider in the exact frozen closure"
    )]
    MissingFrozenExecutableSymlinkTarget {
        package: package::Id,
        binding: PathBuf,
        target: PathBuf,
    },
    #[error(
        "frozen executable provider {package} binding {binding:?} resolves to ambiguous target {target:?} from providers {providers:?}"
    )]
    AmbiguousFrozenExecutableSymlinkTarget {
        package: package::Id,
        binding: PathBuf,
        target: PathBuf,
        providers: Vec<package::Id>,
    },
    #[error("frozen executable provider {package} names a non-regular layout entry at {path:?}")]
    FrozenExecutableLayoutNotRegular { package: package::Id, path: PathBuf },
    #[error("frozen executable provider {package} has non-executable layout mode {mode:#o} at {path:?}")]
    FrozenExecutableLayoutNotExecutable {
        package: package::Id,
        path: PathBuf,
        mode: u32,
    },
    #[error("frozen executable provider {package} has invalid symlink target {target:?} at {path:?}")]
    InvalidFrozenExecutableSymlinkTarget {
        package: package::Id,
        path: PathBuf,
        target: String,
    },
    #[error("frozen executable provider {package} has a symlink cycle at {path:?}")]
    FrozenExecutableSymlinkCycle { package: package::Id, path: PathBuf },
    #[error("frozen executable provider {package} binding {path:?} exceeds the symlink-chain limit {limit}")]
    FrozenExecutableSymlinkLimit {
        package: package::Id,
        path: PathBuf,
        limit: usize,
    },
    #[error(
        "frozen executable path {path:?} traverses materialized directory-symlink redirect {redirect_source:?} -> {target:?}; executable redirects are forbidden"
    )]
    FrozenExecutableDirectoryRedirect {
        path: PathBuf,
        redirect_source: Box<PathBuf>,
        target: Box<PathBuf>,
    },
    #[error("frozen executable from provider {package} at {path:?} has an invalid format: {reason}")]
    InvalidFrozenExecutableFormat {
        package: package::Id,
        path: PathBuf,
        reason: &'static str,
    },
    #[error("frozen ELF from provider {package} at {path:?} exceeds {limit} program headers (got {actual})")]
    FrozenElfProgramHeaderLimit {
        package: package::Id,
        path: PathBuf,
        limit: usize,
        actual: usize,
    },
    #[error(
        "frozen ELF PT_INTERP target from provider {package} at {path:?} is itself interpreted; Linux requires a terminal ELF loader"
    )]
    FrozenElfInterpreterIsInterpreted { package: package::Id, path: PathBuf },
    #[error("frozen executable script from provider {package} at {path:?} has an invalid shebang: {reason}")]
    InvalidFrozenShebang {
        package: package::Id,
        path: PathBuf,
        reason: &'static str,
    },
    #[error("frozen script interpreter at {path:?} is not supplied by the frozen package closure")]
    MissingFrozenInterpreterProvider { path: PathBuf },
    #[error("frozen script interpreter at {path:?} has multiple providers: {providers:?}")]
    AmbiguousFrozenInterpreterProvider { path: PathBuf, providers: Vec<package::Id> },
    #[error("frozen script interpreter layout has a symlink cycle at {path:?}")]
    FrozenInterpreterSymlinkCycle { path: PathBuf },
    #[error("frozen executable interpreter chain cycles through provider {package} at {path:?}")]
    FrozenExecutableInterpreterCycle { package: package::Id, path: PathBuf },
    #[error("frozen script from provider {package} at {path:?} exceeds the interpreter-chain limit {limit}")]
    FrozenShebangInterpreterLimit {
        package: package::Id,
        path: PathBuf,
        limit: usize,
    },
    #[error("frozen executable from provider {package} at {path:?} exceeds the interpreter-graph limit {limit}")]
    FrozenExecutableInterpreterLimit {
        package: package::Id,
        path: PathBuf,
        limit: usize,
    },
    #[error("invalid frozen interpreter root ABI alias path {path:?}")]
    InvalidFrozenInterpreterRootAlias { path: PathBuf },
    #[error("open frozen interpreter root ABI alias {path:?}")]
    OpenFrozenInterpreterRootAlias {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("stat frozen interpreter root ABI alias {path:?}")]
    StatFrozenInterpreterRootAlias {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("read frozen interpreter root ABI alias {path:?}")]
    ReadFrozenInterpreterRootAlias {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("frozen interpreter root ABI alias {path:?} has mode {mode:#o} and {links} links")]
    FrozenInterpreterRootAliasMetadata { path: PathBuf, mode: u32, links: u64 },
    #[error("frozen interpreter root ABI alias {path:?} points to {actual:?}; expected {expected:?}")]
    FrozenInterpreterRootAliasTarget {
        path: PathBuf,
        expected: String,
        actual: OsString,
    },
    #[error(
        "frozen interpreter root ABI alias {path:?} exceeds the target limit {limit} bytes (got at least {actual})"
    )]
    FrozenInterpreterRootAliasTargetTooLong { path: PathBuf, limit: usize, actual: usize },
    #[error("frozen interpreter root ABI alias changed during verification at {path:?}")]
    FrozenInterpreterRootAliasChanged { path: PathBuf },
    #[error("frozen executable root path is invalid: {0:?}")]
    InvalidFrozenExecutableRoot(PathBuf),
    #[error("open frozen executable root {path:?}")]
    OpenFrozenExecutableRoot {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("stat frozen executable root {path:?}")]
    StatFrozenExecutableRoot {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("frozen executable root path was replaced during verification: {0:?}")]
    FrozenExecutableRootReplaced(PathBuf),
    #[error("materialized frozen root destination was replaced after publication: {0:?}")]
    MaterializedFrozenRootReplaced(PathBuf),
    #[error("materialized frozen root belongs to {found:?}, expected this client destination {expected:?}")]
    ForeignMaterializedFrozenRoot { expected: PathBuf, found: PathBuf },
    #[error("open frozen executable from provider {package} at {path:?}")]
    OpenFrozenExecutable {
        package: package::Id,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("stat frozen executable from provider {package} at {path:?}")]
    StatFrozenExecutable {
        package: package::Id,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("open frozen executable symlink from provider {package} at {path:?}")]
    OpenFrozenExecutableSymlink {
        package: package::Id,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("stat frozen executable symlink from provider {package} at {path:?}")]
    StatFrozenExecutableSymlink {
        package: package::Id,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("read frozen executable symlink from provider {package} at {path:?}")]
    ReadFrozenExecutableSymlink {
        package: package::Id,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(
        "frozen executable symlink from provider {package} at {path:?} has mode {actual:#o} and {links} links; expected mode {expected:#o} and one link"
    )]
    FrozenExecutableSymlinkMetadataMismatch {
        package: package::Id,
        path: PathBuf,
        expected: u32,
        actual: u32,
        links: u64,
    },
    #[error(
        "frozen executable symlink from provider {package} at {path:?} points to {actual:?}; expected {expected:?}"
    )]
    FrozenExecutableSymlinkTargetMismatch {
        package: package::Id,
        path: PathBuf,
        expected: String,
        actual: OsString,
    },
    #[error("frozen executable symlink from provider {package} changed during verification at {path:?}")]
    FrozenExecutableSymlinkChanged { package: package::Id, path: PathBuf },
    #[error(
        "frozen executable symlink from provider {package} at {path:?} exceeds the target limit {limit} bytes (got at least {actual})"
    )]
    FrozenExecutableSymlinkTargetTooLong {
        package: package::Id,
        path: PathBuf,
        limit: usize,
        actual: usize,
    },
    #[error("read frozen executable from provider {package} at {path:?}")]
    ReadFrozenExecutable {
        package: package::Id,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(
        "frozen executable from provider {package} at {path:?} is not an independent regular file (mode {mode:#o}, links {links})"
    )]
    FrozenExecutableNotIndependentRegular {
        package: package::Id,
        path: PathBuf,
        mode: u32,
        links: u64,
    },
    #[error("frozen executable from provider {package} at {path:?} has mode {actual:#o}; expected {expected:#o}")]
    FrozenExecutableModeMismatch {
        package: package::Id,
        path: PathBuf,
        expected: u32,
        actual: u32,
    },
    #[error("frozen executable from provider {package} at {path:?} exceeds {limit} bytes (got {actual})")]
    FrozenExecutableByteLimit {
        package: package::Id,
        path: PathBuf,
        limit: u64,
        actual: u64,
    },
    #[error("total frozen executable bytes exceed {limit} (got {actual})")]
    FrozenExecutableTotalByteLimit { limit: u64, actual: u64 },
    #[error(
        "frozen executable from provider {package} at {path:?} changed length while hashing: expected {expected}, got {actual}"
    )]
    FrozenExecutableLengthChanged {
        package: package::Id,
        path: PathBuf,
        expected: u64,
        actual: u64,
    },
    #[error("frozen executable from provider {package} changed while hashing at {path:?}")]
    FrozenExecutableChanged { package: package::Id, path: PathBuf },
    #[error("frozen executable from provider {package} at {path:?} has digest {actual:032x}; expected {expected:032x}")]
    FrozenExecutableDigestMismatch {
        package: package::Id,
        path: PathBuf,
        expected: u128,
        actual: u128,
    },
    #[error("frozen executable path from provider {package} was replaced during verification: {path:?}")]
    FrozenExecutablePathReplaced { package: package::Id, path: PathBuf },
    #[error("frozen executable verification exceeded {seconds} seconds")]
    FrozenExecutableVerificationTimeout { seconds: u64 },
    #[error("frozen-root materialization exceeded {seconds} seconds")]
    FrozenMaterializationTimeout { seconds: u64 },
    #[error("frozen-root independent-copy bytes exceed {limit} (got {actual})")]
    FrozenMaterializationTotalByteLimit { limit: u64, actual: u64 },
    #[error(
        "frozen-root cached asset {digest:032x} changed length between byte preflight and copy: expected {expected}, got {actual}"
    )]
    FrozenMaterializationAssetLengthChanged { digest: u128, expected: u64, actual: u64 },
    #[error("frozen-root cached asset {digest:032x} was not admitted by the byte preflight")]
    FrozenMaterializationAssetMissingFromManifest { digest: u128 },
    #[error("frozen-root destination is invalid: {0:?}")]
    InvalidFrozenRootDestination(PathBuf),
    #[error("frozen-root destination already exists: {0:?}")]
    FrozenRootDestinationExists(PathBuf),
    #[error("package {package} has an invalid frozen-root symlink target: {reason}")]
    InvalidFrozenLayoutSymlinkTarget { package: package::Id, reason: &'static str },
    #[error("frozen-root normalization exceeds {limit} inodes (got {actual})")]
    FrozenNormalizationInodeLimit { limit: usize, actual: usize },
    #[error("frozen-root normalization exceeds {limit} path components (got {actual})")]
    FrozenNormalizationDepthLimit { limit: usize, actual: usize },
    #[error("invalid declarative frozen-root entry {path:?}: {reason}")]
    InvalidFrozenNormalizationDeclaration { path: PathBuf, reason: &'static str },
    #[error("frozen-root filesystem does not match its declaration at {path:?}: {reason}")]
    FrozenNormalizationInventoryMismatch { path: PathBuf, reason: &'static str },
    #[error("frozen-root entry changed while being normalized: {0:?}")]
    FrozenNormalizationEntryChanged(PathBuf),
    #[error("frozen-root staging name changed while its original descriptor was retained: {0:?}")]
    FrozenNormalizationRootChanged(PathBuf),
    #[error("open frozen-root normalization entry {path:?}")]
    OpenFrozenNormalizationEntry {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("inspect frozen-root normalization entry {path:?}")]
    InspectFrozenNormalizationEntry {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("read frozen-root normalization directory {path:?}")]
    ReadFrozenNormalizationDirectory {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("reserve bounded frozen-root normalization inventory for {path:?}")]
    ReserveFrozenNormalizationInventory {
        path: PathBuf,
        #[source]
        source: std::collections::TryReserveError,
    },
    #[error("frozen-root entry carries a non-canonical ACL at {path:?}")]
    FrozenNormalizationAcl {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("normalize frozen-root mode through retained entry {path:?}")]
    NormalizeFrozenEntryMode {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("normalize frozen-root timestamp through retained entry {path:?}")]
    NormalizeFrozenEntryTime {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("read retained frozen-root symlink {path:?}")]
    ReadFrozenNormalizationSymlink {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("frozen-root symlink {path:?} must target {expected:?}, found {actual:?}")]
    FrozenNormalizationSymlinkTargetMismatch {
        path: PathBuf,
        expected: OsString,
        actual: OsString,
    },
    #[error("publish frozen root {stage:?} to {destination:?}")]
    PublishFrozenRoot {
        stage: PathBuf,
        destination: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("open frozen-root destination parent {path:?}")]
    OpenFrozenRootDestinationParent {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("frozen-root destination parent changed while retained: {0:?}")]
    FrozenRootDestinationParentChanged(PathBuf),
    #[error("lock frozen-root destination parent {path:?}")]
    LockFrozenRootDestinationParent {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("create private frozen-root directory {path:?}")]
    CreateFrozenPrivateDirectory {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("open private frozen-root directory {path:?}")]
    OpenFrozenPrivateDirectory {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("normalize private frozen-root directory {path:?}")]
    NormalizeFrozenPrivateDirectory {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("private frozen-root directory changed while retained: {path:?}")]
    FrozenPrivateDirectoryChanged { path: PathBuf },
    #[error(
        "private frozen-root setup failed at {path:?}: {primary}; bounded provisional cleanup also failed: {cleanup}"
    )]
    CleanupFrozenPrivateDirectory {
        path: PathBuf,
        primary: Box<Error>,
        cleanup: Box<Error>,
    },
    #[error("retained frozen-root stage changed before bounded cleanup: {stage:?}")]
    FrozenRetainedStageChanged { stage: PathBuf },
    #[error("inspect frozen-root publication name {path:?}")]
    InspectFrozenPublicationName {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("frozen root inode changed while publishing {stage:?} to {destination:?}")]
    FrozenRootChangedDuringPublication { stage: PathBuf, destination: PathBuf },
    #[error("frozen-root publication namespace changed from {stage:?} to {destination:?}: {reason}")]
    FrozenPublicationNamespaceMismatch {
        stage: PathBuf,
        destination: PathBuf,
        reason: &'static str,
    },
    #[error("{operation} at {path:?}")]
    SyncFrozenPublication {
        path: PathBuf,
        operation: &'static str,
        #[source]
        source: io::Error,
    },
    #[error(
        "frozen-root materialization failed at stage {stage:?}: {primary}; bounded stage cleanup also failed: {cleanup}"
    )]
    CleanupFrozenStage {
        stage: PathBuf,
        primary: Box<Error>,
        cleanup: Box<Error>,
    },
    #[error("detach frozen root {root:?} into private quarantine {quarantine:?}")]
    DetachFrozenRoot {
        root: PathBuf,
        quarantine: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("unsafe frozen root at discard boundary {root:?}: uid={owner}, mode={mode:#o}")]
    UnsafeFrozenRootDiscard { root: PathBuf, owner: u32, mode: u32 },
    #[error("frozen-root discard namespace changed from {root:?} to {quarantine:?}")]
    FrozenDiscardNamespaceMismatch { root: PathBuf, quarantine: PathBuf },
    #[error(
        "frozen-root discard failed for {root:?}: {primary}; restoring its exact public mode also failed: {restore}"
    )]
    RestoreFrozenDiscardRootMode {
        root: PathBuf,
        primary: Box<Error>,
        restore: Box<Error>,
    },
    #[error(
        "frozen-root detach failed for {quarantine:?}: {primary}; exact empty-quarantine cleanup also failed: {cleanup}"
    )]
    CleanupFrozenDiscardQuarantine {
        quarantine: PathBuf,
        primary: Box<Error>,
        cleanup: Box<Error>,
    },
    #[error("frozen root {root:?} changed while detaching it into {quarantine:?}")]
    FrozenRootChangedDuringDiscard { root: PathBuf, quarantine: PathBuf },
    #[error("frozen-root discard exceeds {limit} entries (got {actual})")]
    FrozenDiscardEntryLimit { limit: usize, actual: usize },
    #[error("frozen-root discard exceeds {limit} path components (got {actual})")]
    FrozenDiscardDepthLimit { limit: usize, actual: usize },
    #[error("frozen-root discard directory changed while it was pinned")]
    FrozenDiscardEntryChanged,
    #[error("open frozen-root discard entry {path:?}")]
    OpenFrozenDiscardEntry {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("remove frozen-root discard entry {path:?}")]
    RemoveFrozenDiscardEntry {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("open frozen-root discard directory")]
    OpenFrozenDiscardDirectory {
        #[source]
        source: io::Error,
    },
    #[error("read frozen-root discard directory")]
    ReadFrozenDiscardDirectory {
        #[source]
        source: io::Error,
    },
    #[error("installation")]
    Installation(#[from] installation::Error),
    #[error("fetch package {1}")]
    CacheFetch(#[source] cache::FetchError, package::Name),
    #[error("unpack package {1}, file {2}")]
    CacheUnpack(#[source] cache::UnpackError, package::Name, PathBuf),
    #[error("repository manager")]
    Repository(#[from] repository::manager::Error),
    #[error("package registry query")]
    Registry(#[from] crate::registry::Error),
    #[error("db")]
    Db(#[from] db::Error),
    #[error("prune")]
    Prune(#[from] prune::Error),
    #[error("io")]
    Io(#[from] io::Error),
    #[error("filesystem")]
    Filesystem(#[from] vfs::tree::Error),
    #[error("blit")]
    Blit(#[from] Errno),
    #[error("postblit")]
    PostBlit(#[from] postblit::Error),
    #[error("boot")]
    Boot(#[from] boot::Error),
    #[error("authorize standalone boot synchronization against clean transition evidence")]
    BootSynchronizationAuthority {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },
    #[error("authorize legacy compensating boot repair against exact clean transition evidence")]
    LegacyBootRepairAuthority {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },
    #[error("establish clean system-client startup baseline")]
    SystemStartupGate {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },
    #[error("prepare or authenticate durable state-transition tree identities")]
    StatefulTreeIdentity {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },
    #[error("repair an inactive archived state")]
    ArchivedStateRepair {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },
    #[error("decorate a stateful candidate through retained metadata capabilities")]
    StatefulCandidateMetadata {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },
    #[error("prepare or decorate an ephemeral candidate through retained metadata capabilities")]
    EphemeralCandidateMetadata {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },
    #[error("prepare or revalidate retained ephemeral trigger authority")]
    EphemeralTriggerAuthority {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },
    #[error("stateful candidate {candidate} reached activation without its retained metadata proof")]
    StatefulCandidateMetadataProofRequired { candidate: state::Id },
    #[error("materialize an inactive archived state")]
    ArchivedRepairMaterialization {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },
    #[error("materialize a stateful candidate through retained fixed staging")]
    StatefulCandidateMaterialization {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },
    #[error("{operation} cannot target fixed staging without its retained capability")]
    FixedStagingCapabilityRequired { operation: &'static str },
    #[error("state {state} database record changed between the verify scan and its retained repair")]
    VerifyStateChanged { state: state::Id },
    #[error("apply the selected active state through the durable ActiveReblit route")]
    LiveActiveReblit {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },
    #[error("fixed-staging cooperating-writer coordinator is poisoned")]
    FixedStagingCoordinatorPoisoned,
    #[error(
        "active-state reblit for state {state} committed, but whole-wrapper cleanup ended with {outcome}; do not reverse through fixed staging"
    )]
    ActiveReblitCommittedCleanupIncomplete {
        state: state::Id,
        outcome: &'static str,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },
    #[error(
        "durable tree-identity preparation for candidate {candidate} failed before activation; candidate tree remains at {location:?}, previous state is {previous:?}, and the candidate database row was not invalidated"
    )]
    StatefulTreeIdentityPreparationFailed {
        candidate: state::Id,
        previous: Option<state::Id>,
        location: PathBuf,
        #[source]
        source: Box<Error>,
    },
    /// Had issues processing user-provided string input
    #[error("string processing")]
    Dialog(#[from] tui::dialoguer::Error),
    /// The operation was explicitly cancelled at the user's request
    #[error("cancelled")]
    Cancelled,
    #[error("protect state mutation from interruption")]
    BlitSignalIgnore(#[from] signal::Error),
    #[error("load Gluon system intent or generated state snapshot")]
    LoadSystemModel(#[from] system_model::LoadError),
    #[error("update system model")]
    UpdateSystemModel(#[from] system_model::UpdateError),
    #[error("install")]
    Install(#[source] Box<install::Error>),
    #[error("remove")]
    Remove(#[source] Box<remove::Error>),
    #[error("fetch")]
    Fetch(#[source] Box<fetch::Error>),
    #[error("sync")]
    Sync(#[source] Box<sync::Error>),
    #[error("Gluon system intent doesn't exist at {0:?}")]
    ImportSystemIntentDoesntExist(PathBuf),
}
