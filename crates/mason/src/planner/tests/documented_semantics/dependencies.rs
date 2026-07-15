use stone_recipe::{
    derivation::{
        DerivationPlan, InputOrigin, JobExecutableRole, JobStepSection, NetworkMode, OutputRelation,
        PackageInputSelection,
    },
    package::PackageSpec,
};

use super::{assert_x86_64_platform, dependency_names};

pub(super) fn assert_semantics(declaration: &PackageSpec, plan: &DerivationPlan) {
    assert_eq!(declaration.meta.pname, "shared-capability-origins");
    for dependencies in [
        declaration.builder.required_tools.as_slice(),
        declaration.native_build_inputs.as_slice(),
        declaration.build_inputs.as_slice(),
        declaration.check_inputs.as_slice(),
    ] {
        assert_eq!(dependency_names(dependencies), ["binary(bash)"]);
    }
    let [output] = declaration.outputs.as_slice() else {
        panic!("shared capability example must publish one output");
    };
    assert_eq!(dependency_names(&output.runtime_inputs), ["binary(bash)"]);

    let locked = plan
        .build_lock
        .requests
        .iter()
        .find(|request| request.request == "binary(bash)")
        .expect("shared Bash capability is absent from the frozen closure");
    assert_eq!(locked.origins.len(), 6);
    assert_eq!(
        &locked.origins[..5],
        [
            InputOrigin::BuilderTool {
                selection: PackageInputSelection::Package,
                index: 0,
            },
            InputOrigin::NativeBuild {
                selection: PackageInputSelection::Package,
                index: 0,
            },
            InputOrigin::Build {
                selection: PackageInputSelection::Package,
                index: 0,
            },
            InputOrigin::Check {
                selection: PackageInputSelection::Package,
                index: 0,
            },
            InputOrigin::OutputRuntime {
                output: "out".to_owned(),
                index: 0,
            },
        ]
    );
    assert!(matches!(
        &locked.origins[5],
        InputOrigin::JobExecutable {
            job: 0,
            phase: 0,
            phase_name,
            section: JobStepSection::Steps,
            step: 0,
            role: JobExecutableRole::ShellInterpreter,
        } if phase_name.eq_ignore_ascii_case("build")
    ));
    assert_eq!(
        plan.manifest_build_inputs
            .iter()
            .filter(|relation| relation.canonical_name() == "binary(bash)")
            .count(),
        4,
        "the four package-level roles remain explicit manifest relations"
    );
    let frozen_output = plan.outputs.iter().find(|output| output.name == "out").unwrap();
    assert!(matches!(
        frozen_output.runtime_inputs.as_slice(),
        [OutputRelation::Locked { relation, .. }] if relation.canonical_name() == "binary(bash)"
    ));
    assert_eq!(plan.execution.network, NetworkMode::Disabled);
    assert_x86_64_platform(plan);
}
