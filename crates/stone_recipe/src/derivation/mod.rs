//! Frozen, canonical build plans.
//!
//! [`DerivationPlan`] is the semantic boundary between resolution and
//! execution. It contains values only: the executor may index or borrow these
//! values, but must not infer another dependency, phase, policy, or output.

use std::collections::BTreeMap;

use sha2::{Digest, Sha256};

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
pub use self::{
    collection::{CollectionRulePlan, PathRuleKind},
    output::{OutputPlan, OutputRelation},
    relation::{RelationKind, RelationPlan},
};

mod build_lock;
mod collection;
mod output;
mod provenance;
mod relation;
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
mod tests;
