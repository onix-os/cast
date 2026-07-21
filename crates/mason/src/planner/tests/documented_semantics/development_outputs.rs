use stone_recipe::{
    derivation::{DerivationPlan, InputOrigin, NetworkMode, OutputRelation},
    package::{DependencySpec, PackageSpec},
};

use super::assert_x86_64_platform;

const PACKAGE: &str = "development-interface-closure";
const EXTERNAL_PACKAGE: &str = "interface-annotations";
const EXTERNAL_REQUEST: &str = "interface-annotations-devel";

pub(super) fn assert_semantics(declaration: &PackageSpec, plan: &DerivationPlan) {
    assert_eq!(declaration.meta.pname, PACKAGE);
    assert!(declaration.sources.is_empty());
    assert!(declaration.builder.required_tools.is_empty());
    assert!(declaration.native_build_inputs.is_empty());
    assert!(declaration.build_inputs.is_empty());
    assert!(declaration.check_inputs.is_empty());
    assert_eq!(
        declaration
            .outputs
            .iter()
            .map(|output| output.name.as_str())
            .collect::<Vec<_>>(),
        ["out", "libs", "devel"]
    );

    let [root, libraries, development] = declaration.outputs.as_slice() else {
        panic!("development interface example must publish exactly out, libs, and devel");
    };
    assert!(root.runtime_inputs.is_empty());
    assert!(libraries.runtime_inputs.is_empty());
    assert!(matches!(
        development.runtime_inputs.as_slice(),
        [DependencySpec::Output(local), DependencySpec::Output(external)]
            if local.package.name == PACKAGE
                && local.output == "libs"
                && external.package.name == EXTERNAL_PACKAGE
                && external.output == "devel"
    ));
    assert_eq!(
        declaration
            .outputs
            .iter()
            .flat_map(|output| &output.runtime_inputs)
            .filter(|dependency| {
                matches!(
                    dependency,
                    DependencySpec::Output(output)
                        if output.package.name == EXTERNAL_PACKAGE && output.output == "devel"
                )
            })
            .count(),
        1,
        "the external development interface must belong only to devel"
    );

    let frozen_root = plan.outputs.iter().find(|output| output.name == "out").unwrap();
    let frozen_libraries = plan.outputs.iter().find(|output| output.name == "libs").unwrap();
    let frozen_development = plan.outputs.iter().find(|output| output.name == "devel").unwrap();
    assert!(frozen_root.runtime_inputs.is_empty());
    assert!(frozen_libraries.runtime_inputs.is_empty());
    assert!(matches!(
        frozen_development.runtime_inputs.as_slice(),
        [
            OutputRelation::Planned { output },
            OutputRelation::Locked { relation, .. },
        ] if output == "libs" && relation.canonical_name() == EXTERNAL_REQUEST
    ));

    let external = plan
        .build_lock
        .requests
        .iter()
        .find(|request| request.request == EXTERNAL_REQUEST)
        .expect("external named development output is absent from the frozen closure");
    assert_eq!(
        external.origins,
        [InputOrigin::OutputRuntime {
            output: "devel".to_owned(),
            index: 1,
        }]
    );
    assert_eq!(
        plan.build_lock
            .requests
            .iter()
            .filter(|request| request.request == EXTERNAL_REQUEST)
            .count(),
        1,
        "the external development output must freeze as one exact request"
    );
    assert!(
        plan.manifest_build_inputs
            .iter()
            .all(|relation| relation.canonical_name() != EXTERNAL_REQUEST),
        "an output-local development dependency must not become a package build input"
    );
    assert_eq!(plan.execution.network, NetworkMode::Disabled);
    assert_x86_64_platform(plan);
}
