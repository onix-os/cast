use stone_recipe::{
    UpstreamSpec,
    derivation::{
        DerivationPlan, InputOrigin, JobExecutableRole, JobStepSection, LockedSource, NetworkMode, OutputRelation,
        PackageInputSelection, StepPlan,
    },
    package::{DependencySpec, PackageSpec, StepSpec},
};

use super::{assert_x86_64_platform, dependency_names};

pub(super) fn assert_semantics(declaration: &PackageSpec, plan: &DerivationPlan) {
    assert_eq!(declaration.meta.pname, "generated-packet-library");
    assert_eq!(declaration.architectures, ["x86_64", "aarch64"]);
    assert!(matches!(
        declaration.native_build_inputs.as_slice(),
        [DependencySpec::Output(output)]
            if output.package.name == "packet-schema-tools" && output.output == "compiler"
    ));
    assert!(matches!(
        declaration.build_inputs.as_slice(),
        [DependencySpec::PkgConfig(library)] if library == "packet-runtime"
    ));
    assert_eq!(
        dependency_names(&declaration.builder.required_tools),
        [
            "binary(mkdir)",
            "binary(cc)",
            "binary(bash)",
            "binary(install)",
            "binary(ln)",
            "binary(test)",
        ]
    );
    assert!(matches!(
        declaration.sources.as_slice(),
        [UpstreamSpec::Archive {
            rename: Some(rename),
            strip_dirs: Some(1),
            unpack: true,
            unpack_dir: Some(directory),
            ..
        }] if rename == "generated-packet-library.tar.xz" && directory == "source"
    ));

    let [StepSpec::Run { program, args }, ..] = declaration.builder.phases.build.steps.as_slice() else {
        panic!("native code-generation example must start with one typed generator invocation");
    };
    assert_eq!(program.path, "/usr/libexec/packet-schema/packet-codegen");
    assert!(matches!(
        &program.requirement,
        DependencySpec::Output(output)
            if output.package.name == "packet-schema-tools" && output.output == "compiler"
    ));
    assert_eq!(
        args.as_slice(),
        [
            "--input",
            "source/schema/packet.idl",
            "--header",
            "build/packet-generated.h",
            "--source",
            "build/packet-generated.c",
        ]
    );
    assert_eq!(
        declaration
            .outputs
            .iter()
            .map(|output| output.name.as_str())
            .collect::<Vec<_>>(),
        ["out", "devel"]
    );

    assert!(matches!(
        plan.sources.as_slice(),
        [LockedSource::Archive {
            order: 0,
            sha256,
            filename,
            ..
        }] if sha256 == "7777777777777777777777777777777777777777777777777777777777777777"
            && filename == "generated-packet-library.tar.xz"
    ));
    let build = plan
        .jobs
        .iter()
        .flat_map(|job| &job.phases)
        .find(|phase| phase.name.eq_ignore_ascii_case("build"))
        .expect("native code-generation example lost its build phase");
    assert!(matches!(
        build.steps.first(),
        Some(StepPlan::Run { program, args, .. })
            if program.path == "/usr/libexec/packet-schema/packet-codegen"
                && program.requirement.canonical_name() == "packet-schema-tools-compiler"
                && args.first().map(String::as_str) == Some("--input")
    ));

    let generator = plan
        .build_lock
        .requests
        .iter()
        .find(|request| request.request == "packet-schema-tools-compiler")
        .expect("native generator output is absent from the frozen closure");
    assert!(matches!(
        generator.origins.as_slice(),
        [
            InputOrigin::NativeBuild {
                selection: PackageInputSelection::Package,
                index: 0,
            },
            InputOrigin::JobExecutable {
                job: 0,
                phase: 2,
                phase_name,
                section: JobStepSection::Steps,
                step: 0,
                role: JobExecutableRole::RunProgram,
            },
        ] if phase_name.eq_ignore_ascii_case("build")
    ));
    let target_library = plan
        .build_lock
        .requests
        .iter()
        .find(|request| request.request == "pkgconfig(packet-runtime)")
        .expect("target library is absent from the frozen closure");
    assert_eq!(
        target_library.origins,
        [InputOrigin::Build {
            selection: PackageInputSelection::Package,
            index: 0,
        }]
    );
    let development = plan.outputs.iter().find(|output| output.name == "devel").unwrap();
    assert!(matches!(
        development.runtime_inputs.as_slice(),
        [OutputRelation::Planned { output }] if output == "out"
    ));
    assert_eq!(plan.execution.network, NetworkMode::Disabled);
    assert_x86_64_platform(plan);
}
