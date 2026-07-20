use std::{collections::BTreeMap, path::Path};

use crate::{
    package::{
        BuilderSpec, DependencyKind, DependencyRole, DependencySpec, HooksSpec, PackageSpec, ProgramSpec, StepSpec,
    },
    spec::{is_normalized_relative_path, is_safe_artifact_component},
};

use super::{
    PackageConversionError,
    cycles::validate_output_cycles,
    field_checks::{valid_output_name, valid_program_path, validate_unique_step_values},
};

impl PackageSpec {
    pub(super) fn validate_relations(&self) -> Result<(), PackageConversionError> {
        let mut outputs = BTreeMap::new();
        for (index, output) in self.outputs.iter().enumerate() {
            if !valid_output_name(&output.name) {
                return Err(PackageConversionError::InvalidOutputName {
                    index,
                    name: output.name.clone(),
                });
            }
            if outputs.insert(output.name.as_str(), index).is_some() {
                return Err(PackageConversionError::DuplicateOutput {
                    index,
                    name: output.name.clone(),
                });
            }
        }
        if !outputs.contains_key("out") {
            return Err(PackageConversionError::MissingRootOutput);
        }

        self.validate_dependency_list(
            &self.native_build_inputs,
            "native_build_inputs",
            &outputs,
            DependencyRole::NativeBuild,
        )?;
        self.validate_dependency_list(&self.build_inputs, "build_inputs", &outputs, DependencyRole::Build)?;
        self.validate_dependency_list(&self.check_inputs, "check_inputs", &outputs, DependencyRole::Check)?;
        self.validate_dependency_list(
            &self.builder.required_tools,
            "builder.required_tools",
            &outputs,
            DependencyRole::BuilderTool,
        )?;
        self.validate_builder_programs(&self.builder, &self.hooks, "builder", "hooks", &outputs)?;

        for (index, profile) in self.profiles.iter().enumerate() {
            let parent = format!("profiles[{index}]");
            self.validate_dependency_list(
                &profile.builder.required_tools,
                &format!("{parent}.builder.required_tools"),
                &outputs,
                DependencyRole::BuilderTool,
            )?;
            self.validate_dependency_list(
                &profile.native_build_inputs,
                &format!("{parent}.native_build_inputs"),
                &outputs,
                DependencyRole::NativeBuild,
            )?;
            self.validate_dependency_list(
                &profile.build_inputs,
                &format!("{parent}.build_inputs"),
                &outputs,
                DependencyRole::Build,
            )?;
            self.validate_dependency_list(
                &profile.check_inputs,
                &format!("{parent}.check_inputs"),
                &outputs,
                DependencyRole::Check,
            )?;
            self.validate_builder_programs(
                &profile.builder,
                &profile.hooks,
                &format!("{parent}.builder"),
                &format!("{parent}.hooks"),
                &outputs,
            )?;
        }

        for (index, output) in self.outputs.iter().enumerate() {
            self.validate_dependency_list(
                &output.runtime_inputs,
                &format!("outputs[{index}].runtime_inputs"),
                &outputs,
                DependencyRole::Runtime,
            )?;
            self.validate_dependency_list(
                &output.conflicts,
                &format!("outputs[{index}].conflicts"),
                &outputs,
                DependencyRole::Conflict,
            )?;
        }

        validate_output_cycles(self, &outputs)
    }

    fn validate_dependency_list(
        &self,
        dependencies: &[DependencySpec],
        field: &str,
        outputs: &BTreeMap<&str, usize>,
        role: DependencyRole,
    ) -> Result<(), PackageConversionError> {
        let mut seen = BTreeMap::new();
        for (index, dependency) in dependencies.iter().enumerate() {
            let dependency_field = format!("{field}[{index}]");
            let identity = self.validate_dependency(dependency, &dependency_field, outputs, role.is_provider())?;
            let kind = DependencyKind::of(dependency);
            if !role.accepts(kind) {
                return Err(PackageConversionError::UnsupportedDependencyRole {
                    field: dependency_field,
                    role,
                    kind,
                });
            }
            if let Some(first_index) = seen.insert(identity.clone(), index) {
                return Err(PackageConversionError::DuplicateValue {
                    field: dependency_field,
                    value: identity,
                    first_field: format!("{field}[{first_index}]"),
                });
            }
        }
        Ok(())
    }

    fn validate_dependency(
        &self,
        dependency: &DependencySpec,
        field: &str,
        outputs: &BTreeMap<&str, usize>,
        provider: bool,
    ) -> Result<String, PackageConversionError> {
        let parsed = if provider {
            dependency.provider().map(|relation| relation.to_name())
        } else {
            dependency.dependency().map(|relation| relation.to_name())
        };
        let parsed = parsed.map_err(|source| {
            if provider {
                PackageConversionError::InvalidProvider {
                    field: field.to_owned(),
                    source,
                }
            } else {
                PackageConversionError::InvalidDependency {
                    field: field.to_owned(),
                    source,
                }
            }
        })?;
        if let Some((package, output)) = dependency.package_and_output()
            && package == self.meta.pname
            && !outputs.contains_key(output)
        {
            return Err(PackageConversionError::MissingOutputReference {
                field: field.to_owned(),
                package: package.to_owned(),
                output: output.to_owned(),
            });
        }
        Ok(parsed)
    }

    fn validate_builder_programs(
        &self,
        builder: &BuilderSpec,
        hooks: &HooksSpec,
        builder_field: &str,
        hooks_field: &str,
        outputs: &BTreeMap<&str, usize>,
    ) -> Result<(), PackageConversionError> {
        for (name, phase) in [
            ("setup", &builder.phases.setup),
            ("build", &builder.phases.build),
            ("install", &builder.phases.install),
            ("check", &builder.phases.check),
            ("workload", &builder.phases.workload),
        ] {
            self.validate_steps(&phase.steps, &format!("{builder_field}.phases.{name}.steps"), outputs)?;
        }

        for (name, steps) in [
            ("pre_setup", hooks.pre_setup.as_slice()),
            ("post_setup", hooks.post_setup.as_slice()),
            ("pre_build", hooks.pre_build.as_slice()),
            ("post_build", hooks.post_build.as_slice()),
            ("pre_check", hooks.pre_check.as_slice()),
            ("post_check", hooks.post_check.as_slice()),
            ("pre_install", hooks.pre_install.as_slice()),
            ("post_install", hooks.post_install.as_slice()),
            ("pre_workload", hooks.pre_workload.as_slice()),
            ("post_workload", hooks.post_workload.as_slice()),
        ] {
            self.validate_steps(steps, &format!("{hooks_field}.{name}"), outputs)?;
        }

        Ok(())
    }

    fn validate_steps(
        &self,
        steps: &[StepSpec],
        field: &str,
        outputs: &BTreeMap<&str, usize>,
    ) -> Result<(), PackageConversionError> {
        for (index, step) in steps.iter().enumerate() {
            let field = format!("{field}[{index}]");
            match step {
                StepSpec::Run { program, args } => {
                    self.validate_program(program, &format!("{field}.program"), outputs)?;
                    for (argument_index, argument) in args.iter().enumerate() {
                        if argument.contains('\0') {
                            return Err(PackageConversionError::InvalidText {
                                field: format!("{field}.args[{argument_index}]"),
                                value: argument.clone(),
                                requirement: "must not contain NUL characters",
                            });
                        }
                    }
                }
                StepSpec::RunBuilt { program, args } => {
                    if !is_normalized_relative_path(&program.path) {
                        return Err(PackageConversionError::InvalidText {
                            field: format!("{field}.program.path"),
                            value: program.path.clone(),
                            requirement: "must be a normalized relative path below the build working directory",
                        });
                    }
                    for (argument_index, argument) in args.iter().enumerate() {
                        if argument.contains('\0') {
                            return Err(PackageConversionError::InvalidText {
                                field: format!("{field}.args[{argument_index}]"),
                                value: argument.clone(),
                                requirement: "must not contain NUL characters",
                            });
                        }
                    }
                }
                StepSpec::Shell {
                    interpreter,
                    declared_programs,
                    script,
                } => {
                    self.validate_program(interpreter, &format!("{field}.interpreter"), outputs)?;
                    if script.trim().is_empty() || script.contains('\0') {
                        return Err(PackageConversionError::InvalidText {
                            field: format!("{field}.script"),
                            value: script.clone(),
                            requirement: "must be non-empty and contain no NUL characters",
                        });
                    }
                    let mut paths = BTreeMap::new();
                    paths.insert(interpreter.path.as_str(), format!("{field}.interpreter.path"));
                    for (program_index, program) in declared_programs.iter().enumerate() {
                        let program_field = format!("{field}.declared_programs[{program_index}]");
                        self.validate_program(program, &program_field, outputs)?;
                        if let Some(first_field) = paths.insert(program.path.as_str(), format!("{program_field}.path"))
                        {
                            return Err(PackageConversionError::DuplicateValue {
                                field: format!("{program_field}.path"),
                                value: program.path.clone(),
                                first_field,
                            });
                        }
                    }
                }
                StepSpec::CMakeConfigure { flags }
                | StepSpec::MesonSetup { flags }
                | StepSpec::AutotoolsConfigure { flags } => {
                    validate_unique_step_values(
                        flags,
                        &format!("{field}.flags"),
                        "must be non-empty, trimmed, and contain no control characters",
                        false,
                    )?;
                }
                StepSpec::CargoBuild { features } | StepSpec::CargoTest { features } => {
                    validate_unique_step_values(
                        features,
                        &format!("{field}.features"),
                        "must be non-empty, trimmed, and contain no control characters",
                        false,
                    )?;
                }
                StepSpec::CargoInstall { binaries } => {
                    validate_unique_step_values(
                        binaries,
                        &format!("{field}.binaries"),
                        "must be one normalized binary name",
                        true,
                    )?;
                }
                StepSpec::CMakeBuild
                | StepSpec::CMakeInstall
                | StepSpec::CMakeTest
                | StepSpec::MesonBuild
                | StepSpec::MesonInstall
                | StepSpec::MesonTest
                | StepSpec::AutotoolsBuild
                | StepSpec::AutotoolsInstall
                | StepSpec::AutotoolsTest => {}
            }
        }
        Ok(())
    }

    fn validate_program(
        &self,
        program: &ProgramSpec,
        field: &str,
        outputs: &BTreeMap<&str, usize>,
    ) -> Result<(), PackageConversionError> {
        let path_field = format!("{field}.path");
        if !valid_program_path(&program.path) {
            return Err(PackageConversionError::InvalidProgramPath {
                field: path_field,
                value: program.path.clone(),
            });
        }

        let requirement_field = format!("{field}.requirement");
        self.validate_dependency(&program.requirement, &requirement_field, outputs, false)?;

        let expected = match &program.requirement {
            DependencySpec::Package(_) | DependencySpec::Output(_) => {
                if Path::new(&program.path)
                    .parent()
                    .is_some_and(|parent| parent == Path::new("/usr/bin") || parent == Path::new("/usr/sbin"))
                {
                    return Err(PackageConversionError::AmbiguousPackageProgramPath {
                        field: path_field,
                        value: program.path.clone(),
                    });
                }
                return Ok(());
            }
            DependencySpec::Binary(target) => {
                if !is_safe_artifact_component(target) {
                    return Err(PackageConversionError::InvalidProgramRequirement {
                        field: requirement_field,
                        requirement: program.requirement.clone(),
                    });
                }
                format!("/usr/bin/{target}")
            }
            DependencySpec::SystemBinary(target) => {
                if !is_safe_artifact_component(target) {
                    return Err(PackageConversionError::InvalidProgramRequirement {
                        field: requirement_field,
                        requirement: program.requirement.clone(),
                    });
                }
                format!("/usr/sbin/{target}")
            }
            requirement => {
                return Err(PackageConversionError::UnsupportedProgramRequirement {
                    field: requirement_field,
                    requirement: requirement.clone(),
                });
            }
        };
        if program.path != expected {
            return Err(PackageConversionError::ProgramRequirementPathMismatch {
                field: path_field,
                requirement: program.requirement.clone(),
                expected,
                actual: program.path.clone(),
            });
        }

        Ok(())
    }
}
