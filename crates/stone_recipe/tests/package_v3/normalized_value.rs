use stone_recipe::package::{
    BuilderSpec, HooksSpec, MetaSpec, OutputSpec, PackageRef as ExpectedPackageRef, PackageSpec,
    PhaseSpec, PhasesSpec,
};
use stone_recipe::{OptionsSpec, ToolchainSpec};

#[test]
fn normalized_package_root_matches_the_complete_owned_value() {
    let evaluated = evaluate_default_package(&authored(
        r#"
{
    meta = {
        pname = "phase-zero",
        version = "1.2.3",
        release = 4,
        homepage = "https://example.invalid/phase-zero",
        license = ["MPL-2.0"],
    },
    builder = a.builder.custom a.empty.builder,
    sources = [],
    native_build_inputs = [a.dep.binary "ninja"],
    build_inputs = [a.dep.package "zlib"],
    check_inputs = [],
    outputs = a.outputs.explicit [a.output "out"],
    options = a.some.options (a.options {
        toolchain = a.toolchain.gnu,
        cspgo = a.false,
        samplepgo = a.false,
        debug = a.true,
        strip = a.false,
        networking = a.false,
        compressman = a.true,
        lastrip = a.false,
    }),
    profiles = [],
    architectures = ["x86_64"],
    tuning = [],
    emul32 = a.false,
    mold = a.true,
    hooks = a.some.hooks a.empty.hooks,
}
"#,
    ))
    .unwrap();

    let expected = PackageSpec {
        meta: MetaSpec {
            pname: "phase-zero".to_owned(),
            version: "1.2.3".to_owned(),
            release: 4,
            homepage: "https://example.invalid/phase-zero".to_owned(),
            license: vec!["MPL-2.0".to_owned()],
        },
        builder: BuilderSpec {
            required_tools: Vec::new(),
            environment: Vec::new(),
            phases: PhasesSpec {
                setup: PhaseSpec { steps: Vec::new() },
                build: PhaseSpec { steps: Vec::new() },
                install: PhaseSpec { steps: Vec::new() },
                check: PhaseSpec { steps: Vec::new() },
                workload: PhaseSpec { steps: Vec::new() },
            },
            supported_hooks: SupportedHooksSpec {
                setup: true,
                build: true,
                check: true,
                install: true,
                workload: true,
            },
        },
        hooks: HooksSpec {
            pre_setup: Vec::new(),
            post_setup: Vec::new(),
            pre_build: Vec::new(),
            post_build: Vec::new(),
            pre_check: Vec::new(),
            post_check: Vec::new(),
            pre_install: Vec::new(),
            post_install: Vec::new(),
            pre_workload: Vec::new(),
            post_workload: Vec::new(),
        },
        native_build_inputs: vec![DependencySpec::Binary("ninja".to_owned())],
        build_inputs: vec![DependencySpec::Package(ExpectedPackageRef {
            name: "zlib".to_owned(),
        })],
        check_inputs: Vec::new(),
        outputs: vec![OutputSpec {
            name: "out".to_owned(),
            include_in_manifest: true,
            summary: None,
            description: None,
            provides_exclude: Vec::new(),
            runtime_inputs: Vec::new(),
            runtime_exclude: Vec::new(),
            paths: Vec::new(),
            conflicts: Vec::new(),
        }],
        options: OptionsSpec {
            toolchain: ToolchainSpec::Gnu,
            cspgo: false,
            samplepgo: false,
            debug: true,
            strip: false,
            networking: false,
            compressman: true,
            lastrip: false,
        },
        profiles: Vec::new(),
        sources: Vec::new(),
        architectures: vec!["x86_64".to_owned()],
        tuning: Vec::new(),
        emul32: false,
        mold: true,
    };

    assert_eq!(evaluated.value, expected);
}
