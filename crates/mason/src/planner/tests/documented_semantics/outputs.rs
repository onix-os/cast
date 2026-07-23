use stone_recipe::{
    PathSpec,
    derivation::{CollectionRulePlan, DerivationPlan, NetworkMode, OutputRelation, PathRuleKind, StepPlan},
    package::{PackageSpec, StepSpec},
};

use super::{assert_x86_64_platform, dependency_names};

pub(super) fn assert_semantics(declaration: &PackageSpec, plan: &DerivationPlan) {
    assert_eq!(declaration.meta.pname, "typed-output-routing");
    assert!(declaration.sources.is_empty());
    assert_eq!(
        dependency_names(&declaration.builder.required_tools),
        ["binary(bash)", "binary(install)", "binary(ln)"]
    );
    assert_eq!(
        declaration
            .outputs
            .iter()
            .map(|output| output.name.as_str())
            .collect::<Vec<_>>(),
        ["out", "commands", "links", "activation"]
    );
    assert!(matches!(
        declaration.outputs[0].paths.as_slice(),
        [PathSpec::Any { path }] if path == "*"
    ));
    assert!(matches!(
        declaration.outputs[1].paths.as_slice(),
        [PathSpec::Exe { path }] if path == "/usr/bin/typed-router"
    ));
    assert!(matches!(
        declaration.outputs[2].paths.as_slice(),
        [PathSpec::Symlink { path }] if path == "/usr/bin/typed-router-current"
    ));
    assert!(matches!(
        declaration.outputs[3].paths.as_slice(),
        [PathSpec::Any { path }] if path == "/usr/lib/tmpfiles.d/typed-router.conf"
    ));

    let [
        StepSpec::Shell {
            interpreter,
            declared_programs,
            script,
        },
    ] = declaration.builder.phases.install.steps.as_slice()
    else {
        panic!("typed output routing must retain one explicit install step");
    };
    assert_eq!(interpreter.path, "/usr/bin/bash");
    assert_eq!(
        declared_programs
            .iter()
            .map(|program| program.path.as_str())
            .collect::<Vec<_>>(),
        ["/usr/bin/install", "/usr/bin/ln"]
    );
    assert!(script.contains("typed-router-current"));
    assert!(script.contains("p /run/typed-router/events.fifo"));
    assert!(script.contains("/tmpfiles.d/typed-router.conf"));

    assert_eq!(
        plan.collection_rules,
        [
            CollectionRulePlan {
                output: "out".to_owned(),
                kind: PathRuleKind::Any,
                pattern: "*".to_owned(),
            },
            CollectionRulePlan {
                output: "commands".to_owned(),
                kind: PathRuleKind::Executable,
                pattern: "/usr/bin/typed-router".to_owned(),
            },
            CollectionRulePlan {
                output: "links".to_owned(),
                kind: PathRuleKind::Symlink,
                pattern: "/usr/bin/typed-router-current".to_owned(),
            },
            CollectionRulePlan {
                output: "activation".to_owned(),
                kind: PathRuleKind::Any,
                pattern: "/usr/lib/tmpfiles.d/typed-router.conf".to_owned(),
            },
        ]
    );
    let install = plan
        .jobs
        .iter()
        .flat_map(|job| &job.phases)
        .find(|phase| phase.name.eq_ignore_ascii_case("install"))
        .expect("typed output routing lost its install phase");
    assert!(matches!(
        install.steps.as_slice(),
        [StepPlan::Shell { declared_programs, script, .. }]
            if declared_programs.iter().map(|program| program.requirement.canonical_name()).collect::<Vec<_>>()
                == ["binary(install)", "binary(ln)"]
                && script.contains("tmpfiles.d")
    ));
    let commands = plan.outputs.iter().find(|output| output.name == "commands").unwrap();
    assert!(matches!(
        commands.runtime_inputs.as_slice(),
        [OutputRelation::Locked { relation, .. }] if relation.canonical_name() == "binary(bash)"
    ));
    let links = plan.outputs.iter().find(|output| output.name == "links").unwrap();
    assert!(matches!(
        links.runtime_inputs.as_slice(),
        [OutputRelation::Planned { output }] if output == "commands"
    ));
    assert_eq!(plan.execution.network, NetworkMode::Disabled);
    assert_x86_64_platform(plan);
}
