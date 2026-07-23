use stone_recipe::package::{
    BuilderSpec, HooksSpec, MetaSpec, OutputSpec, PackageRef as ExpectedPackageRef, PackageSpec,
    PhaseSpec, PhasesSpec,
};
use stone_recipe::{OptionsSpec, ToolchainSpec};

#[test]
fn normalized_package_root_matches_the_complete_owned_value() {
    let evaluated = evaluate_gluon(&authored(
        r#"
{
    meta = b.meta {
        pname = "phase-zero",
        version = "1.2.3",
        release = 4,
        homepage = "https://example.invalid/phase-zero",
        license = ["MPL-2.0"],
    },
    builder = b.defaults.builder,
    hooks = b.defaults.hooks,
    native_build_inputs = [b.dep.binary "ninja"],
    build_inputs = [b.dep.package "zlib"],
    check_inputs = [],
    outputs = [b.output "out"],
    options = b.options {
        toolchain = b.toolchain.gnu,
        cspgo = b.boolean.false,
        samplepgo = b.boolean.false,
        debug = b.boolean.true,
        strip = b.boolean.false,
        networking = b.boolean.false,
        compressman = b.boolean.true,
        lastrip = b.boolean.false,
    },
    profiles = [],
    sources = [],
    architectures = ["x86_64"],
    tuning = [],
    emul32 = b.boolean.false,
    mold = b.boolean.true,
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

    assert_eq!(evaluated.package, expected);
}
