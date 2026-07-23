use super::*;

pub(crate) fn executor_fingerprint(cast_version: &str, cast_fingerprint: &str) -> String {
    hash_fields([
        "cast-executor-identity-v1",
        EXECUTOR_ABI,
        cast_version,
        cast_fingerprint,
    ])
}

pub(super) fn selected_builder_identity(
    recipe: &crate::Recipe,
    target: &stone_recipe::build_policy::TargetPolicySpec,
) -> LockedIdentity {
    let builder = recipe.build_target_builder(target);
    let hooks = recipe.build_target_hooks(target);
    let profile = recipe.build_target_profile_key(target);
    LockedIdentity {
        name: structural_builder_name(builder),
        fingerprint: structural_builder_fingerprint(builder, hooks, profile),
    }
}

/// Human-readable structural builder family. This is intentionally only a
/// label: the fingerprint below is the authoritative identity and commits to
/// the complete selected values.
pub(super) fn structural_builder_name(builder: &BuilderSpec) -> String {
    if builder.environment.is_empty() {
        return "custom".to_owned();
    }
    builder
        .environment
        .iter()
        .map(|environment| match environment {
            BuilderEnvironmentSpec::CMake => "cast.builders.cmake.v2",
            BuilderEnvironmentSpec::Meson => "cast.builders.meson.v2",
            BuilderEnvironmentSpec::Cargo => "cast.builders.cargo.v2",
            BuilderEnvironmentSpec::Autotools => "cast.builders.autotools.v2",
        })
        .collect::<Vec<_>>()
        .join("+")
}

pub(super) fn structural_builder_fingerprint(
    builder: &BuilderSpec,
    hooks: &HooksSpec,
    profile: Option<&str>,
) -> String {
    let mut encoder = StructuralBuilderEncoder::new();
    match profile {
        Some(profile) => {
            encoder.variant(1);
            encoder.string(profile);
        }
        None => encoder.variant(0),
    }
    encode_builder(&mut encoder, builder);
    encode_hooks(&mut encoder, hooks);
    encoder.finish()
}

struct StructuralBuilderEncoder(Sha256);

impl StructuralBuilderEncoder {
    fn new() -> Self {
        let mut digest = Sha256::new();
        digest.update(b"cast-structural-builder-v2\0");
        Self(digest)
    }

    fn variant(&mut self, value: u8) {
        self.0.update([value]);
    }

    fn bool(&mut self, value: bool) {
        self.variant(u8::from(value));
    }

    fn string(&mut self, value: &str) {
        self.0.update((value.len() as u64).to_le_bytes());
        self.0.update(value.as_bytes());
    }

    fn strings(&mut self, values: &[String]) {
        self.len(values.len());
        for value in values {
            self.string(value);
        }
    }

    fn len(&mut self, value: usize) {
        self.0.update((value as u64).to_le_bytes());
    }

    fn finish(self) -> String {
        format!("{:x}", self.0.finalize())
    }
}

fn encode_builder(encoder: &mut StructuralBuilderEncoder, builder: &BuilderSpec) {
    encoder.len(builder.required_tools.len());
    for dependency in &builder.required_tools {
        encode_dependency(encoder, dependency);
    }
    encoder.len(builder.environment.len());
    for environment in &builder.environment {
        encoder.variant(match environment {
            BuilderEnvironmentSpec::CMake => 0,
            BuilderEnvironmentSpec::Meson => 1,
            BuilderEnvironmentSpec::Cargo => 2,
            BuilderEnvironmentSpec::Autotools => 3,
        });
    }
    encode_phase(encoder, &builder.phases.setup);
    encode_phase(encoder, &builder.phases.build);
    encode_phase(encoder, &builder.phases.install);
    encode_phase(encoder, &builder.phases.check);
    encode_phase(encoder, &builder.phases.workload);
    encoder.bool(builder.supported_hooks.setup);
    encoder.bool(builder.supported_hooks.build);
    encoder.bool(builder.supported_hooks.check);
    encoder.bool(builder.supported_hooks.install);
    encoder.bool(builder.supported_hooks.workload);
}

fn encode_hooks(encoder: &mut StructuralBuilderEncoder, hooks: &HooksSpec) {
    for steps in [
        &hooks.pre_setup,
        &hooks.post_setup,
        &hooks.pre_build,
        &hooks.post_build,
        &hooks.pre_check,
        &hooks.post_check,
        &hooks.pre_install,
        &hooks.post_install,
        &hooks.pre_workload,
        &hooks.post_workload,
    ] {
        encode_steps(encoder, steps);
    }
}

fn encode_phase(encoder: &mut StructuralBuilderEncoder, phase: &PhaseSpec) {
    encode_steps(encoder, &phase.steps);
}

fn encode_steps(encoder: &mut StructuralBuilderEncoder, steps: &[StepSpec]) {
    encoder.len(steps.len());
    for step in steps {
        match step {
            StepSpec::Run { program, args } => {
                encoder.variant(0);
                encode_program(encoder, program);
                encoder.strings(args);
            }
            StepSpec::RunBuilt { program, args } => {
                encoder.variant(18);
                encoder.string(&program.path);
                encoder.strings(args);
            }
            StepSpec::Shell {
                interpreter,
                declared_programs,
                script,
            } => {
                encoder.variant(1);
                encode_program(encoder, interpreter);
                encoder.len(declared_programs.len());
                for program in declared_programs {
                    encode_program(encoder, program);
                }
                encoder.string(script);
            }
            StepSpec::CMakeConfigure { flags } => {
                encoder.variant(2);
                encoder.strings(flags);
            }
            StepSpec::CMakeBuild => encoder.variant(3),
            StepSpec::CMakeInstall => encoder.variant(4),
            StepSpec::CMakeTest => encoder.variant(5),
            StepSpec::MesonSetup { flags } => {
                encoder.variant(6);
                encoder.strings(flags);
            }
            StepSpec::MesonBuild => encoder.variant(7),
            StepSpec::MesonInstall => encoder.variant(8),
            StepSpec::MesonTest => encoder.variant(9),
            // Variant 10 was the removed CargoFetch escape hatch. Keep later
            // tags stable so removing an impure operation does not change the
            // identity of valid structural builders.
            StepSpec::CargoBuild { features } => {
                encoder.variant(11);
                encoder.strings(features);
            }
            StepSpec::CargoInstall { binaries } => {
                encoder.variant(12);
                encoder.strings(binaries);
            }
            StepSpec::CargoTest { features } => {
                encoder.variant(13);
                encoder.strings(features);
            }
            StepSpec::AutotoolsConfigure { flags } => {
                encoder.variant(14);
                encoder.strings(flags);
            }
            StepSpec::AutotoolsBuild => encoder.variant(15),
            StepSpec::AutotoolsInstall => encoder.variant(16),
            StepSpec::AutotoolsTest => encoder.variant(17),
        }
    }
}

fn encode_program(encoder: &mut StructuralBuilderEncoder, program: &ProgramSpec) {
    encoder.string(&program.path);
    encode_dependency(encoder, &program.requirement);
}

fn encode_dependency(encoder: &mut StructuralBuilderEncoder, dependency: &DependencySpec) {
    match dependency {
        DependencySpec::Package(package) => {
            encoder.variant(0);
            encoder.string(&package.name);
        }
        DependencySpec::Output(output) => {
            encoder.variant(1);
            encoder.string(&output.package.name);
            encoder.string(&output.output);
        }
        DependencySpec::Binary(value) => {
            encoder.variant(2);
            encoder.string(value);
        }
        DependencySpec::SystemBinary(value) => {
            encoder.variant(3);
            encoder.string(value);
        }
        DependencySpec::PkgConfig(value) => {
            encoder.variant(4);
            encoder.string(value);
        }
        DependencySpec::PkgConfig32(value) => {
            encoder.variant(5);
            encoder.string(value);
        }
        DependencySpec::Soname(value) => {
            encoder.variant(6);
            encoder.string(value);
        }
        DependencySpec::CMake(value) => {
            encoder.variant(7);
            encoder.string(value);
        }
        DependencySpec::Python(value) => {
            encoder.variant(8);
            encoder.string(value);
        }
        DependencySpec::Interpreter(value) => {
            encoder.variant(9);
            encoder.string(value);
        }
    }
}

pub(super) fn toolchain_fingerprint(toolchain: &str, policy_fingerprint: &str) -> String {
    hash_fields([toolchain, policy_fingerprint])
}

pub(super) fn target_fingerprint(target: &str, policy_fingerprint: &str) -> String {
    // The composed policy fingerprint binds the complete target value; the
    // exact target name selects one member of that validated catalog.
    hash_fields(["cast-target-selection-v1", policy_fingerprint, target])
}

pub(super) fn platform(policy: &stone_recipe::build_policy::PlatformPolicySpec) -> Platform {
    Platform {
        architecture: policy.architecture.clone(),
        vendor: policy.vendor.clone(),
        operating_system: policy.operating_system.clone(),
        abi: policy.abi.clone(),
    }
}

pub(super) fn hash_fields<'a>(fields: impl IntoIterator<Item = &'a str>) -> String {
    let mut digest = Sha256::new();
    for field in fields {
        digest.update((field.len() as u64).to_le_bytes());
        digest.update(field.as_bytes());
    }
    format!("{:x}", digest.finalize())
}

pub(super) fn aggregate_inputs(inputs: Vec<build::root::UnresolvedInput>) -> Vec<RequestedInput> {
    let mut aggregated = BTreeMap::<String, BTreeSet<InputOrigin>>::new();
    for input in inputs {
        aggregated.entry(input.request).or_default().insert(input.origin);
    }
    aggregated
        .into_iter()
        .map(|(request, origins)| RequestedInput {
            request,
            origins: origins.into_iter().collect(),
        })
        .collect()
}

pub(super) fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
