use stone_recipe::{
    derivation::{
        DerivationPlan, InputOrigin, JobExecutableRole, JobStepSection, NetworkMode, PackageInputSelection, StepPlan,
    },
    package::PackageSpec,
};

use super::{assert_locked_request_origin, assert_x86_64_platform, dependency_names};

pub(super) fn assert_semantics(declaration: &PackageSpec, plan: &DerivationPlan) {
    assert_eq!(declaration.meta.pname, "target-profile-specialization");
    assert_eq!(
        dependency_names(&declaration.builder.required_tools),
        ["binary(base-builder-tool)"]
    );
    assert_eq!(
        dependency_names(&declaration.native_build_inputs),
        ["binary(base-native-tool)"]
    );
    assert_eq!(
        dependency_names(&declaration.build_inputs),
        ["pkgconfig(base-target-library)"]
    );
    assert_eq!(dependency_names(&declaration.check_inputs), ["binary(base-check-tool)"]);

    let [profile] = declaration.profiles.as_slice() else {
        panic!("target specialization must declare exactly one profile");
    };
    assert_eq!(profile.name, "x86_64");
    assert_eq!(
        dependency_names(&profile.builder.required_tools),
        ["binary(profile-builder-tool)"]
    );
    assert_eq!(
        dependency_names(&profile.native_build_inputs),
        ["binary(profile-native-tool)"]
    );
    assert_eq!(
        dependency_names(&profile.build_inputs),
        ["pkgconfig(profile-target-library)"]
    );
    assert_eq!(dependency_names(&profile.check_inputs), ["binary(profile-check-tool)"]);

    let selected = PackageInputSelection::Profile {
        name: "x86_64".to_owned(),
    };
    for (request, origin) in [
        (
            "binary(profile-builder-tool)",
            InputOrigin::BuilderTool {
                selection: selected.clone(),
                index: 0,
            },
        ),
        (
            "binary(profile-native-tool)",
            InputOrigin::NativeBuild {
                selection: selected.clone(),
                index: 0,
            },
        ),
        (
            "pkgconfig(profile-target-library)",
            InputOrigin::Build {
                selection: selected.clone(),
                index: 0,
            },
        ),
        (
            "binary(profile-check-tool)",
            InputOrigin::Check {
                selection: selected,
                index: 0,
            },
        ),
    ] {
        assert_locked_request_origin(plan, request, origin);
    }
    assert!(
        plan.build_lock
            .requests
            .iter()
            .all(|request| !request.request.contains("base-")),
        "base builder, hooks, and inputs must not leak through an exact target profile"
    );

    let build = plan
        .jobs
        .iter()
        .flat_map(|job| &job.phases)
        .find(|phase| phase.name.eq_ignore_ascii_case("build"))
        .expect("selected target profile lost its build phase");
    assert!(matches!(
        build.pre.as_slice(),
        [StepPlan::Run { program, args, .. }]
            if program.requirement.canonical_name() == "binary(profile-hook-runner)"
                && args.as_slice() == ["--target", "x86_64"]
    ));
    assert!(matches!(
        build.steps.as_slice(),
        [StepPlan::Run { program, args, .. }]
            if program.requirement.canonical_name() == "binary(profile-phase-runner)"
                && args.as_slice() == ["--target", "x86_64"]
    ));

    for (request, section) in [
        ("binary(profile-hook-runner)", JobStepSection::Pre),
        ("binary(profile-phase-runner)", JobStepSection::Steps),
    ] {
        let locked = plan
            .build_lock
            .requests
            .iter()
            .find(|locked| locked.request == request)
            .unwrap_or_else(|| panic!("missing selected profile executable {request}"));
        assert!(matches!(
            locked.origins.as_slice(),
            [InputOrigin::JobExecutable {
                job: 0,
                phase: 0,
                phase_name,
                section: frozen_section,
                step: 0,
                role: JobExecutableRole::RunProgram,
            }] if phase_name.eq_ignore_ascii_case("build") && *frozen_section == section
        ));
    }
    assert_eq!(plan.execution.network, NetworkMode::Disabled);
    assert_x86_64_platform(plan);
}
