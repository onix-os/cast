//! Language-agnostic structural builder lowering.
//!
//! This is the Rust replacement for the former Gluon `cast.builders.*` modules
//! (`builders/cmake.glu`, `meson.glu`, `cargo.glu`, `autotools.glu`). An authored
//! recipe — in any configuration language — supplies a minimal [`BuilderRequest`]
//! (a builder kind plus its flags/features), and this module lowers it into the
//! fully structural [`BuilderSpec`] the domain and executor consume. Because the
//! lowering lives here rather than in a config-language module, Gluon and Lua are
//! interchangeable authoring syntaxes: neither hosts the builder logic.

use super::{
    BuilderEnvironmentSpec, BuilderSpec, DependencySpec, PhaseSpec, PhasesSpec, StepSpec,
    SupportedHooksSpec,
};

/// A structural builder authoring request, lowered by [`lower_builder`].
///
/// The `Custom` variant preserves the explicit escape hatch: an author that
/// steps outside the standard build systems supplies a complete [`BuilderSpec`]
/// as data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuilderRequest {
    Cmake { flags: Vec<String>, run_tests: bool },
    Meson { flags: Vec<String>, run_tests: bool },
    Cargo {
        features: Vec<String>,
        binaries: Vec<String>,
        run_tests: bool,
    },
    Autotools { flags: Vec<String>, run_tests: bool },
    Custom(Box<BuilderSpec>),
}

fn binary(name: &str) -> DependencySpec {
    DependencySpec::Binary(name.to_owned())
}

fn check_phase(run_tests: bool, step: StepSpec) -> PhaseSpec {
    PhaseSpec::new(run_tests.then_some(step))
}

/// Lower a structural builder request into its complete [`BuilderSpec`].
///
/// Every standard builder produces exactly the tools, environment, and typed
/// step phases its former `.glu` module produced; `check` is populated only when
/// `run_tests` is set, matching the `b.boolean.when config.run_tests` gate.
pub fn lower_builder(request: BuilderRequest) -> BuilderSpec {
    match request {
        BuilderRequest::Cmake { flags, run_tests } => BuilderSpec {
            required_tools: vec![binary("sh"), binary("ninja")],
            environment: vec![BuilderEnvironmentSpec::CMake],
            phases: PhasesSpec {
                setup: PhaseSpec::new([StepSpec::CMakeConfigure { flags }]),
                build: PhaseSpec::new([StepSpec::CMakeBuild]),
                install: PhaseSpec::new([StepSpec::CMakeInstall]),
                check: check_phase(run_tests, StepSpec::CMakeTest),
                workload: PhaseSpec::default(),
            },
            supported_hooks: SupportedHooksSpec::all(),
        },
        BuilderRequest::Meson { flags, run_tests } => BuilderSpec {
            required_tools: vec![
                binary("cmake"),
                binary("sh"),
                binary("ninja"),
                binary("pkgconf"),
            ],
            environment: vec![BuilderEnvironmentSpec::Meson],
            phases: PhasesSpec {
                setup: PhaseSpec::new([StepSpec::MesonSetup { flags }]),
                build: PhaseSpec::new([StepSpec::MesonBuild]),
                install: PhaseSpec::new([StepSpec::MesonInstall]),
                check: check_phase(run_tests, StepSpec::MesonTest),
                workload: PhaseSpec::default(),
            },
            supported_hooks: SupportedHooksSpec::all(),
        },
        BuilderRequest::Cargo {
            features,
            binaries,
            run_tests,
        } => BuilderSpec {
            required_tools: Vec::new(),
            environment: vec![BuilderEnvironmentSpec::Cargo],
            phases: PhasesSpec {
                setup: PhaseSpec::default(),
                build: PhaseSpec::new([StepSpec::CargoBuild {
                    features: features.clone(),
                }]),
                install: PhaseSpec::new([StepSpec::CargoInstall { binaries }]),
                check: check_phase(run_tests, StepSpec::CargoTest { features }),
                workload: PhaseSpec::default(),
            },
            supported_hooks: SupportedHooksSpec::all(),
        },
        BuilderRequest::Autotools { flags, run_tests } => BuilderSpec {
            required_tools: vec![
                binary("autoconf"),
                binary("automake"),
                binary("awk"),
                binary("grep"),
                binary("install"),
                binary("sed"),
            ],
            environment: vec![BuilderEnvironmentSpec::Autotools],
            phases: PhasesSpec {
                setup: PhaseSpec::new([StepSpec::AutotoolsConfigure { flags }]),
                build: PhaseSpec::new([StepSpec::AutotoolsBuild]),
                install: PhaseSpec::new([StepSpec::AutotoolsInstall]),
                check: check_phase(run_tests, StepSpec::AutotoolsTest),
                workload: PhaseSpec::default(),
            },
            supported_hooks: SupportedHooksSpec::all(),
        },
        BuilderRequest::Custom(spec) => *spec,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cmake_lowers_to_the_same_structure_the_gluon_module_produced() {
        let spec = lower_builder(BuilderRequest::Cmake {
            flags: vec!["-DBUILD_SHARED_LIBS=ON".to_owned()],
            run_tests: true,
        });
        assert_eq!(spec.required_tools, vec![binary("sh"), binary("ninja")]);
        assert_eq!(spec.environment, vec![BuilderEnvironmentSpec::CMake]);
        assert_eq!(
            spec.phases.setup.steps,
            vec![StepSpec::CMakeConfigure {
                flags: vec!["-DBUILD_SHARED_LIBS=ON".to_owned()]
            }]
        );
        assert_eq!(spec.phases.build.steps, vec![StepSpec::CMakeBuild]);
        assert_eq!(spec.phases.install.steps, vec![StepSpec::CMakeInstall]);
        assert_eq!(spec.phases.check.steps, vec![StepSpec::CMakeTest]);
        assert!(spec.phases.workload.is_empty());
        assert_eq!(spec.supported_hooks, SupportedHooksSpec::all());
    }

    #[test]
    fn run_tests_false_leaves_the_check_phase_empty() {
        for request in [
            BuilderRequest::Cmake { flags: vec![], run_tests: false },
            BuilderRequest::Meson { flags: vec![], run_tests: false },
            BuilderRequest::Autotools { flags: vec![], run_tests: false },
            BuilderRequest::Cargo {
                features: vec![],
                binaries: vec![],
                run_tests: false,
            },
        ] {
            assert!(lower_builder(request).phases.check.is_empty());
        }
    }

    #[test]
    fn cargo_has_no_setup_and_threads_features_into_build_and_check() {
        let spec = lower_builder(BuilderRequest::Cargo {
            features: vec!["cli".to_owned()],
            binaries: vec!["hello".to_owned()],
            run_tests: true,
        });
        assert!(spec.required_tools.is_empty());
        assert!(spec.phases.setup.is_empty());
        assert_eq!(
            spec.phases.build.steps,
            vec![StepSpec::CargoBuild { features: vec!["cli".to_owned()] }]
        );
        assert_eq!(
            spec.phases.install.steps,
            vec![StepSpec::CargoInstall { binaries: vec!["hello".to_owned()] }]
        );
        assert_eq!(
            spec.phases.check.steps,
            vec![StepSpec::CargoTest { features: vec!["cli".to_owned()] }]
        );
    }

    #[test]
    fn meson_and_autotools_carry_their_exact_tool_closures() {
        let meson = lower_builder(BuilderRequest::Meson { flags: vec![], run_tests: true });
        assert_eq!(
            meson.required_tools,
            vec![binary("cmake"), binary("sh"), binary("ninja"), binary("pkgconf")]
        );
        let autotools = lower_builder(BuilderRequest::Autotools { flags: vec![], run_tests: true });
        assert_eq!(
            autotools.required_tools,
            vec![
                binary("autoconf"),
                binary("automake"),
                binary("awk"),
                binary("grep"),
                binary("install"),
                binary("sed"),
            ]
        );
    }

    #[test]
    fn custom_passes_the_authored_builder_through_unchanged() {
        let custom = BuilderSpec {
            required_tools: vec![binary("sh")],
            environment: Vec::new(),
            phases: PhasesSpec::default(),
            supported_hooks: SupportedHooksSpec::all(),
        };
        assert_eq!(
            lower_builder(BuilderRequest::Custom(Box::new(custom.clone()))),
            custom
        );
    }
}
