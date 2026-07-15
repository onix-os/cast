use crate::{
    PathSpec, UpstreamSpec,
    package::{BuilderSpec, DependencySpec, HooksSpec, OutputSpec, PackageSpec, PhaseSpec, ProgramSpec, StepSpec},
};

use super::{PackageConversionError, PackageValidationLimits};

pub(super) struct PackageBudget {
    limits: PackageValidationLimits,
    total_items: usize,
    total_text_bytes: usize,
}

impl PackageBudget {
    pub(super) fn new(limits: PackageValidationLimits) -> Self {
        Self {
            limits,
            total_items: 0,
            total_text_bytes: 0,
        }
    }

    pub(super) fn validate(mut self, package: &PackageSpec) -> Result<(), PackageConversionError> {
        self.metadata_text("meta.pname", &package.meta.pname)?;
        self.metadata_text("meta.version", &package.meta.version)?;
        self.metadata_text("meta.homepage", &package.meta.homepage)?;
        self.collection("meta.license", package.meta.license.len())?;
        for (index, value) in package.meta.license.iter().enumerate() {
            self.metadata_text(&format!("meta.license[{index}]"), value)?;
        }

        self.builder("builder", &package.builder)?;
        self.hooks("hooks", &package.hooks)?;
        self.dependencies("native_build_inputs", &package.native_build_inputs)?;
        self.dependencies("build_inputs", &package.build_inputs)?;
        self.dependencies("check_inputs", &package.check_inputs)?;

        self.collection_with_limit("outputs", package.outputs.len(), self.limits.max_outputs)?;
        for (index, output) in package.outputs.iter().enumerate() {
            self.output(&format!("outputs[{index}]"), output)?;
        }

        self.collection_with_limit("profiles", package.profiles.len(), self.limits.max_profiles)?;
        for (index, profile) in package.profiles.iter().enumerate() {
            let field = format!("profiles[{index}]");
            self.text(&format!("{field}.name"), &profile.name)?;
            self.builder(&format!("{field}.builder"), &profile.builder)?;
            self.hooks(&format!("{field}.hooks"), &profile.hooks)?;
            self.dependencies(&format!("{field}.native_build_inputs"), &profile.native_build_inputs)?;
            self.dependencies(&format!("{field}.build_inputs"), &profile.build_inputs)?;
            self.dependencies(&format!("{field}.check_inputs"), &profile.check_inputs)?;
        }

        self.collection_with_limit("sources", package.sources.len(), self.limits.max_sources)?;
        for (index, source) in package.sources.iter().enumerate() {
            self.source(&format!("sources[{index}]"), source)?;
        }

        self.collection("architectures", package.architectures.len())?;
        for (index, architecture) in package.architectures.iter().enumerate() {
            self.text(&format!("architectures[{index}]"), architecture)?;
        }

        self.collection("tuning", package.tuning.len())?;
        for (index, entry) in package.tuning.iter().enumerate() {
            self.text(&format!("tuning[{index}].key"), &entry.key)?;
            if let crate::TuningSpec::Config { value } = &entry.value {
                self.text(&format!("tuning[{index}].value"), value)?;
            }
        }
        Ok(())
    }

    fn metadata_text(&mut self, field: &str, value: &str) -> Result<(), PackageConversionError> {
        self.text_with_limit(field, value, self.limits.max_metadata_bytes)
    }

    fn text(&mut self, field: &str, value: &str) -> Result<(), PackageConversionError> {
        self.text_with_limit(field, value, self.limits.max_text_bytes)
    }

    fn text_with_limit(&mut self, field: &str, value: &str, limit: usize) -> Result<(), PackageConversionError> {
        let actual = value.len();
        if actual > limit {
            return Err(PackageConversionError::LimitExceeded {
                field: field.to_owned(),
                actual,
                limit,
                unit: "bytes",
            });
        }
        self.total_text_bytes =
            self.total_text_bytes
                .checked_add(actual)
                .ok_or_else(|| PackageConversionError::LimitExceeded {
                    field: field.to_owned(),
                    actual: usize::MAX,
                    limit: self.limits.max_total_text_bytes,
                    unit: "total text bytes",
                })?;
        if self.total_text_bytes > self.limits.max_total_text_bytes {
            return Err(PackageConversionError::LimitExceeded {
                field: field.to_owned(),
                actual: self.total_text_bytes,
                limit: self.limits.max_total_text_bytes,
                unit: "total text bytes",
            });
        }
        Ok(())
    }

    fn collection(&mut self, field: &str, actual: usize) -> Result<(), PackageConversionError> {
        self.collection_with_limit(field, actual, self.limits.max_collection_items)
    }

    fn collection_with_limit(
        &mut self,
        field: &str,
        actual: usize,
        limit: usize,
    ) -> Result<(), PackageConversionError> {
        if actual > limit {
            return Err(PackageConversionError::LimitExceeded {
                field: field.to_owned(),
                actual,
                limit,
                unit: "items",
            });
        }
        self.total_items =
            self.total_items
                .checked_add(actual)
                .ok_or_else(|| PackageConversionError::LimitExceeded {
                    field: field.to_owned(),
                    actual: usize::MAX,
                    limit: self.limits.max_total_items,
                    unit: "total items",
                })?;
        if self.total_items > self.limits.max_total_items {
            return Err(PackageConversionError::LimitExceeded {
                field: field.to_owned(),
                actual: self.total_items,
                limit: self.limits.max_total_items,
                unit: "total items",
            });
        }
        Ok(())
    }

    fn source(&mut self, field: &str, source: &UpstreamSpec) -> Result<(), PackageConversionError> {
        match source {
            UpstreamSpec::Archive {
                url,
                hash,
                rename,
                unpack_dir,
                ..
            } => {
                self.text(&format!("{field}.url"), url)?;
                self.text(&format!("{field}.hash"), hash)?;
                if let Some(value) = rename {
                    self.text(&format!("{field}.rename"), value)?;
                }
                if let Some(value) = unpack_dir {
                    self.text(&format!("{field}.unpack_dir"), value)?;
                }
            }
            UpstreamSpec::Git {
                url,
                git_ref,
                clone_dir,
            } => {
                self.text(&format!("{field}.url"), url)?;
                self.text(&format!("{field}.git_ref"), git_ref)?;
                if let Some(value) = clone_dir {
                    self.text(&format!("{field}.clone_dir"), value)?;
                }
            }
        }
        Ok(())
    }

    fn output(&mut self, field: &str, output: &OutputSpec) -> Result<(), PackageConversionError> {
        self.text(&format!("{field}.name"), &output.name)?;
        if let Some(value) = &output.summary {
            self.text(&format!("{field}.summary"), value)?;
        }
        if let Some(value) = &output.description {
            self.text(&format!("{field}.description"), value)?;
        }
        self.strings(&format!("{field}.provides_exclude"), &output.provides_exclude)?;
        self.dependencies(&format!("{field}.runtime_inputs"), &output.runtime_inputs)?;
        self.strings(&format!("{field}.runtime_exclude"), &output.runtime_exclude)?;
        self.collection(&format!("{field}.paths"), output.paths.len())?;
        for (index, path) in output.paths.iter().enumerate() {
            let value = match path {
                PathSpec::Any { path }
                | PathSpec::Exe { path }
                | PathSpec::Symlink { path }
                | PathSpec::Special { path } => path,
            };
            self.text(&format!("{field}.paths[{index}].path"), value)?;
        }
        self.dependencies(&format!("{field}.conflicts"), &output.conflicts)
    }

    fn builder(&mut self, field: &str, builder: &BuilderSpec) -> Result<(), PackageConversionError> {
        self.dependencies(&format!("{field}.required_tools"), &builder.required_tools)?;
        self.collection(&format!("{field}.environment"), builder.environment.len())?;
        self.phase(&format!("{field}.phases.setup"), &builder.phases.setup)?;
        self.phase(&format!("{field}.phases.build"), &builder.phases.build)?;
        self.phase(&format!("{field}.phases.install"), &builder.phases.install)?;
        self.phase(&format!("{field}.phases.check"), &builder.phases.check)?;
        self.phase(&format!("{field}.phases.workload"), &builder.phases.workload)
    }

    fn hooks(&mut self, field: &str, hooks: &HooksSpec) -> Result<(), PackageConversionError> {
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
            self.steps(&format!("{field}.{name}"), steps)?;
        }
        Ok(())
    }

    fn phase(&mut self, field: &str, phase: &PhaseSpec) -> Result<(), PackageConversionError> {
        self.steps(&format!("{field}.steps"), &phase.steps)
    }

    fn steps(&mut self, field: &str, steps: &[StepSpec]) -> Result<(), PackageConversionError> {
        self.collection(field, steps.len())?;
        for (index, step) in steps.iter().enumerate() {
            self.step(&format!("{field}[{index}]"), step)?;
        }
        Ok(())
    }

    fn step(&mut self, field: &str, step: &StepSpec) -> Result<(), PackageConversionError> {
        match step {
            StepSpec::Run { program, args } => {
                self.program(&format!("{field}.program"), program)?;
                self.strings(&format!("{field}.args"), args)?;
            }
            StepSpec::RunBuilt { program, args } => {
                self.text(&format!("{field}.program.path"), &program.path)?;
                self.strings(&format!("{field}.args"), args)?;
            }
            StepSpec::Shell {
                interpreter,
                declared_programs,
                script,
            } => {
                self.program(&format!("{field}.interpreter"), interpreter)?;
                self.collection(&format!("{field}.declared_programs"), declared_programs.len())?;
                for (index, program) in declared_programs.iter().enumerate() {
                    self.program(&format!("{field}.declared_programs[{index}]"), program)?;
                }
                self.text(&format!("{field}.script"), script)?;
            }
            StepSpec::CMakeConfigure { flags }
            | StepSpec::MesonSetup { flags }
            | StepSpec::AutotoolsConfigure { flags } => self.strings(&format!("{field}.flags"), flags)?,
            StepSpec::CargoBuild { features } | StepSpec::CargoTest { features } => {
                self.strings(&format!("{field}.features"), features)?;
            }
            StepSpec::CargoInstall { binaries } => {
                self.strings(&format!("{field}.binaries"), binaries)?;
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
        Ok(())
    }

    fn strings(&mut self, field: &str, values: &[String]) -> Result<(), PackageConversionError> {
        self.collection(field, values.len())?;
        for (index, value) in values.iter().enumerate() {
            self.text(&format!("{field}[{index}]"), value)?;
        }
        Ok(())
    }

    fn dependencies(&mut self, field: &str, dependencies: &[DependencySpec]) -> Result<(), PackageConversionError> {
        self.collection(field, dependencies.len())?;
        for (index, dependency) in dependencies.iter().enumerate() {
            self.dependency(&format!("{field}[{index}]"), dependency)?;
        }
        Ok(())
    }

    fn dependency(&mut self, field: &str, dependency: &DependencySpec) -> Result<(), PackageConversionError> {
        match dependency {
            DependencySpec::Package(package) => self.text(&format!("{field}.package"), &package.name),
            DependencySpec::Output(output) => {
                self.text(&format!("{field}.package"), &output.package.name)?;
                self.text(&format!("{field}.output"), &output.output)
            }
            DependencySpec::Binary(value)
            | DependencySpec::SystemBinary(value)
            | DependencySpec::PkgConfig(value)
            | DependencySpec::PkgConfig32(value)
            | DependencySpec::Soname(value)
            | DependencySpec::CMake(value)
            | DependencySpec::Python(value)
            | DependencySpec::Interpreter(value) => self.text(field, value),
        }
    }

    fn program(&mut self, field: &str, program: &ProgramSpec) -> Result<(), PackageConversionError> {
        self.text(&format!("{field}.path"), &program.path)?;
        self.dependency(&format!("{field}.requirement"), &program.requirement)
    }
}
