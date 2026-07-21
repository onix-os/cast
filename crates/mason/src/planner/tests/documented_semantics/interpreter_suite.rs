use fs_err as fs;
use stone_recipe::{
    PathSpec,
    derivation::{
        CollectionRulePlan, DerivationPlan, NetworkMode, OutputRelation, PathRuleKind, StepPlan,
    },
    package::{PackageSpec, StepSpec},
};

use super::{
    PackageExampleMatrix, WriteOutcome, assert_x86_64_platform, dependency_names, plan_for_build,
};

const SETUP_SCRIPT: &str = r#"printf '%s\n' \
    '#!@BASH@' \
    'set -eu' \
    'test "${1-}" = "--self-test"' \
    'printf "%s\n" "explicit interpreter suite: bash"' \
    > cast-interpreter-shell
printf '%s\n' \
    '#!@PYTHON@' \
    'import sys' \
    'if sys.argv[1:] != ["--self-test"]:' \
    '    raise SystemExit(2)' \
    'print("explicit interpreter suite: python")' \
    > cast-interpreter-python"#;
const INSTALL_SCRIPT: &str = r#"install -Dm755 cast-interpreter-shell "${CAST_INSTALL_ROOT}${CAST_BINDIR}/cast-interpreter-shell"
install -Dm755 cast-interpreter-python "${CAST_INSTALL_ROOT}${CAST_BINDIR}/cast-interpreter-python""#;
const BASH_REPLACEMENT: &str = "s|^#!@BASH@$|#!/usr/bin/bash|";
const PYTHON_REPLACEMENT: &str = "s|^#!@PYTHON@$|#!/usr/bin/python3|";

pub(super) fn assert_semantics(declaration: &PackageSpec, plan: &DerivationPlan) {
    assert_eq!(declaration.meta.pname, "explicit-interpreter-suite");
    assert_eq!(declaration.meta.version, "1.0.0");
    assert!(declaration.sources.is_empty());
    assert_eq!(declaration.architectures, ["native"]);
    assert_eq!(
        dependency_names(&declaration.builder.required_tools),
        [
            "binary(bash)",
            "binary(python3)",
            "binary(sed)",
            "binary(grep)",
            "binary(install)",
        ]
    );

    assert_authored_steps(declaration);
    assert_authored_output(declaration);
    assert!(
        all_authored_steps(declaration)
            .iter()
            .all(|step| !format!("{step:?}").contains("/usr/bin/env")),
        "the authored script bytes and commands must not retain ambient interpreter discovery"
    );

    for module in ["interpreters.glu", "package.glu"] {
        assert!(
            plan.provenance
                .recipe
                .imported_modules
                .iter()
                .any(|imported| imported.logical_name == module),
            "the frozen interpreter suite lost imported module {module}"
        );
    }
    assert!(plan.sources.is_empty());
    assert_frozen_steps(plan);
    assert_eq!(
        plan.collection_rules,
        [
            CollectionRulePlan {
                output: "out".to_owned(),
                kind: PathRuleKind::Executable,
                pattern: "/usr/bin/cast-interpreter-shell".to_owned(),
            },
            CollectionRulePlan {
                output: "out".to_owned(),
                kind: PathRuleKind::Executable,
                pattern: "/usr/bin/cast-interpreter-python".to_owned(),
            },
        ]
    );
    let [output] = plan.outputs.as_slice() else {
        panic!("the frozen interpreter suite must publish one output");
    };
    assert_eq!(output.name, "out");
    assert!(matches!(
        output.runtime_inputs.as_slice(),
        [
            OutputRelation::Locked { relation: bash, .. },
            OutputRelation::Locked { relation: python, .. },
        ] if bash.canonical_name() == "binary(bash)"
            && python.canonical_name() == "binary(python3)"
    ));
    assert_eq!(
        plan.manifest_build_inputs
            .iter()
            .map(|relation| relation.canonical_name())
            .collect::<Vec<_>>(),
        [
            "binary(bash)",
            "binary(python3)",
            "binary(sed)",
            "binary(grep)",
            "binary(install)",
        ]
    );
    assert_eq!(plan.execution.network, NetworkMode::Disabled);
    assert_x86_64_platform(plan);
}

fn assert_authored_steps(declaration: &PackageSpec) {
    let [
        StepSpec::Shell {
            interpreter,
            declared_programs,
            script,
        },
        StepSpec::Run {
            program: bash_sed,
            args: bash_args,
        },
        StepSpec::Run {
            program: python_sed,
            args: python_args,
        },
    ] = declaration.builder.phases.setup.steps.as_slice()
    else {
        panic!("interpreter setup must author two scripts and replace two exact shebangs");
    };
    assert_eq!(interpreter.path, "/usr/bin/bash");
    assert!(declared_programs.is_empty());
    assert_eq!(script, SETUP_SCRIPT);
    assert_eq!(bash_sed.path, "/usr/bin/sed");
    assert_eq!(python_sed.path, "/usr/bin/sed");
    assert_eq!(
        bash_args,
        &["-i", "-e", BASH_REPLACEMENT, "cast-interpreter-shell"]
    );
    assert_eq!(
        python_args,
        &["-i", "-e", PYTHON_REPLACEMENT, "cast-interpreter-python"]
    );

    let [
        StepSpec::Run {
            program: bash_grep,
            args: bash_grep_args,
        },
        StepSpec::Run {
            program: python_grep,
            args: python_grep_args,
        },
        StepSpec::Run {
            program: bash,
            args: bash_args,
        },
        StepSpec::Run {
            program: python,
            args: python_args,
        },
    ] = declaration.builder.phases.check.steps.as_slice()
    else {
        panic!("interpreter checks must verify and invoke both exact scripts");
    };
    assert_eq!(bash_grep.path, "/usr/bin/grep");
    assert_eq!(python_grep.path, "/usr/bin/grep");
    assert_eq!(
        bash_grep_args,
        &["-Fqx", "#!/usr/bin/bash", "cast-interpreter-shell"]
    );
    assert_eq!(
        python_grep_args,
        &["-Fqx", "#!/usr/bin/python3", "cast-interpreter-python"]
    );
    assert_eq!(bash.path, "/usr/bin/bash");
    assert_eq!(bash_args, &["cast-interpreter-shell", "--self-test"]);
    assert_eq!(python.path, "/usr/bin/python3");
    assert_eq!(python_args, &["cast-interpreter-python", "--self-test"]);

    let [StepSpec::Shell {
        interpreter,
        declared_programs,
        script,
    }] = declaration.builder.phases.install.steps.as_slice()
    else {
        panic!("interpreter installation must remain one explicit shell step");
    };
    assert_eq!(interpreter.path, "/usr/bin/bash");
    assert!(matches!(declared_programs.as_slice(), [install] if install.path == "/usr/bin/install"));
    assert_eq!(script, INSTALL_SCRIPT);
    assert!(declaration.builder.phases.build.steps.is_empty());
    assert!(declaration.builder.phases.workload.steps.is_empty());
}

fn assert_authored_output(declaration: &PackageSpec) {
    let [output] = declaration.outputs.as_slice() else {
        panic!("the interpreter suite must publish one exact output");
    };
    assert_eq!(dependency_names(&output.runtime_inputs), ["binary(bash)", "binary(python3)"]);
    assert_eq!(
        output.paths,
        [
            PathSpec::Exe {
                path: "/usr/bin/cast-interpreter-shell".to_owned(),
            },
            PathSpec::Exe {
                path: "/usr/bin/cast-interpreter-python".to_owned(),
            },
        ]
    );
}

fn all_authored_steps(declaration: &PackageSpec) -> Vec<&StepSpec> {
    [
        &declaration.builder.phases.setup,
        &declaration.builder.phases.build,
        &declaration.builder.phases.check,
        &declaration.builder.phases.install,
        &declaration.builder.phases.workload,
    ]
    .into_iter()
    .flat_map(|phase| &phase.steps)
    .collect()
}

fn assert_frozen_steps(plan: &DerivationPlan) {
    let phase = |name: &str| {
        plan.jobs
            .iter()
            .flat_map(|job| &job.phases)
            .find(|phase| phase.name.eq_ignore_ascii_case(name))
            .unwrap_or_else(|| panic!("frozen interpreter suite lost {name} phase"))
    };
    let setup = phase("setup");
    assert!(matches!(
        setup.steps.as_slice(),
        [
            StepPlan::Shell {
                interpreter,
                declared_programs,
                script,
                ..
            },
            StepPlan::Run {
                program: bash_sed,
                args: bash_args,
                ..
            },
            StepPlan::Run {
                program: python_sed,
                args: python_args,
                ..
            },
        ] if interpreter.path == "/usr/bin/bash"
            && interpreter.requirement.canonical_name() == "binary(bash)"
            && declared_programs.is_empty()
            && script == SETUP_SCRIPT
            && bash_sed.path == "/usr/bin/sed"
            && bash_sed.requirement.canonical_name() == "binary(sed)"
            && bash_args == &["-i", "-e", BASH_REPLACEMENT, "cast-interpreter-shell"]
            && python_sed.path == "/usr/bin/sed"
            && python_sed.requirement.canonical_name() == "binary(sed)"
            && python_args == &["-i", "-e", PYTHON_REPLACEMENT, "cast-interpreter-python"]
    ));

    let check = phase("check");
    assert!(matches!(
        check.steps.as_slice(),
        [
            StepPlan::Run { program: bash_grep, args: bash_grep_args, .. },
            StepPlan::Run { program: python_grep, args: python_grep_args, .. },
            StepPlan::Run { program: bash, args: bash_args, .. },
            StepPlan::Run { program: python, args: python_args, .. },
        ] if bash_grep.path == "/usr/bin/grep"
            && bash_grep.requirement.canonical_name() == "binary(grep)"
            && bash_grep_args == &["-Fqx", "#!/usr/bin/bash", "cast-interpreter-shell"]
            && python_grep.path == "/usr/bin/grep"
            && python_grep.requirement.canonical_name() == "binary(grep)"
            && python_grep_args == &["-Fqx", "#!/usr/bin/python3", "cast-interpreter-python"]
            && bash.path == "/usr/bin/bash"
            && bash.requirement.canonical_name() == "binary(bash)"
            && bash_args == &["cast-interpreter-shell", "--self-test"]
            && python.path == "/usr/bin/python3"
            && python.requirement.canonical_name() == "binary(python3)"
            && python_args == &["cast-interpreter-python", "--self-test"]
    ));

    let install = phase("install");
    assert!(matches!(
        install.steps.as_slice(),
        [StepPlan::Shell { interpreter, declared_programs, script, .. }]
            if interpreter.path == "/usr/bin/bash"
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
            .all(|step| !matches!(step, StepPlan::RunBuilt { .. } | StepPlan::ExtractArchive { .. })
                && !format!("{step:?}").contains("/usr/bin/env")),
        "the normalized plan must retain explicit interpreters without source extraction or RunBuilt"
    );
}

pub(super) fn assert_source_and_import_invalidation(matrix: &PackageExampleMatrix) {
    let example = matrix
        .examples
        .iter()
        .find(|example| example.name == "explicit-interpreter-suite")
        .expect("the explicit example inventory contains explicit-interpreter-suite");
    let original_builder = matrix.builder(example);
    let original_declaration = original_builder.recipe.declaration.clone();
    let original_fingerprint = original_builder.recipe.fingerprint.clone();
    drop(original_builder);
    let original = plan_for_build(matrix.env(), matrix.request(example, false), &matrix.output_dir)
        .expect("reuse the original explicit-interpreter-suite build lock");

    let root_source = fs::read_to_string(&example.recipe_path).unwrap();
    fs::write(
        &example.recipe_path,
        format!("{root_source}\n// Root-source invalidation proof.\n"),
    )
    .unwrap();
    assert!(
        plan_for_build(matrix.env(), matrix.request(example, false), &matrix.output_dir).is_err(),
        "a changed root source must invalidate the existing build lock"
    );
    let changed_root_builder = matrix.builder(example);
    assert_eq!(changed_root_builder.recipe.declaration, original_declaration);
    assert_ne!(
        changed_root_builder.recipe.fingerprint.root_source_sha256,
        original_fingerprint.root_source_sha256
    );
    assert_eq!(
        imported_sha256(&changed_root_builder.recipe.fingerprint, "interpreters.glu"),
        imported_sha256(&original_fingerprint, "interpreters.glu")
    );
    drop(changed_root_builder);
    let changed_root = plan_for_build(matrix.env(), matrix.request(example, true), &matrix.output_dir)
        .expect("refresh the root-changed explicit-interpreter-suite build lock");
    assert_eq!(changed_root.lock_outcome, Some(WriteOutcome::Written));
    assert_ne!(changed_root.plan.canonical_bytes(), original.plan.canonical_bytes());
    assert_ne!(changed_root.plan.derivation_id(), original.plan.derivation_id());

    fs::write(&example.recipe_path, &root_source).unwrap();
    let restored = plan_for_build(matrix.env(), matrix.request(example, true), &matrix.output_dir)
        .expect("restore the original explicit-interpreter-suite build lock");
    assert_eq!(restored.plan.canonical_bytes(), original.plan.canonical_bytes());
    assert_eq!(restored.plan.derivation_id(), original.plan.derivation_id());

    let interpreters_path = example.recipe_path.with_file_name("interpreters.glu");
    let interpreters_source = fs::read_to_string(&interpreters_path).unwrap();
    const BASH_INPUT: &str = "interpreter \"bash\" \"@BASH@\"";
    const CHANGED_BASH_INPUT: &str = "interpreter \"sh\" \"@BASH@\"";
    assert_eq!(interpreters_source.matches(BASH_INPUT).count(), 1);
    fs::write(
        &interpreters_path,
        interpreters_source.replacen(BASH_INPUT, CHANGED_BASH_INPUT, 1),
    )
    .unwrap();
    assert!(
        plan_for_build(matrix.env(), matrix.request(example, false), &matrix.output_dir).is_err(),
        "a changed imported interpreter module must invalidate the existing build lock"
    );
    let changed_import_builder = matrix.builder(example);
    assert_eq!(
        changed_import_builder.recipe.fingerprint.root_source_sha256,
        original_fingerprint.root_source_sha256
    );
    assert_ne!(
        imported_sha256(&changed_import_builder.recipe.fingerprint, "interpreters.glu"),
        imported_sha256(&original_fingerprint, "interpreters.glu")
    );
    assert_ne!(changed_import_builder.recipe.declaration, original_declaration);
    assert_eq!(
        dependency_names(&changed_import_builder.recipe.declaration.outputs[0].runtime_inputs),
        ["binary(sh)", "binary(python3)"]
    );
    drop(changed_import_builder);
    let changed_import = plan_for_build(matrix.env(), matrix.request(example, true), &matrix.output_dir)
        .expect("refresh the import-changed explicit-interpreter-suite build lock");
    assert_eq!(changed_import.lock_outcome, Some(WriteOutcome::Written));
    assert_ne!(changed_import.plan.canonical_bytes(), original.plan.canonical_bytes());
    assert_ne!(changed_import.plan.derivation_id(), original.plan.derivation_id());
    assert!(
        changed_import
            .plan
            .build_lock
            .requests
            .iter()
            .any(|request| request.request == "binary(sh)")
    );
    assert!(
        changed_import
            .plan
            .build_lock
            .requests
            .iter()
            .all(|request| request.request != "binary(bash)"),
        "the original interpreter request must leave the changed closure"
    );

    fs::write(&interpreters_path, interpreters_source).unwrap();
    let restored = plan_for_build(matrix.env(), matrix.request(example, true), &matrix.output_dir)
        .expect("restore the original interpreter-module build lock");
    assert_eq!(restored.plan.canonical_bytes(), original.plan.canonical_bytes());
    assert_eq!(restored.plan.derivation_id(), original.plan.derivation_id());
}

fn imported_sha256<'a>(
    fingerprint: &'a gluon_config::EvaluationFingerprint,
    logical_name: &str,
) -> &'a str {
    fingerprint
        .imported_modules
        .iter()
        .find(|module| module.logical_name == logical_name)
        .unwrap_or_else(|| panic!("missing imported module {logical_name}"))
        .sha256
        .as_str()
}
