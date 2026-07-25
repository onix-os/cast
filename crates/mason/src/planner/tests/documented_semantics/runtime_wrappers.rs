use stone_recipe::{
    PathSpec,
    derivation::{
        CollectionRulePlan, DerivationPlan, NetworkMode, OutputRelation, PathRuleKind, StepPlan,
    },
    package::{DependencySpec, PackageSpec, StepSpec},
};

use super::{assert_x86_64_platform, dependency_names};

const INSTALL_SCRIPT: &str = r#"printf '%s' '#!/usr/bin/bash
export GRAPHITE_PLUGIN_DIR=/usr/lib/graphite-renderer/plugins
export GRAPHITE_DATA_DIR=/usr/share/graphite-renderer
export SSL_CERT_FILE=/usr/share/system-trust/ca-bundle.pem
export GSETTINGS_SCHEMA_DIR=/usr/share/glib-2.0/schemas
exec /usr/libexec/graphite-renderer/graphite-renderer "$@"
' > graphite-renderer
/usr/bin/install -Dm755 graphite-renderer "${CAST_INSTALL_ROOT}${CAST_BINDIR}/graphite-renderer""#;

const RUNTIME_INPUTS: [(&str, &str); 5] = [
    ("graphite-renderer", "runtime"),
    ("graphite-codecs", "plugins"),
    ("graphite-assets", "data"),
    ("system-trust", "certificates"),
    ("desktop-schema-registry", "schemas"),
];

pub(super) fn assert_semantics(declaration: &PackageSpec, plan: &DerivationPlan) {
    assert_eq!(declaration.meta.pname, "graphite-renderer-wrapper");
    assert_eq!(declaration.meta.version, "1.0.0");
    assert_eq!(declaration.architectures, ["native"]);
    assert!(declaration.sources.is_empty());
    assert!(!declaration.options.networking);
    assert_eq!(
        dependency_names(&declaration.builder.required_tools),
        ["binary(bash)", "binary(install)"]
    );

    assert_authored_install(declaration);
    assert_authored_output(declaration);
    assert_runtime_wrapper_bytes(INSTALL_SCRIPT);

    for module in ["runtime.glu", "package.glu"] {
        assert!(
            plan.provenance
                .recipe
                .modules
                .iter()
                .any(|imported| imported.logical_name == module),
            "the frozen runtime wrapper lost imported module {module}"
        );
    }
    assert!(plan.sources.is_empty());
    assert_frozen_install(plan);
    assert_eq!(
        plan.collection_rules,
        [CollectionRulePlan {
            output: "out".to_owned(),
            kind: PathRuleKind::Executable,
            pattern: "/usr/bin/graphite-renderer".to_owned(),
        }]
    );

    let [output] = plan.outputs.as_slice() else {
        panic!("the frozen runtime wrapper must publish one output");
    };
    assert_eq!(output.name, "out");
    assert_eq!(
        locked_runtime_names(&output.runtime_inputs),
        [
            "graphite-renderer-runtime",
            "graphite-codecs-plugins",
            "graphite-assets-data",
            "system-trust-certificates",
            "desktop-schema-registry-schemas",
            "binary(bash)",
        ]
    );
    assert_eq!(
        plan.manifest_build_inputs
            .iter()
            .map(|relation| relation.canonical_name())
            .collect::<Vec<_>>(),
        ["binary(bash)", "binary(install)"]
    );
    assert_eq!(plan.execution.network, NetworkMode::Disabled);
    assert_x86_64_platform(plan);
}

fn assert_authored_install(declaration: &PackageSpec) {
    assert!(declaration.builder.phases.setup.steps.is_empty());
    assert!(declaration.builder.phases.build.steps.is_empty());
    assert!(declaration.builder.phases.check.steps.is_empty());
    assert!(declaration.builder.phases.workload.steps.is_empty());

    let [StepSpec::Shell {
        interpreter,
        declared_programs,
        script,
    }] = declaration.builder.phases.install.steps.as_slice()
    else {
        panic!("runtime wrapper installation must remain one explicit shell step");
    };
    assert_eq!(interpreter.path, "/usr/bin/bash");
    assert_eq!(interpreter.requirement.dependency().unwrap().to_name(), "binary(bash)");
    assert!(matches!(declared_programs.as_slice(), [install]
        if install.path == "/usr/bin/install"
            && install.requirement.dependency().unwrap().to_name() == "binary(install)"));
    assert_eq!(script, INSTALL_SCRIPT);
}

fn assert_authored_output(declaration: &PackageSpec) {
    let [output] = declaration.outputs.as_slice() else {
        panic!("the runtime wrapper must publish one exact output");
    };
    assert_eq!(output.name, "out");
    assert_eq!(
        output.paths,
        [PathSpec::Exe {
            path: "/usr/bin/graphite-renderer".to_owned(),
        }]
    );
    assert_eq!(output.runtime_inputs.len(), RUNTIME_INPUTS.len() + 1);
    for (dependency, (package, named_output)) in output.runtime_inputs.iter().zip(RUNTIME_INPUTS) {
        assert!(matches!(dependency,
            DependencySpec::Output(output)
                if output.package.name == package && output.output == named_output));
    }
    assert!(matches!(output.runtime_inputs.last(), Some(DependencySpec::Binary(shell))
        if shell == "bash"));
}

fn assert_frozen_install(plan: &DerivationPlan) {
    let install = plan
        .jobs
        .iter()
        .flat_map(|job| &job.phases)
        .find(|phase| phase.name.eq_ignore_ascii_case("install"))
        .expect("the frozen runtime wrapper lost its install phase");
    assert!(matches!(
        install.steps.as_slice(),
        [StepPlan::Shell {
            interpreter,
            declared_programs,
            script,
            ..
        }] if interpreter.path == "/usr/bin/bash"
            && interpreter.requirement.canonical_name() == "binary(bash)"
            && matches!(declared_programs.as_slice(), [program]
                if program.path == "/usr/bin/install"
                    && program.requirement.canonical_name() == "binary(install)")
            && script == INSTALL_SCRIPT
    ));
    assert!(
        plan.jobs
            .iter()
            .flat_map(|job| &job.phases)
            .flat_map(|phase| phase.pre.iter().chain(&phase.steps).chain(&phase.post))
            .all(|step| !matches!(step, StepPlan::ExtractArchive { .. } | StepPlan::RunBuilt { .. })),
        "a source-less wrapper must not acquire extraction or build-tree execution"
    );
}

fn assert_runtime_wrapper_bytes(script: &str) {
    assert!(!script.contains("/usr/bin/env"));
    assert!(!script.contains("export PATH="));
    assert!(!script.contains("\nPATH="));
    assert!(script.contains("#!/usr/bin/bash\n"));
    assert!(script.contains("export GRAPHITE_PLUGIN_DIR=/usr/lib/graphite-renderer/plugins\n"));
    assert!(script.contains("export GRAPHITE_DATA_DIR=/usr/share/graphite-renderer\n"));
    assert!(script.contains("export SSL_CERT_FILE=/usr/share/system-trust/ca-bundle.pem\n"));
    assert!(script.contains("export GSETTINGS_SCHEMA_DIR=/usr/share/glib-2.0/schemas\n"));
    assert!(script.contains(
        "\nexec /usr/libexec/graphite-renderer/graphite-renderer \"$@\"\n"
    ));
}

fn locked_runtime_names(runtime_inputs: &[OutputRelation]) -> Vec<String> {
    runtime_inputs
        .iter()
        .map(|input| match input {
            OutputRelation::Locked { relation, .. } => relation.canonical_name(),
            OutputRelation::Planned { output } => {
                panic!("external runtime wrapper input became local output {output}")
            }
        })
        .collect()
}
