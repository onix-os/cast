// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

//! Frozen, canonical build plans.
//!
//! [`DerivationPlan`] is the semantic boundary between resolution and
//! execution. It contains values only: the executor may index or borrow these
//! values, but must not infer another dependency, phase, policy, or output.

use std::collections::{BTreeMap, BTreeSet};

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
        self.layout.validate()?;
        if self.execution.jobs == 0 {
            return Err(DerivationValidationError::ZeroJobs);
        }

        let mut source_orders = BTreeSet::new();
        for (index, source) in self.sources.iter().enumerate() {
            let order = source.order();
            if !source_orders.insert(order) {
                return Err(DerivationValidationError::DuplicateSourceOrder { order });
            }
            if usize::try_from(order) != Ok(index) {
                return Err(DerivationValidationError::UnexpectedSourceOrder { index, order });
            }
            source.validate(index)?;
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
        for (index, output) in self.outputs.iter().enumerate() {
            output.validate(index)?;
            if !output_names.insert(output.name.as_str()) {
                return Err(DerivationValidationError::DuplicateOutput {
                    name: output.name.clone(),
                });
            }
        }
        for (index, output) in self.outputs.iter().enumerate() {
            for (dependency_index, dependency) in output.runtime_inputs.iter().enumerate() {
                match dependency {
                    OutputRelation::Locked(dependency) if !self.build_lock.contains_output(dependency) => {
                        return Err(DerivationValidationError::UnknownOutputReference {
                            field: format!("outputs[{index}].runtime_inputs[{dependency_index}]"),
                            package: dependency.package_id.clone(),
                            output: dependency.output.clone(),
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
}

impl PackageIdentity {
    fn validate(&self) -> Result<(), DerivationValidationError> {
        require_nonempty("package.name", &self.name)?;
        require_nonempty("package.version", &self.version)?;
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
    }
}

/// One source with all mutable resolution removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LockedSource {
    Archive {
        order: u32,
        url: String,
        sha256: String,
    },
    Git {
        order: u32,
        url: String,
        requested_ref: String,
        commit: String,
    },
}

impl LockedSource {
    pub fn order(&self) -> u32 {
        match self {
            Self::Archive { order, .. } | Self::Git { order, .. } => *order,
        }
    }

    fn validate(&self, index: usize) -> Result<(), DerivationValidationError> {
        match self {
            Self::Archive { url, sha256, .. } => {
                require_nonempty(&format!("sources[{index}].url"), url)?;
                validate_url(index, url)?;
                require_nonempty(&format!("sources[{index}].sha256"), sha256)
            }
            Self::Git {
                url,
                requested_ref,
                commit,
                ..
            } => {
                require_nonempty(&format!("sources[{index}].url"), url)?;
                validate_url(index, url)?;
                require_nonempty(&format!("sources[{index}].requested_ref"), requested_ref)?;
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
            Self::Archive { order, url, sha256 } => {
                encoder.variant(0);
                encoder.u32(*order);
                encoder.string(url);
                encoder.string(sha256);
            }
            Self::Git {
                order,
                url,
                requested_ref,
                commit,
            } => {
                encoder.variant(1);
                encoder.u32(*order);
                encoder.string(url);
                encoder.string(requested_ref);
                encoder.string(commit);
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
    pub build_dir: String,
    pub work_dir: String,
    pub phases: Vec<PhasePlan>,
}

impl JobPlan {
    fn validate(&self, job_index: usize) -> Result<(), DerivationValidationError> {
        require_nonempty(&format!("jobs[{job_index}].build_dir"), &self.build_dir)?;
        require_nonempty(&format!("jobs[{job_index}].work_dir"), &self.work_dir)?;
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

/// One declared package output after template and package composition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputPlan {
    pub name: String,
    pub paths: Vec<PathRulePlan>,
    pub runtime_inputs: Vec<OutputRelation>,
    pub conflicts: Vec<String>,
}

impl OutputPlan {
    fn validate(&self, index: usize) -> Result<(), DerivationValidationError> {
        require_nonempty(&format!("outputs[{index}].name"), &self.name)?;
        for (rule_index, rule) in self.paths.iter().enumerate() {
            require_nonempty(&format!("outputs[{index}].paths[{rule_index}].pattern"), &rule.pattern)?;
        }
        Ok(())
    }

    fn encode(&self, encoder: &mut CanonicalEncoder) {
        encoder.string(&self.name);
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
    Locked(LockedOutputRef),
    Planned { output: String },
}

impl OutputRelation {
    fn encode(&self, encoder: &mut CanonicalEncoder) {
        match self {
            Self::Locked(reference) => {
                encoder.variant(0);
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
    #[error("sources: duplicate source order {order}")]
    DuplicateSourceOrder { order: u32 },
    #[error("sources[{index}].order: expected canonical order {index}, found {order}")]
    UnexpectedSourceOrder { index: usize, order: u32 },
    #[error("jobs[{job}].phases: duplicate phase name `{name}`")]
    DuplicatePhase { job: usize, name: String },
    #[error("outputs: duplicate output name `{name}`")]
    DuplicateOutput { name: String },
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
    #[error(transparent)]
    BuildLock(#[from] BuildLockValidationError),
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

fn validate_url(index: usize, value: &str) -> Result<(), DerivationValidationError> {
    url::Url::parse(value).map_err(|source| DerivationValidationError::InvalidSourceUrl {
        index,
        value: value.to_owned(),
        source,
    })?;
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
            },
            build_lock::sample_lock(),
        );
        plan.boulder_version = "0.26.6".to_owned();
        plan.recipe_fingerprint = "recipe-fingerprint".to_owned();
        plan.source_lock_digest = "source-lock-digest".to_owned();
        plan.sources = vec![LockedSource::Archive {
            order: 0,
            url: "https://example.invalid/hello.tar.zst".to_owned(),
            sha256: "archive-sha256".to_owned(),
        }];
        plan.jobs = vec![JobPlan {
            pgo_stage: None,
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
        plan.outputs = vec![OutputPlan {
            name: "out".to_owned(),
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
        plan.outputs[0]
            .runtime_inputs
            .push(OutputRelation::Locked(LockedOutputRef {
                package_id: "missing".to_owned(),
                output: "out".to_owned(),
            }));

        assert!(matches!(
            plan.validate(),
            Err(DerivationValidationError::UnknownOutputReference { field, .. })
                if field == "outputs[0].runtime_inputs[0]"
        ));
    }
}
