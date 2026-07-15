use std::path::Path;

use gluon_config::{Evaluator, Source, SourceRoot};
use stone_recipe::{
    UpstreamSpec,
    derivation::{
        CollectionRulePlan, DerivationPlan, InputOrigin, JobExecutableRole, JobStepSection, LockedSource, NetworkMode,
        PackageInputSelection, PathRuleKind, StepPlan,
    },
    package::{PackageSpec, StepSpec, evaluate_gluon_with},
};

use super::{assert_x86_64_platform, dependency_names};

pub(super) fn assert_semantics(declaration: &PackageSpec, plan: &DerivationPlan) {
    assert_eq!(declaration.meta.pname, "optional-source-graph");
    assert_eq!(
        dependency_names(&declaration.native_build_inputs),
        ["binary(component-preparer)"]
    );
    assert_eq!(
        declaration
            .outputs
            .iter()
            .map(|output| output.name.as_str())
            .collect::<Vec<_>>(),
        ["out", "components"]
    );
    let [primary, component] = declaration.sources.as_slice() else {
        panic!("enabled component graph must retain two ordered sources");
    };
    assert!(matches!(
        primary,
        UpstreamSpec::Archive {
            rename: Some(rename),
            strip_dirs: Some(1),
            unpack: true,
            unpack_dir: Some(directory),
            ..
        } if rename == "optional-source-graph.tar.xz" && directory == "application"
    ));
    assert!(matches!(
        component,
        UpstreamSpec::Archive {
            rename: Some(rename),
            strip_dirs: Some(1),
            unpack: true,
            unpack_dir: Some(directory),
            ..
        } if rename == "optional-source-graph-components.tar.xz" && directory == "components"
    ));
    assert!(matches!(
        declaration.hooks.pre_setup.as_slice(),
        [StepSpec::Run { program, args }]
            if program.path == "/usr/bin/component-preparer"
                && args.as_slice() == ["--source", "../components"]
    ));

    assert!(matches!(
        plan.sources.as_slice(),
        [
            LockedSource::Archive {
                order: 0,
                sha256,
                filename,
                ..
            },
            LockedSource::Archive {
                order: 1,
                sha256: component_sha256,
                filename: component_filename,
                ..
            },
        ] if sha256 == "1111111111111111111111111111111111111111111111111111111111111111"
            && filename == "optional-source-graph.tar.xz"
            && component_sha256 == "2222222222222222222222222222222222222222222222222222222222222222"
            && component_filename == "optional-source-graph-components.tar.xz"
    ));
    let prepare = plan
        .jobs
        .iter()
        .flat_map(|job| &job.phases)
        .find(|phase| phase.name.eq_ignore_ascii_case("prepare"))
        .expect("enabled source graph lost its prepare phase");
    assert!(matches!(
        prepare.steps.as_slice(),
        [
            StepPlan::ExtractArchive {
                source: 0,
                destination,
                strip_components: 1,
            },
            StepPlan::ExtractArchive {
                source: 1,
                destination: component_destination,
                strip_components: 1,
            },
        ] if destination == "application" && component_destination == "components"
    ));
    assert_eq!(
        plan.collection_rules,
        [
            CollectionRulePlan {
                output: "out".to_owned(),
                kind: PathRuleKind::Any,
                pattern: "*".to_owned(),
            },
            CollectionRulePlan {
                output: "components".to_owned(),
                kind: PathRuleKind::Any,
                pattern: "/usr/share/optional-source-graph/components".to_owned(),
            },
        ]
    );
    let component_request = plan
        .build_lock
        .requests
        .iter()
        .find(|request| request.request == "binary(component-preparer)")
        .expect("enabled component graph lost its native preparation tool");
    assert!(matches!(
        component_request.origins.as_slice(),
        [
            InputOrigin::NativeBuild {
                selection: PackageInputSelection::Package,
                index: 0,
            },
            InputOrigin::JobExecutable {
                job: 0,
                phase: 1,
                phase_name,
                section: JobStepSection::Pre,
                step: 0,
                role: JobExecutableRole::RunProgram,
            },
        ] if phase_name.eq_ignore_ascii_case("setup")
    ));

    assert_disabled_factory_variant();
    assert_eq!(plan.execution.network, NetworkMode::Disabled);
    assert_x86_64_platform(plan);
}

fn assert_disabled_factory_variant() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/examples/gluon/packages/optional-component-source-graph");
    let source_root = SourceRoot::new(&root).expect("open optional component example source root");
    let evaluator = Evaluator::default().with_source_root(source_root);
    let source = Source::new(
        "disabled.glu",
        r#"let b = import! cast.package.v3
let make_package = import! "./package.glu"

make_package { component = b.boolean.false }
"#,
    );
    let disabled = evaluate_gluon_with(&evaluator, &source)
        .expect("evaluate the disabled optional component factory variant")
        .package;

    assert_eq!(disabled.meta.pname, "optional-source-graph");
    assert!(disabled.native_build_inputs.is_empty());
    assert!(disabled.hooks.pre_setup.is_empty());
    assert_eq!(
        disabled
            .outputs
            .iter()
            .map(|output| output.name.as_str())
            .collect::<Vec<_>>(),
        ["out"]
    );
    assert!(
        matches!(disabled.sources.as_slice(), [UpstreamSpec::Archive { rename: Some(rename), .. }]
        if rename == "optional-source-graph.tar.xz")
    );
}
