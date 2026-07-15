//! Frozen, canonical build plans.
//!
//! [`DerivationPlan`] is the semantic boundary between resolution and
//! execution. It contains values only: the executor may index or borrow these
//! values, but must not infer another dependency, phase, policy, or output.

use std::collections::BTreeMap;

use sha2::{Digest, Sha256};
use stone::relation::{Dependency, Kind as StoneRelationKind, Provider};

use crate::build_policy::{
    AnalyzerKind, CompilerCachePolicySpec, SandboxDevPolicySpec, SandboxFilesystemPolicySpec, SandboxPolicySpec,
    SandboxSysPolicySpec, SandboxTmpPolicySpec,
};

pub use self::build_lock::{
    AnalyzerRole, BUILD_LOCK_FILE_NAME, BUILD_LOCK_SCHEMA_VERSION, BuildLock, BuildLockDecodeError,
    BuildLockValidationError, CompilerCacheRole, CompilerExecutableRole, InputOrigin, JobExecutableRole,
    JobStepSection, LockedIdentity, LockedOutput, LockedOutputRef, LockedPackage, LockedRequest, PackageInputSelection,
    Platform, RepositorySnapshot, RequestedInput, decode_build_lock, encode_build_lock, requested_inputs_digest,
};
pub use self::provenance::{
    DerivationProvenance, PolicyLayerProvenance, PolicyProvenance, PolicyTransitionProvenance,
    ProfileFragmentProvenance, policy_composition_identity, profile_aggregate_fingerprint,
};
pub use self::validation::{DerivationValidationError, DerivationValidationLimits};

mod build_lock;
mod provenance;
mod validation;

#[cfg(test)]
use self::validation::ProcessDataBudget;

/// Current schema used by [`DerivationPlan`].
pub const DERIVATION_PLAN_SCHEMA_VERSION: u32 = 16;

const DERIVATION_HASH_DOMAIN: &[u8] = b"os-tools-derivation-plan\0";

/// A completely resolved, target-specific build description.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerivationPlan {
    pub schema_version: u32,
    pub cast_version: String,
    pub cast_fingerprint: String,
    pub package: PackageIdentity,
    pub provenance: DerivationProvenance,
    pub source_lock_digest: String,
    pub sources: Vec<LockedSource>,
    pub build_lock: BuildLock,
    pub jobs: Vec<JobPlan>,
    pub environment: BTreeMap<String, String>,
    pub layout: BuilderLayout,
    pub execution: ExecutionPolicy,
    pub toolchain_commands: ToolchainCommandsPlan,
    pub analysis: AnalysisPlan,
    pub manifest_build_inputs: Vec<RelationPlan>,
    pub collection_rules: Vec<CollectionRulePlan>,
    pub outputs: Vec<OutputPlan>,
    pub source_date_epoch: i64,
}

impl DerivationPlan {
    /// Construct a plan using the current schema.
    pub fn new(package: PackageIdentity, build_lock: BuildLock, provenance: DerivationProvenance) -> Self {
        Self {
            schema_version: DERIVATION_PLAN_SCHEMA_VERSION,
            cast_version: String::new(),
            cast_fingerprint: String::new(),
            package,
            provenance,
            source_lock_digest: String::new(),
            sources: Vec::new(),
            build_lock,
            jobs: Vec::new(),
            environment: BTreeMap::new(),
            layout: BuilderLayout::default(),
            execution: ExecutionPolicy::default(),
            toolchain_commands: ToolchainCommandsPlan::default(),
            analysis: AnalysisPlan::default(),
            manifest_build_inputs: Vec::new(),
            collection_rules: Vec::new(),
            outputs: Vec::new(),
            source_date_epoch: 0,
        }
    }

    /// Encode the plan into the stable binary representation used for its
    /// identity.
    ///
    /// Declaration order is retained for phases, steps, hooks, arguments, PGO
    /// stages, analyzer handlers, and collection rules. Semantically unordered
    /// collections such as locked sources and outputs are sorted by stable keys.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut encoder = CanonicalEncoder::new(DERIVATION_HASH_DOMAIN);
        encoder.u32(self.schema_version);
        encoder.string(&self.cast_version);
        encoder.string(&self.cast_fingerprint);
        self.package.encode(&mut encoder);
        self.provenance.encode(&mut encoder);
        encoder.string(&self.source_lock_digest);

        let mut sources = self.sources.iter().collect::<Vec<_>>();
        sources.sort_by_key(|source| source.order());
        encoder.sequence(&sources, |encoder, source| source.encode(encoder));

        self.build_lock.encode_canonical(&mut encoder);
        encoder.sequence(&self.jobs, |encoder, job| job.encode(encoder));
        encoder.map(&self.environment);
        self.layout.encode(&mut encoder);
        self.execution.encode(&mut encoder);
        self.toolchain_commands.encode(&mut encoder);

        self.analysis.encode(&mut encoder);
        let mut manifest_build_inputs = self.manifest_build_inputs.clone();
        manifest_build_inputs.sort();
        encoder.sequence(&manifest_build_inputs, |encoder, relation| relation.encode(encoder));
        encoder.sequence(&self.collection_rules, |encoder, rule| rule.encode(encoder));

        let mut outputs = self.outputs.iter().collect::<Vec<_>>();
        outputs.sort_by(|left, right| left.name.cmp(&right.name));
        encoder.sequence(&outputs, |encoder, output| output.encode(encoder));
        encoder.i64(self.source_date_epoch);
        encoder.finish()
    }

    /// Hash the canonical plan with SHA-256.
    pub fn derivation_id(&self) -> DerivationId {
        DerivationId(format!("{:x}", Sha256::digest(self.canonical_bytes())))
    }
}

/// Stable hexadecimal identity of a frozen derivation.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DerivationId(String);

impl DerivationId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for DerivationId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Identity of the package being built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageIdentity {
    pub name: String,
    pub version: String,
    pub source_release: u64,
    pub build_release: u64,
    pub homepage: String,
    pub licenses: Vec<String>,
    pub architecture: String,
}

impl PackageIdentity {
    fn encode(&self, encoder: &mut CanonicalEncoder) {
        encoder.string(&self.name);
        encoder.string(&self.version);
        encoder.u64(self.source_release);
        encoder.u64(self.build_release);
        encoder.string(&self.homepage);
        let mut licenses = self.licenses.clone();
        licenses.sort();
        encoder.strings(&licenses);
        encoder.string(&self.architecture);
    }
}

/// One source with all mutable resolution removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LockedSource {
    Archive {
        order: u32,
        url: String,
        sha256: String,
        filename: String,
    },
    Git {
        order: u32,
        url: String,
        requested_ref: String,
        commit: String,
        materialization_sha256: String,
        directory: String,
    },
}

impl LockedSource {
    pub fn order(&self) -> u32 {
        match self {
            Self::Archive { order, .. } | Self::Git { order, .. } => *order,
        }
    }

    fn destination(&self) -> (&'static str, &str) {
        match self {
            Self::Archive { filename, .. } => ("filename", filename),
            Self::Git { directory, .. } => ("directory", directory),
        }
    }

    fn encode(&self, encoder: &mut CanonicalEncoder) {
        match self {
            Self::Archive {
                order,
                url,
                sha256,
                filename,
            } => {
                encoder.variant(0);
                encoder.u32(*order);
                encoder.string(url);
                encoder.string(sha256);
                encoder.string(filename);
            }
            Self::Git {
                order,
                url,
                requested_ref,
                commit,
                materialization_sha256,
                directory,
            } => {
                encoder.variant(1);
                encoder.u32(*order);
                encoder.string(url);
                encoder.string(requested_ref);
                encoder.string(commit);
                encoder.string(materialization_sha256);
                encoder.string(directory);
            }
        }
    }
}

/// One named build phase with ordered hooks and steps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhasePlan {
    pub name: String,
    pub pre: Vec<StepPlan>,
    pub steps: Vec<StepPlan>,
    pub post: Vec<StepPlan>,
}

impl PhasePlan {
    fn encode(&self, encoder: &mut CanonicalEncoder) {
        encoder.string(&self.name);
        encoder.sequence(&self.pre, |encoder, step| step.encode(encoder));
        encoder.sequence(&self.steps, |encoder, step| step.encode(encoder));
        encoder.sequence(&self.post, |encoder, step| step.encode(encoder));
    }
}

/// One executor invocation. PGO stages are distinct jobs because each has its
/// own ordered phase set and build/work directories.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobPlan {
    pub pgo_stage: Option<String>,
    pub pgo_dir: Option<String>,
    pub build_dir: String,
    pub work_dir: String,
    pub phases: Vec<PhasePlan>,
}

impl JobPlan {
    fn encode(&self, encoder: &mut CanonicalEncoder) {
        match &self.pgo_stage {
            Some(stage) => {
                encoder.variant(1);
                encoder.string(stage);
            }
            None => encoder.variant(0),
        }
        encode_optional_string(encoder, self.pgo_dir.as_deref());
        encoder.string(&self.build_dir);
        encoder.string(&self.work_dir);
        encoder.sequence(&self.phases, |encoder, phase| phase.encode(encoder));
    }
}

/// One explicit executor step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepPlan {
    Run {
        program: ExecutablePlan,
        args: Vec<String>,
        environment: BTreeMap<String, String>,
        working_dir: String,
    },
    /// Execute an exact native Linux ELF image retained below the build
    /// working directory rather than resolving an external provider
    /// capability. Scripts remain explicit [`Self::Shell`] steps;
    /// descriptor-executed shebangs fail closed without a pathname fallback.
    RunBuilt {
        program: String,
        args: Vec<String>,
        environment: BTreeMap<String, String>,
        working_dir: String,
    },
    Shell {
        interpreter: ExecutablePlan,
        declared_programs: Vec<ExecutablePlan>,
        script: String,
        environment: BTreeMap<String, String>,
        working_dir: String,
    },
    /// Built-in, fail-closed extraction of one exact locked archive.
    ExtractArchive {
        source: u32,
        destination: String,
        strip_components: u32,
    },
}

impl StepPlan {
    fn encode(&self, encoder: &mut CanonicalEncoder) {
        match self {
            Self::Run {
                program,
                args,
                environment,
                working_dir,
            } => {
                encoder.variant(0);
                program.encode(encoder);
                encoder.strings(args);
                encoder.map(environment);
                encoder.string(working_dir);
            }
            Self::RunBuilt {
                program,
                args,
                environment,
                working_dir,
            } => {
                encoder.variant(3);
                encoder.string(program);
                encoder.strings(args);
                encoder.map(environment);
                encoder.string(working_dir);
            }
            Self::Shell {
                interpreter,
                declared_programs,
                script,
                environment,
                working_dir,
            } => {
                encoder.variant(1);
                interpreter.encode(encoder);
                encoder.sequence(declared_programs, |encoder, program| program.encode(encoder));
                encoder.string(script);
                encoder.map(environment);
                encoder.string(working_dir);
            }
            Self::ExtractArchive {
                source,
                destination,
                strip_components,
            } => {
                encoder.variant(2);
                encoder.u32(*source);
                encoder.string(destination);
                encoder.u32(*strip_components);
            }
        }
    }
}

/// Guest paths that are visible to build steps.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BuilderLayout {
    pub hostname: String,
    pub guest_root: String,
    pub artifacts_dir: String,
    pub build_dir: String,
    pub source_dir: String,
    pub recipe_dir: String,
    pub install_dir: String,
    pub package_dir: String,
    pub ccache_dir: String,
    pub sccache_dir: String,
    pub go_cache_dir: String,
    pub go_mod_cache_dir: String,
    pub cargo_cache_dir: String,
    pub zig_cache_dir: String,
}

impl BuilderLayout {
    pub fn from_policy(sandbox: &SandboxPolicySpec, cache: &CompilerCachePolicySpec) -> Self {
        Self {
            hostname: sandbox.hostname.clone(),
            guest_root: sandbox.guest_root.clone(),
            artifacts_dir: sandbox.artifacts_dir.clone(),
            build_dir: sandbox.build_dir.clone(),
            source_dir: sandbox.source_dir.clone(),
            recipe_dir: sandbox.recipe_dir.clone(),
            install_dir: sandbox.install_dir.clone(),
            package_dir: sandbox.package_dir.clone(),
            ccache_dir: cache.ccache_dir.clone(),
            sccache_dir: cache.sccache_dir.clone(),
            go_cache_dir: cache.go_cache_dir.clone(),
            go_mod_cache_dir: cache.go_mod_cache_dir.clone(),
            cargo_cache_dir: cache.cargo_cache_dir.clone(),
            zig_cache_dir: cache.zig_cache_dir.clone(),
        }
    }

    pub fn cache_destinations(&self) -> [(&'static str, &str); 6] {
        [
            ("ccache", &self.ccache_dir),
            ("sccache", &self.sccache_dir),
            ("gocache", &self.go_cache_dir),
            ("gomodcache", &self.go_mod_cache_dir),
            ("cargocache", &self.cargo_cache_dir),
            ("zigcache", &self.zig_cache_dir),
        ]
    }

    fn encode(&self, encoder: &mut CanonicalEncoder) {
        encoder.string(&self.hostname);
        encoder.string(&self.guest_root);
        encoder.string(&self.artifacts_dir);
        encoder.string(&self.build_dir);
        encoder.string(&self.source_dir);
        encoder.string(&self.recipe_dir);
        encoder.string(&self.install_dir);
        encoder.string(&self.package_dir);
        encoder.string(&self.ccache_dir);
        encoder.string(&self.sccache_dir);
        encoder.string(&self.go_cache_dir);
        encoder.string(&self.go_mod_cache_dir);
        encoder.string(&self.cargo_cache_dir);
        encoder.string(&self.zig_cache_dir);
    }
}

/// Semantic execution choices visible to the build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionPolicy {
    /// Executor implementation contract, kept separate from the authored
    /// structural builder selected in [`BuildLock::builder`].
    pub executor: LockedIdentity,
    pub root_materialization: RootMaterializationMode,
    pub credentials: ExecutionCredentials,
    pub network: NetworkMode,
    pub filesystems: FilesystemPolicy,
    pub compiler_cache: bool,
    pub jobs: u32,
}

impl Default for ExecutionPolicy {
    fn default() -> Self {
        Self {
            executor: LockedIdentity {
                name: String::new(),
                fingerprint: String::new(),
            },
            root_materialization: RootMaterializationMode::LockedClosure,
            credentials: ExecutionCredentials::Unspecified,
            network: NetworkMode::Disabled,
            filesystems: FilesystemPolicy::default(),
            compiler_cache: false,
            jobs: 1,
        }
    }
}

impl ExecutionPolicy {
    fn encode(&self, encoder: &mut CanonicalEncoder) {
        self.executor.encode(encoder);
        encoder.variant(match self.root_materialization {
            RootMaterializationMode::LockedClosure => 0,
            RootMaterializationMode::PackageManagerState => 1,
        });
        encoder.variant(match self.credentials {
            ExecutionCredentials::IsolatedRoot => 0,
            ExecutionCredentials::Unspecified => 1,
        });
        encoder.variant(match self.network {
            NetworkMode::Disabled => 0,
            NetworkMode::Enabled => 1,
        });
        self.filesystems.encode(encoder);
        encoder.bool(self.compiler_cache);
        encoder.u32(self.jobs);
    }
}

/// Credentials exposed to every build and package-analysis process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionCredentials {
    /// Namespace ID zero maps only to the invoking caller's user and group.
    IsolatedRoot,
    /// Fail-closed value used by incomplete manually constructed plans.
    Unspecified,
}

impl ExecutionCredentials {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::IsolatedRoot => "isolated-root",
            Self::Unspecified => "unspecified",
        }
    }
}

/// How the build root is created from the package closure.
///
/// Frozen derivations permit only [`Self::LockedClosure`]: the executor may
/// materialize the exact package IDs already present in [`BuildLock`] and the
/// fixed build-root ABI, but may not consult package-manager state, compose a
/// system model, resolve providers, or discover transaction triggers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootMaterializationMode {
    LockedClosure,
    /// Represented so validation can reject attempts to cross the freeze
    /// boundary instead of silently selecting the stateful installation path.
    PackageManagerState,
}

impl RootMaterializationMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LockedClosure => "locked-closure",
            Self::PackageManagerState => "package-manager-state",
        }
    }
}

/// Complete pseudo-filesystem selection frozen into an execution plan.
///
/// Unlike the generic container API, this type makes proc unconditionally
/// absent and cannot express any `/sys` mount or a full host `/dev` view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FilesystemPolicy {
    pub proc: ProcFilesystem,
    pub tmp: TmpFilesystem,
    pub sys: SysFilesystem,
    pub dev: DevFilesystem,
}

impl Default for FilesystemPolicy {
    fn default() -> Self {
        Self {
            proc: ProcFilesystem::None,
            tmp: TmpFilesystem::Empty,
            sys: SysFilesystem::None,
            dev: DevFilesystem::Minimal,
        }
    }
}

impl From<&SandboxFilesystemPolicySpec> for FilesystemPolicy {
    fn from(policy: &SandboxFilesystemPolicySpec) -> Self {
        Self {
            proc: ProcFilesystem::None,
            tmp: match policy.tmp {
                SandboxTmpPolicySpec::Empty => TmpFilesystem::Empty,
            },
            sys: match policy.sys {
                SandboxSysPolicySpec::None => SysFilesystem::None,
            },
            dev: match policy.dev {
                SandboxDevPolicySpec::None => DevFilesystem::None,
                SandboxDevPolicySpec::Minimal => DevFilesystem::Minimal,
            },
        }
    }
}

impl FilesystemPolicy {
    fn encode(&self, encoder: &mut CanonicalEncoder) {
        encoder.variant(match self.proc {
            ProcFilesystem::None => 0,
        });
        encoder.variant(match self.tmp {
            TmpFilesystem::Empty => 0,
        });
        encoder.variant(match self.sys {
            SysFilesystem::None => 0,
        });
        encoder.variant(match self.dev {
            DevFilesystem::None => 0,
            DevFilesystem::Minimal => 1,
        });
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcFilesystem {
    None,
}

impl ProcFilesystem {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TmpFilesystem {
    Empty,
}

impl TmpFilesystem {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Empty => "empty",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SysFilesystem {
    None,
}

impl SysFilesystem {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DevFilesystem {
    None,
    Minimal,
}

impl DevFilesystem {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Minimal => "minimal",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkMode {
    Disabled,
    Enabled,
}

/// One executable guest path and the capability whose exact provider is
/// already resolved by [`BuildLock::requests`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutablePlan {
    pub path: String,
    pub requirement: RelationPlan,
}

impl ExecutablePlan {
    fn encode(&self, encoder: &mut CanonicalEncoder) {
        encoder.string(&self.path);
        self.requirement.encode(encoder);
    }
}

/// One frozen executable command with argv token identity preserved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutableCommandPlan {
    pub program: ExecutablePlan,
    pub args: Vec<String>,
}

impl ExecutableCommandPlan {
    fn encode(&self, encoder: &mut CanonicalEncoder) {
        self.program.encode(encoder);
        encoder.strings(&self.args);
    }
}

/// One semantic compiler role and its exact executable command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompilerCommandPlan {
    pub role: CompilerExecutableRole,
    pub command: ExecutableCommandPlan,
}

/// Every selected compiler, cache wrapper, and optional Mold command.
///
/// Compiler commands use a closed role sequence so a plan cannot silently
/// omit an environment-visible tool or introduce an unclassified executable.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolchainCommandsPlan {
    pub compilers: Vec<CompilerCommandPlan>,
    pub ccache: Option<ExecutablePlan>,
    pub sccache: Option<ExecutablePlan>,
    pub mold: Option<ExecutableCommandPlan>,
}

impl ToolchainCommandsPlan {
    pub const COMPILER_ROLES: [CompilerExecutableRole; 13] = [
        CompilerExecutableRole::Cc,
        CompilerExecutableRole::Cxx,
        CompilerExecutableRole::Objc,
        CompilerExecutableRole::Objcxx,
        CompilerExecutableRole::Cpp,
        CompilerExecutableRole::Objcpp,
        CompilerExecutableRole::Objcxxcpp,
        CompilerExecutableRole::Ar,
        CompilerExecutableRole::Ld,
        CompilerExecutableRole::Objcopy,
        CompilerExecutableRole::Nm,
        CompilerExecutableRole::Ranlib,
        CompilerExecutableRole::Strip,
    ];

    fn encode(&self, encoder: &mut CanonicalEncoder) {
        encoder.sequence(&self.compilers, |encoder, compiler| {
            compiler.role.encode(encoder);
            compiler.command.encode(encoder);
        });
        encode_optional_executable(encoder, self.ccache.as_ref());
        encode_optional_executable(encoder, self.sccache.as_ref());
        match &self.mold {
            Some(mold) => {
                encoder.variant(1);
                mold.encode(encoder);
            }
            None => encoder.variant(0),
        }
    }
}

fn encode_optional_executable(encoder: &mut CanonicalEncoder, executable: Option<&ExecutablePlan>) {
    match executable {
        Some(executable) => {
            encoder.variant(1);
            executable.encode(encoder);
        }
        None => encoder.variant(0),
    }
}

/// Exact analyzer programs reachable from the frozen handler/options graph.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AnalysisToolsPlan {
    pub pkg_config: Option<ExecutablePlan>,
    pub python: Option<ExecutablePlan>,
    pub objcopy: Option<ExecutablePlan>,
    pub strip: Option<ExecutablePlan>,
}

/// Frozen ordered handlers, tools, and switches consumed by package analysis.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalysisPlan {
    pub handlers: Vec<AnalyzerKind>,
    pub tools: AnalysisToolsPlan,
    pub debug: bool,
    pub strip: bool,
    pub compress_man: bool,
    pub remove_libtool: bool,
}

impl Default for AnalysisPlan {
    fn default() -> Self {
        Self {
            handlers: Vec::new(),
            tools: AnalysisToolsPlan::default(),
            debug: false,
            strip: true,
            compress_man: true,
            remove_libtool: true,
        }
    }
}

impl AnalysisPlan {
    fn encode(&self, encoder: &mut CanonicalEncoder) {
        encoder.sequence(&self.handlers, |encoder, handler| {
            encoder.variant(match handler {
                AnalyzerKind::IgnoreBlocked => 0,
                AnalyzerKind::Binary => 1,
                AnalyzerKind::Elf => 2,
                AnalyzerKind::PkgConfig => 3,
                AnalyzerKind::Python => 4,
                AnalyzerKind::CMake => 5,
                AnalyzerKind::CompressMan => 6,
                AnalyzerKind::IncludeAny => 7,
            });
        });
        encode_optional_analyzer_tool(encoder, self.tools.pkg_config.as_ref());
        encode_optional_analyzer_tool(encoder, self.tools.python.as_ref());
        encode_optional_analyzer_tool(encoder, self.tools.objcopy.as_ref());
        encode_optional_analyzer_tool(encoder, self.tools.strip.as_ref());
        encoder.bool(self.debug);
        encoder.bool(self.strip);
        encoder.bool(self.compress_man);
        encoder.bool(self.remove_libtool);
    }
}

fn encode_optional_analyzer_tool(encoder: &mut CanonicalEncoder, tool: Option<&ExecutablePlan>) {
    match tool {
        Some(tool) => {
            encoder.variant(1);
            tool.encode(encoder);
        }
        None => encoder.variant(0),
    }
}

/// A typed package relation carried across the derivation freeze boundary.
///
/// The kind and target are stored separately so execution never has to parse
/// authored `kind(target)` syntax. The same canonical value can be lowered
/// infallibly to either Stone relation role after [`DerivationPlan::validate`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RelationPlan {
    pub kind: RelationKind,
    pub name: String,
}

impl RelationPlan {
    pub fn to_dependency(&self) -> Dependency {
        Dependency {
            kind: self.kind.into(),
            name: self.name.clone(),
        }
    }

    pub fn to_provider(&self) -> Provider {
        Provider {
            kind: self.kind.into(),
            name: self.name.clone(),
        }
    }

    pub fn canonical_name(&self) -> String {
        self.to_dependency().to_name()
    }

    fn encode(&self, encoder: &mut CanonicalEncoder) {
        encoder.variant(self.kind as u8);
        encoder.string(&self.name);
    }
}

impl From<&Dependency> for RelationPlan {
    fn from(relation: &Dependency) -> Self {
        Self {
            kind: relation.kind.into(),
            name: relation.name.clone(),
        }
    }
}

impl From<Dependency> for RelationPlan {
    fn from(relation: Dependency) -> Self {
        Self {
            kind: relation.kind.into(),
            name: relation.name,
        }
    }
}

impl From<&Provider> for RelationPlan {
    fn from(relation: &Provider) -> Self {
        Self {
            kind: relation.kind.into(),
            name: relation.name.clone(),
        }
    }
}

impl From<Provider> for RelationPlan {
    fn from(relation: Provider) -> Self {
        Self {
            kind: relation.kind.into(),
            name: relation.name,
        }
    }
}

/// Capability namespace retained explicitly in a frozen relation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum RelationKind {
    PackageName = 0,
    SharedLibrary = 1,
    PkgConfig = 2,
    Interpreter = 3,
    CMake = 4,
    Python = 5,
    Binary = 6,
    SystemBinary = 7,
    PkgConfig32 = 8,
}

impl From<StoneRelationKind> for RelationKind {
    fn from(kind: StoneRelationKind) -> Self {
        match kind {
            StoneRelationKind::PackageName => Self::PackageName,
            StoneRelationKind::SharedLibrary => Self::SharedLibrary,
            StoneRelationKind::PkgConfig => Self::PkgConfig,
            StoneRelationKind::Interpreter => Self::Interpreter,
            StoneRelationKind::CMake => Self::CMake,
            StoneRelationKind::Python => Self::Python,
            StoneRelationKind::Binary => Self::Binary,
            StoneRelationKind::SystemBinary => Self::SystemBinary,
            StoneRelationKind::PkgConfig32 => Self::PkgConfig32,
        }
    }
}

impl From<RelationKind> for StoneRelationKind {
    fn from(kind: RelationKind) -> Self {
        match kind {
            RelationKind::PackageName => Self::PackageName,
            RelationKind::SharedLibrary => Self::SharedLibrary,
            RelationKind::PkgConfig => Self::PkgConfig,
            RelationKind::Interpreter => Self::Interpreter,
            RelationKind::CMake => Self::CMake,
            RelationKind::Python => Self::Python,
            RelationKind::Binary => Self::Binary,
            RelationKind::SystemBinary => Self::SystemBinary,
            RelationKind::PkgConfig32 => Self::PkgConfig32,
        }
    }
}

/// One declared package output after template and package composition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputPlan {
    pub name: String,
    pub package_name: String,
    pub include_in_manifest: bool,
    pub summary: Option<String>,
    pub description: Option<String>,
    pub provides_exclude: Vec<String>,
    pub runtime_exclude: Vec<String>,
    pub runtime_inputs: Vec<OutputRelation>,
    pub conflicts: Vec<RelationPlan>,
}

impl OutputPlan {
    fn encode(&self, encoder: &mut CanonicalEncoder) {
        encoder.string(&self.name);
        encoder.string(&self.package_name);
        encoder.bool(self.include_in_manifest);
        encode_optional_string(encoder, self.summary.as_deref());
        encode_optional_string(encoder, self.description.as_deref());
        let mut provides_exclude = self.provides_exclude.clone();
        provides_exclude.sort();
        encoder.strings(&provides_exclude);
        let mut runtime_exclude = self.runtime_exclude.clone();
        runtime_exclude.sort();
        encoder.strings(&runtime_exclude);

        let mut runtime_inputs = self.runtime_inputs.iter().collect::<Vec<_>>();
        runtime_inputs.sort();
        encoder.sequence(&runtime_inputs, |encoder, dependency| dependency.encode(encoder));
        let mut conflicts = self.conflicts.clone();
        conflicts.sort();
        encoder.sequence(&conflicts, |encoder, relation| relation.encode(encoder));
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum OutputRelation {
    Locked {
        relation: RelationPlan,
        reference: LockedOutputRef,
    },
    Planned {
        output: String,
    },
}

impl OutputRelation {
    fn encode(&self, encoder: &mut CanonicalEncoder) {
        match self {
            Self::Locked { relation, reference } => {
                encoder.variant(0);
                relation.encode(encoder);
                reference.encode(encoder);
            }
            Self::Planned { output } => {
                encoder.variant(1);
                encoder.string(output);
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathRuleKind {
    Any,
    Executable,
    Symlink,
    Special,
}

/// One collector rule in exact matching precedence order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectionRulePlan {
    pub output: String,
    pub kind: PathRuleKind,
    pub pattern: String,
}

impl CollectionRulePlan {
    fn encode(&self, encoder: &mut CanonicalEncoder) {
        encoder.string(&self.output);
        encoder.variant(match self.kind {
            PathRuleKind::Any => 0,
            PathRuleKind::Executable => 1,
            PathRuleKind::Symlink => 2,
            PathRuleKind::Special => 3,
        });
        encoder.string(&self.pattern);
    }
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn encode_optional_string(encoder: &mut CanonicalEncoder, value: Option<&str>) {
    match value {
        Some(value) => {
            encoder.variant(1);
            encoder.string(value);
        }
        None => encoder.variant(0),
    }
}

pub(super) struct CanonicalEncoder {
    bytes: Vec<u8>,
}

impl CanonicalEncoder {
    fn new(domain: &[u8]) -> Self {
        Self { bytes: domain.to_vec() }
    }

    pub(super) fn bool(&mut self, value: bool) {
        self.bytes.push(u8::from(value));
    }

    pub(super) fn variant(&mut self, value: u8) {
        self.bytes.push(value);
    }

    pub(super) fn u32(&mut self, value: u32) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    pub(super) fn u64(&mut self, value: u64) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    pub(super) fn i64(&mut self, value: i64) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    pub(super) fn string(&mut self, value: &str) {
        self.u64(value.len() as u64);
        self.bytes.extend_from_slice(value.as_bytes());
    }

    pub(super) fn strings(&mut self, values: &[String]) {
        self.sequence(values, |encoder, value| encoder.string(value));
    }

    pub(super) fn sequence<T>(&mut self, values: &[T], mut encode: impl FnMut(&mut Self, &T)) {
        self.u64(values.len() as u64);
        for value in values {
            encode(self, value);
        }
    }

    pub(super) fn map(&mut self, values: &BTreeMap<String, String>) {
        self.u64(values.len() as u64);
        for (key, value) in values {
            self.string(key);
            self.string(value);
        }
    }

    fn finish(self) -> Vec<u8> {
        self.bytes
    }
}

#[cfg(test)]
mod tests {
    use gluon_config::{EvaluationFingerprint, Evaluator, ImportPolicy, Source};

    use crate::{build_policy::layers::BuildPolicyOperation, spec::SourceUrlValidationError};

    use super::*;

    const SOURCE_LOCK_BYTES: &[u8] = b"canonical sample source lock";
    type NamedMutation<T> = (&'static str, Box<dyn Fn(&mut T)>);

    fn evaluation(logical_name: &str, source: &str, explicit_inputs: &[u8]) -> EvaluationFingerprint {
        Evaluator::default()
            .evaluate_with_inputs::<i64>(&Source::new(logical_name, source), explicit_inputs)
            .unwrap()
            .fingerprint
    }

    fn evaluation_with_import(logical_name: &str, explicit_inputs: &[u8]) -> EvaluationFingerprint {
        let policy = ImportPolicy::new()
            .with_embedded_module("sample.provenance", "4")
            .unwrap();
        Evaluator::default()
            .with_import_policy(policy)
            .evaluate_with_inputs::<i64>(&Source::new(logical_name, "import! sample.provenance"), explicit_inputs)
            .unwrap()
            .fingerprint
    }

    fn sample_provenance() -> DerivationProvenance {
        let profiles = vec![
            ProfileFragmentProvenance {
                logical_name: "vendor/profile.glu".to_owned(),
                evaluation: evaluation_with_import("profile.glu", &[]),
            },
            ProfileFragmentProvenance {
                logical_name: "admin/profile.d/local.glu".to_owned(),
                evaluation: evaluation("profile.d/local.glu", "2", &[]),
            },
        ];
        let layers = vec![
            PolicyLayerProvenance {
                name: "foundation".to_owned(),
                transitions: vec![PolicyTransitionProvenance {
                    operation: BuildPolicyOperation::Add,
                    origin: "default.glu".to_owned(),
                    evaluation: evaluation_with_import("default.glu", &[]),
                }],
            },
            PolicyLayerProvenance {
                name: "site".to_owned(),
                transitions: Vec::new(),
            },
        ];
        let policy_inputs = policy_composition_identity("aerynos", &layers);
        DerivationProvenance {
            recipe: evaluation_with_import("stone.glu", SOURCE_LOCK_BYTES),
            profiles,
            policy: PolicyProvenance {
                name: "aerynos".to_owned(),
                root: evaluation("policy.glu", "5", &policy_inputs),
                layers,
            },
        }
    }

    fn sample_plan() -> DerivationPlan {
        let provenance = sample_provenance();
        let mut build_lock = build_lock::sample_lock();
        build_lock.requests.extend(
            [
                "pkg-config",
                "python3",
                "llvm-objcopy",
                "llvm-strip",
                "objcopy",
                "strip",
                "cmake",
                "bash",
            ]
            .into_iter()
            .map(|name| {
                let mut origins = vec![InputOrigin::Policy {
                    source: "policy.glu".to_owned(),
                    field: "build_root.base".to_owned(),
                    index: 0,
                }];
                if name == "cmake" {
                    origins.extend(
                        ToolchainCommandsPlan::COMPILER_ROLES
                            .into_iter()
                            .map(|role| InputOrigin::CompilerExecutable { role }),
                    );
                }
                LockedRequest {
                    request: format!("binary({name})"),
                    package_id: "hello-id".to_owned(),
                    output: "out".to_owned(),
                    origins,
                }
            }),
        );
        build_lock.policy.name = provenance.policy.name.clone();
        build_lock.policy.fingerprint = provenance.policy.root.sha256.clone();
        build_lock.profile.fingerprint = profile_aggregate_fingerprint(&provenance.profiles);
        build_lock.normalize();
        let mut plan = DerivationPlan::new(
            PackageIdentity {
                name: "hello".to_owned(),
                version: "1.0.0".to_owned(),
                source_release: 1,
                build_release: 1,
                homepage: "https://example.invalid/hello".to_owned(),
                licenses: vec!["MPL-2.0".to_owned()],
                architecture: "x86_64".to_owned(),
            },
            build_lock,
            provenance,
        );
        plan.cast_version = "0.26.6".to_owned();
        plan.cast_fingerprint = "sha256:test-cast-semantics".to_owned();
        plan.source_lock_digest = sha256(SOURCE_LOCK_BYTES);
        plan.sources = vec![LockedSource::Archive {
            order: 0,
            url: "https://example.invalid/hello.tar.zst".to_owned(),
            sha256: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
            filename: "hello.tar.zst".to_owned(),
        }];
        plan.jobs = vec![JobPlan {
            pgo_stage: None,
            pgo_dir: None,
            build_dir: "/mason/build".to_owned(),
            work_dir: "/mason/build/hello".to_owned(),
            phases: vec![PhasePlan {
                name: "build".to_owned(),
                pre: Vec::new(),
                steps: vec![StepPlan::Run {
                    program: ExecutablePlan {
                        path: "/usr/bin/cmake".to_owned(),
                        requirement: RelationPlan {
                            kind: RelationKind::Binary,
                            name: "cmake".to_owned(),
                        },
                    },
                    args: vec!["--build".to_owned(), ".".to_owned()],
                    environment: BTreeMap::from([("CFLAGS".to_owned(), "-O2".to_owned())]),
                    working_dir: "/mason/build".to_owned(),
                }],
                post: Vec::new(),
            }],
        }];
        plan.environment = BTreeMap::from([
            ("HOME".to_owned(), "/mason/build".to_owned()),
            ("PATH".to_owned(), "/usr/bin:/bin".to_owned()),
        ]);
        plan.layout = BuilderLayout {
            hostname: "cast-builder".to_owned(),
            guest_root: "/mason".to_owned(),
            artifacts_dir: "/mason/artefacts".to_owned(),
            build_dir: "/mason/build".to_owned(),
            source_dir: "/mason/sources".to_owned(),
            recipe_dir: "/mason/recipe".to_owned(),
            install_dir: "/mason/install".to_owned(),
            package_dir: "/mason/recipe/pkg".to_owned(),
            ccache_dir: "/mason/ccache".to_owned(),
            sccache_dir: "/mason/sccache".to_owned(),
            go_cache_dir: "/mason/gocache".to_owned(),
            go_mod_cache_dir: "/mason/gomodcache".to_owned(),
            cargo_cache_dir: "/mason/cargocache".to_owned(),
            zig_cache_dir: "/mason/zigcache".to_owned(),
        };
        plan.execution = ExecutionPolicy {
            executor: LockedIdentity {
                name: "cast-executor-v1".to_owned(),
                fingerprint: "executor-fingerprint".to_owned(),
            },
            root_materialization: RootMaterializationMode::LockedClosure,
            credentials: ExecutionCredentials::IsolatedRoot,
            network: NetworkMode::Disabled,
            filesystems: FilesystemPolicy::default(),
            compiler_cache: false,
            jobs: 4,
        };
        plan.toolchain_commands.compilers = ToolchainCommandsPlan::COMPILER_ROLES
            .into_iter()
            .map(|role| CompilerCommandPlan {
                role,
                command: ExecutableCommandPlan {
                    program: sample_analyzer_tool("cmake"),
                    args: (role == CompilerExecutableRole::Cpp)
                        .then(|| vec!["-E".to_owned()])
                        .unwrap_or_default(),
                },
            })
            .collect();
        plan.analysis.handlers = vec![AnalyzerKind::Elf, AnalyzerKind::Python, AnalyzerKind::IncludeAny];
        plan.analysis.tools.python = Some(sample_analyzer_tool("python3"));
        plan.analysis.tools.strip = Some(sample_analyzer_tool("llvm-strip"));
        plan.manifest_build_inputs = vec![RelationPlan {
            kind: RelationKind::Binary,
            name: "cmake".to_owned(),
        }];
        plan.collection_rules = vec![
            CollectionRulePlan {
                output: "out".to_owned(),
                kind: PathRuleKind::Any,
                pattern: "*".to_owned(),
            },
            CollectionRulePlan {
                output: "out".to_owned(),
                kind: PathRuleKind::Executable,
                pattern: "/usr/bin/*".to_owned(),
            },
        ];
        plan.outputs = vec![OutputPlan {
            name: "out".to_owned(),
            package_name: "hello".to_owned(),
            include_in_manifest: true,
            summary: Some("Hello".to_owned()),
            description: None,
            provides_exclude: Vec::new(),
            runtime_exclude: Vec::new(),
            runtime_inputs: Vec::new(),
            conflicts: vec![RelationPlan {
                kind: RelationKind::PackageName,
                name: "busybox".to_owned(),
            }],
        }];
        plan.source_date_epoch = 1_700_000_000;
        plan
    }

    fn sample_analyzer_tool(name: &str) -> ExecutablePlan {
        ExecutablePlan {
            path: format!("/usr/bin/{name}"),
            requirement: RelationPlan {
                kind: RelationKind::Binary,
                name: name.to_owned(),
            },
        }
    }

    fn sample_git_source(order: u32, directory: &str) -> LockedSource {
        LockedSource::Git {
            order,
            url: format!("https://example.invalid/source-{order}.git"),
            requested_ref: "main".to_owned(),
            commit: "0123456789abcdef0123456789abcdef01234567".to_owned(),
            materialization_sha256: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_owned(),
            directory: directory.to_owned(),
        }
    }

    fn sample_step_mut(plan: &mut DerivationPlan) -> &mut StepPlan {
        &mut plan.jobs[0].phases[0].steps[0]
    }

    fn insert_prepare_archive_steps(plan: &mut DerivationPlan, steps: Vec<StepPlan>) {
        plan.jobs[0].phases.insert(
            0,
            PhasePlan {
                name: "Prepare".to_owned(),
                pre: Vec::new(),
                steps,
                post: Vec::new(),
            },
        );
    }

    fn archive_step(source: u32, destination: &str, strip_components: u32) -> StepPlan {
        StepPlan::ExtractArchive {
            source,
            destination: destination.to_owned(),
            strip_components,
        }
    }

    fn make_sample_shell(plan: &mut DerivationPlan) {
        let (environment, working_dir) = match sample_step_mut(plan) {
            StepPlan::Run {
                environment,
                working_dir,
                ..
            } => (environment.clone(), working_dir.clone()),
            StepPlan::RunBuilt { .. } | StepPlan::Shell { .. } | StepPlan::ExtractArchive { .. } => return,
        };
        *sample_step_mut(plan) = StepPlan::Shell {
            interpreter: sample_analyzer_tool("bash"),
            declared_programs: vec![sample_analyzer_tool("cmake")],
            script: "printf '%s\\n' hardened".to_owned(),
            environment,
            working_dir,
        };
    }

    fn make_sample_run_built(plan: &mut DerivationPlan) {
        let (environment, working_dir) = match sample_step_mut(plan) {
            StepPlan::Run {
                environment,
                working_dir,
                ..
            } => (environment.clone(), working_dir.clone()),
            StepPlan::RunBuilt { .. } | StepPlan::Shell { .. } | StepPlan::ExtractArchive { .. } => return,
        };
        *sample_step_mut(plan) = StepPlan::RunBuilt {
            program: "/mason/build/bin/self-test".to_owned(),
            args: vec!["--verify".to_owned()],
            environment,
            working_dir,
        };
    }

    fn measured_process_budget(plan: &DerivationPlan) -> ProcessDataBudget {
        let mut budget = ProcessDataBudget::new(DerivationValidationLimits::default());
        budget.validate(plan).unwrap();
        budget
    }

    fn assert_process_limit(
        error: DerivationValidationError,
        expected_field: &str,
        expected_actual: usize,
        expected_limit: usize,
        expected_unit: &'static str,
    ) {
        let DerivationValidationError::LimitExceeded {
            field,
            actual,
            limit,
            unit,
        } = error
        else {
            panic!("expected a process-data limit, found: {error}");
        };
        assert_eq!(field, expected_field);
        assert_eq!(actual, expected_actual);
        assert_eq!(limit, expected_limit);
        assert_eq!(unit, expected_unit);
    }

    #[test]
    fn identical_plans_have_identical_bytes_and_ids() {
        let first = sample_plan();
        let repeated = sample_plan();

        assert_eq!(first.canonical_bytes(), repeated.canonical_bytes());
        assert_eq!(first.derivation_id(), repeated.derivation_id());
        assert_eq!(first.derivation_id().as_str().len(), 64);
        first.validate().unwrap();
    }

    #[test]
    fn validation_rejects_pre_structural_archive_schema_fourteen() {
        let mut plan = sample_plan();
        plan.schema_version = 14;

        assert!(matches!(
            plan.validate(),
            Err(DerivationValidationError::UnsupportedSchema {
                found: 14,
                expected: DERIVATION_PLAN_SCHEMA_VERSION,
            })
        ));
    }

    #[test]
    fn structural_archive_steps_are_prepare_only_locked_bounded_and_nonoverlapping() {
        let mut valid = sample_plan();
        insert_prepare_archive_steps(&mut valid, vec![archive_step(0, "vendor/source", 1)]);
        valid.validate().unwrap();

        let mut hook = sample_plan();
        hook.jobs[0].phases.insert(
            0,
            PhasePlan {
                name: "Prepare".to_owned(),
                pre: vec![archive_step(0, "source", 1)],
                steps: Vec::new(),
                post: Vec::new(),
            },
        );
        assert!(matches!(
            hook.validate(),
            Err(DerivationValidationError::ArchiveStepOutsidePrepare { ref field })
                if field == "jobs[0].phases[0].pre[0]"
        ));

        let mut wrong_kind = sample_plan();
        wrong_kind.sources.push(sample_git_source(1, "git-source"));
        insert_prepare_archive_steps(&mut wrong_kind, vec![archive_step(1, "source", 1)]);
        assert!(matches!(
            wrong_kind.validate(),
            Err(DerivationValidationError::InvalidArchiveStepSource { source_index: 1, .. })
        ));

        let mut unsafe_destination = sample_plan();
        insert_prepare_archive_steps(&mut unsafe_destination, vec![archive_step(0, "../source", 1)]);
        assert!(matches!(
            unsafe_destination.validate(),
            Err(DerivationValidationError::UnsafeArchiveStepDestination { .. })
        ));

        let mut excessive_strip = sample_plan();
        insert_prepare_archive_steps(&mut excessive_strip, vec![archive_step(0, "source", 129)]);
        assert!(matches!(
            excessive_strip.validate(),
            Err(DerivationValidationError::ArchiveStripComponentsLimit {
                found: 129,
                limit: 128,
                ..
            })
        ));

        let mut overlapping = sample_plan();
        insert_prepare_archive_steps(
            &mut overlapping,
            vec![archive_step(0, "source", 1), archive_step(0, "source/nested", 1)],
        );
        assert!(matches!(
            overlapping.validate(),
            Err(DerivationValidationError::OverlappingArchiveDestinations { job: 0 })
        ));
    }

    #[test]
    fn archive_destinations_cannot_merge_with_git_sources_in_either_source_order() {
        let mut archive_first = sample_plan();
        archive_first.sources.push(sample_git_source(1, "source"));
        insert_prepare_archive_steps(&mut archive_first, vec![archive_step(0, "source/nested", 1)]);

        let mut git_first = sample_plan();
        git_first.sources = vec![
            sample_git_source(0, "source"),
            LockedSource::Archive {
                order: 1,
                url: "https://example.invalid/hello.tar.zst".to_owned(),
                sha256: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
                filename: "hello.tar.zst".to_owned(),
            },
        ];
        insert_prepare_archive_steps(&mut git_first, vec![archive_step(1, "source", 1)]);

        for plan in [archive_first, git_first] {
            assert!(matches!(
                plan.validate(),
                Err(DerivationValidationError::ArchiveDestinationOverlapsGitSource {
                    job: 0,
                    ref destination,
                    ref directory,
                    ..
                }) if destination.starts_with("source") && directory == "source"
            ));
        }
    }

    #[test]
    fn validation_rejects_embedded_nul_in_every_process_data_class() {
        let mutations: Vec<(&str, Box<dyn Fn(&mut DerivationPlan)>)> = vec![
            (
                "environment[0].name",
                Box::new(|plan| {
                    plan.environment.clear();
                    plan.environment.insert("BAD\0NAME".to_owned(), String::new());
                }),
            ),
            (
                "environment[0].value",
                Box::new(|plan| {
                    plan.environment.clear();
                    plan.environment.insert("GOOD".to_owned(), "bad\0value".to_owned());
                }),
            ),
            (
                "layout.build_dir",
                Box::new(|plan| plan.layout.build_dir = "/mason/bu\0ild".to_owned()),
            ),
            (
                "jobs[0].pgo_dir",
                Box::new(|plan| {
                    plan.jobs[0].pgo_stage = Some("one".to_owned());
                    plan.jobs[0].pgo_dir = Some("/mason/pgo\0one".to_owned());
                }),
            ),
            (
                "toolchain_commands.compilers[0].command.program.path",
                Box::new(|plan| {
                    plan.toolchain_commands.compilers[0].command.program.path = "/usr/bin/cm\0ake".to_owned();
                }),
            ),
            (
                "toolchain_commands.compilers[0].command.args[0]",
                Box::new(|plan| {
                    plan.toolchain_commands.compilers[0]
                        .command
                        .args
                        .push("bad\0argument".to_owned());
                }),
            ),
            (
                "jobs[0].phases[0].steps[0].program.path",
                Box::new(|plan| {
                    let StepPlan::Run { program, .. } = sample_step_mut(plan) else {
                        unreachable!()
                    };
                    program.path = "/usr/bin/cm\0ake".to_owned();
                }),
            ),
            (
                "jobs[0].phases[0].steps[0].args[0]",
                Box::new(|plan| {
                    let StepPlan::Run { args, .. } = sample_step_mut(plan) else {
                        unreachable!()
                    };
                    args[0] = "--bu\0ild".to_owned();
                }),
            ),
            (
                "jobs[0].phases[0].steps[0].environment[0].name",
                Box::new(|plan| {
                    let StepPlan::Run { environment, .. } = sample_step_mut(plan) else {
                        unreachable!()
                    };
                    environment.clear();
                    environment.insert("BAD\0NAME".to_owned(), String::new());
                }),
            ),
            (
                "jobs[0].phases[0].steps[0].environment[0].value",
                Box::new(|plan| {
                    let StepPlan::Run { environment, .. } = sample_step_mut(plan) else {
                        unreachable!()
                    };
                    environment.clear();
                    environment.insert("GOOD".to_owned(), "bad\0value".to_owned());
                }),
            ),
            (
                "jobs[0].phases[0].steps[0].working_dir",
                Box::new(|plan| {
                    let StepPlan::Run { working_dir, .. } = sample_step_mut(plan) else {
                        unreachable!()
                    };
                    *working_dir = "/mason/bu\0ild".to_owned();
                }),
            ),
            (
                "jobs[0].phases[0].steps[0].interpreter.path",
                Box::new(|plan| {
                    make_sample_shell(plan);
                    let StepPlan::Shell { interpreter, .. } = sample_step_mut(plan) else {
                        unreachable!()
                    };
                    interpreter.path = "/usr/bin/ba\0sh".to_owned();
                }),
            ),
            (
                "jobs[0].phases[0].steps[0].declared_programs[0].path",
                Box::new(|plan| {
                    make_sample_shell(plan);
                    let StepPlan::Shell { declared_programs, .. } = sample_step_mut(plan) else {
                        unreachable!()
                    };
                    declared_programs[0].path = "/usr/bin/cm\0ake".to_owned();
                }),
            ),
            (
                "jobs[0].phases[0].steps[0].script",
                Box::new(|plan| {
                    make_sample_shell(plan);
                    let StepPlan::Shell { script, .. } = sample_step_mut(plan) else {
                        unreachable!()
                    };
                    *script = "printf bad\0script".to_owned();
                }),
            ),
        ];

        for (expected_field, mutate) in mutations {
            let mut plan = sample_plan();
            mutate(&mut plan);
            assert!(
                matches!(
                    plan.validate(),
                    Err(DerivationValidationError::EmbeddedNul { field }) if field == expected_field
                ),
                "NUL in {expected_field} crossed the freeze boundary"
            );
        }
    }

    #[test]
    fn validation_requires_portable_environment_names_globally_and_per_step() {
        for invalid in ["", "9STARTS_WITH_DIGIT", "HAS-DASH", "HAS=EQUALS", "NÖN_ASCII"] {
            let mut global = sample_plan();
            global.environment.clear();
            global.environment.insert(invalid.to_owned(), String::new());
            assert!(matches!(
                global.validate(),
                Err(DerivationValidationError::InvalidEnvironmentName { field })
                    if field == "environment[0].name"
            ));

            let mut local = sample_plan();
            let StepPlan::Run { environment, .. } = sample_step_mut(&mut local) else {
                unreachable!()
            };
            environment.clear();
            environment.insert(invalid.to_owned(), String::new());
            assert!(matches!(
                local.validate(),
                Err(DerivationValidationError::InvalidEnvironmentName { field })
                    if field == "jobs[0].phases[0].steps[0].environment[0].name"
            ));
        }

        for valid in ["_", "A", "A9_B"] {
            let mut plan = sample_plan();
            plan.environment.clear();
            plan.environment.insert(valid.to_owned(), String::new());
            plan.validate().unwrap();
        }
    }

    #[test]
    fn validation_distinguishes_safe_empty_arguments_from_missing_programs_and_scripts() {
        let mut no_arguments = sample_plan();
        let StepPlan::Run { args, .. } = sample_step_mut(&mut no_arguments) else {
            unreachable!()
        };
        args.clear();
        no_arguments.validate().unwrap();

        let mut empty_argument = sample_plan();
        let StepPlan::Run { args, .. } = sample_step_mut(&mut empty_argument) else {
            unreachable!()
        };
        *args = vec![String::new()];
        empty_argument.validate().unwrap();

        let mut missing_program = sample_plan();
        let StepPlan::Run { program, .. } = sample_step_mut(&mut missing_program) else {
            unreachable!()
        };
        program.path.clear();
        assert!(matches!(
            missing_program.validate(),
            Err(DerivationValidationError::UnsafeAbsolutePath { field, .. })
                if field == "jobs[0].phases[0].steps[0].program.path"
        ));

        let mut missing_script = sample_plan();
        make_sample_shell(&mut missing_script);
        let StepPlan::Shell { script, .. } = sample_step_mut(&mut missing_script) else {
            unreachable!()
        };
        script.clear();
        assert!(matches!(
            missing_script.validate(),
            Err(DerivationValidationError::Empty { field })
                if field == "jobs[0].phases[0].steps[0].script"
        ));

        let mut missing_interpreter = sample_plan();
        make_sample_shell(&mut missing_interpreter);
        let StepPlan::Shell { interpreter, .. } = sample_step_mut(&mut missing_interpreter) else {
            unreachable!()
        };
        interpreter.path.clear();
        assert!(matches!(
            missing_interpreter.validate(),
            Err(DerivationValidationError::UnsafeAbsolutePath { field, .. })
                if field == "jobs[0].phases[0].steps[0].interpreter.path"
        ));
    }

    #[test]
    fn process_collection_limits_accept_n_and_reject_n_plus_one() {
        let mut two_jobs = sample_plan();
        two_jobs.jobs.push(two_jobs.jobs[0].clone());
        let job_limits = DerivationValidationLimits {
            max_jobs: 2,
            ..DerivationValidationLimits::default()
        };
        two_jobs.validate_with_limits(job_limits).unwrap();
        let mut three_jobs = two_jobs;
        three_jobs.jobs.push(three_jobs.jobs[0].clone());
        assert_process_limit(
            three_jobs.validate_with_limits(job_limits).unwrap_err(),
            "jobs",
            3,
            2,
            "items",
        );

        let mut two_phases = sample_plan();
        let mut second_phase = two_phases.jobs[0].phases[0].clone();
        second_phase.name = "check".to_owned();
        two_phases.jobs[0].phases.push(second_phase);
        let phase_limits = DerivationValidationLimits {
            max_phases_per_job: 2,
            ..DerivationValidationLimits::default()
        };
        two_phases.validate_with_limits(phase_limits).unwrap();
        let mut three_phases = two_phases;
        let mut third_phase = three_phases.jobs[0].phases[0].clone();
        third_phase.name = "install".to_owned();
        three_phases.jobs[0].phases.push(third_phase);
        assert_process_limit(
            three_phases.validate_with_limits(phase_limits).unwrap_err(),
            "jobs[0].phases",
            3,
            2,
            "items",
        );

        let mut two_steps = sample_plan();
        let second_step = two_steps.jobs[0].phases[0].steps[0].clone();
        two_steps.jobs[0].phases[0].steps.push(second_step);
        let section_limits = DerivationValidationLimits {
            max_steps_per_section: 2,
            max_total_steps: 2,
            ..DerivationValidationLimits::default()
        };
        two_steps.validate_with_limits(section_limits).unwrap();
        let mut three_steps = two_steps;
        let third_step = three_steps.jobs[0].phases[0].steps[0].clone();
        three_steps.jobs[0].phases[0].steps.push(third_step);
        assert_process_limit(
            three_steps.validate_with_limits(section_limits).unwrap_err(),
            "jobs[0].phases[0].steps",
            3,
            2,
            "items",
        );

        let mut two_total_steps = sample_plan();
        let pre_step = two_total_steps.jobs[0].phases[0].steps[0].clone();
        two_total_steps.jobs[0].phases[0].pre = vec![pre_step];
        let total_step_limits = DerivationValidationLimits {
            max_steps_per_section: 2,
            max_total_steps: 2,
            ..DerivationValidationLimits::default()
        };
        two_total_steps.validate_with_limits(total_step_limits).unwrap();
        let mut three_total_steps = two_total_steps;
        let post_step = three_total_steps.jobs[0].phases[0].steps[0].clone();
        three_total_steps.jobs[0].phases[0].post = vec![post_step];
        assert_process_limit(
            three_total_steps.validate_with_limits(total_step_limits).unwrap_err(),
            "jobs[0].phases[0].post",
            3,
            2,
            "total steps",
        );

        let argument_limits = DerivationValidationLimits {
            max_arguments_per_step: 2,
            ..DerivationValidationLimits::default()
        };
        sample_plan().validate_with_limits(argument_limits).unwrap();
        let mut three_arguments = sample_plan();
        let StepPlan::Run { args, .. } = sample_step_mut(&mut three_arguments) else {
            unreachable!()
        };
        args.push("--verbose".to_owned());
        assert_process_limit(
            three_arguments.validate_with_limits(argument_limits).unwrap_err(),
            "jobs[0].phases[0].steps[0].args",
            3,
            2,
            "items",
        );

        let mut two_programs = sample_plan();
        make_sample_shell(&mut two_programs);
        let StepPlan::Shell { declared_programs, .. } = sample_step_mut(&mut two_programs) else {
            unreachable!()
        };
        declared_programs.push(declared_programs[0].clone());
        let program_limits = DerivationValidationLimits {
            max_declared_programs_per_step: 2,
            ..DerivationValidationLimits::default()
        };
        two_programs.validate_with_limits(program_limits).unwrap();
        let mut three_programs = two_programs;
        let StepPlan::Shell { declared_programs, .. } = sample_step_mut(&mut three_programs) else {
            unreachable!()
        };
        declared_programs.push(declared_programs[0].clone());
        assert_process_limit(
            three_programs.validate_with_limits(program_limits).unwrap_err(),
            "jobs[0].phases[0].steps[0].declared_programs",
            3,
            2,
            "items",
        );
    }

    #[test]
    fn environment_and_string_limits_accept_n_and_reject_n_plus_one() {
        let environment_limits = DerivationValidationLimits {
            max_environment_entries: 3,
            ..DerivationValidationLimits::default()
        };
        sample_plan().validate_with_limits(environment_limits).unwrap();
        let mut four_effective = sample_plan();
        let StepPlan::Run { environment, .. } = sample_step_mut(&mut four_effective) else {
            unreachable!()
        };
        environment.insert("LDFLAGS".to_owned(), "-Wl,--as-needed".to_owned());
        assert_process_limit(
            four_effective.validate_with_limits(environment_limits).unwrap_err(),
            "jobs[0].phases[0].steps[0].effective_environment",
            4,
            3,
            "items",
        );

        let name_limits = DerivationValidationLimits {
            max_environment_name_bytes: 6,
            ..DerivationValidationLimits::default()
        };
        sample_plan().validate_with_limits(name_limits).unwrap();
        let mut seven_byte_name = sample_plan();
        let StepPlan::Run { environment, .. } = sample_step_mut(&mut seven_byte_name) else {
            unreachable!()
        };
        environment.clear();
        environment.insert("CFLAGSS".to_owned(), String::new());
        assert_process_limit(
            seven_byte_name.validate_with_limits(name_limits).unwrap_err(),
            "jobs[0].phases[0].steps[0].environment[0].name",
            7,
            6,
            "bytes",
        );

        let mut sixty_four_bytes = sample_plan();
        let StepPlan::Run { args, .. } = sample_step_mut(&mut sixty_four_bytes) else {
            unreachable!()
        };
        args[0] = "x".repeat(64);
        let string_limits = DerivationValidationLimits {
            max_process_string_bytes: 64,
            ..DerivationValidationLimits::default()
        };
        sixty_four_bytes.validate_with_limits(string_limits).unwrap();
        let mut sixty_five_bytes = sixty_four_bytes;
        let StepPlan::Run { args, .. } = sample_step_mut(&mut sixty_five_bytes) else {
            unreachable!()
        };
        args[0].push('x');
        assert_process_limit(
            sixty_five_bytes.validate_with_limits(string_limits).unwrap_err(),
            "jobs[0].phases[0].steps[0].args[0]",
            65,
            64,
            "bytes",
        );

        let mut sixty_four_byte_path = sample_plan();
        sixty_four_byte_path.layout.cargo_cache_dir = format!("/mason/{}", "x".repeat(57));
        assert_eq!(sixty_four_byte_path.layout.cargo_cache_dir.len(), 64);
        let path_limits = DerivationValidationLimits {
            max_path_bytes: 64,
            ..DerivationValidationLimits::default()
        };
        sixty_four_byte_path.validate_with_limits(path_limits).unwrap();
        let mut sixty_five_byte_path = sixty_four_byte_path;
        sixty_five_byte_path.layout.cargo_cache_dir.push('x');
        assert_process_limit(
            sixty_five_byte_path.validate_with_limits(path_limits).unwrap_err(),
            "layout.cargo_cache_dir",
            65,
            64,
            "path bytes",
        );
    }

    #[test]
    fn execve_and_aggregate_limits_accept_n_and_reject_n_plus_one() {
        let plan = sample_plan();
        let probe_limits = DerivationValidationLimits {
            max_execve_bytes: 0,
            ..DerivationValidationLimits::default()
        };
        let DerivationValidationError::LimitExceeded {
            actual: execve_bytes,
            field,
            ..
        } = plan.validate_with_limits(probe_limits).unwrap_err()
        else {
            unreachable!()
        };
        assert_eq!(field, "jobs[0].phases[0].steps[0].execve");

        let execve_limits = DerivationValidationLimits {
            max_execve_bytes: execve_bytes,
            ..DerivationValidationLimits::default()
        };
        plan.validate_with_limits(execve_limits).unwrap();
        let mut one_more_execve_byte = plan.clone();
        let StepPlan::Run { args, .. } = sample_step_mut(&mut one_more_execve_byte) else {
            unreachable!()
        };
        args[0].push('x');
        assert_process_limit(
            one_more_execve_byte.validate_with_limits(execve_limits).unwrap_err(),
            "jobs[0].phases[0].steps[0].execve",
            execve_bytes + 1,
            execve_bytes,
            "bytes",
        );

        let measured = measured_process_budget(&plan);
        let item_limits = DerivationValidationLimits {
            max_total_process_items: measured.total_items,
            ..DerivationValidationLimits::default()
        };
        plan.validate_with_limits(item_limits).unwrap();
        let mut one_more_item = plan.clone();
        let StepPlan::Run { args, .. } = sample_step_mut(&mut one_more_item) else {
            unreachable!()
        };
        args.push(String::new());
        assert_process_limit(
            one_more_item.validate_with_limits(item_limits).unwrap_err(),
            "jobs[0].phases[0].steps[0].environment",
            measured.total_items + 1,
            measured.total_items,
            "total process items",
        );

        let text_limits = DerivationValidationLimits {
            max_total_process_text_bytes: measured.total_text_bytes,
            ..DerivationValidationLimits::default()
        };
        plan.validate_with_limits(text_limits).unwrap();
        let mut one_more_text_byte = plan;
        let StepPlan::Run { args, .. } = sample_step_mut(&mut one_more_text_byte) else {
            unreachable!()
        };
        args[0].push('x');
        assert_process_limit(
            one_more_text_byte.validate_with_limits(text_limits).unwrap_err(),
            "jobs[0].phases[0].steps[0].working_dir",
            measured.total_text_bytes + 1,
            measured.total_text_bytes,
            "total process text bytes",
        );
    }

    #[test]
    fn frozen_filesystem_policy_is_explicit_restricted_and_ordered() {
        let policy = FilesystemPolicy::default();
        assert_eq!(policy.proc, ProcFilesystem::None);
        assert_eq!(policy.tmp, TmpFilesystem::Empty);
        assert_eq!(policy.sys, SysFilesystem::None);
        assert_eq!(policy.dev, DevFilesystem::Minimal);

        let mut encoder = CanonicalEncoder::new(&[]);
        policy.encode(&mut encoder);
        assert_eq!(encoder.finish(), [0, 0, 0, 1]);
    }

    #[test]
    fn every_allowed_filesystem_policy_change_changes_derivation_identity() {
        let original = sample_plan();
        let original_id = original.derivation_id();

        let mut without_dev = original;
        without_dev.execution.filesystems.dev = DevFilesystem::None;
        assert_ne!(original_id, without_dev.derivation_id());
    }

    #[test]
    fn validation_rejects_enabled_frozen_networking() {
        let mut plan = sample_plan();
        plan.execution.network = NetworkMode::Enabled;

        assert!(matches!(
            plan.validate(),
            Err(DerivationValidationError::NetworkEnabled)
        ));
    }

    #[test]
    fn validation_requires_complete_executor_identity() {
        for (field, clear) in [
            (
                "execution.executor.name",
                Box::new(|plan: &mut DerivationPlan| plan.execution.executor.name.clear())
                    as Box<dyn Fn(&mut DerivationPlan)>,
            ),
            (
                "execution.executor.fingerprint",
                Box::new(|plan: &mut DerivationPlan| plan.execution.executor.fingerprint.clear()),
            ),
        ] {
            let mut plan = sample_plan();
            clear(&mut plan);
            assert!(matches!(
                plan.validate(),
                Err(DerivationValidationError::Empty { field: actual }) if actual == field
            ));
        }
    }

    #[test]
    fn validation_rejects_package_manager_state_root_materialization() {
        let mut plan = sample_plan();
        plan.execution.root_materialization = RootMaterializationMode::PackageManagerState;

        assert!(matches!(
            plan.validate(),
            Err(DerivationValidationError::PackageManagerRootMaterialization)
        ));
    }

    #[test]
    fn validation_requires_explicit_isolated_credentials() {
        let mut plan = sample_plan();
        plan.execution.credentials = ExecutionCredentials::Unspecified;

        assert!(matches!(
            plan.validate(),
            Err(DerivationValidationError::UnspecifiedExecutionCredentials)
        ));
    }

    #[test]
    fn complete_evaluation_fingerprint_is_part_of_canonical_identity() {
        let original = sample_plan();
        let original_id = original.derivation_id();
        assert_eq!(original.provenance.recipe.imported_modules.len(), 1);
        assert_eq!(
            original.provenance.recipe.imported_modules[0].logical_name,
            "sample.provenance"
        );
        let mutations: Vec<NamedMutation<EvaluationFingerprint>> = vec![
            (
                "root-logical-name",
                Box::new(|fingerprint| fingerprint.root_logical_name.push_str("-changed")),
            ),
            (
                "root-source-sha256",
                Box::new(|fingerprint| fingerprint.root_source_sha256.push('0')),
            ),
            (
                "import-logical-name",
                Box::new(|fingerprint| fingerprint.imported_modules[0].logical_name.push_str("-changed")),
            ),
            (
                "import-sha256",
                Box::new(|fingerprint| fingerprint.imported_modules[0].sha256.push('0')),
            ),
            (
                "gluon-version",
                Box::new(|fingerprint| fingerprint.gluon_version = "test-gluon-version"),
            ),
            (
                "configuration-abi",
                Box::new(|fingerprint| fingerprint.configuration_abi_version += 1),
            ),
            (
                "evaluator-policy-abi",
                Box::new(|fingerprint| fingerprint.evaluator_policy_version += 1),
            ),
            (
                "explicit-inputs-sha256",
                Box::new(|fingerprint| fingerprint.explicit_inputs_sha256.push('0')),
            ),
            ("aggregate-sha256", Box::new(|fingerprint| fingerprint.sha256.push('0'))),
        ];

        for (name, mutate) in mutations {
            let mut changed = original.clone();
            mutate(&mut changed.provenance.recipe);
            assert_ne!(original_id, changed.derivation_id(), "{name} was not hashed");
        }
    }

    #[test]
    fn nested_provenance_shape_and_order_are_part_of_canonical_identity() {
        let original = sample_plan();
        let original_id = original.derivation_id();
        let mutations: Vec<NamedMutation<DerivationProvenance>> = vec![
            (
                "profile-logical-name",
                Box::new(|provenance| provenance.profiles[0].logical_name.push_str("-changed")),
            ),
            ("profile-order", Box::new(|provenance| provenance.profiles.reverse())),
            (
                "profile-evaluation",
                Box::new(|provenance| provenance.profiles[0].evaluation.root_logical_name.push_str("-changed")),
            ),
            (
                "policy-name",
                Box::new(|provenance| provenance.policy.name.push_str("-changed")),
            ),
            (
                "policy-root",
                Box::new(|provenance| provenance.policy.root.root_logical_name.push_str("-changed")),
            ),
            (
                "empty-policy-layer",
                Box::new(|provenance| {
                    provenance.policy.layers.pop();
                }),
            ),
            (
                "policy-layer-order",
                Box::new(|provenance| provenance.policy.layers.reverse()),
            ),
            (
                "policy-layer-name",
                Box::new(|provenance| provenance.policy.layers[0].name.push_str("-changed")),
            ),
            (
                "policy-transition-operation",
                Box::new(|provenance| {
                    provenance.policy.layers[0].transitions[0].operation = BuildPolicyOperation::Replace;
                }),
            ),
            (
                "policy-transition-origin",
                Box::new(|provenance| provenance.policy.layers[0].transitions[0].origin.push_str("-changed")),
            ),
            (
                "policy-transition-evaluation",
                Box::new(|provenance| {
                    provenance.policy.layers[0].transitions[0]
                        .evaluation
                        .root_logical_name
                        .push_str("-changed");
                }),
            ),
        ];

        for (name, mutate) in mutations {
            let mut changed = original.clone();
            mutate(&mut changed.provenance);
            assert_ne!(original_id, changed.derivation_id(), "{name} was not hashed");
        }
    }

    #[test]
    fn v2_provenance_aggregate_helpers_preserve_nested_semantics() {
        let provenance = sample_provenance();
        let profile_identity = profile_aggregate_fingerprint(&provenance.profiles);
        let policy_identity = policy_composition_identity(&provenance.policy.name, &provenance.policy.layers);

        assert_eq!(profile_identity, profile_aggregate_fingerprint(&provenance.profiles));
        assert_eq!(
            policy_identity,
            policy_composition_identity(&provenance.policy.name, &provenance.policy.layers)
        );

        let mut profiles = provenance.profiles.clone();
        profiles.reverse();
        assert_ne!(profile_identity, profile_aggregate_fingerprint(&profiles));
        profiles = provenance.profiles.clone();
        profiles[0].logical_name.push_str("-changed");
        assert_ne!(profile_identity, profile_aggregate_fingerprint(&profiles));
        profiles = provenance.profiles.clone();
        profiles[0].evaluation.evaluator_policy_version += 1;
        assert_ne!(profile_identity, profile_aggregate_fingerprint(&profiles));

        let mut layers = provenance.policy.layers.clone();
        layers.pop();
        assert_ne!(
            policy_identity,
            policy_composition_identity(&provenance.policy.name, &layers),
            "an empty named layer is semantic"
        );
        layers = provenance.policy.layers.clone();
        layers.reverse();
        assert_ne!(
            policy_identity,
            policy_composition_identity(&provenance.policy.name, &layers)
        );
        layers = provenance.policy.layers.clone();
        layers[0].transitions[0].evaluation.configuration_abi_version += 1;
        assert_ne!(
            policy_identity,
            policy_composition_identity(&provenance.policy.name, &layers)
        );
    }

    #[test]
    fn validation_rejects_invalid_nested_evaluation_fingerprints_at_the_exact_field() {
        let cases: Vec<NamedMutation<DerivationPlan>> = vec![
            (
                "provenance.recipe",
                Box::new(|plan| plan.provenance.recipe.sha256.push('0')),
            ),
            (
                "provenance.profiles[0].evaluation",
                Box::new(|plan| plan.provenance.profiles[0].evaluation.sha256.push('0')),
            ),
            (
                "provenance.policy.root",
                Box::new(|plan| plan.provenance.policy.root.sha256.push('0')),
            ),
            (
                "provenance.policy.layers[0].transitions[0].evaluation",
                Box::new(|plan| {
                    plan.provenance.policy.layers[0].transitions[0]
                        .evaluation
                        .sha256
                        .push('0');
                }),
            ),
        ];

        for (expected, corrupt) in cases {
            let mut plan = sample_plan();
            corrupt(&mut plan);
            assert!(matches!(
                plan.validate(),
                Err(DerivationValidationError::InvalidEvaluationFingerprint { field, .. })
                    if field == expected
            ));
        }
    }

    #[test]
    fn validation_rejects_ambient_or_non_normalized_provenance_names() {
        let cases: Vec<NamedMutation<DerivationPlan>> = vec![
            (
                "provenance.recipe.root_logical_name",
                Box::new(|plan| plan.provenance.recipe.root_logical_name = "/home/user/stone.glu".to_owned()),
            ),
            (
                "provenance.recipe.imported_modules[0].logical_name",
                Box::new(|plan| {
                    plan.provenance.recipe.imported_modules[0].logical_name = "nested/../module.glu".to_owned();
                }),
            ),
            (
                "provenance.profiles[0].logical_name",
                Box::new(|plan| plan.provenance.profiles[0].logical_name = "C:\\profile.glu".to_owned()),
            ),
            (
                "provenance.profiles[0].evaluation.root_logical_name",
                Box::new(|plan| {
                    plan.provenance.profiles[0].evaluation.root_logical_name = "./profile.glu".to_owned();
                }),
            ),
            (
                "provenance.policy.root.root_logical_name",
                Box::new(|plan| plan.provenance.policy.root.root_logical_name = "policy//root.glu".to_owned()),
            ),
            (
                "provenance.policy.layers[0].transitions[0].evaluation.root_logical_name",
                Box::new(|plan| {
                    plan.provenance.policy.layers[0].transitions[0]
                        .evaluation
                        .root_logical_name = "/etc/policy.glu".to_owned();
                }),
            ),
        ];

        for (expected, corrupt) in cases {
            let mut plan = sample_plan();
            corrupt(&mut plan);
            assert!(matches!(
                plan.validate(),
                Err(DerivationValidationError::InvalidLogicalName { field, .. })
                    if field == expected
            ));
        }
    }

    #[test]
    fn validation_binds_recipe_and_profiles_to_their_locked_inputs() {
        let mut recipe = sample_plan();
        recipe.source_lock_digest = sha256(b"different source lock");
        assert!(matches!(
            recipe.validate(),
            Err(DerivationValidationError::RecipeSourceLockDigestMismatch { .. })
        ));

        let mut blank = sample_plan();
        blank.provenance.profiles[0].logical_name = "  ".to_owned();
        assert!(matches!(
            blank.validate(),
            Err(DerivationValidationError::Empty { field })
                if field == "provenance.profiles[0].logical_name"
        ));

        let mut duplicate = sample_plan();
        duplicate.provenance.profiles[1].logical_name = duplicate.provenance.profiles[0].logical_name.clone();
        assert!(matches!(
            duplicate.validate(),
            Err(DerivationValidationError::DuplicateProfileLogicalName {
                first_index: 0,
                duplicate_index: 1,
                ..
            })
        ));

        let mut aggregate = sample_plan();
        aggregate.build_lock.profile.fingerprint.push_str("-changed");
        assert!(matches!(
            aggregate.validate(),
            Err(DerivationValidationError::ProfileAggregateMismatch { .. })
        ));
    }

    #[test]
    fn validation_binds_policy_name_root_and_composition_to_the_build_lock() {
        let mut name = sample_plan();
        name.build_lock.policy.name.push_str("-changed");
        assert!(matches!(
            name.validate(),
            Err(DerivationValidationError::PolicyNameMismatch { .. })
        ));

        let mut root = sample_plan();
        root.build_lock.policy.fingerprint.push_str("-changed");
        assert!(matches!(
            root.validate(),
            Err(DerivationValidationError::PolicyAggregateMismatch { .. })
        ));

        let mut duplicate = sample_plan();
        duplicate.provenance.policy.layers[1].name = duplicate.provenance.policy.layers[0].name.clone();
        assert!(matches!(
            duplicate.validate(),
            Err(DerivationValidationError::DuplicatePolicyLayer {
                first_index: 0,
                duplicate_index: 1,
                ..
            })
        ));

        let mut composition = sample_plan();
        composition.provenance.policy.layers[1].name.push_str("-changed");
        assert!(matches!(
            composition.validate(),
            Err(DerivationValidationError::PolicyCompositionDigestMismatch { .. })
        ));
    }

    #[test]
    fn validation_rejects_non_normalized_policy_origins() {
        for origin in [
            "/absolute.glu",
            "C:\\policy.glu",
            "nested//policy.glu",
            "nested/./policy.glu",
            "nested/../policy.glu",
        ] {
            let mut plan = sample_plan();
            plan.provenance.policy.layers[0].transitions[0].origin = origin.to_owned();
            assert!(matches!(
                plan.validate(),
                Err(DerivationValidationError::InvalidPolicyOrigin { value, .. }) if value == origin
            ));
        }
    }

    #[test]
    fn validation_replays_policy_transition_state() {
        for operation in [BuildPolicyOperation::Replace, BuildPolicyOperation::Modify] {
            let mut missing_initial_state = sample_plan();
            missing_initial_state.provenance.policy.layers[0].transitions[0].operation = operation;
            assert!(matches!(
                missing_initial_state.validate(),
                Err(DerivationValidationError::InvalidPolicyTransition {
                    operation: actual,
                    ..
                }) if actual == operation
            ));
        }

        let mut second_add = sample_plan();
        let repeated_add = second_add.provenance.policy.layers[0].transitions[0].clone();
        second_add.provenance.policy.layers[0].transitions.push(repeated_add);
        assert!(matches!(
            second_add.validate(),
            Err(DerivationValidationError::InvalidPolicyTransition {
                operation: BuildPolicyOperation::Add,
                ..
            })
        ));

        let mut absent = sample_plan();
        absent.provenance.policy.layers[0].transitions.clear();
        assert!(matches!(
            absent.validate(),
            Err(DerivationValidationError::MissingPolicyState)
        ));
    }

    #[test]
    fn validation_requires_complete_cast_implementation_identity() {
        for (field, clear) in [
            (
                "cast_version",
                Box::new(|plan: &mut DerivationPlan| plan.cast_version.clear()) as Box<dyn Fn(&mut DerivationPlan)>,
            ),
            (
                "cast_fingerprint",
                Box::new(|plan: &mut DerivationPlan| plan.cast_fingerprint.clear()),
            ),
        ] {
            let mut plan = sample_plan();
            clear(&mut plan);
            assert!(matches!(
                plan.validate(),
                Err(DerivationValidationError::Empty { field: actual }) if actual == field
            ));
        }
    }

    #[test]
    fn validation_rejects_artifact_filename_escape_components() {
        for name in [
            "",
            ".",
            "..",
            "/tmp/escape",
            "../../escape",
            "name/child",
            "name\\child",
        ] {
            let mut plan = sample_plan();
            plan.package.name = name.to_owned();
            assert!(matches!(
                plan.validate(),
                Err(DerivationValidationError::InvalidPackageName { field, value })
                    if field == "package.name" && value == name
            ));
        }

        for version in ["1/../../escape", "1\\escape", "1\ninvalid"] {
            let mut plan = sample_plan();
            plan.package.version = version.to_owned();
            assert!(matches!(
                plan.validate(),
                Err(DerivationValidationError::InvalidArtifactComponent { field, value })
                    if field == "package.version" && value == version
            ));
        }

        let mut non_numeric_version = sample_plan();
        non_numeric_version.package.version = "v1.0".to_owned();
        assert!(matches!(
            non_numeric_version.validate(),
            Err(DerivationValidationError::InvalidPackageVersion { value }) if value == "v1.0"
        ));

        let mut output_name = sample_plan();
        output_name.outputs[0].name = "../escape".to_owned();
        assert!(matches!(
            output_name.validate(),
            Err(DerivationValidationError::InvalidPackageName { field, .. })
                if field == "outputs[0].name"
        ));

        let mut package_name = sample_plan();
        package_name.outputs[0].package_name = "../escape".to_owned();
        assert!(matches!(
            package_name.validate(),
            Err(DerivationValidationError::InvalidPackageName { field, .. })
                if field == "outputs[0].package_name"
        ));
    }

    #[test]
    fn typed_relations_lower_to_both_stone_roles_without_reparsing() {
        for kind in [
            StoneRelationKind::PackageName,
            StoneRelationKind::SharedLibrary,
            StoneRelationKind::PkgConfig,
            StoneRelationKind::Interpreter,
            StoneRelationKind::CMake,
            StoneRelationKind::Python,
            StoneRelationKind::Binary,
            StoneRelationKind::SystemBinary,
            StoneRelationKind::PkgConfig32,
        ] {
            let dependency = Dependency::new(kind, "target(with-nesting)").unwrap();
            let relation = RelationPlan::from(&dependency);
            assert_eq!(relation.to_dependency(), dependency);
            assert_eq!(relation.to_provider().kind, dependency.kind);
            assert_eq!(relation.to_provider().name, dependency.name);
        }
    }

    #[test]
    fn validation_rejects_unsupported_artifact_architecture_at_freeze() {
        let mut plan = sample_plan();
        plan.package.architecture = "mips64".to_owned();
        plan.build_lock.target_platform.architecture = "mips64".to_owned();

        let error = plan.validate().unwrap_err();
        assert!(matches!(
            error,
            DerivationValidationError::UnsupportedArtifactArchitecture { ref value, .. }
                if value == "mips64"
        ));
        assert_eq!(
            error.to_string(),
            "package.architecture: unsupported Stone artifact architecture \"mips64\"; expected one of x86_64, x86, aarch64, riscv64"
        );
    }

    #[test]
    fn validation_rejects_every_invalid_output_exclusion_before_freeze() {
        for (field, mutate) in [
            (
                "outputs[0].provides_exclude[0]",
                Box::new(|plan: &mut DerivationPlan| plan.outputs[0].provides_exclude.push("(".to_owned()))
                    as Box<dyn Fn(&mut DerivationPlan)>,
            ),
            (
                "outputs[0].runtime_exclude[0]",
                Box::new(|plan: &mut DerivationPlan| plan.outputs[0].runtime_exclude.push("[".to_owned())),
            ),
        ] {
            let mut plan = sample_plan();
            mutate(&mut plan);
            assert!(matches!(
                plan.validate(),
                Err(DerivationValidationError::InvalidRegex { field: actual, .. })
                    if actual == field
            ));
        }
    }

    #[test]
    fn validation_rejects_invalid_collection_globs_before_freeze() {
        let mut plan = sample_plan();
        plan.collection_rules[1].pattern = "[".to_owned();

        let error = plan.validate().unwrap_err();
        assert!(matches!(
            error,
            DerivationValidationError::InvalidGlob { ref field, .. }
                if field == "collection_rules[1].pattern"
        ));
        assert!(error.to_string().contains("collection_rules[1].pattern"));
    }

    #[test]
    fn validation_requires_the_explicit_root_output_but_allows_empty_splits() {
        let mut missing = sample_plan();
        missing.outputs[0].name = "dev".to_owned();
        for rule in &mut missing.collection_rules {
            rule.output = "dev".to_owned();
        }
        assert!(matches!(
            missing.validate(),
            Err(DerivationValidationError::MissingRootOutput)
        ));

        let mut mismatched = sample_plan();
        mismatched.outputs[0].package_name = "other".to_owned();
        assert!(matches!(
            mismatched.validate(),
            Err(DerivationValidationError::RootOutputPackageMismatch {
                index: 0,
                expected,
                found,
            }) if expected == "hello" && found == "other"
        ));

        let mut excluded = sample_plan();
        excluded.outputs[0].include_in_manifest = false;
        assert!(matches!(
            excluded.validate(),
            Err(DerivationValidationError::RootOutputExcludedFromManifest { index: 0 })
        ));

        let mut empty_split = sample_plan();
        empty_split.outputs.push(OutputPlan {
            name: "empty".to_owned(),
            package_name: "hello-empty".to_owned(),
            include_in_manifest: true,
            summary: None,
            description: None,
            provides_exclude: Vec::new(),
            runtime_exclude: Vec::new(),
            runtime_inputs: Vec::new(),
            conflicts: Vec::new(),
        });
        empty_split.validate().unwrap();
    }

    #[test]
    fn validation_rejects_invalid_typed_relation_targets_with_exact_fields() {
        let mut manifest = sample_plan();
        manifest.manifest_build_inputs[0].name.clear();
        assert!(matches!(
            manifest.validate(),
            Err(DerivationValidationError::InvalidRelation { field, .. })
                if field == "manifest_build_inputs[0]"
        ));

        let mut conflict = sample_plan();
        conflict.outputs[0].conflicts[0].name = "unbalanced)".to_owned();
        assert!(matches!(
            conflict.validate(),
            Err(DerivationValidationError::InvalidRelation { field, .. })
                if field == "outputs[0].conflicts[0]"
        ));
    }

    #[test]
    fn analyzer_handler_order_is_semantic_while_output_order_is_not() {
        let mut first = sample_plan();
        first.outputs.push(OutputPlan {
            name: "dev".to_owned(),
            package_name: "hello-devel".to_owned(),
            include_in_manifest: true,
            summary: None,
            description: None,
            provides_exclude: Vec::new(),
            runtime_exclude: Vec::new(),
            runtime_inputs: Vec::new(),
            conflicts: Vec::new(),
        });
        let mut outputs_reordered = first.clone();
        outputs_reordered.outputs.reverse();

        assert_eq!(first.canonical_bytes(), outputs_reordered.canonical_bytes());
        assert_eq!(first.derivation_id(), outputs_reordered.derivation_id());

        let mut handlers_reordered = first.clone();
        handlers_reordered.analysis.handlers.swap(0, 1);

        assert_ne!(first.canonical_bytes(), handlers_reordered.canonical_bytes());
        assert_ne!(first.derivation_id(), handlers_reordered.derivation_id());
    }

    #[test]
    fn analysis_handler_validation_repeats_policy_invariants() {
        let mut empty = sample_plan();
        empty.analysis.handlers.clear();
        assert!(matches!(
            empty.validate(),
            Err(DerivationValidationError::Empty { field }) if field == "analysis.handlers"
        ));

        let mut duplicate = sample_plan();
        duplicate.analysis.handlers.insert(1, AnalyzerKind::Elf);
        assert!(matches!(
            duplicate.validate(),
            Err(DerivationValidationError::DuplicateAnalyzer { name }) if name == "Elf"
        ));

        let mut missing = sample_plan();
        missing.analysis.handlers.pop();
        assert!(matches!(
            missing.validate(),
            Err(DerivationValidationError::MissingAnalyzer { name }) if name == "IncludeAny"
        ));

        let mut misplaced = sample_plan();
        misplaced.analysis.handlers.swap(0, 2);
        assert!(matches!(
            misplaced.validate(),
            Err(DerivationValidationError::AnalyzerMustBeLast { name }) if name == "IncludeAny"
        ));
    }

    #[test]
    fn analyzer_tool_validation_is_exact_and_fail_closed() {
        let mut missing = sample_plan();
        missing.analysis.tools.python = None;
        assert!(matches!(
            missing.validate(),
            Err(DerivationValidationError::MissingAnalyzerTool { field })
                if field == "analysis.tools.python"
        ));

        let mut unexpected = sample_plan();
        unexpected.analysis.tools.pkg_config = Some(sample_analyzer_tool("pkg-config"));
        assert!(matches!(
            unexpected.validate(),
            Err(DerivationValidationError::UnexpectedAnalyzerTool { field })
                if field == "analysis.tools.pkg_config"
        ));

        let mut non_executable = sample_plan();
        non_executable.analysis.tools.python.as_mut().unwrap().requirement.kind = RelationKind::PkgConfig;
        assert!(matches!(
            non_executable.validate(),
            Err(DerivationValidationError::ExecutableRequirementNotRunnable { field, .. })
                if field == "analysis.tools.python.requirement"
        ));

        let mut unsafe_name = sample_plan();
        unsafe_name.analysis.tools.python.as_mut().unwrap().requirement.name = "../python3".to_owned();
        assert!(matches!(
            unsafe_name.validate(),
            Err(DerivationValidationError::InvalidExecutableRequirement { field, .. })
                if field == "analysis.tools.python.requirement"
        ));

        let mut program_mismatch = sample_plan();
        program_mismatch.analysis.tools.python.as_mut().unwrap().path = "/usr/bin/not-python".to_owned();
        assert!(matches!(
            program_mismatch.validate(),
            Err(DerivationValidationError::ExecutablePathMismatch { field, .. })
                if field == "analysis.tools.python.path"
        ));

        let mut unlocked = sample_plan();
        let python = unlocked.analysis.tools.python.as_mut().unwrap();
        python.requirement.name = "unlocked-python".to_owned();
        python.path = "/usr/bin/unlocked-python".to_owned();
        assert!(matches!(
            unlocked.validate(),
            Err(DerivationValidationError::UnlockedExecutable { field, request })
                if field == "analysis.tools.python.requirement" && request == "binary(unlocked-python)"
        ));
    }

    #[test]
    fn every_structural_executable_is_path_bound_and_exactly_locked() {
        for path in ["cmake", "/", "/usr/bin/../bin/cmake", "/usr/bin//cmake"] {
            let mut plan = sample_plan();
            let StepPlan::Run { program, .. } = &mut plan.jobs[0].phases[0].steps[0] else {
                unreachable!()
            };
            program.path = path.to_owned();
            assert!(matches!(
                plan.validate(),
                Err(DerivationValidationError::UnsafeAbsolutePath { field, .. })
                    if field == "jobs[0].phases[0].steps[0].program.path"
            ));
        }

        let mut unsupported = sample_plan();
        let StepPlan::Run { program, .. } = &mut unsupported.jobs[0].phases[0].steps[0] else {
            unreachable!()
        };
        program.requirement.kind = RelationKind::PkgConfig;
        assert!(matches!(
            unsupported.validate(),
            Err(DerivationValidationError::ExecutableRequirementNotRunnable { field, .. })
                if field == "jobs[0].phases[0].steps[0].program.requirement"
        ));

        let mut mismatched = sample_plan();
        let StepPlan::Run { program, .. } = &mut mismatched.jobs[0].phases[0].steps[0] else {
            unreachable!()
        };
        program.requirement.kind = RelationKind::SystemBinary;
        assert!(matches!(
            mismatched.validate(),
            Err(DerivationValidationError::ExecutablePathMismatch { field, expected, .. })
                if field == "jobs[0].phases[0].steps[0].program.path" && expected == "/usr/sbin/cmake"
        ));

        let mut unlocked_run = sample_plan();
        let StepPlan::Run { program, .. } = &mut unlocked_run.jobs[0].phases[0].steps[0] else {
            unreachable!()
        };
        program.path = "/usr/bin/unlocked-run".to_owned();
        program.requirement.name = "unlocked-run".to_owned();
        assert!(matches!(
            unlocked_run.validate(),
            Err(DerivationValidationError::UnlockedExecutable { field, request })
                if field == "jobs[0].phases[0].steps[0].program.requirement"
                    && request == "binary(unlocked-run)"
        ));

        let shell = |interpreter: ExecutablePlan, declared_programs: Vec<ExecutablePlan>| StepPlan::Shell {
            interpreter,
            declared_programs,
            script: "true".to_owned(),
            environment: BTreeMap::new(),
            working_dir: "/mason/build".to_owned(),
        };
        let mut unlocked_interpreter = sample_plan();
        unlocked_interpreter.jobs[0].phases[0].steps = vec![shell(
            ExecutablePlan {
                path: "/usr/bin/unlocked-shell".to_owned(),
                requirement: RelationPlan {
                    kind: RelationKind::Binary,
                    name: "unlocked-shell".to_owned(),
                },
            },
            Vec::new(),
        )];
        assert!(matches!(
            unlocked_interpreter.validate(),
            Err(DerivationValidationError::UnlockedExecutable { field, .. })
                if field == "jobs[0].phases[0].steps[0].interpreter.requirement"
        ));

        let mut unlocked_declared = sample_plan();
        unlocked_declared.jobs[0].phases[0].steps = vec![shell(
            sample_analyzer_tool("bash"),
            vec![ExecutablePlan {
                path: "/usr/bin/unlocked-declared".to_owned(),
                requirement: RelationPlan {
                    kind: RelationKind::Binary,
                    name: "unlocked-declared".to_owned(),
                },
            }],
        )];
        assert!(matches!(
            unlocked_declared.validate(),
            Err(DerivationValidationError::UnlockedExecutable { field, .. })
                if field == "jobs[0].phases[0].steps[0].declared_programs[0].requirement"
        ));
    }

    #[test]
    fn unusual_program_paths_require_an_explicit_package_capability() {
        let mut plan = sample_plan();
        plan.build_lock.requests.push(LockedRequest {
            request: "odd-tool".to_owned(),
            package_id: "hello-id".to_owned(),
            output: "out".to_owned(),
            origins: vec![InputOrigin::JobExecutable {
                job: 0,
                phase: 0,
                phase_name: "build".to_owned(),
                section: JobStepSection::Steps,
                step: 0,
                role: JobExecutableRole::RunProgram,
            }],
        });
        plan.build_lock.normalize();
        {
            let StepPlan::Run { program, .. } = &mut plan.jobs[0].phases[0].steps[0] else {
                unreachable!()
            };
            *program = ExecutablePlan {
                path: "/opt/odd/bin/tool".to_owned(),
                requirement: RelationPlan {
                    kind: RelationKind::PackageName,
                    name: "odd-tool".to_owned(),
                },
            };
        }
        plan.validate().unwrap();

        let StepPlan::Run { program, .. } = &mut plan.jobs[0].phases[0].steps[0] else {
            unreachable!()
        };
        program.path = "/usr/bin/tool".to_owned();
        assert!(matches!(
            plan.validate(),
            Err(DerivationValidationError::AmbiguousPackageExecutable { field, .. })
                if field == "jobs[0].phases[0].steps[0].program.path"
        ));
    }

    #[test]
    fn every_required_semantic_mutation_changes_identity() {
        let original = sample_plan();
        let original_id = original.derivation_id();
        let mutations: Vec<(&str, Box<dyn Fn(&mut DerivationPlan)>)> = vec![
            ("cast-version", Box::new(|plan| plan.cast_version.push_str("-changed"))),
            (
                "cast-implementation",
                Box::new(|plan| plan.cast_fingerprint.push_str("-changed")),
            ),
            (
                "source",
                Box::new(|plan| match &mut plan.sources[0] {
                    LockedSource::Archive { sha256, .. } => sha256.push_str("-changed"),
                    LockedSource::Git { .. } => unreachable!(),
                }),
            ),
            (
                "source-materialization",
                Box::new(|plan| match &mut plan.sources[0] {
                    LockedSource::Archive { filename, .. } => filename.push_str("-changed"),
                    LockedSource::Git { .. } => unreachable!(),
                }),
            ),
            (
                "dependency",
                Box::new(|plan| plan.build_lock.packages[0].package_id.push_str("-changed")),
            ),
            (
                "input-origin",
                Box::new(|plan| {
                    plan.build_lock.requests[0].origins[0] = InputOrigin::Check {
                        selection: PackageInputSelection::Package,
                        index: 0,
                    };
                }),
            ),
            (
                "target-platform",
                Box::new(|plan| plan.build_lock.target_platform.architecture = "aarch64".to_owned()),
            ),
            (
                "policy",
                Box::new(|plan| plan.build_lock.policy.fingerprint.push_str("-changed")),
            ),
            (
                "target-policy",
                Box::new(|plan| plan.build_lock.target.fingerprint.push_str("-changed")),
            ),
            (
                "profile",
                Box::new(|plan| plan.build_lock.profile.fingerprint.push_str("-changed")),
            ),
            (
                "toolchain",
                Box::new(|plan| plan.build_lock.toolchain.fingerprint.push_str("-changed")),
            ),
            (
                "builder",
                Box::new(|plan| plan.build_lock.builder.fingerprint.push_str("-changed")),
            ),
            (
                "phase",
                Box::new(|plan| match &mut plan.jobs[0].phases[0].steps[0] {
                    StepPlan::Run { args, .. } => args.push("--verbose".to_owned()),
                    StepPlan::RunBuilt { .. } | StepPlan::Shell { .. } | StepPlan::ExtractArchive { .. } => {
                        unreachable!()
                    }
                }),
            ),
            (
                "step-program-path",
                Box::new(|plan| {
                    let StepPlan::Run { program, .. } = &mut plan.jobs[0].phases[0].steps[0] else {
                        unreachable!()
                    };
                    program.path.push_str("-changed");
                }),
            ),
            (
                "step-program-requirement",
                Box::new(|plan| {
                    let StepPlan::Run { program, .. } = &mut plan.jobs[0].phases[0].steps[0] else {
                        unreachable!()
                    };
                    program.requirement.name.push_str("-changed");
                }),
            ),
            (
                "compiler-command-path",
                Box::new(|plan| {
                    plan.toolchain_commands.compilers[0]
                        .command
                        .program
                        .path
                        .push_str("-changed");
                }),
            ),
            (
                "compiler-command-requirement",
                Box::new(|plan| {
                    plan.toolchain_commands.compilers[0]
                        .command
                        .program
                        .requirement
                        .name
                        .push_str("-changed");
                }),
            ),
            (
                "compiler-command-argument",
                Box::new(|plan| {
                    plan.toolchain_commands.compilers[0]
                        .command
                        .args
                        .push("--identity".to_owned());
                }),
            ),
            (
                "environment",
                Box::new(|plan| {
                    plan.environment.insert("LANG".to_owned(), "C".to_owned());
                }),
            ),
            (
                "root-materialization",
                Box::new(|plan| {
                    plan.execution.root_materialization = RootMaterializationMode::PackageManagerState;
                }),
            ),
            (
                "credentials",
                Box::new(|plan| plan.execution.credentials = ExecutionCredentials::Unspecified),
            ),
            (
                "executor",
                Box::new(|plan| plan.execution.executor.fingerprint.push_str("-changed")),
            ),
            (
                "package-metadata",
                Box::new(|plan| plan.package.homepage.push_str("/changed")),
            ),
            (
                "package-architecture",
                Box::new(|plan| plan.package.architecture = "aarch64".to_owned()),
            ),
            ("analysis", Box::new(|plan| plan.analysis.strip = !plan.analysis.strip)),
            (
                "analysis-tool-program",
                Box::new(|plan| {
                    plan.analysis.tools.python.as_mut().unwrap().path.push_str("-changed");
                }),
            ),
            (
                "analysis-tool-requirement",
                Box::new(|plan| {
                    plan.analysis
                        .tools
                        .python
                        .as_mut()
                        .unwrap()
                        .requirement
                        .name
                        .push_str("-changed");
                }),
            ),
            (
                "manifest-build-input-name",
                Box::new(|plan| plan.manifest_build_inputs[0].name.push_str("-changed")),
            ),
            (
                "manifest-build-input-kind",
                Box::new(|plan| plan.manifest_build_inputs[0].kind = RelationKind::SystemBinary),
            ),
            (
                "collection-rule-order",
                Box::new(|plan| plan.collection_rules.reverse()),
            ),
            (
                "collection-rule-kind",
                Box::new(|plan| plan.collection_rules[0].kind = PathRuleKind::Special),
            ),
            (
                "output",
                Box::new(|plan| plan.outputs[0].conflicts[0].name.push_str("-changed")),
            ),
            (
                "output-manifest-membership",
                Box::new(|plan| plan.outputs[0].include_in_manifest = false),
            ),
            ("timestamp", Box::new(|plan| plan.source_date_epoch += 1)),
        ];

        for (name, mutate) in mutations {
            let mut changed = original.clone();
            mutate(&mut changed);
            assert_ne!(original_id, changed.derivation_id(), "{name} mutation was not hashed");
        }
    }

    #[test]
    fn toolchain_commands_are_complete_exact_locked_and_cache_consistent() {
        let original = sample_plan();
        original.validate().unwrap();

        let mut missing = original.clone();
        missing.toolchain_commands.compilers.pop();
        assert!(matches!(
            missing.validate(),
            Err(DerivationValidationError::CompilerCommandCount {
                found: 12,
                expected: 13,
            })
        ));

        let mut reordered = original.clone();
        reordered.toolchain_commands.compilers.swap(0, 1);
        assert!(matches!(
            reordered.validate(),
            Err(DerivationValidationError::UnexpectedCompilerCommandRole {
                index: 0,
                expected: CompilerExecutableRole::Cc,
                found: CompilerExecutableRole::Cxx,
            })
        ));

        let mut mismatched = original.clone();
        mismatched.toolchain_commands.compilers[0].command.program.path = "/usr/bin/not-cmake".to_owned();
        assert!(matches!(
            mismatched.validate(),
            Err(DerivationValidationError::ExecutablePathMismatch { field, .. })
                if field == "toolchain_commands.compilers[0].command.program.path"
        ));

        let mut unlocked = original.clone();
        unlocked.toolchain_commands.compilers[0].command.program = ExecutablePlan {
            path: "/usr/bin/unlocked-compiler".to_owned(),
            requirement: RelationPlan {
                kind: RelationKind::Binary,
                name: "unlocked-compiler".to_owned(),
            },
        };
        assert!(matches!(
            unlocked.validate(),
            Err(DerivationValidationError::UnlockedExecutable { field, request })
                if field == "toolchain_commands.compilers[0].command.program.requirement"
                    && request == "binary(unlocked-compiler)"
        ));

        let mut missing_cache = original.clone();
        missing_cache.execution.compiler_cache = true;
        assert!(matches!(
            missing_cache.validate(),
            Err(DerivationValidationError::CompilerCacheCommandMismatch {
                enabled: true,
                ccache: false,
                sccache: false,
            })
        ));

        let mut cached = original.clone();
        cached.execution.compiler_cache = true;
        cached.toolchain_commands.ccache = Some(sample_analyzer_tool("cmake"));
        cached.toolchain_commands.sccache = Some(sample_analyzer_tool("cmake"));
        let cache_origins = &mut cached
            .build_lock
            .requests
            .iter_mut()
            .find(|request| request.request == "binary(cmake)")
            .unwrap()
            .origins;
        cache_origins.extend([
            InputOrigin::CompilerCache {
                role: CompilerCacheRole::Ccache,
            },
            InputOrigin::CompilerCache {
                role: CompilerCacheRole::Sccache,
            },
        ]);
        cached.build_lock.normalize();
        cached.validate().unwrap();
        assert_ne!(original.derivation_id(), cached.derivation_id());

        let mut mold = original.clone();
        mold.toolchain_commands.mold = Some(ExecutableCommandPlan {
            program: sample_analyzer_tool("cmake"),
            args: vec!["--mold-identity".to_owned()],
        });
        mold.build_lock
            .requests
            .iter_mut()
            .find(|request| request.request == "binary(cmake)")
            .unwrap()
            .origins
            .push(InputOrigin::MoldLinker);
        mold.build_lock.normalize();
        mold.validate().unwrap();
        assert_ne!(original.derivation_id(), mold.derivation_id());

        let mut arguments = original.clone();
        arguments.toolchain_commands.compilers[0]
            .command
            .args
            .push("argument identity".to_owned());
        arguments.validate().unwrap();
        assert_ne!(original.derivation_id(), arguments.derivation_id());
    }

    #[test]
    fn origin_only_role_changes_invalidate_the_derivation_identity() {
        let first = sample_plan();
        first.validate().unwrap();
        let mut changed = first.clone();
        let resolution = {
            let request = &mut changed.build_lock.requests[0];
            let resolution = (
                request.request.clone(),
                request.package_id.clone(),
                request.output.clone(),
            );
            request.origins = vec![InputOrigin::Check {
                selection: PackageInputSelection::Package,
                index: 0,
            }];
            resolution
        };
        changed.validate().unwrap();

        assert_eq!(
            (
                &changed.build_lock.requests[0].request,
                &changed.build_lock.requests[0].package_id,
                &changed.build_lock.requests[0].output,
            ),
            (&resolution.0, &resolution.1, &resolution.2)
        );
        assert_ne!(first.build_lock.digest(), changed.build_lock.digest());
        assert_ne!(first.derivation_id(), changed.derivation_id());
    }

    #[test]
    fn shell_interpreter_and_declared_programs_change_identity() {
        let mut original = sample_plan();
        original.jobs[0].phases[0].steps = vec![StepPlan::Shell {
            interpreter: sample_analyzer_tool("bash"),
            declared_programs: vec![sample_analyzer_tool("cmake")],
            script: "cmake --build .".to_owned(),
            environment: BTreeMap::new(),
            working_dir: "/mason/build".to_owned(),
        }];
        original.validate().unwrap();
        let original_id = original.derivation_id();

        let mutations: [fn(&mut StepPlan); 4] = [
            |step: &mut StepPlan| {
                let StepPlan::Shell { interpreter, .. } = step else {
                    unreachable!()
                };
                interpreter.path.push_str("-changed");
            },
            |step: &mut StepPlan| {
                let StepPlan::Shell { interpreter, .. } = step else {
                    unreachable!()
                };
                interpreter.requirement.name.push_str("-changed");
            },
            |step: &mut StepPlan| {
                let StepPlan::Shell { declared_programs, .. } = step else {
                    unreachable!()
                };
                declared_programs[0].path.push_str("-changed");
            },
            |step: &mut StepPlan| {
                let StepPlan::Shell { declared_programs, .. } = step else {
                    unreachable!()
                };
                declared_programs[0].requirement.name.push_str("-changed");
            },
        ];
        for mutate in mutations {
            let mut changed = original.clone();
            mutate(&mut changed.jobs[0].phases[0].steps[0]);
            assert_ne!(original_id, changed.derivation_id());
        }
    }

    #[test]
    fn run_built_is_contained_and_fully_identity_bearing() {
        let mut original = sample_plan();
        make_sample_run_built(&mut original);
        original.validate().unwrap();
        let original_id = original.derivation_id();

        let mut changed_path = original.clone();
        let StepPlan::RunBuilt { program, .. } = sample_step_mut(&mut changed_path) else {
            unreachable!()
        };
        *program = "/mason/build/bin/other-test".to_owned();
        changed_path.validate().unwrap();
        assert_ne!(original_id, changed_path.derivation_id());

        let mut changed_args = original.clone();
        let StepPlan::RunBuilt { args, .. } = sample_step_mut(&mut changed_args) else {
            unreachable!()
        };
        args.push("--thorough".to_owned());
        changed_args.validate().unwrap();
        assert_ne!(original_id, changed_args.derivation_id());

        for invalid in [
            "/mason/build",
            "/mason/other/self-test",
            "mason/build/bin/self-test",
            "/mason/build/../escape",
        ] {
            let mut plan = original.clone();
            let StepPlan::RunBuilt { program, .. } = sample_step_mut(&mut plan) else {
                unreachable!()
            };
            *program = invalid.to_owned();
            assert!(plan.validate().is_err(), "{invalid:?} escaped RunBuilt validation");
        }
    }

    #[test]
    fn every_frozen_layout_value_changes_identity() {
        let original = sample_plan();
        let original_id = original.derivation_id();
        let mutations: Vec<(&str, Box<dyn Fn(&mut BuilderLayout)>)> = vec![
            ("hostname", Box::new(|layout| layout.hostname.push_str("-changed"))),
            ("guest-root", Box::new(|layout| layout.guest_root.push_str("-changed"))),
            (
                "artifacts-dir",
                Box::new(|layout| layout.artifacts_dir.push_str("-changed")),
            ),
            ("build-dir", Box::new(|layout| layout.build_dir.push_str("-changed"))),
            ("source-dir", Box::new(|layout| layout.source_dir.push_str("-changed"))),
            ("recipe-dir", Box::new(|layout| layout.recipe_dir.push_str("-changed"))),
            (
                "install-dir",
                Box::new(|layout| layout.install_dir.push_str("-changed")),
            ),
            (
                "package-dir",
                Box::new(|layout| layout.package_dir.push_str("-changed")),
            ),
            ("ccache-dir", Box::new(|layout| layout.ccache_dir.push_str("-changed"))),
            (
                "sccache-dir",
                Box::new(|layout| layout.sccache_dir.push_str("-changed")),
            ),
            (
                "go-cache-dir",
                Box::new(|layout| layout.go_cache_dir.push_str("-changed")),
            ),
            (
                "go-mod-cache-dir",
                Box::new(|layout| layout.go_mod_cache_dir.push_str("-changed")),
            ),
            (
                "cargo-cache-dir",
                Box::new(|layout| layout.cargo_cache_dir.push_str("-changed")),
            ),
            (
                "zig-cache-dir",
                Box::new(|layout| layout.zig_cache_dir.push_str("-changed")),
            ),
        ];

        for (name, mutate) in mutations {
            let mut changed = original.clone();
            mutate(&mut changed.layout);
            assert_ne!(original_id, changed.derivation_id(), "{name} mutation was not hashed");
        }
    }

    #[test]
    fn non_default_frozen_layout_is_valid_and_changes_identity() {
        let original = sample_plan();
        let mut changed = original.clone();
        changed.layout = BuilderLayout {
            hostname: "forge-builder".to_owned(),
            guest_root: "/forge".to_owned(),
            artifacts_dir: "/forge/output".to_owned(),
            build_dir: "/forge/work".to_owned(),
            source_dir: "/forge/sources".to_owned(),
            recipe_dir: "/forge/recipe".to_owned(),
            install_dir: "/forge/destination".to_owned(),
            package_dir: "/forge/recipe/package".to_owned(),
            ccache_dir: "/forge/cache-cc".to_owned(),
            sccache_dir: "/forge/cache-rust".to_owned(),
            go_cache_dir: "/forge/cache-go".to_owned(),
            go_mod_cache_dir: "/forge/cache-go-mod".to_owned(),
            cargo_cache_dir: "/forge/cache-cargo".to_owned(),
            zig_cache_dir: "/forge/cache-zig".to_owned(),
        };
        changed.jobs[0].build_dir = "/forge/work".to_owned();
        changed.jobs[0].work_dir = "/forge/work/hello".to_owned();
        let StepPlan::Run { working_dir, .. } = &mut changed.jobs[0].phases[0].steps[0] else {
            unreachable!()
        };
        *working_dir = "/forge/work".to_owned();
        changed.environment.insert("HOME".to_owned(), "/forge/work".to_owned());

        changed.validate().unwrap();
        assert_ne!(original.derivation_id(), changed.derivation_id());
    }

    #[test]
    fn phase_order_remains_semantic() {
        let mut first = sample_plan();
        first.jobs.push(JobPlan {
            pgo_stage: Some("use".to_owned()),
            pgo_dir: Some("/mason/build-pgo".to_owned()),
            build_dir: "/mason/build".to_owned(),
            work_dir: "/mason/build/hello".to_owned(),
            phases: Vec::new(),
        });
        let mut reordered = first.clone();
        reordered.jobs.reverse();

        assert_ne!(first.derivation_id(), reordered.derivation_id());
    }

    #[test]
    fn validation_requires_normalized_non_root_absolute_layout_paths() {
        for value in [
            "relative/build",
            "/",
            "/mason/../escape",
            "/mason/./build",
            "/mason//build",
            "/mason/build/",
        ] {
            let mut plan = sample_plan();
            plan.layout.build_dir = value.to_owned();
            assert!(matches!(
                plan.validate(),
                Err(DerivationValidationError::UnsafeAbsolutePath { field, value: found })
                    if field == "layout.build_dir" && found == value
            ));
        }
    }

    #[test]
    fn validation_rejects_invalid_hostnames_and_overlapping_layout_paths() {
        for hostname in ["", "-builder", "builder-", "bad host", "bad/host"] {
            let mut plan = sample_plan();
            plan.layout.hostname = hostname.to_owned();
            assert!(matches!(
                plan.validate(),
                Err(DerivationValidationError::InvalidSandboxHostname { value }) if value == hostname
            ));
        }

        let mut overlapping = sample_plan();
        overlapping.layout.ccache_dir = "/mason/build/cache".to_owned();
        assert!(matches!(
            overlapping.validate(),
            Err(DerivationValidationError::OverlappingLayoutPath {
                field,
                other_field,
                ..
            }) if field == "layout.ccache_dir" && other_field == "layout.build_dir"
        ));
    }

    #[test]
    fn validation_contains_layout_and_job_paths_in_their_frozen_roots() {
        let mut outside_layout = sample_plan();
        outside_layout.layout.source_dir = "/outside/sources".to_owned();
        assert!(matches!(
            outside_layout.validate(),
            Err(DerivationValidationError::PathOutsideRoot { field, root, .. })
                if field == "layout.source_dir" && root == "/mason"
        ));

        let mut outside_layout_build = sample_plan();
        outside_layout_build.jobs[0].build_dir = "/outside/build".to_owned();
        outside_layout_build.jobs[0].work_dir = "/outside/build/work".to_owned();
        assert!(matches!(
            outside_layout_build.validate(),
            Err(DerivationValidationError::PathOutsideRoot { field, root_field, .. })
                if field == "jobs[0].build_dir" && root_field == "layout.build_dir"
        ));

        let mut outside_job_build = sample_plan();
        outside_job_build.jobs[0].work_dir = "/mason/other".to_owned();
        assert!(matches!(
            outside_job_build.validate(),
            Err(DerivationValidationError::PathOutsideRoot { field, root_field, .. })
                if field == "jobs[0].work_dir" && root_field == "jobs[0].build_dir"
        ));

        let mut outside_pgo = sample_plan();
        outside_pgo.jobs[0].pgo_stage = Some("one".to_owned());
        outside_pgo.jobs[0].pgo_dir = Some("/outside/pgo".to_owned());
        assert!(matches!(
            outside_pgo.validate(),
            Err(DerivationValidationError::PathOutsideRoot { field, root_field, .. })
                if field == "jobs[0].pgo_dir" && root_field == "layout.build_dir"
        ));
    }

    #[test]
    fn validation_rejects_traversal_and_escape_in_every_step_working_directory() {
        for working_dir in [
            "relative",
            "/mason/build/../outside",
            "/mason/build//nested",
            "/mason/install",
        ] {
            let mut plan = sample_plan();
            let StepPlan::Run {
                working_dir: frozen, ..
            } = &mut plan.jobs[0].phases[0].steps[0]
            else {
                unreachable!()
            };
            *frozen = working_dir.to_owned();
            assert!(matches!(
                plan.validate(),
                Err(DerivationValidationError::UnsafeAbsolutePath { .. })
                    | Err(DerivationValidationError::PathOutsideRoot { .. })
            ));
        }

        let mut shell_plan = sample_plan();
        shell_plan.jobs[0].phases[0].steps = vec![StepPlan::Shell {
            interpreter: ExecutablePlan {
                path: "/usr/bin/bash".to_owned(),
                requirement: RelationPlan {
                    kind: RelationKind::Binary,
                    name: "bash".to_owned(),
                },
            },
            declared_programs: Vec::new(),
            script: "true".to_owned(),
            environment: BTreeMap::new(),
            working_dir: "/tmp/ambient".to_owned(),
        }];
        assert!(matches!(
            shell_plan.validate(),
            Err(DerivationValidationError::PathOutsideRoot { field, .. })
                if field == "jobs[0].phases[0].steps[0].working_dir"
        ));
    }

    #[test]
    fn validation_freezes_only_the_executable_phase_vocabulary() {
        let mut supported = sample_plan();
        supported.jobs[0].phases = ["Prepare", "setup", "BUILD", "install", "check", "workload"]
            .into_iter()
            .map(|name| PhasePlan {
                name: name.to_owned(),
                pre: Vec::new(),
                steps: Vec::new(),
                post: Vec::new(),
            })
            .collect();
        supported.validate().unwrap();

        for name in ["environment", "ambient-phase", ""] {
            let mut plan = sample_plan();
            plan.jobs[0].phases[0].name = name.to_owned();
            assert!(matches!(
                plan.validate(),
                Err(DerivationValidationError::UnsupportedPhase {
                    job: 0,
                    phase: 0,
                    name: found,
                }) if found == name
            ));
        }

        let mut duplicate = sample_plan();
        duplicate.jobs[0].phases.push(PhasePlan {
            name: "BUILD".to_owned(),
            pre: Vec::new(),
            steps: Vec::new(),
            post: Vec::new(),
        });
        assert!(matches!(
            duplicate.validate(),
            Err(DerivationValidationError::DuplicatePhase { job: 0, .. })
        ));
    }

    #[test]
    fn validation_requires_exact_pgo_vocabulary_and_stage_directory_pairing() {
        for stage in ["one", "two", "use"] {
            let mut plan = sample_plan();
            plan.jobs[0].pgo_stage = Some(stage.to_owned());
            plan.jobs[0].pgo_dir = Some("/mason/build/profile".to_owned());
            plan.validate().unwrap();
        }

        let mut unsupported = sample_plan();
        unsupported.jobs[0].pgo_stage = Some("ONE".to_owned());
        unsupported.jobs[0].pgo_dir = Some("/mason/build/profile".to_owned());
        assert!(matches!(
            unsupported.validate(),
            Err(DerivationValidationError::UnsupportedPgoStage { job: 0, stage })
                if stage == "ONE"
        ));

        for (stage, directory) in [
            (Some("one".to_owned()), None),
            (None, Some("/mason/build/profile".to_owned())),
        ] {
            let mut plan = sample_plan();
            plan.jobs[0].pgo_stage = stage;
            plan.jobs[0].pgo_dir = directory;
            assert!(matches!(
                plan.validate(),
                Err(DerivationValidationError::PgoStageDirectoryMismatch { job: 0, .. })
            ));
        }
    }

    #[test]
    fn validation_rejects_output_relations_outside_the_locked_closure() {
        let mut plan = sample_plan();
        plan.outputs[0].runtime_inputs.push(OutputRelation::Locked {
            relation: RelationPlan {
                kind: RelationKind::PackageName,
                name: "missing".to_owned(),
            },
            reference: LockedOutputRef {
                package_id: "missing".to_owned(),
                output: "out".to_owned(),
            },
        });

        assert!(matches!(
            plan.validate(),
            Err(DerivationValidationError::UnknownOutputReference { field, .. })
                if field == "outputs[0].runtime_inputs[0]"
        ));
    }

    #[test]
    fn validation_rejects_duplicate_emitted_package_names() {
        let mut plan = sample_plan();
        let mut duplicate = plan.outputs[0].clone();
        duplicate.name = "dev".to_owned();
        plan.outputs.push(duplicate);

        assert!(matches!(
            plan.validate(),
            Err(DerivationValidationError::DuplicateOutputPackage { package })
                if package == "hello"
        ));
    }

    #[test]
    fn validation_binds_artifact_architecture_to_the_frozen_target_platform() {
        let mut plan = sample_plan();
        plan.package.architecture = "x86".to_owned();

        assert!(matches!(
            plan.validate(),
            Err(DerivationValidationError::ArtifactTargetArchitectureMismatch {
                artifact,
                target,
            }) if artifact == "x86" && target == "x86_64"
        ));
    }

    #[test]
    fn validation_rejects_source_materialization_path_escape() {
        for value in [
            "",
            ".",
            "..",
            "../escape",
            "/absolute",
            "nested/file",
            "nested\\file",
            "line\nbreak",
            "escape\u{1b}",
        ] {
            let mut plan = sample_plan();
            let LockedSource::Archive { filename, .. } = &mut plan.sources[0] else {
                unreachable!()
            };
            *filename = value.to_owned();
            assert!(matches!(
                plan.validate(),
                Err(DerivationValidationError::UnsafeSourceDestination {
                    index: 0,
                    field: "filename",
                    ..
                })
            ));
        }
    }

    #[test]
    fn frozen_sources_apply_the_shared_secure_transport_policy() {
        let archive_cases = [
            "http://example.invalid/hello.tar.zst",
            "file:///tmp/hello.tar.zst",
            "ssh://example.invalid/hello.tar.zst",
        ];
        for value in archive_cases {
            let mut plan = sample_plan();
            let LockedSource::Archive { url, .. } = &mut plan.sources[0] else {
                unreachable!()
            };
            *url = value.to_owned();
            assert!(matches!(
                plan.validate(),
                Err(DerivationValidationError::InvalidSourceUrl {
                    index: 0,
                    source: SourceUrlValidationError::UnsupportedScheme { .. },
                })
            ));
        }

        for value in ["https://example.invalid/source.git", "ssh://example.invalid/source.git"] {
            let mut plan = sample_plan();
            plan.sources = vec![sample_git_source(0, "hello.git")];
            let LockedSource::Git { url, .. } = &mut plan.sources[0] else {
                unreachable!()
            };
            *url = value.to_owned();
            plan.validate().unwrap();
        }

        for value in [
            "http://example.invalid/source.git",
            "git://example.invalid/source.git",
            "file:///tmp/source.git",
        ] {
            let mut plan = sample_plan();
            plan.sources = vec![sample_git_source(0, "hello.git")];
            let LockedSource::Git { url, .. } = &mut plan.sources[0] else {
                unreachable!()
            };
            *url = value.to_owned();
            assert!(matches!(
                plan.validate(),
                Err(DerivationValidationError::InvalidSourceUrl {
                    index: 0,
                    source: SourceUrlValidationError::UnsupportedScheme { .. },
                })
            ));
        }
    }

    #[test]
    fn frozen_source_url_errors_are_field_specific_and_secret_free() {
        for value in [
            "https://user:do-not-print@example.invalid/hello.tar.zst",
            "https://example.invalid/hello.tar.zst#do-not-print",
        ] {
            let mut plan = sample_plan();
            let LockedSource::Archive { url, .. } = &mut plan.sources[0] else {
                unreachable!()
            };
            *url = value.to_owned();
            let error = plan.validate().unwrap_err();
            let message = error.to_string();
            assert!(message.starts_with("sources[0].url:"));
            assert!(!message.contains("user"));
            assert!(!message.contains("do-not-print"));
        }
    }

    #[test]
    fn validation_requires_a_canonical_lowercase_git_commit() {
        for value in [
            String::new(),
            "a".repeat(39),
            "a".repeat(41),
            format!("{}g", "a".repeat(39)),
            "A".repeat(40),
            "é".repeat(20),
        ] {
            let mut plan = sample_plan();
            plan.sources = vec![sample_git_source(0, "hello.git")];
            let LockedSource::Git { commit, .. } = &mut plan.sources[0] else {
                unreachable!()
            };
            *commit = value.clone();

            let error = plan.validate().unwrap_err();
            assert!(matches!(
                error,
                DerivationValidationError::InvalidGitCommit {
                    index: 0,
                    value: ref found,
                } if found == &value
            ));
            assert_eq!(
                error.to_string(),
                format!(
                    "sources[0].commit: expected exactly 40 lowercase ASCII hexadecimal characters, found `{value}`"
                )
            );
        }
    }

    #[test]
    fn validation_requires_a_canonical_archive_sha256() {
        for value in [
            String::new(),
            "a".repeat(63),
            "a".repeat(65),
            format!("{}g", "a".repeat(63)),
            "A".repeat(64),
        ] {
            let mut plan = sample_plan();
            let LockedSource::Archive { sha256, .. } = &mut plan.sources[0] else {
                unreachable!()
            };
            *sha256 = value;
            assert!(matches!(
                plan.validate(),
                Err(DerivationValidationError::InvalidArchiveSha256 { index: 0, .. })
            ));
        }
    }

    #[test]
    fn validation_requires_a_lowercase_git_materialization_sha256() {
        for value in [
            String::new(),
            "a".repeat(63),
            "a".repeat(65),
            format!("{}g", "a".repeat(63)),
            "A".repeat(64),
            "é".repeat(32),
        ] {
            let mut plan = sample_plan();
            plan.sources = vec![sample_git_source(0, "hello.git")];
            let LockedSource::Git {
                materialization_sha256, ..
            } = &mut plan.sources[0]
            else {
                unreachable!()
            };
            *materialization_sha256 = value.clone();

            let error = plan.validate().unwrap_err();
            assert!(matches!(
                error,
                DerivationValidationError::InvalidGitMaterializationSha256 {
                    index: 0,
                    value: ref found,
                } if found == &value
            ));
            assert_eq!(
                error.to_string(),
                format!(
                    "sources[0].materialization_sha256: expected exactly 64 lowercase ASCII hexadecimal characters, found `{value}`"
                )
            );
        }
    }

    #[test]
    fn validation_rejects_duplicate_source_materialization_destinations_across_kinds() {
        let mut plan = sample_plan();
        plan.sources.push(LockedSource::Git {
            order: 1,
            url: "https://example.invalid/other.git".to_owned(),
            requested_ref: "main".to_owned(),
            commit: "0123456789abcdef0123456789abcdef01234567".to_owned(),
            materialization_sha256: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_owned(),
            directory: "hello.tar.zst".to_owned(),
        });

        let error = plan.validate().unwrap_err();
        assert!(matches!(
            error,
            DerivationValidationError::DuplicateSourceDestination {
                index: 1,
                field: "directory",
                first_index: 0,
                first_field: "filename",
                ref value,
            } if value == "hello.tar.zst"
        ));
        assert_eq!(
            error.to_string(),
            "sources[1].directory: duplicate materialization destination \"hello.tar.zst\"; first declared at sources[0].filename"
        );
    }

    #[test]
    fn validation_rejects_planned_output_cycles_with_the_closing_edge() {
        let mut plan = sample_plan();
        plan.outputs[0].runtime_inputs.push(OutputRelation::Planned {
            output: "dev".to_owned(),
        });
        plan.outputs.push(OutputPlan {
            name: "dev".to_owned(),
            package_name: "hello-devel".to_owned(),
            include_in_manifest: true,
            summary: None,
            description: None,
            provides_exclude: Vec::new(),
            runtime_exclude: Vec::new(),
            runtime_inputs: vec![OutputRelation::Planned {
                output: "out".to_owned(),
            }],
            conflicts: Vec::new(),
        });

        let error = plan.validate().unwrap_err();
        assert!(matches!(
            error,
            DerivationValidationError::PlannedOutputCycle { ref field, ref cycle }
                if field == "outputs[1].runtime_inputs[0]"
                    && cycle.iter().map(String::as_str).eq(["out", "dev", "out"])
        ));
        assert_eq!(
            error.to_string(),
            "outputs[1].runtime_inputs[0]: planned output dependency cycle: out -> dev -> out"
        );
    }

    #[test]
    fn changing_only_git_materialization_digest_changes_derivation_identity() {
        let mut first = sample_plan();
        first.sources = vec![sample_git_source(0, "hello.git")];
        first.validate().unwrap();

        let mut changed = first.clone();
        let LockedSource::Git {
            materialization_sha256, ..
        } = &mut changed.sources[0]
        else {
            unreachable!()
        };
        *materialization_sha256 = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc".to_owned();
        changed.validate().unwrap();

        assert_ne!(first.canonical_bytes(), changed.canonical_bytes());
        assert_ne!(first.derivation_id(), changed.derivation_id());
    }

    #[test]
    fn source_construction_order_has_a_stable_canonical_identity() {
        let mut canonical = sample_plan();
        canonical.sources.push(sample_git_source(1, "hello.git"));
        canonical.validate().unwrap();

        let mut constructed_in_reverse = canonical.clone();
        constructed_in_reverse.sources.reverse();

        assert_eq!(canonical.canonical_bytes(), constructed_in_reverse.canonical_bytes());
        assert_eq!(canonical.derivation_id(), constructed_in_reverse.derivation_id());
    }

    #[test]
    fn git_materialization_directory_is_validated_and_hashed() {
        let mut first = sample_plan();
        first.sources = vec![LockedSource::Git {
            order: 0,
            url: "https://example.invalid/hello.git".to_owned(),
            requested_ref: "main".to_owned(),
            commit: "0123456789abcdef0123456789abcdef01234567".to_owned(),
            materialization_sha256: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_owned(),
            directory: "hello.git".to_owned(),
        }];
        first.validate().unwrap();

        let mut changed = first.clone();
        if let LockedSource::Git { directory, .. } = &mut changed.sources[0] {
            *directory = "other.git".to_owned();
        } else {
            unreachable!()
        }
        changed.validate().unwrap();
        assert_ne!(first.derivation_id(), changed.derivation_id());

        if let LockedSource::Git { directory, .. } = &mut changed.sources[0] {
            *directory = "../escape".to_owned();
        }
        assert!(matches!(
            changed.validate(),
            Err(DerivationValidationError::UnsafeSourceDestination {
                index: 0,
                field: "directory",
                ..
            })
        ));
    }
}
