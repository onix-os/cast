use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

use stone::relation::Dependency;

use crate::{
    build_policy::{AnalyzerKind, SUPPORTED_ARTIFACT_ARCHITECTURES},
    spec::{
        SourceUrlKind, is_canonical_git_commit, is_canonical_sha256, is_normalized_relative_path,
        is_safe_artifact_component,
    },
};

use super::{
    super::{
        AnalysisPlan, BuildLock, BuilderLayout, CollectionRulePlan, CompilerCacheRole, DERIVATION_PLAN_SCHEMA_VERSION,
        DerivationPlan, ExecutableCommandPlan, ExecutablePlan, ExecutionCredentials, ExecutionPolicy, InputOrigin,
        JobPlan, LockedSource, NetworkMode, OutputPlan, OutputRelation, PackageIdentity, PhasePlan, RelationKind,
        RelationPlan, RootMaterializationMode, StepPlan, ToolchainCommandsPlan,
    },
    DerivationValidationError,
    field_checks::{
        require_nonempty, validate_artifact_component, validate_glob, validate_package_name, validate_regex,
        validate_source_destination, validate_url,
    },
    limits::DerivationValidationLimits,
    output_cycles::validate_planned_output_cycles,
    path_checks::{
        reject_layout_path_overlaps, require_path_contained, require_proper_path_child,
        validate_normalized_absolute_path, validate_sandbox_hostname, validate_step_working_dir,
    },
    process_budget::ProcessDataBudget,
};

impl DerivationPlan {
    /// Validate invariants required before a plan can cross the freeze
    /// boundary.
    pub fn validate(&self) -> Result<(), DerivationValidationError> {
        self.validate_with_limits(DerivationValidationLimits::default())
    }

    /// Validate a plan with explicit process-data limits.
    ///
    /// Production execution uses [`Self::validate`] and therefore the stable
    /// default limits. This entry point makes boundary behavior testable and
    /// lets importers apply a stricter admission policy without weakening the
    /// executor's defaults.
    pub fn validate_with_limits(&self, limits: DerivationValidationLimits) -> Result<(), DerivationValidationError> {
        if self.schema_version != DERIVATION_PLAN_SCHEMA_VERSION {
            return Err(DerivationValidationError::UnsupportedSchema {
                found: self.schema_version,
                expected: DERIVATION_PLAN_SCHEMA_VERSION,
            });
        }

        let mut process_data = ProcessDataBudget::new(limits);
        process_data.validate(self)?;

        require_nonempty("cast_version", &self.cast_version)?;
        require_nonempty("cast_fingerprint", &self.cast_fingerprint)?;
        self.package.validate()?;
        if !SUPPORTED_ARTIFACT_ARCHITECTURES.contains(&self.package.architecture.as_str()) {
            return Err(DerivationValidationError::UnsupportedArtifactArchitecture {
                value: self.package.architecture.clone(),
                supported: SUPPORTED_ARTIFACT_ARCHITECTURES.join(", "),
            });
        }
        require_nonempty("source_lock_digest", &self.source_lock_digest)?;
        self.build_lock.validate()?;
        self.provenance.validate(&self.source_lock_digest, &self.build_lock)?;
        if self.package.architecture != self.build_lock.target_platform.architecture {
            return Err(DerivationValidationError::ArtifactTargetArchitectureMismatch {
                artifact: self.package.architecture.clone(),
                target: self.build_lock.target_platform.architecture.clone(),
            });
        }
        self.layout.validate()?;
        self.execution.validate()?;
        self.toolchain_commands
            .validate(&self.build_lock, self.execution.compiler_cache)?;

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
            job.validate(
                index,
                Path::new(&self.layout.build_dir),
                &self.sources,
                &self.build_lock,
            )?;
        }
        for (index, input) in self.manifest_build_inputs.iter().enumerate() {
            input.validate(&format!("manifest_build_inputs[{index}]"))?;
        }
        self.analysis.validate(&self.build_lock)?;

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
        if !root.include_in_manifest {
            return Err(DerivationValidationError::RootOutputExcludedFromManifest { index: root_index });
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
}

impl PackageIdentity {
    fn validate(&self) -> Result<(), DerivationValidationError> {
        validate_package_name("package.name", &self.name)?;
        if !self.version.starts_with(|character: char| character.is_ascii_digit()) {
            return Err(DerivationValidationError::InvalidPackageVersion {
                value: self.version.clone(),
            });
        }
        validate_artifact_component("package.version", &self.version)?;
        require_nonempty("package.architecture", &self.architecture)?;
        if self.source_release == 0 {
            return Err(DerivationValidationError::ZeroSourceRelease);
        }
        if self.build_release == 0 {
            return Err(DerivationValidationError::ZeroBuildRelease);
        }
        Ok(())
    }
}

impl LockedSource {
    fn validate(&self, index: usize) -> Result<(), DerivationValidationError> {
        match self {
            Self::Archive {
                url, sha256, filename, ..
            } => {
                require_nonempty(&format!("sources[{index}].url"), url)?;
                validate_url(index, SourceUrlKind::Archive, url)?;
                if !is_canonical_sha256(sha256) {
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
                materialization_sha256,
                directory,
                ..
            } => {
                require_nonempty(&format!("sources[{index}].url"), url)?;
                validate_url(index, SourceUrlKind::Git, url)?;
                require_nonempty(&format!("sources[{index}].requested_ref"), requested_ref)?;
                validate_source_destination(index, "directory", directory)?;
                if !is_canonical_git_commit(commit) {
                    return Err(DerivationValidationError::InvalidGitCommit {
                        index,
                        value: commit.clone(),
                    });
                }
                if !is_canonical_sha256(materialization_sha256) {
                    return Err(DerivationValidationError::InvalidGitMaterializationSha256 {
                        index,
                        value: materialization_sha256.clone(),
                    });
                }
                Ok(())
            }
        }
    }
}

impl PhasePlan {
    fn validate(
        &self,
        parent: &str,
        build_dir: &Path,
        sources: &[LockedSource],
        build_lock: &BuildLock,
    ) -> Result<(), DerivationValidationError> {
        for (group, steps) in [("pre", &self.pre), ("steps", &self.steps), ("post", &self.post)] {
            for (step_index, step) in steps.iter().enumerate() {
                step.validate(
                    &format!("{parent}.{group}[{step_index}]"),
                    build_dir,
                    sources,
                    build_lock,
                    self.name.eq_ignore_ascii_case("prepare") && group == "steps",
                )?;
            }
        }
        Ok(())
    }
}

impl JobPlan {
    fn validate(
        &self,
        job_index: usize,
        layout_build_dir: &Path,
        sources: &[LockedSource],
        build_lock: &BuildLock,
    ) -> Result<(), DerivationValidationError> {
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
            phase.validate(
                &format!("jobs[{job_index}].phases[{phase_index}]"),
                build_dir,
                sources,
                build_lock,
            )?;
        }
        let archive_destinations = self
            .phases
            .iter()
            .flat_map(|phase| phase.pre.iter().chain(&phase.steps).chain(&phase.post))
            .filter_map(|step| match step {
                StepPlan::ExtractArchive { destination, .. } => Some(destination.as_str()),
                StepPlan::Run { .. } | StepPlan::RunBuilt { .. } | StepPlan::Shell { .. } => None,
            })
            .collect::<Vec<_>>();
        for (index, destination) in archive_destinations.iter().enumerate() {
            let destination = Path::new(destination);
            if archive_destinations.iter().skip(index + 1).any(|other| {
                let other = Path::new(other);
                destination == other || destination.starts_with(other) || other.starts_with(destination)
            }) {
                return Err(DerivationValidationError::OverlappingArchiveDestinations { job: job_index });
            }
        }
        for destination in archive_destinations {
            let destination_path = Path::new(destination);
            for (source, directory) in sources.iter().enumerate().filter_map(|(source, value)| match value {
                LockedSource::Git { directory, .. } => Some((source, directory.as_str())),
                LockedSource::Archive { .. } => None,
            }) {
                let git_path = Path::new(directory);
                if destination_path == git_path
                    || destination_path.starts_with(git_path)
                    || git_path.starts_with(destination_path)
                {
                    return Err(DerivationValidationError::ArchiveDestinationOverlapsGitSource {
                        job: job_index,
                        destination: destination.to_owned(),
                        source_index: source,
                        directory: directory.to_owned(),
                    });
                }
            }
        }
        Ok(())
    }
}

impl StepPlan {
    fn validate(
        &self,
        field: &str,
        build_dir: &Path,
        sources: &[LockedSource],
        build_lock: &BuildLock,
        archive_allowed: bool,
    ) -> Result<(), DerivationValidationError> {
        match self {
            Self::Run {
                program, working_dir, ..
            } => {
                program.validate(&format!("{field}.program"), build_lock)?;
                validate_step_working_dir(field, working_dir, build_dir)
            }
            Self::RunBuilt {
                program, working_dir, ..
            } => {
                validate_step_working_dir(field, working_dir, build_dir)?;
                let working_dir = Path::new(working_dir);
                let program_field = format!("{field}.program");
                let program = validate_normalized_absolute_path(&program_field, program)?;
                require_proper_path_child(&program_field, program, "step working_dir", working_dir)
            }
            Self::Shell {
                interpreter,
                declared_programs,
                script,
                working_dir,
                ..
            } => {
                interpreter.validate(&format!("{field}.interpreter"), build_lock)?;
                for (index, program) in declared_programs.iter().enumerate() {
                    program.validate(&format!("{field}.declared_programs[{index}]"), build_lock)?;
                }
                require_nonempty(&format!("{field}.script"), script)?;
                validate_step_working_dir(field, working_dir, build_dir)
            }
            Self::ExtractArchive {
                source,
                destination,
                strip_components,
            } => {
                if !archive_allowed {
                    return Err(DerivationValidationError::ArchiveStepOutsidePrepare {
                        field: field.to_owned(),
                    });
                }
                let source_index =
                    usize::try_from(*source).map_err(|_| DerivationValidationError::InvalidArchiveStepSource {
                        field: field.to_owned(),
                        source_index: *source,
                    })?;
                if !matches!(sources.get(source_index), Some(LockedSource::Archive { .. })) {
                    return Err(DerivationValidationError::InvalidArchiveStepSource {
                        field: field.to_owned(),
                        source_index: *source,
                    });
                }
                if !is_normalized_relative_path(destination) {
                    return Err(DerivationValidationError::UnsafeArchiveStepDestination {
                        field: field.to_owned(),
                        destination: destination.clone(),
                    });
                }
                if *strip_components > 128 {
                    return Err(DerivationValidationError::ArchiveStripComponentsLimit {
                        field: field.to_owned(),
                        found: *strip_components,
                        limit: 128,
                    });
                }
                Ok(())
            }
        }
    }
}

impl BuilderLayout {
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
}

impl ExecutionPolicy {
    fn validate(&self) -> Result<(), DerivationValidationError> {
        require_nonempty("execution.executor.name", &self.executor.name)?;
        require_nonempty("execution.executor.fingerprint", &self.executor.fingerprint)?;
        if matches!(self.root_materialization, RootMaterializationMode::PackageManagerState) {
            return Err(DerivationValidationError::PackageManagerRootMaterialization);
        }
        if matches!(self.credentials, ExecutionCredentials::Unspecified) {
            return Err(DerivationValidationError::UnspecifiedExecutionCredentials);
        }
        if matches!(self.network, NetworkMode::Enabled) {
            return Err(DerivationValidationError::NetworkEnabled);
        }
        if self.jobs == 0 {
            return Err(DerivationValidationError::ZeroJobs);
        }
        Ok(())
    }
}

impl ExecutablePlan {
    fn validate(&self, field: &str, build_lock: &BuildLock) -> Result<(), DerivationValidationError> {
        let path = validate_normalized_absolute_path(&format!("{field}.path"), &self.path)?;
        self.requirement.validate(&format!("{field}.requirement"))?;
        match self.requirement.kind {
            RelationKind::Binary | RelationKind::SystemBinary => {
                if !is_safe_artifact_component(&self.requirement.name) {
                    return Err(DerivationValidationError::InvalidExecutableRequirement {
                        field: format!("{field}.requirement"),
                        value: self.requirement.name.clone(),
                    });
                }
                let prefix = if self.requirement.kind == RelationKind::Binary {
                    "/usr/bin"
                } else {
                    "/usr/sbin"
                };
                let expected = format!("{prefix}/{}", self.requirement.name);
                if self.path != expected {
                    return Err(DerivationValidationError::ExecutablePathMismatch {
                        field: format!("{field}.path"),
                        expected,
                        found: self.path.clone(),
                    });
                }
            }
            RelationKind::PackageName => {
                if path
                    .parent()
                    .is_some_and(|parent| parent == Path::new("/usr/bin") || parent == Path::new("/usr/sbin"))
                {
                    return Err(DerivationValidationError::AmbiguousPackageExecutable {
                        field: format!("{field}.path"),
                        value: self.path.clone(),
                    });
                }
            }
            _ => {
                return Err(DerivationValidationError::ExecutableRequirementNotRunnable {
                    field: format!("{field}.requirement"),
                    value: self.requirement.canonical_name(),
                });
            }
        }
        let request = self.requirement.canonical_name();
        if !build_lock.requests.iter().any(|locked| locked.request == request) {
            return Err(DerivationValidationError::UnlockedExecutable {
                field: format!("{field}.requirement"),
                request,
            });
        }
        Ok(())
    }
}

impl ExecutableCommandPlan {
    fn validate(&self, field: &str, build_lock: &BuildLock) -> Result<(), DerivationValidationError> {
        self.program.validate(&format!("{field}.program"), build_lock)
    }
}

impl ToolchainCommandsPlan {
    fn validate(&self, build_lock: &BuildLock, compiler_cache: bool) -> Result<(), DerivationValidationError> {
        if self.compilers.len() != Self::COMPILER_ROLES.len() {
            return Err(DerivationValidationError::CompilerCommandCount {
                found: self.compilers.len(),
                expected: Self::COMPILER_ROLES.len(),
            });
        }
        for (index, (compiler, expected)) in self.compilers.iter().zip(Self::COMPILER_ROLES).enumerate() {
            if compiler.role != expected {
                return Err(DerivationValidationError::UnexpectedCompilerCommandRole {
                    index,
                    expected,
                    found: compiler.role,
                });
            }
            compiler
                .command
                .validate(&format!("toolchain_commands.compilers[{index}].command"), build_lock)?;
            require_executable_origin(
                build_lock,
                &compiler.command.program,
                &format!("toolchain_commands.compilers[{index}].command.program.requirement"),
                &InputOrigin::CompilerExecutable { role: expected },
            )?;
        }

        if self.ccache.is_some() != compiler_cache || self.sccache.is_some() != compiler_cache {
            return Err(DerivationValidationError::CompilerCacheCommandMismatch {
                enabled: compiler_cache,
                ccache: self.ccache.is_some(),
                sccache: self.sccache.is_some(),
            });
        }
        if let Some(ccache) = &self.ccache {
            ccache.validate("toolchain_commands.ccache", build_lock)?;
            require_executable_origin(
                build_lock,
                ccache,
                "toolchain_commands.ccache.requirement",
                &InputOrigin::CompilerCache {
                    role: CompilerCacheRole::Ccache,
                },
            )?;
        }
        if let Some(sccache) = &self.sccache {
            sccache.validate("toolchain_commands.sccache", build_lock)?;
            require_executable_origin(
                build_lock,
                sccache,
                "toolchain_commands.sccache.requirement",
                &InputOrigin::CompilerCache {
                    role: CompilerCacheRole::Sccache,
                },
            )?;
        }
        if let Some(mold) = &self.mold {
            mold.validate("toolchain_commands.mold", build_lock)?;
            require_executable_origin(
                build_lock,
                &mold.program,
                "toolchain_commands.mold.program.requirement",
                &InputOrigin::MoldLinker,
            )?;
        }
        Ok(())
    }
}

fn require_executable_origin(
    build_lock: &BuildLock,
    executable: &ExecutablePlan,
    field: &str,
    expected: &InputOrigin,
) -> Result<(), DerivationValidationError> {
    let request = executable.requirement.canonical_name();
    if build_lock
        .requests
        .iter()
        .find(|locked| locked.request == request)
        .is_some_and(|locked| locked.origins.contains(expected))
    {
        Ok(())
    } else {
        Err(DerivationValidationError::MissingExecutableInputOrigin {
            field: field.to_owned(),
            request,
            expected: format!("{expected:?}"),
        })
    }
}

impl AnalysisPlan {
    fn validate(&self, build_lock: &BuildLock) -> Result<(), DerivationValidationError> {
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

        let has_elf = self.handlers.contains(&AnalyzerKind::Elf);
        validate_analyzer_tool(
            "analysis.tools.pkg_config",
            self.handlers.contains(&AnalyzerKind::PkgConfig),
            self.tools.pkg_config.as_ref(),
            build_lock,
        )?;
        validate_analyzer_tool(
            "analysis.tools.python",
            self.handlers.contains(&AnalyzerKind::Python),
            self.tools.python.as_ref(),
            build_lock,
        )?;
        validate_analyzer_tool(
            "analysis.tools.objcopy",
            has_elf && self.debug,
            self.tools.objcopy.as_ref(),
            build_lock,
        )?;
        validate_analyzer_tool(
            "analysis.tools.strip",
            has_elf && self.strip,
            self.tools.strip.as_ref(),
            build_lock,
        )?;

        Ok(())
    }
}

fn validate_analyzer_tool(
    field: &str,
    required: bool,
    tool: Option<&ExecutablePlan>,
    build_lock: &BuildLock,
) -> Result<(), DerivationValidationError> {
    match (required, tool) {
        (true, Some(tool)) => tool.validate(field, build_lock),
        (true, None) => Err(DerivationValidationError::MissingAnalyzerTool {
            field: field.to_owned(),
        }),
        (false, Some(_)) => Err(DerivationValidationError::UnexpectedAnalyzerTool {
            field: field.to_owned(),
        }),
        (false, None) => Ok(()),
    }
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
}

impl OutputPlan {
    fn validate(&self, index: usize) -> Result<(), DerivationValidationError> {
        validate_package_name(&format!("outputs[{index}].name"), &self.name)?;
        validate_package_name(&format!("outputs[{index}].package_name"), &self.package_name)?;
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
}

impl CollectionRulePlan {
    fn validate(&self, index: usize) -> Result<(), DerivationValidationError> {
        require_nonempty(&format!("collection_rules[{index}].output"), &self.output)?;
        let field = format!("collection_rules[{index}].pattern");
        require_nonempty(&field, &self.pattern)?;
        validate_glob(&field, &self.pattern)
    }
}
