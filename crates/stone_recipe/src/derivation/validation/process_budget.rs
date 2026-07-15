use std::{collections::BTreeMap, mem::size_of};

use super::{
    super::{DerivationPlan, ExecutableCommandPlan, ExecutablePlan, PhasePlan, StepPlan, ToolchainCommandsPlan},
    DerivationValidationError,
    field_checks::{reject_embedded_nul, valid_environment_name},
    limits::DerivationValidationLimits,
};

/// Admission accounting for process-facing paths, strings, and collections
/// in a frozen plan.
pub(in crate::derivation) struct ProcessDataBudget {
    limits: DerivationValidationLimits,
    total_steps: usize,
    pub(in crate::derivation) total_items: usize,
    pub(in crate::derivation) total_text_bytes: usize,
}

impl ProcessDataBudget {
    pub(in crate::derivation) fn new(limits: DerivationValidationLimits) -> Self {
        Self {
            limits,
            total_steps: 0,
            total_items: 0,
            total_text_bytes: 0,
        }
    }

    pub(in crate::derivation) fn validate(&mut self, plan: &DerivationPlan) -> Result<(), DerivationValidationError> {
        self.collection_with_limit("jobs", plan.jobs.len(), self.limits.max_jobs)?;
        self.environment("environment", &plan.environment)?;

        for (field, value) in [
            ("layout.guest_root", plan.layout.guest_root.as_str()),
            ("layout.artifacts_dir", plan.layout.artifacts_dir.as_str()),
            ("layout.build_dir", plan.layout.build_dir.as_str()),
            ("layout.source_dir", plan.layout.source_dir.as_str()),
            ("layout.recipe_dir", plan.layout.recipe_dir.as_str()),
            ("layout.install_dir", plan.layout.install_dir.as_str()),
            ("layout.package_dir", plan.layout.package_dir.as_str()),
            ("layout.ccache_dir", plan.layout.ccache_dir.as_str()),
            ("layout.sccache_dir", plan.layout.sccache_dir.as_str()),
            ("layout.go_cache_dir", plan.layout.go_cache_dir.as_str()),
            ("layout.go_mod_cache_dir", plan.layout.go_mod_cache_dir.as_str()),
            ("layout.cargo_cache_dir", plan.layout.cargo_cache_dir.as_str()),
            ("layout.zig_cache_dir", plan.layout.zig_cache_dir.as_str()),
        ] {
            self.path(field, value)?;
        }

        for (field, tool) in [
            ("analysis.tools.pkg_config", plan.analysis.tools.pkg_config.as_ref()),
            ("analysis.tools.python", plan.analysis.tools.python.as_ref()),
            ("analysis.tools.objcopy", plan.analysis.tools.objcopy.as_ref()),
            ("analysis.tools.strip", plan.analysis.tools.strip.as_ref()),
        ] {
            if let Some(tool) = tool {
                self.executable(field, tool)?;
            }
        }

        self.collection_with_limit(
            "toolchain_commands.compilers",
            plan.toolchain_commands.compilers.len(),
            ToolchainCommandsPlan::COMPILER_ROLES.len(),
        )?;
        for (index, compiler) in plan.toolchain_commands.compilers.iter().enumerate() {
            self.executable_command(
                &format!("toolchain_commands.compilers[{index}].command"),
                &compiler.command,
            )?;
        }
        for (field, program) in [
            ("toolchain_commands.ccache", plan.toolchain_commands.ccache.as_ref()),
            ("toolchain_commands.sccache", plan.toolchain_commands.sccache.as_ref()),
        ] {
            if let Some(program) = program {
                self.executable(field, program)?;
            }
        }
        if let Some(mold) = &plan.toolchain_commands.mold {
            self.executable_command("toolchain_commands.mold", mold)?;
        }

        for (job_index, job) in plan.jobs.iter().enumerate() {
            let field = format!("jobs[{job_index}]");
            self.path(&format!("{field}.build_dir"), &job.build_dir)?;
            self.path(&format!("{field}.work_dir"), &job.work_dir)?;
            if let Some(pgo_dir) = &job.pgo_dir {
                self.path(&format!("{field}.pgo_dir"), pgo_dir)?;
            }
            self.collection_with_limit(
                &format!("{field}.phases"),
                job.phases.len(),
                self.limits.max_phases_per_job,
            )?;
            for (phase_index, phase) in job.phases.iter().enumerate() {
                self.phase(&format!("{field}.phases[{phase_index}]"), phase, &plan.environment)?;
            }
        }
        Ok(())
    }

    fn phase(
        &mut self,
        field: &str,
        phase: &PhasePlan,
        global_environment: &BTreeMap<String, String>,
    ) -> Result<(), DerivationValidationError> {
        for (section, steps) in [("pre", &phase.pre), ("steps", &phase.steps), ("post", &phase.post)] {
            let section_field = format!("{field}.{section}");
            self.collection_with_limit(&section_field, steps.len(), self.limits.max_steps_per_section)?;
            self.total_steps =
                self.total_steps
                    .checked_add(steps.len())
                    .ok_or_else(|| DerivationValidationError::LimitExceeded {
                        field: section_field.clone(),
                        actual: usize::MAX,
                        limit: self.limits.max_total_steps,
                        unit: "total steps",
                    })?;
            if self.total_steps > self.limits.max_total_steps {
                return Err(DerivationValidationError::LimitExceeded {
                    field: section_field,
                    actual: self.total_steps,
                    limit: self.limits.max_total_steps,
                    unit: "total steps",
                });
            }
            for (step_index, step) in steps.iter().enumerate() {
                self.step(&format!("{field}.{section}[{step_index}]"), step, global_environment)?;
            }
        }
        Ok(())
    }

    fn step(
        &mut self,
        field: &str,
        step: &StepPlan,
        global_environment: &BTreeMap<String, String>,
    ) -> Result<(), DerivationValidationError> {
        match step {
            StepPlan::Run {
                program,
                args,
                environment,
                working_dir,
            } => {
                self.executable(&format!("{field}.program"), program)?;
                self.collection_with_limit(&format!("{field}.args"), args.len(), self.limits.max_arguments_per_step)?;
                for (index, argument) in args.iter().enumerate() {
                    self.process_string(&format!("{field}.args[{index}]"), argument)?;
                }
                self.environment(&format!("{field}.environment"), environment)?;
                self.path(&format!("{field}.working_dir"), working_dir)?;
                self.validate_effective_environment(field, global_environment, environment)?;
                self.validate_execve(
                    field,
                    &program.path,
                    args.iter().map(String::as_str),
                    global_environment,
                    environment,
                )
            }
            StepPlan::RunBuilt {
                program,
                args,
                environment,
                working_dir,
            } => {
                self.path(&format!("{field}.program"), program)?;
                self.collection_with_limit(&format!("{field}.args"), args.len(), self.limits.max_arguments_per_step)?;
                for (index, argument) in args.iter().enumerate() {
                    self.process_string(&format!("{field}.args[{index}]"), argument)?;
                }
                self.environment(&format!("{field}.environment"), environment)?;
                self.path(&format!("{field}.working_dir"), working_dir)?;
                self.validate_effective_environment(field, global_environment, environment)?;
                self.validate_execve(
                    field,
                    program,
                    args.iter().map(String::as_str),
                    global_environment,
                    environment,
                )
            }
            StepPlan::Shell {
                interpreter,
                declared_programs,
                script,
                environment,
                working_dir,
            } => {
                self.executable(&format!("{field}.interpreter"), interpreter)?;
                self.collection_with_limit(
                    &format!("{field}.declared_programs"),
                    declared_programs.len(),
                    self.limits.max_declared_programs_per_step,
                )?;
                for (index, program) in declared_programs.iter().enumerate() {
                    self.executable(&format!("{field}.declared_programs[{index}]"), program)?;
                }
                self.process_string(&format!("{field}.script"), script)?;
                self.environment(&format!("{field}.environment"), environment)?;
                self.path(&format!("{field}.working_dir"), working_dir)?;
                self.validate_effective_environment(field, global_environment, environment)?;
                self.validate_execve(
                    field,
                    &interpreter.path,
                    ["-c", script.as_str()],
                    global_environment,
                    environment,
                )
            }
            StepPlan::ExtractArchive { destination, .. } => self.path(&format!("{field}.destination"), destination),
        }
    }

    fn executable(&mut self, field: &str, executable: &ExecutablePlan) -> Result<(), DerivationValidationError> {
        let path_field = format!("{field}.path");
        self.path(&path_field, &executable.path)?;
        self.require_limit(
            &path_field,
            executable.path.len(),
            self.limits.max_process_string_bytes,
            "bytes",
        )
    }

    fn executable_command(
        &mut self,
        field: &str,
        command: &ExecutableCommandPlan,
    ) -> Result<(), DerivationValidationError> {
        self.executable(&format!("{field}.program"), &command.program)?;
        self.collection_with_limit(
            &format!("{field}.args"),
            command.args.len(),
            self.limits.max_arguments_per_step,
        )?;
        for (index, argument) in command.args.iter().enumerate() {
            self.process_string(&format!("{field}.args[{index}]"), argument)?;
        }
        Ok(())
    }

    fn environment(
        &mut self,
        field: &str,
        environment: &BTreeMap<String, String>,
    ) -> Result<(), DerivationValidationError> {
        self.collection_with_limit(field, environment.len(), self.limits.max_environment_entries)?;
        for (index, (name, value)) in environment.iter().enumerate() {
            self.environment_name(&format!("{field}[{index}].name"), name)?;
            self.process_string(&format!("{field}[{index}].value"), value)?;
        }
        Ok(())
    }

    fn environment_name(&mut self, field: &str, value: &str) -> Result<(), DerivationValidationError> {
        self.require_limit(field, value.len(), self.limits.max_environment_name_bytes, "bytes")?;
        reject_embedded_nul(field, value)?;
        if !valid_environment_name(value) {
            return Err(DerivationValidationError::InvalidEnvironmentName {
                field: field.to_owned(),
            });
        }
        self.add_text(field, value.len())
    }

    fn process_string(&mut self, field: &str, value: &str) -> Result<(), DerivationValidationError> {
        self.require_limit(field, value.len(), self.limits.max_process_string_bytes, "bytes")?;
        reject_embedded_nul(field, value)?;
        self.add_text(field, value.len())
    }

    fn path(&mut self, field: &str, value: &str) -> Result<(), DerivationValidationError> {
        self.require_limit(field, value.len(), self.limits.max_path_bytes, "path bytes")?;
        reject_embedded_nul(field, value)?;
        self.add_text(field, value.len())
    }

    fn validate_effective_environment(
        &self,
        field: &str,
        global: &BTreeMap<String, String>,
        step: &BTreeMap<String, String>,
    ) -> Result<(), DerivationValidationError> {
        let added = step.keys().filter(|name| !global.contains_key(name.as_str())).count();
        let actual = global.len().saturating_add(added);
        self.require_limit(
            &format!("{field}.effective_environment"),
            actual,
            self.limits.max_environment_entries,
            "items",
        )
    }

    fn validate_execve<'a>(
        &self,
        field: &str,
        program: &str,
        arguments: impl IntoIterator<Item = &'a str>,
        global: &BTreeMap<String, String>,
        step: &BTreeMap<String, String>,
    ) -> Result<(), DerivationValidationError> {
        let mut bytes = program.len().saturating_add(1);
        let mut strings = 1usize;
        for argument in arguments {
            bytes = bytes.saturating_add(argument.len().saturating_add(1));
            strings = strings.saturating_add(1);
        }
        for (name, value) in global
            .iter()
            .filter(|(name, _)| !step.contains_key(name.as_str()))
            .chain(step.iter())
        {
            // NAME=VALUE plus the terminating NUL.
            let entry = name.len().saturating_add(value.len()).saturating_add(2);
            bytes = bytes.saturating_add(entry);
            strings = strings.saturating_add(1);
        }
        // Account for the terminating pointer in each of argv and envp.
        let pointers = strings.saturating_add(2).saturating_mul(size_of::<*const u8>());
        bytes = bytes.saturating_add(pointers);
        self.require_limit(&format!("{field}.execve"), bytes, self.limits.max_execve_bytes, "bytes")
    }

    fn collection_with_limit(
        &mut self,
        field: &str,
        actual: usize,
        limit: usize,
    ) -> Result<(), DerivationValidationError> {
        self.require_limit(field, actual, limit, "items")?;
        self.total_items = self.total_items.saturating_add(actual);
        if self.total_items > self.limits.max_total_process_items {
            return Err(DerivationValidationError::LimitExceeded {
                field: field.to_owned(),
                actual: self.total_items,
                limit: self.limits.max_total_process_items,
                unit: "total process items",
            });
        }
        Ok(())
    }

    fn add_text(&mut self, field: &str, bytes: usize) -> Result<(), DerivationValidationError> {
        self.total_text_bytes = self.total_text_bytes.saturating_add(bytes);
        if self.total_text_bytes > self.limits.max_total_process_text_bytes {
            return Err(DerivationValidationError::LimitExceeded {
                field: field.to_owned(),
                actual: self.total_text_bytes,
                limit: self.limits.max_total_process_text_bytes,
                unit: "total process text bytes",
            });
        }
        Ok(())
    }

    fn require_limit(
        &self,
        field: &str,
        actual: usize,
        limit: usize,
        unit: &'static str,
    ) -> Result<(), DerivationValidationError> {
        if actual > limit {
            Err(DerivationValidationError::LimitExceeded {
                field: field.to_owned(),
                actual,
                limit,
                unit,
            })
        } else {
            Ok(())
        }
    }
}
