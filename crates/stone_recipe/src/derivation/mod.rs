// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

//! Frozen, canonical build plans.
//!
//! [`DerivationPlan`] is the semantic boundary between resolution and
//! execution. It contains values only: the executor may index or borrow these
//! values, but must not infer another dependency, phase, policy, or output.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path, PathBuf},
};

use sha2::{Digest, Sha256};
use stone::relation::{Dependency, Kind as StoneRelationKind, Provider};
use thiserror::Error;

use crate::build_policy::{AnalyzerKind, CompilerCachePolicySpec, SUPPORTED_ARTIFACT_ARCHITECTURES, SandboxPolicySpec};

pub use self::build_lock::{
    BUILD_LOCK_FILE_NAME, BUILD_LOCK_SCHEMA_VERSION, BuildLock, BuildLockDecodeError, BuildLockValidationError,
    LockedIdentity, LockedOutput, LockedOutputRef, LockedPackage, LockedRequest, Platform, RepositorySnapshot,
    decode_build_lock, encode_build_lock,
};

mod build_lock;

/// Current schema used by [`DerivationPlan`].
pub const DERIVATION_PLAN_SCHEMA_VERSION: u32 = 4;

const DERIVATION_HASH_DOMAIN: &[u8] = b"os-tools-derivation-plan\0";

/// A completely resolved, target-specific build description.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerivationPlan {
    pub schema_version: u32,
    pub boulder_version: String,
    pub boulder_fingerprint: String,
    pub package: PackageIdentity,
    pub recipe_fingerprint: String,
    pub source_lock_digest: String,
    pub sources: Vec<LockedSource>,
    pub build_lock: BuildLock,
    pub jobs: Vec<JobPlan>,
    pub environment: BTreeMap<String, String>,
    pub layout: BuilderLayout,
    pub execution: ExecutionPolicy,
    pub analysis: AnalysisPlan,
    pub manifest_build_inputs: Vec<RelationPlan>,
    pub collection_rules: Vec<CollectionRulePlan>,
    pub outputs: Vec<OutputPlan>,
    pub source_date_epoch: i64,
}

impl DerivationPlan {
    /// Construct a plan using the current schema.
    pub fn new(package: PackageIdentity, build_lock: BuildLock) -> Self {
        Self {
            schema_version: DERIVATION_PLAN_SCHEMA_VERSION,
            boulder_version: String::new(),
            boulder_fingerprint: String::new(),
            package,
            recipe_fingerprint: String::new(),
            source_lock_digest: String::new(),
            sources: Vec::new(),
            build_lock,
            jobs: Vec::new(),
            environment: BTreeMap::new(),
            layout: BuilderLayout::default(),
            execution: ExecutionPolicy::default(),
            analysis: AnalysisPlan::default(),
            manifest_build_inputs: Vec::new(),
            collection_rules: Vec::new(),
            outputs: Vec::new(),
            source_date_epoch: 0,
        }
    }

    /// Validate invariants required before a plan can cross the freeze
    /// boundary.
    pub fn validate(&self) -> Result<(), DerivationValidationError> {
        if self.schema_version != DERIVATION_PLAN_SCHEMA_VERSION {
            return Err(DerivationValidationError::UnsupportedSchema {
                found: self.schema_version,
                expected: DERIVATION_PLAN_SCHEMA_VERSION,
            });
        }

        require_nonempty("boulder_version", &self.boulder_version)?;
        require_nonempty("boulder_fingerprint", &self.boulder_fingerprint)?;
        self.package.validate()?;
        if !SUPPORTED_ARTIFACT_ARCHITECTURES.contains(&self.package.architecture.as_str()) {
            return Err(DerivationValidationError::UnsupportedArtifactArchitecture {
                value: self.package.architecture.clone(),
                supported: SUPPORTED_ARTIFACT_ARCHITECTURES.join(", "),
            });
        }
        require_nonempty("recipe_fingerprint", &self.recipe_fingerprint)?;
        require_nonempty("source_lock_digest", &self.source_lock_digest)?;
        self.build_lock.validate()?;
        if self.package.architecture != self.build_lock.target_platform.architecture {
            return Err(DerivationValidationError::ArtifactTargetArchitectureMismatch {
                artifact: self.package.architecture.clone(),
                target: self.build_lock.target_platform.architecture.clone(),
            });
        }
        self.layout.validate()?;
        if self.execution.jobs == 0 {
            return Err(DerivationValidationError::ZeroJobs);
        }

        let mut source_orders = BTreeSet::new();
        let mut source_destinations = BTreeMap::new();
        for (index, source) in self.sources.iter().enumerate() {
            let order = source.order();
            if !source_orders.insert(order) {
                return Err(DerivationValidationError::DuplicateSourceOrder { order });
            }
            if usize::try_from(order) != Ok(index) {
                return Err(DerivationValidationError::UnexpectedSourceOrder { index, order });
            }
            source.validate(index)?;
            let (field, destination) = source.destination();
            if let Some((first_index, first_field)) = source_destinations.insert(destination, (index, field)) {
                return Err(DerivationValidationError::DuplicateSourceDestination {
                    index,
                    field,
                    value: destination.to_owned(),
                    first_index,
                    first_field,
                });
            }
        }

        for (index, job) in self.jobs.iter().enumerate() {
            job.validate(index, Path::new(&self.layout.build_dir))?;
        }
        for (index, input) in self.manifest_build_inputs.iter().enumerate() {
            input.validate(&format!("manifest_build_inputs[{index}]"))?;
        }
        self.analysis.validate()?;

        let mut output_names = BTreeSet::new();
        let mut output_package_names = BTreeSet::new();
        for (index, output) in self.outputs.iter().enumerate() {
            output.validate(index)?;
            if !output_names.insert(output.name.as_str()) {
                return Err(DerivationValidationError::DuplicateOutput {
                    name: output.name.clone(),
                });
            }
            if !output_package_names.insert(output.package_name.as_str()) {
                return Err(DerivationValidationError::DuplicateOutputPackage {
                    package: output.package_name.clone(),
                });
            }
        }
        let (root_index, root) = self
            .outputs
            .iter()
            .enumerate()
            .find(|(_, output)| output.name == "out")
            .ok_or(DerivationValidationError::MissingRootOutput)?;
        if root.package_name != self.package.name {
            return Err(DerivationValidationError::RootOutputPackageMismatch {
                index: root_index,
                expected: self.package.name.clone(),
                found: root.package_name.clone(),
            });
        }
        for (index, rule) in self.collection_rules.iter().enumerate() {
            rule.validate(index)?;
            if !output_names.contains(rule.output.as_str()) {
                return Err(DerivationValidationError::UnknownPlannedOutput {
                    field: format!("collection_rules[{index}].output"),
                    output: rule.output.clone(),
                });
            }
        }
        for (index, output) in self.outputs.iter().enumerate() {
            for (dependency_index, dependency) in output.runtime_inputs.iter().enumerate() {
                let field = format!("outputs[{index}].runtime_inputs[{dependency_index}]");
                if let OutputRelation::Locked { relation, .. } = dependency {
                    relation.validate(&field)?;
                }
                match dependency {
                    OutputRelation::Locked { relation, reference }
                        if !self.build_lock.contains_output(reference)
                            || !self.build_lock.requests.iter().any(|locked| {
                                locked.request == relation.canonical_name()
                                    && locked.package_id == reference.package_id
                                    && locked.output == reference.output
                            }) =>
                    {
                        return Err(DerivationValidationError::UnknownOutputReference {
                            field,
                            package: reference.package_id.clone(),
                            output: reference.output.clone(),
                        });
                    }
                    OutputRelation::Planned { output } if !output_names.contains(output.as_str()) => {
                        return Err(DerivationValidationError::UnknownPlannedOutput {
                            field,
                            output: output.clone(),
                        });
                    }
                    _ => {}
                }
            }
        }
        validate_planned_output_cycles(&self.outputs)?;

        Ok(())
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
        encoder.string(&self.boulder_version);
        encoder.string(&self.boulder_fingerprint);
        self.package.encode(&mut encoder);
        encoder.string(&self.recipe_fingerprint);
        encoder.string(&self.source_lock_digest);

        let mut sources = self.sources.iter().collect::<Vec<_>>();
        sources.sort_by_key(|source| source.order());
        encoder.sequence(&sources, |encoder, source| source.encode(encoder));

        self.build_lock.encode_canonical(&mut encoder);
        encoder.sequence(&self.jobs, |encoder, job| job.encode(encoder));
        encoder.map(&self.environment);
        self.layout.encode(&mut encoder);
        self.execution.encode(&mut encoder);

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
    fn validate(&self) -> Result<(), DerivationValidationError> {
        require_nonempty("package.name", &self.name)?;
        require_nonempty("package.version", &self.version)?;
        require_nonempty("package.architecture", &self.architecture)?;
        if self.source_release == 0 {
            return Err(DerivationValidationError::ZeroSourceRelease);
        }
        if self.build_release == 0 {
            return Err(DerivationValidationError::ZeroBuildRelease);
        }
        Ok(())
    }

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

    fn validate(&self, index: usize) -> Result<(), DerivationValidationError> {
        match self {
            Self::Archive {
                url, sha256, filename, ..
            } => {
                require_nonempty(&format!("sources[{index}].url"), url)?;
                validate_url(index, url)?;
                if sha256.len() != 64 || !sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                    return Err(DerivationValidationError::InvalidArchiveSha256 {
                        index,
                        value: sha256.clone(),
                    });
                }
                validate_source_destination(index, "filename", filename)
            }
            Self::Git {
                url,
                requested_ref,
                commit,
                directory,
                ..
            } => {
                require_nonempty(&format!("sources[{index}].url"), url)?;
                validate_url(index, url)?;
                require_nonempty(&format!("sources[{index}].requested_ref"), requested_ref)?;
                validate_source_destination(index, "directory", directory)?;
                if commit.len() != 40 || !commit.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                    return Err(DerivationValidationError::InvalidGitCommit {
                        index,
                        value: commit.clone(),
                    });
                }
                Ok(())
            }
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
                directory,
            } => {
                encoder.variant(1);
                encoder.u32(*order);
                encoder.string(url);
                encoder.string(requested_ref);
                encoder.string(commit);
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
    fn validate(&self, parent: &str, build_dir: &Path) -> Result<(), DerivationValidationError> {
        for (group, steps) in [("pre", &self.pre), ("steps", &self.steps), ("post", &self.post)] {
            for (step_index, step) in steps.iter().enumerate() {
                step.validate(&format!("{parent}.{group}[{step_index}]"), build_dir)?;
            }
        }
        Ok(())
    }

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
    fn validate(&self, job_index: usize, layout_build_dir: &Path) -> Result<(), DerivationValidationError> {
        let build_field = format!("jobs[{job_index}].build_dir");
        let work_field = format!("jobs[{job_index}].work_dir");
        let build_dir = validate_normalized_absolute_path(&build_field, &self.build_dir)?;
        require_path_contained(&build_field, build_dir, "layout.build_dir", layout_build_dir)?;
        let work_dir = validate_normalized_absolute_path(&work_field, &self.work_dir)?;
        require_path_contained(&work_field, work_dir, &build_field, build_dir)?;

        match (&self.pgo_stage, &self.pgo_dir) {
            (Some(stage), Some(pgo_dir)) => {
                if !matches!(stage.as_str(), "one" | "two" | "use") {
                    return Err(DerivationValidationError::UnsupportedPgoStage {
                        job: job_index,
                        stage: stage.clone(),
                    });
                }
                let field = format!("jobs[{job_index}].pgo_dir");
                let pgo_dir = validate_normalized_absolute_path(&field, pgo_dir)?;
                require_path_contained(&field, pgo_dir, "layout.build_dir", layout_build_dir)?;
            }
            (None, None) => {}
            _ => {
                return Err(DerivationValidationError::PgoStageDirectoryMismatch {
                    job: job_index,
                    stage: self.pgo_stage.clone(),
                    directory: self.pgo_dir.clone(),
                });
            }
        }

        let mut phase_names = BTreeSet::new();
        for (phase_index, phase) in self.phases.iter().enumerate() {
            let name = phase.name.to_ascii_lowercase();
            if !matches!(
                name.as_str(),
                "prepare" | "setup" | "build" | "install" | "check" | "workload"
            ) {
                return Err(DerivationValidationError::UnsupportedPhase {
                    job: job_index,
                    phase: phase_index,
                    name: phase.name.clone(),
                });
            }
            if !phase_names.insert(name) {
                return Err(DerivationValidationError::DuplicatePhase {
                    job: job_index,
                    name: phase.name.clone(),
                });
            }
            phase.validate(&format!("jobs[{job_index}].phases[{phase_index}]"), build_dir)?;
        }
        Ok(())
    }

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
        program: String,
        args: Vec<String>,
        environment: BTreeMap<String, String>,
        working_dir: String,
    },
    Shell {
        interpreter: String,
        script: String,
        environment: BTreeMap<String, String>,
        working_dir: String,
    },
}

impl StepPlan {
    fn validate(&self, field: &str, build_dir: &Path) -> Result<(), DerivationValidationError> {
        match self {
            Self::Run {
                program, working_dir, ..
            } => {
                require_nonempty(&format!("{field}.program"), program)?;
                validate_step_working_dir(field, working_dir, build_dir)
            }
            Self::Shell {
                interpreter,
                script,
                working_dir,
                ..
            } => {
                require_nonempty(&format!("{field}.interpreter"), interpreter)?;
                require_nonempty(&format!("{field}.script"), script)?;
                validate_step_working_dir(field, working_dir, build_dir)
            }
        }
    }

    fn encode(&self, encoder: &mut CanonicalEncoder) {
        match self {
            Self::Run {
                program,
                args,
                environment,
                working_dir,
            } => {
                encoder.variant(0);
                encoder.string(program);
                encoder.strings(args);
                encoder.map(environment);
                encoder.string(working_dir);
            }
            Self::Shell {
                interpreter,
                script,
                environment,
                working_dir,
            } => {
                encoder.variant(1);
                encoder.string(interpreter);
                encoder.string(script);
                encoder.map(environment);
                encoder.string(working_dir);
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

    fn validate(&self) -> Result<(), DerivationValidationError> {
        validate_sandbox_hostname(&self.hostname)?;
        let guest_root = validate_normalized_absolute_path("layout.guest_root", &self.guest_root)?;
        for (field, value) in [
            ("layout.artifacts_dir", &self.artifacts_dir),
            ("layout.build_dir", &self.build_dir),
            ("layout.source_dir", &self.source_dir),
            ("layout.recipe_dir", &self.recipe_dir),
            ("layout.install_dir", &self.install_dir),
            ("layout.package_dir", &self.package_dir),
            ("layout.ccache_dir", &self.ccache_dir),
            ("layout.sccache_dir", &self.sccache_dir),
            ("layout.go_cache_dir", &self.go_cache_dir),
            ("layout.go_mod_cache_dir", &self.go_mod_cache_dir),
            ("layout.cargo_cache_dir", &self.cargo_cache_dir),
            ("layout.zig_cache_dir", &self.zig_cache_dir),
        ] {
            let path = validate_normalized_absolute_path(field, value)?;
            require_proper_path_child(field, path, "layout.guest_root", guest_root)?;
        }
        require_proper_path_child(
            "layout.package_dir",
            Path::new(&self.package_dir),
            "layout.recipe_dir",
            Path::new(&self.recipe_dir),
        )?;
        reject_layout_path_overlaps(&[
            ("layout.artifacts_dir", &self.artifacts_dir),
            ("layout.build_dir", &self.build_dir),
            ("layout.source_dir", &self.source_dir),
            ("layout.recipe_dir", &self.recipe_dir),
            ("layout.install_dir", &self.install_dir),
            ("layout.ccache_dir", &self.ccache_dir),
            ("layout.sccache_dir", &self.sccache_dir),
            ("layout.go_cache_dir", &self.go_cache_dir),
            ("layout.go_mod_cache_dir", &self.go_mod_cache_dir),
            ("layout.cargo_cache_dir", &self.cargo_cache_dir),
            ("layout.zig_cache_dir", &self.zig_cache_dir),
        ])?;
        Ok(())
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
    pub network: NetworkMode,
    pub compiler_cache: bool,
    pub jobs: u32,
}

impl Default for ExecutionPolicy {
    fn default() -> Self {
        Self {
            network: NetworkMode::Disabled,
            compiler_cache: false,
            jobs: 1,
        }
    }
}

impl ExecutionPolicy {
    fn encode(&self, encoder: &mut CanonicalEncoder) {
        encoder.variant(match self.network {
            NetworkMode::Disabled => 0,
            NetworkMode::Enabled => 1,
        });
        encoder.bool(self.compiler_cache);
        encoder.u32(self.jobs);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkMode {
    Disabled,
    Enabled,
}

/// Frozen ordered handlers and switches consumed by package analysis.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalysisPlan {
    pub handlers: Vec<AnalyzerKind>,
    pub toolchain: AnalysisToolchain,
    pub debug: bool,
    pub strip: bool,
    pub compress_man: bool,
    pub remove_libtool: bool,
}

impl Default for AnalysisPlan {
    fn default() -> Self {
        Self {
            handlers: Vec::new(),
            toolchain: AnalysisToolchain::Llvm,
            debug: false,
            strip: true,
            compress_man: true,
            remove_libtool: true,
        }
    }
}

impl AnalysisPlan {
    fn validate(&self) -> Result<(), DerivationValidationError> {
        if self.handlers.is_empty() {
            return Err(DerivationValidationError::Empty {
                field: "analysis.handlers".to_owned(),
            });
        }

        let mut handlers = BTreeSet::new();
        for handler in &self.handlers {
            if !handlers.insert(*handler) {
                return Err(DerivationValidationError::DuplicateAnalyzer {
                    name: handler.as_str().to_owned(),
                });
            }
        }

        let Some(include_any) = self
            .handlers
            .iter()
            .position(|handler| *handler == AnalyzerKind::IncludeAny)
        else {
            return Err(DerivationValidationError::MissingAnalyzer {
                name: AnalyzerKind::IncludeAny.as_str().to_owned(),
            });
        };
        if include_any + 1 != self.handlers.len() {
            return Err(DerivationValidationError::AnalyzerMustBeLast {
                name: AnalyzerKind::IncludeAny.as_str().to_owned(),
            });
        }

        Ok(())
    }

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
        encoder.variant(match self.toolchain {
            AnalysisToolchain::Llvm => 0,
            AnalysisToolchain::Gnu => 1,
        });
        encoder.bool(self.debug);
        encoder.bool(self.strip);
        encoder.bool(self.compress_man);
        encoder.bool(self.remove_libtool);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnalysisToolchain {
    Llvm,
    Gnu,
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
    fn validate(&self, field: &str) -> Result<(), DerivationValidationError> {
        Dependency::new(self.kind.into(), self.name.clone())
            .map(|_| ())
            .map_err(|source| DerivationValidationError::InvalidRelation {
                field: field.to_owned(),
                value: self.name.clone(),
                source,
            })
    }

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
    fn validate(&self, index: usize) -> Result<(), DerivationValidationError> {
        require_nonempty(&format!("outputs[{index}].name"), &self.name)?;
        require_nonempty(&format!("outputs[{index}].package_name"), &self.package_name)?;
        for (pattern_index, pattern) in self.provides_exclude.iter().enumerate() {
            validate_regex(&format!("outputs[{index}].provides_exclude[{pattern_index}]"), pattern)?;
        }
        for (pattern_index, pattern) in self.runtime_exclude.iter().enumerate() {
            validate_regex(&format!("outputs[{index}].runtime_exclude[{pattern_index}]"), pattern)?;
        }
        for (conflict_index, conflict) in self.conflicts.iter().enumerate() {
            conflict.validate(&format!("outputs[{index}].conflicts[{conflict_index}]"))?;
        }
        Ok(())
    }

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
    fn validate(&self, index: usize) -> Result<(), DerivationValidationError> {
        require_nonempty(&format!("collection_rules[{index}].output"), &self.output)?;
        let field = format!("collection_rules[{index}].pattern");
        require_nonempty(&field, &self.pattern)?;
        validate_glob(&field, &self.pattern)
    }

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

#[derive(Debug, Error)]
pub enum DerivationValidationError {
    #[error("schema_version: unsupported schema {found}; expected {expected}")]
    UnsupportedSchema { found: u32, expected: u32 },
    #[error("{field}: value must not be empty")]
    Empty { field: String },
    #[error("package.build_release: value must be greater than zero")]
    ZeroBuildRelease,
    #[error("package.source_release: value must be greater than zero")]
    ZeroSourceRelease,
    #[error("execution.jobs: value must be greater than zero")]
    ZeroJobs,
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
    #[error("analysis.handlers: duplicate analyzer `{name}`")]
    DuplicateAnalyzer { name: String },
    #[error("analysis.handlers: required analyzer `{name}` is missing")]
    MissingAnalyzer { name: String },
    #[error("analysis.handlers: analyzer `{name}` must be last")]
    AnalyzerMustBeLast { name: String },
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
    #[error("sources[{index}].url: invalid URL `{value}`")]
    InvalidSourceUrl {
        index: usize,
        value: String,
        #[source]
        source: url::ParseError,
    },
    #[error("sources[{index}].commit: expected a complete 40-hex Git commit, found `{value}`")]
    InvalidGitCommit { index: usize, value: String },
    #[error("sources[{index}].sha256: expected exactly 64 ASCII hexadecimal characters, found `{value}`")]
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

fn validate_planned_output_cycles(outputs: &[OutputPlan]) -> Result<(), DerivationValidationError> {
    let edges = outputs
        .iter()
        .enumerate()
        .map(|(output_index, output)| {
            let dependencies = output
                .runtime_inputs
                .iter()
                .enumerate()
                .filter_map(|(dependency_index, dependency)| match dependency {
                    OutputRelation::Planned { output } => Some((
                        output.as_str(),
                        format!("outputs[{output_index}].runtime_inputs[{dependency_index}]"),
                    )),
                    OutputRelation::Locked { .. } => None,
                })
                .collect();
            (output.name.as_str(), dependencies)
        })
        .collect::<BTreeMap<_, Vec<_>>>();

    let mut visited = BTreeSet::new();
    for output in outputs {
        let mut visiting = BTreeSet::new();
        let mut path = Vec::new();
        visit_planned_output(&output.name, &edges, &mut visiting, &mut visited, &mut path)?;
    }
    Ok(())
}

fn visit_planned_output<'a>(
    output: &'a str,
    edges: &BTreeMap<&'a str, Vec<(&'a str, String)>>,
    visiting: &mut BTreeSet<&'a str>,
    visited: &mut BTreeSet<&'a str>,
    path: &mut Vec<&'a str>,
) -> Result<(), DerivationValidationError> {
    if visited.contains(output) {
        return Ok(());
    }

    visiting.insert(output);
    path.push(output);
    for (dependency, field) in edges.get(output).into_iter().flatten() {
        if visiting.contains(dependency) {
            let start = path.iter().position(|entry| entry == dependency).unwrap_or(0);
            let mut cycle = path[start..]
                .iter()
                .map(|entry| (*entry).to_owned())
                .collect::<Vec<_>>();
            cycle.push((*dependency).to_owned());
            return Err(DerivationValidationError::PlannedOutputCycle {
                field: field.clone(),
                cycle,
            });
        }
        visit_planned_output(dependency, edges, visiting, visited, path)?;
    }
    path.pop();
    visiting.remove(output);
    visited.insert(output);
    Ok(())
}

fn require_nonempty(field: &str, value: &str) -> Result<(), DerivationValidationError> {
    if value.is_empty() {
        Err(DerivationValidationError::Empty {
            field: field.to_owned(),
        })
    } else {
        Ok(())
    }
}

fn validate_regex(field: &str, value: &str) -> Result<(), DerivationValidationError> {
    regex::Regex::new(value)
        .map(|_| ())
        .map_err(|source| DerivationValidationError::InvalidRegex {
            field: field.to_owned(),
            value: value.to_owned(),
            source,
        })
}

fn validate_glob(field: &str, value: &str) -> Result<(), DerivationValidationError> {
    glob::Pattern::new(value)
        .map(|_| ())
        .map_err(|source| DerivationValidationError::InvalidGlob {
            field: field.to_owned(),
            value: value.to_owned(),
            source,
        })
}

fn validate_normalized_absolute_path<'a>(field: &str, value: &'a str) -> Result<&'a Path, DerivationValidationError> {
    let path = Path::new(value);
    let mut normalized = PathBuf::new();
    let mut normal_components = 0usize;
    let mut safe_components = true;
    for component in path.components() {
        match component {
            Component::RootDir if normalized.as_os_str().is_empty() => normalized.push(component.as_os_str()),
            Component::Normal(_) => {
                normal_components += 1;
                normalized.push(component.as_os_str());
            }
            Component::Prefix(_) | Component::RootDir | Component::CurDir | Component::ParentDir => {
                safe_components = false;
            }
        }
    }
    if !path.is_absolute() || normal_components == 0 || !safe_components || normalized.as_os_str() != path.as_os_str() {
        return Err(DerivationValidationError::UnsafeAbsolutePath {
            field: field.to_owned(),
            value: value.to_owned(),
        });
    }
    Ok(path)
}

fn require_path_contained(
    field: &str,
    path: &Path,
    root_field: &str,
    root: &Path,
) -> Result<(), DerivationValidationError> {
    if path.starts_with(root) {
        Ok(())
    } else {
        Err(DerivationValidationError::PathOutsideRoot {
            field: field.to_owned(),
            value: path.display().to_string(),
            root_field: root_field.to_owned(),
            root: root.display().to_string(),
        })
    }
}

fn require_proper_path_child(
    field: &str,
    path: &Path,
    root_field: &str,
    root: &Path,
) -> Result<(), DerivationValidationError> {
    if path != root && path.starts_with(root) {
        Ok(())
    } else {
        Err(DerivationValidationError::PathOutsideRoot {
            field: field.to_owned(),
            value: path.display().to_string(),
            root_field: root_field.to_owned(),
            root: root.display().to_string(),
        })
    }
}

fn validate_sandbox_hostname(value: &str) -> Result<(), DerivationValidationError> {
    let labels_are_valid = !value.is_empty()
        && value.len() <= 64
        && value.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                && label.as_bytes().first().is_some_and(u8::is_ascii_alphanumeric)
                && label.as_bytes().last().is_some_and(u8::is_ascii_alphanumeric)
        });
    if labels_are_valid {
        Ok(())
    } else {
        Err(DerivationValidationError::InvalidSandboxHostname {
            value: value.to_owned(),
        })
    }
}

fn reject_layout_path_overlaps(paths: &[(&str, &str)]) -> Result<(), DerivationValidationError> {
    for (index, (field, value)) in paths.iter().enumerate() {
        for (other_field, other) in &paths[..index] {
            if Path::new(value).starts_with(other) || Path::new(other).starts_with(value) {
                return Err(DerivationValidationError::OverlappingLayoutPath {
                    field: (*field).to_owned(),
                    value: (*value).to_owned(),
                    other_field: (*other_field).to_owned(),
                    other: (*other).to_owned(),
                });
            }
        }
    }
    Ok(())
}

fn validate_step_working_dir(
    step_field: &str,
    working_dir: &str,
    build_dir: &Path,
) -> Result<(), DerivationValidationError> {
    let field = format!("{step_field}.working_dir");
    let working_dir = validate_normalized_absolute_path(&field, working_dir)?;
    require_path_contained(&field, working_dir, "job build_dir", build_dir)
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

fn validate_url(index: usize, value: &str) -> Result<(), DerivationValidationError> {
    url::Url::parse(value).map_err(|source| DerivationValidationError::InvalidSourceUrl {
        index,
        value: value.to_owned(),
        source,
    })?;
    Ok(())
}

fn validate_source_destination(
    index: usize,
    field: &'static str,
    value: &str,
) -> Result<(), DerivationValidationError> {
    let path = Path::new(value);
    if value.is_empty()
        || path.is_absolute()
        || path.components().count() != 1
        || !path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
    {
        return Err(DerivationValidationError::UnsafeSourceDestination {
            index,
            field,
            value: value.to_owned(),
        });
    }
    Ok(())
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
    use super::*;

    fn sample_plan() -> DerivationPlan {
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
            build_lock::sample_lock(),
        );
        plan.boulder_version = "0.26.6".to_owned();
        plan.boulder_fingerprint = "sha256:test-boulder-semantics".to_owned();
        plan.recipe_fingerprint = "recipe-fingerprint".to_owned();
        plan.source_lock_digest = "source-lock-digest".to_owned();
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
                    program: "/usr/bin/cmake".to_owned(),
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
            hostname: "boulder".to_owned(),
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
            network: NetworkMode::Disabled,
            compiler_cache: false,
            jobs: 4,
        };
        plan.analysis.handlers = vec![AnalyzerKind::Elf, AnalyzerKind::Python, AnalyzerKind::IncludeAny];
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
    fn validation_requires_complete_boulder_implementation_identity() {
        for (field, clear) in [
            (
                "boulder_version",
                Box::new(|plan: &mut DerivationPlan| plan.boulder_version.clear()) as Box<dyn Fn(&mut DerivationPlan)>,
            ),
            (
                "boulder_fingerprint",
                Box::new(|plan: &mut DerivationPlan| plan.boulder_fingerprint.clear()),
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
    fn every_required_semantic_mutation_changes_identity() {
        let original = sample_plan();
        let original_id = original.derivation_id();
        let mutations: Vec<(&str, Box<dyn Fn(&mut DerivationPlan)>)> = vec![
            (
                "boulder-version",
                Box::new(|plan| plan.boulder_version.push_str("-changed")),
            ),
            (
                "boulder-implementation",
                Box::new(|plan| plan.boulder_fingerprint.push_str("-changed")),
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
                    StepPlan::Shell { .. } => unreachable!(),
                }),
            ),
            (
                "environment",
                Box::new(|plan| {
                    plan.environment.insert("LANG".to_owned(), "C".to_owned());
                }),
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
            interpreter: "/usr/bin/bash".to_owned(),
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
        for value in ["", ".", "../escape", "/absolute", "nested/file"] {
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
    fn validation_requires_a_complete_archive_sha256() {
        for value in [
            "",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaag",
        ] {
            let mut plan = sample_plan();
            let LockedSource::Archive { sha256, .. } = &mut plan.sources[0] else {
                unreachable!()
            };
            *sha256 = value.to_owned();
            assert!(matches!(
                plan.validate(),
                Err(DerivationValidationError::InvalidArchiveSha256 { index: 0, .. })
            ));
        }

        let mut uppercase = sample_plan();
        let LockedSource::Archive { sha256, .. } = &mut uppercase.sources[0] else {
            unreachable!()
        };
        *sha256 = "ABCDEFABCDEFABCDEFABCDEFABCDEFABCDEFABCDEFABCDEFABCDEFABCDEFABCD".to_owned();
        uppercase.validate().unwrap();
    }

    #[test]
    fn validation_rejects_duplicate_source_materialization_destinations_across_kinds() {
        let mut plan = sample_plan();
        plan.sources.push(LockedSource::Git {
            order: 1,
            url: "https://example.invalid/other.git".to_owned(),
            requested_ref: "main".to_owned(),
            commit: "0123456789abcdef0123456789abcdef01234567".to_owned(),
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
    fn git_materialization_directory_is_validated_and_hashed() {
        let mut first = sample_plan();
        first.sources = vec![LockedSource::Git {
            order: 0,
            url: "https://example.invalid/hello.git".to_owned(),
            requested_ref: "main".to_owned(),
            commit: "0123456789abcdef0123456789abcdef01234567".to_owned(),
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
