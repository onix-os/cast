// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

//! Frozen, canonical build plans.
//!
//! [`DerivationPlan`] is the semantic boundary between resolution and
//! execution. It contains values only: the executor may index or borrow these
//! values, but must not infer another dependency, phase, policy, or output.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path},
};

use sha2::{Digest, Sha256};
use thiserror::Error;

pub use self::build_lock::{
    BUILD_LOCK_FILE_NAME, BUILD_LOCK_SCHEMA_VERSION, BuildLock, BuildLockDecodeError, BuildLockValidationError,
    LockedIdentity, LockedOutput, LockedOutputRef, LockedPackage, LockedRequest, Platform, RepositorySnapshot,
    decode_build_lock, encode_build_lock,
};

mod build_lock;

/// Current schema used by [`DerivationPlan`].
pub const DERIVATION_PLAN_SCHEMA_VERSION: u32 = 1;

const DERIVATION_HASH_DOMAIN: &[u8] = b"os-tools-derivation-plan\0";

/// A completely resolved, target-specific build description.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerivationPlan {
    pub schema_version: u32,
    pub boulder_version: String,
    pub package: PackageIdentity,
    pub recipe_fingerprint: String,
    pub source_lock_digest: String,
    pub sources: Vec<LockedSource>,
    pub build_lock: BuildLock,
    pub jobs: Vec<JobPlan>,
    pub environment: BTreeMap<String, String>,
    pub layout: BuilderLayout,
    pub execution: ExecutionPolicy,
    pub tuning: Vec<String>,
    pub analyzers: Vec<LockedIdentity>,
    pub analysis: AnalysisPlan,
    pub manifest_build_inputs: Vec<String>,
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
            package,
            recipe_fingerprint: String::new(),
            source_lock_digest: String::new(),
            sources: Vec::new(),
            build_lock,
            jobs: Vec::new(),
            environment: BTreeMap::new(),
            layout: BuilderLayout::default(),
            execution: ExecutionPolicy::default(),
            tuning: Vec::new(),
            analyzers: Vec::new(),
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
        self.package.validate()?;
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
            job.validate(index)?;
        }
        for (index, flag) in self.tuning.iter().enumerate() {
            require_nonempty(&format!("tuning[{index}]"), flag)?;
        }
        let mut analyzer_names = BTreeSet::new();
        for (index, analyzer) in self.analyzers.iter().enumerate() {
            analyzer.validate(&format!("analyzers[{index}]"))?;
            if !analyzer_names.insert(analyzer.name.as_str()) {
                return Err(DerivationValidationError::DuplicateAnalyzer {
                    name: analyzer.name.clone(),
                });
            }
        }

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
                match dependency {
                    OutputRelation::Locked { request, reference }
                        if !self.build_lock.contains_output(reference)
                            || !self.build_lock.requests.iter().any(|locked| {
                                locked.request == *request
                                    && locked.package_id == reference.package_id
                                    && locked.output == reference.output
                            }) =>
                    {
                        return Err(DerivationValidationError::UnknownOutputReference {
                            field: format!("outputs[{index}].runtime_inputs[{dependency_index}]"),
                            package: reference.package_id.clone(),
                            output: reference.output.clone(),
                        });
                    }
                    OutputRelation::Planned { output } if !output_names.contains(output.as_str()) => {
                        return Err(DerivationValidationError::UnknownPlannedOutput {
                            field: format!("outputs[{index}].runtime_inputs[{dependency_index}]"),
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
    /// Declaration order is retained for phases, steps, hooks, arguments,
    /// PGO stages, and tuning flags. Semantically unordered collections such
    /// as locked sources, analyzers, and outputs are sorted by stable keys.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut encoder = CanonicalEncoder::new(DERIVATION_HASH_DOMAIN);
        encoder.u32(self.schema_version);
        encoder.string(&self.boulder_version);
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
        encoder.strings(&self.tuning);

        let mut analyzers = self.analyzers.iter().collect::<Vec<_>>();
        analyzers.sort_by(|left, right| {
            left.name
                .cmp(&right.name)
                .then_with(|| left.fingerprint.cmp(&right.fingerprint))
        });
        encoder.sequence(&analyzers, |encoder, analyzer| analyzer.encode(encoder));
        self.analysis.encode(&mut encoder);
        let mut manifest_build_inputs = self.manifest_build_inputs.clone();
        manifest_build_inputs.sort();
        encoder.strings(&manifest_build_inputs);
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
    fn validate(&self, parent: &str) -> Result<(), DerivationValidationError> {
        for (group, steps) in [("pre", &self.pre), ("steps", &self.steps), ("post", &self.post)] {
            for (step_index, step) in steps.iter().enumerate() {
                step.validate(&format!("{parent}.{group}[{step_index}]"))?;
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
    fn validate(&self, job_index: usize) -> Result<(), DerivationValidationError> {
        require_nonempty(&format!("jobs[{job_index}].build_dir"), &self.build_dir)?;
        require_nonempty(&format!("jobs[{job_index}].work_dir"), &self.work_dir)?;
        if let Some(pgo_dir) = &self.pgo_dir {
            require_nonempty(&format!("jobs[{job_index}].pgo_dir"), pgo_dir)?;
        }
        let mut phase_names = BTreeSet::new();
        for (phase_index, phase) in self.phases.iter().enumerate() {
            require_nonempty(&format!("jobs[{job_index}].phases[{phase_index}].name"), &phase.name)?;
            if !phase_names.insert(phase.name.as_str()) {
                return Err(DerivationValidationError::DuplicatePhase {
                    job: job_index,
                    name: phase.name.clone(),
                });
            }
            phase.validate(&format!("jobs[{job_index}].phases[{phase_index}]"))?;
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
    fn validate(&self, field: &str) -> Result<(), DerivationValidationError> {
        match self {
            Self::Run {
                program, working_dir, ..
            } => {
                require_nonempty(&format!("{field}.program"), program)?;
                require_nonempty(&format!("{field}.working_dir"), working_dir)
            }
            Self::Shell {
                interpreter,
                script,
                working_dir,
                ..
            } => {
                require_nonempty(&format!("{field}.interpreter"), interpreter)?;
                require_nonempty(&format!("{field}.script"), script)?;
                require_nonempty(&format!("{field}.working_dir"), working_dir)
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
    pub build_dir: String,
    pub source_dir: String,
    pub install_dir: String,
    pub package_dir: String,
}

impl BuilderLayout {
    fn validate(&self) -> Result<(), DerivationValidationError> {
        for (field, value) in [
            ("layout.build_dir", &self.build_dir),
            ("layout.source_dir", &self.source_dir),
            ("layout.install_dir", &self.install_dir),
            ("layout.package_dir", &self.package_dir),
        ] {
            require_nonempty(field, value)?;
            if !value.starts_with('/') {
                return Err(DerivationValidationError::NonAbsoluteGuestPath {
                    field: field.to_owned(),
                    value: value.clone(),
                });
            }
        }
        Ok(())
    }

    fn encode(&self, encoder: &mut CanonicalEncoder) {
        encoder.string(&self.build_dir);
        encoder.string(&self.source_dir);
        encoder.string(&self.install_dir);
        encoder.string(&self.package_dir);
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

/// Frozen switches consumed by package analysis.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalysisPlan {
    pub toolchain: AnalysisToolchain,
    pub debug: bool,
    pub strip: bool,
    pub compress_man: bool,
    pub remove_libtool: bool,
}

impl Default for AnalysisPlan {
    fn default() -> Self {
        Self {
            toolchain: AnalysisToolchain::Llvm,
            debug: false,
            strip: true,
            compress_man: true,
            remove_libtool: true,
        }
    }
}

impl AnalysisPlan {
    fn encode(&self, encoder: &mut CanonicalEncoder) {
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

/// One declared package output after template and package composition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputPlan {
    pub name: String,
    pub package_name: String,
    pub summary: Option<String>,
    pub description: Option<String>,
    pub provides_exclude: Vec<String>,
    pub runtime_exclude: Vec<String>,
    pub paths: Vec<PathRulePlan>,
    pub runtime_inputs: Vec<OutputRelation>,
    pub conflicts: Vec<String>,
}

impl OutputPlan {
    fn validate(&self, index: usize) -> Result<(), DerivationValidationError> {
        require_nonempty(&format!("outputs[{index}].name"), &self.name)?;
        require_nonempty(&format!("outputs[{index}].package_name"), &self.package_name)?;
        for (rule_index, rule) in self.paths.iter().enumerate() {
            require_nonempty(&format!("outputs[{index}].paths[{rule_index}].pattern"), &rule.pattern)?;
        }
        Ok(())
    }

    fn encode(&self, encoder: &mut CanonicalEncoder) {
        encoder.string(&self.name);
        encoder.string(&self.package_name);
        encode_optional_string(encoder, self.summary.as_deref());
        encode_optional_string(encoder, self.description.as_deref());
        let mut provides_exclude = self.provides_exclude.clone();
        provides_exclude.sort();
        encoder.strings(&provides_exclude);
        let mut runtime_exclude = self.runtime_exclude.clone();
        runtime_exclude.sort();
        encoder.strings(&runtime_exclude);
        encoder.sequence(&self.paths, |encoder, path| path.encode(encoder));

        let mut runtime_inputs = self.runtime_inputs.iter().collect::<Vec<_>>();
        runtime_inputs.sort();
        encoder.sequence(&runtime_inputs, |encoder, dependency| dependency.encode(encoder));
        let mut conflicts = self.conflicts.clone();
        conflicts.sort();
        encoder.strings(&conflicts);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum OutputRelation {
    Locked {
        request: String,
        reference: LockedOutputRef,
    },
    Planned {
        output: String,
    },
}

impl OutputRelation {
    fn encode(&self, encoder: &mut CanonicalEncoder) {
        match self {
            Self::Locked { request, reference } => {
                encoder.variant(0);
                encoder.string(request);
                reference.encode(encoder);
            }
            Self::Planned { output } => {
                encoder.variant(1);
                encoder.string(output);
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathRulePlan {
    pub kind: PathRuleKind,
    pub pattern: String,
}

impl PathRulePlan {
    fn encode(&self, encoder: &mut CanonicalEncoder) {
        encoder.variant(match self.kind {
            PathRuleKind::Any => 0,
            PathRuleKind::Executable => 1,
            PathRuleKind::Symlink => 2,
            PathRuleKind::Special => 3,
        });
        encoder.string(&self.pattern);
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
        require_nonempty(&format!("collection_rules[{index}].pattern"), &self.pattern)
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
    #[error("analyzers: duplicate analyzer name `{name}`")]
    DuplicateAnalyzer { name: String },
    #[error("{field}: unknown locked output `{package}:{output}`")]
    UnknownOutputReference {
        field: String,
        package: String,
        output: String,
    },
    #[error("{field}: unknown planned output `{output}`")]
    UnknownPlannedOutput { field: String, output: String },
    #[error("{field}: planned output dependency cycle: {}", cycle.join(" -> "))]
    PlannedOutputCycle { field: String, cycle: Vec<String> },
    #[error("{field}: guest path must be absolute, found `{value}`")]
    NonAbsoluteGuestPath { field: String, value: String },
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
            build_dir: "/mason/build".to_owned(),
            source_dir: "/mason/sources".to_owned(),
            install_dir: "/mason/install".to_owned(),
            package_dir: "/mason/package".to_owned(),
        };
        plan.execution = ExecutionPolicy {
            network: NetworkMode::Disabled,
            compiler_cache: false,
            jobs: 4,
        };
        plan.tuning = vec!["-O2".to_owned(), "-pipe".to_owned()];
        plan.analyzers = vec![LockedIdentity {
            name: "elf".to_owned(),
            fingerprint: "elf-analyzer-v1".to_owned(),
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
            summary: Some("Hello".to_owned()),
            description: None,
            provides_exclude: Vec::new(),
            runtime_exclude: Vec::new(),
            paths: vec![PathRulePlan {
                kind: PathRuleKind::Any,
                pattern: "*".to_owned(),
            }],
            runtime_inputs: Vec::new(),
            conflicts: Vec::new(),
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
    fn unordered_analyzers_and_outputs_do_not_change_identity() {
        let mut first = sample_plan();
        first.analyzers.push(LockedIdentity {
            name: "python".to_owned(),
            fingerprint: "python-analyzer-v1".to_owned(),
        });
        first.outputs.push(OutputPlan {
            name: "dev".to_owned(),
            package_name: "hello-devel".to_owned(),
            summary: None,
            description: None,
            provides_exclude: Vec::new(),
            runtime_exclude: Vec::new(),
            paths: Vec::new(),
            runtime_inputs: Vec::new(),
            conflicts: Vec::new(),
        });
        let mut reordered = first.clone();
        reordered.analyzers.reverse();
        reordered.outputs.reverse();

        assert_eq!(first.canonical_bytes(), reordered.canonical_bytes());
        assert_eq!(first.derivation_id(), reordered.derivation_id());
    }

    #[test]
    fn every_required_semantic_mutation_changes_identity() {
        let original = sample_plan();
        let original_id = original.derivation_id();
        let mutations: Vec<(&str, Box<dyn Fn(&mut DerivationPlan)>)> = vec![
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
                "target",
                Box::new(|plan| plan.build_lock.target_platform.architecture = "aarch64".to_owned()),
            ),
            (
                "policy",
                Box::new(|plan| plan.build_lock.policy.fingerprint.push_str("-changed")),
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
                "manifest-build-input",
                Box::new(|plan| plan.manifest_build_inputs.push("cmake".to_owned())),
            ),
            (
                "collection-rule-order",
                Box::new(|plan| plan.collection_rules.reverse()),
            ),
            (
                "output",
                Box::new(|plan| plan.outputs[0].paths[0].pattern = "/usr/bin/*".to_owned()),
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
    fn validation_rejects_output_relations_outside_the_locked_closure() {
        let mut plan = sample_plan();
        plan.outputs[0].runtime_inputs.push(OutputRelation::Locked {
            request: "missing".to_owned(),
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
            summary: None,
            description: None,
            provides_exclude: Vec::new(),
            runtime_exclude: Vec::new(),
            paths: Vec::new(),
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
