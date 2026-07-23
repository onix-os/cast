use stone_recipe::{
    derivation::{CollectionRulePlan, DerivationPlan, NetworkMode, PathRuleKind, StepPlan},
    package::{PackageSpec, StepSpec},
};

use super::{assert_x86_64_platform, dependency_names};

pub(super) fn assert_semantics(declaration: &PackageSpec, plan: &DerivationPlan) {
    assert_eq!(declaration.meta.pname, "userspace-defaults");
    assert!(declaration.sources.is_empty());
    assert_eq!(
        dependency_names(&declaration.builder.required_tools),
        ["binary(bash)", "binary(install)"]
    );

    let [
        StepSpec::Shell {
            interpreter,
            declared_programs,
            script,
        },
    ] = declaration.builder.phases.install.steps.as_slice()
    else {
        panic!("generated configuration must retain one explicit install shell step");
    };
    assert_eq!(interpreter.path, "/usr/bin/bash");
    assert_eq!(
        declared_programs
            .iter()
            .map(|program| program.path.as_str())
            .collect::<Vec<_>>(),
        ["/usr/bin/install"]
    );
    assert!(script.contains("mode = \"declarative\""));
    assert!(script.contains("${CAST_INSTALL_ROOT}${CAST_DATADIR}/userspace/defaults.conf"));

    assert!(plan.sources.is_empty());
    assert!(
        plan.jobs
            .iter()
            .flat_map(|job| &job.phases)
            .flat_map(|phase| phase.pre.iter().chain(&phase.steps).chain(&phase.post))
            .all(|step| !matches!(step, StepPlan::ExtractArchive { .. })),
        "source-less generated data must not gain an extraction step"
    );
    let install = plan
        .jobs
        .iter()
        .flat_map(|job| &job.phases)
        .find(|phase| phase.name.eq_ignore_ascii_case("install"))
        .expect("generated configuration lost its frozen install phase");
    assert!(matches!(
        install.steps.as_slice(),
        [StepPlan::Shell {
            interpreter,
            declared_programs,
            script,
            ..
        }] if interpreter.requirement.canonical_name() == "binary(bash)"
            && declared_programs.len() == 1
            && declared_programs[0].requirement.canonical_name() == "binary(install)"
            && script.contains("userspace-defaults.conf")
    ));
    assert_eq!(
        plan.collection_rules,
        [CollectionRulePlan {
            output: "out".to_owned(),
            kind: PathRuleKind::Any,
            pattern: "/usr/share/userspace/defaults.conf".to_owned(),
        }]
    );
    assert_eq!(plan.execution.network, NetworkMode::Disabled);
    assert_x86_64_platform(plan);
}
