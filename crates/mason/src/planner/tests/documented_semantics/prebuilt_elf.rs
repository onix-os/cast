use std::{collections::BTreeSet, path::Path};

use stone_recipe::{
    PathSpec, UpstreamSpec,
    derivation::{
        AnalyzerRole, CollectionRulePlan, CompilerExecutableRole, DerivationPlan, InputOrigin,
        JobExecutableRole, JobStepSection, LockedSource, NetworkMode, OutputRelation,
        PackageInputSelection, PathRuleKind, StepPlan,
    },
    package::{DependencySpec, PackageSpec, StepSpec},
};

use super::{assert_x86_64_platform, dependency_names};

const PACKAGE: &str = "quartz-inspector-bin";
const VERSION: &str = "3.4.1";
const EXECUTABLE: &str = "bin/quartz-inspector";
const INSTALLED_PATH: &str = "/usr/bin/quartz-inspector";
const ARCHIVE_URL: &str =
    "https://example.invalid/quartz-inspector/releases/3.4.1/quartz-inspector-3.4.1-x86_64.tar.xz";
const ARCHIVE_SHA256: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
const ARCHIVE_FILENAME: &str = "quartz-inspector-3.4.1-x86_64.tar.xz";
const UNPACK_DIRECTORY: &str = "quartz-inspector";
const INTERPRETER_RELATION: &str = "interpreter(/usr/lib/ld-linux-x86-64.so.2(x86_64))";
const LIBC_RELATION: &str = "soname(libc.so.6(x86_64))";
const INSTALL_SCRIPT: &str =
    r#"/usr/bin/install -Dm755 bin/quartz-inspector "${CAST_INSTALL_ROOT}${CAST_BINDIR}/quartz-inspector""#;

pub(super) fn assert_semantics(declaration: &PackageSpec, plan: &DerivationPlan) {
    assert_eq!(declaration.meta.pname, PACKAGE);
    assert_eq!(declaration.meta.version, VERSION);
    assert_eq!(declaration.architectures, ["x86_64"]);
    assert!(declaration.options.debug);
    assert!(declaration.options.strip);
    assert!(!declaration.options.networking);

    assert_authored_archive(declaration);
    assert_authored_phase_contract(declaration);
    assert_authored_outputs(declaration);

    for module in ["artifact.glu", "package.glu"] {
        assert!(
            plan.provenance
                .recipe
                .modules
                .iter()
                .any(|imported| imported.logical_name == module),
            "prebuilt ELF plan lost imported module {module}"
        );
    }

    assert_frozen_archive(plan);
    assert_frozen_phase_contract(plan);
    assert_frozen_outputs(plan);
    assert_frozen_executable_origins(plan);

    assert_eq!(plan.execution.network, NetworkMode::Disabled);
    assert_x86_64_platform(plan);
}

fn assert_authored_archive(declaration: &PackageSpec) {
    assert!(matches!(
        declaration.sources.as_slice(),
        [UpstreamSpec::Archive {
            url,
            hash,
            rename: Some(rename),
            strip_dirs: Some(1),
            unpack: true,
            unpack_dir: Some(unpack_dir),
        }] if url == ARCHIVE_URL
            && hash == ARCHIVE_SHA256
            && rename == ARCHIVE_FILENAME
            && unpack_dir == UNPACK_DIRECTORY
    ));
}

fn assert_authored_phase_contract(declaration: &PackageSpec) {
    assert_eq!(
        dependency_names(&declaration.builder.required_tools),
        ["binary(install)"]
    );
    assert!(declaration.native_build_inputs.is_empty());
    assert!(declaration.build_inputs.is_empty());
    assert!(declaration.check_inputs.is_empty());
    assert!(declaration.builder.phases.setup.steps.is_empty());
    assert!(declaration.builder.phases.build.steps.is_empty());
    assert!(declaration.builder.phases.workload.steps.is_empty());

    assert!(matches!(
        declaration.builder.phases.check.steps.as_slice(),
        [StepSpec::RunBuilt { program, args }]
            if program.path == EXECUTABLE && args.as_slice() == ["--self-test"]
    ));
    assert!(matches!(
        declaration.builder.phases.install.steps.as_slice(),
        [StepSpec::Shell {
            interpreter,
            declared_programs,
            script,
        }] if interpreter.path == "/usr/bin/dash"
            && matches!(&interpreter.requirement, DependencySpec::Binary(name) if name == "dash")
            && matches!(declared_programs.as_slice(), [install]
                if install.path == "/usr/bin/install"
                    && matches!(&install.requirement, DependencySpec::Binary(name) if name == "install"))
            && script == INSTALL_SCRIPT
    ));
}

fn assert_authored_outputs(declaration: &PackageSpec) {
    let [root, debugging] = declaration.outputs.as_slice() else {
        panic!("prebuilt ELF must publish exactly out and dbginfo");
    };
    assert_eq!(root.name, "out");
    assert!(root.include_in_manifest);
    assert_eq!(dependency_names(&root.runtime_inputs), [INTERPRETER_RELATION, LIBC_RELATION]);
    assert_eq!(
        root.paths,
        [PathSpec::Exe {
            path: INSTALLED_PATH.to_owned(),
        }]
    );
    assert_eq!(debugging.name, "dbginfo");
    assert!(!debugging.include_in_manifest);
    assert!(debugging.runtime_inputs.is_empty());
    assert_eq!(
        debugging.paths,
        [PathSpec::Any {
            path: "/usr/lib/debug".to_owned(),
        }]
    );
}

fn assert_frozen_archive(plan: &DerivationPlan) {
    assert!(matches!(
        plan.sources.as_slice(),
        [LockedSource::Archive {
            order: 0,
            url,
            sha256,
            filename,
        }] if url == ARCHIVE_URL && sha256 == ARCHIVE_SHA256 && filename == ARCHIVE_FILENAME
    ));
}

fn assert_frozen_phase_contract(plan: &DerivationPlan) {
    let [job] = plan.jobs.as_slice() else {
        panic!("prebuilt ELF must freeze as one target job");
    };
    assert_eq!(
        job.phases.iter().map(|phase| phase.name.as_str()).collect::<Vec<_>>(),
        ["Prepare", "Install", "Check"],
        "prebuilt ELF must not acquire setup, compile, link, or workload phases"
    );

    let [prepare, install, check] = job.phases.as_slice() else {
        panic!("prebuilt ELF phase topology drifted");
    };
    assert!(prepare.pre.is_empty() && prepare.post.is_empty());
    assert!(matches!(
        prepare.steps.as_slice(),
        [StepPlan::ExtractArchive {
            source: 0,
            destination,
            strip_components: 1,
        }] if destination == UNPACK_DIRECTORY
    ));
    assert!(install.pre.is_empty() && install.post.is_empty());
    assert!(matches!(
        install.steps.as_slice(),
        [StepPlan::Shell {
            interpreter,
            declared_programs,
            script,
            working_dir,
            ..
        }] if interpreter.path == "/usr/bin/dash"
            && interpreter.requirement.canonical_name() == "binary(dash)"
            && matches!(declared_programs.as_slice(), [program]
                if program.path == "/usr/bin/install"
                    && program.requirement.canonical_name() == "binary(install)")
            && script == INSTALL_SCRIPT
            && working_dir == &job.work_dir
    ));
    assert!(check.pre.is_empty() && check.post.is_empty());
    assert!(matches!(
        check.steps.as_slice(),
        [StepPlan::RunBuilt {
            program,
            args,
            working_dir,
            ..
        }] if program == &Path::new(&job.work_dir).join(EXECUTABLE).display().to_string()
            && args.as_slice() == ["--self-test"]
            && working_dir == &job.work_dir
    ));
}

fn assert_frozen_outputs(plan: &DerivationPlan) {
    assert_eq!(
        plan.collection_rules,
        [
            CollectionRulePlan {
                output: "out".to_owned(),
                kind: PathRuleKind::Executable,
                pattern: INSTALLED_PATH.to_owned(),
            },
            CollectionRulePlan {
                output: "dbginfo".to_owned(),
                kind: PathRuleKind::Any,
                pattern: "/usr/lib/debug".to_owned(),
            },
        ]
    );
    let [root, debugging] = plan.outputs.as_slice() else {
        panic!("frozen prebuilt ELF must retain out and dbginfo");
    };
    assert_eq!(root.name, "out");
    assert!(root.include_in_manifest);
    assert_eq!(
        root.runtime_inputs
            .iter()
            .map(|relation| match relation {
                OutputRelation::Locked { relation, .. } => relation.canonical_name(),
                OutputRelation::Planned { output } => panic!("unexpected local runtime output {output}"),
            })
            .collect::<Vec<_>>(),
        [INTERPRETER_RELATION, LIBC_RELATION]
    );
    assert_eq!(debugging.name, "dbginfo");
    assert!(!debugging.include_in_manifest);
    assert!(debugging.runtime_inputs.is_empty());

    assert!(plan.analysis.debug);
    assert!(plan.analysis.strip);
    let objcopy = plan.analysis.tools.objcopy.as_ref().expect("debug splitting lost objcopy");
    assert_eq!(objcopy.path, "/usr/bin/llvm-objcopy");
    assert_eq!(objcopy.requirement.canonical_name(), "binary(llvm-objcopy)");
    let strip = plan.analysis.tools.strip.as_ref().expect("ELF stripping lost llvm-strip");
    assert_eq!(strip.path, "/usr/bin/llvm-strip");
    assert_eq!(strip.requirement.canonical_name(), "binary(llvm-strip)");
}

fn assert_frozen_executable_origins(plan: &DerivationPlan) {
    let request = |name: &str| {
        plan.build_lock
            .requests
            .iter()
            .find(|request| request.request == name)
            .unwrap_or_else(|| panic!("prebuilt ELF frozen closure lost {name}"))
    };

    assert_eq!(
        request("binary(dash)").origins,
        [InputOrigin::JobExecutable {
            job: 0,
            phase: 1,
            phase_name: "Install".to_owned(),
            section: JobStepSection::Steps,
            step: 0,
            role: JobExecutableRole::ShellInterpreter,
        }]
    );
    assert_eq!(
        request("binary(install)").origins,
        [
            InputOrigin::BuilderTool {
                selection: PackageInputSelection::Package,
                index: 0,
            },
            InputOrigin::JobExecutable {
                job: 0,
                phase: 1,
                phase_name: "Install".to_owned(),
                section: JobStepSection::Steps,
                step: 0,
                role: JobExecutableRole::ShellDeclaredProgram { index: 0 },
            },
        ]
    );
    for (name, index) in [(INTERPRETER_RELATION, 0), (LIBC_RELATION, 1)] {
        assert_eq!(
            request(name).origins,
            [InputOrigin::OutputRuntime {
                output: "out".to_owned(),
                index,
            }]
        );
    }

    let analyzer_origins = plan
        .build_lock
        .requests
        .iter()
        .flat_map(|request| {
            request.origins.iter().filter_map(move |origin| match origin {
                InputOrigin::Analyzer {
                    role: role @ (AnalyzerRole::Objcopy | AnalyzerRole::Strip),
                } => Some((request.request.as_str(), *role)),
                _ => None,
            })
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(
        analyzer_origins,
        BTreeSet::from([
            ("binary(llvm-objcopy)", AnalyzerRole::Objcopy),
            ("binary(llvm-strip)", AnalyzerRole::Strip),
        ])
    );

    for (name, role, field) in [
        (
            "binary(llvm-objcopy)",
            AnalyzerRole::Objcopy,
            "build_root.analyzer_tools.llvm.objcopy",
        ),
        (
            "binary(llvm-strip)",
            AnalyzerRole::Strip,
            "build_root.analyzer_tools.llvm.strip",
        ),
    ] {
        let origins = &request(name).origins;
        assert!(origins.contains(&InputOrigin::Analyzer { role }));
        assert!(origins.iter().any(|origin| {
            matches!(origin, InputOrigin::Policy { field: actual, .. } if actual == field)
        }));
    }

    for role in [CompilerExecutableRole::Cc, CompilerExecutableRole::Ld] {
        let compiler_request = plan
            .build_lock
            .requests
            .iter()
            .find(|request| {
                request
                    .origins
                    .contains(&InputOrigin::CompilerExecutable { role })
            })
            .unwrap_or_else(|| panic!("repository policy lost compiler role {role:?}"));
        assert!(
            compiler_request.origins.iter().all(|origin| !matches!(
                origin,
                InputOrigin::BuilderTool { .. }
                    | InputOrigin::NativeBuild { .. }
                    | InputOrigin::Build { .. }
                    | InputOrigin::Check { .. }
                    | InputOrigin::JobExecutable { .. }
            )),
            "policy compiler {role:?} became a package-authored or executed dependency"
        );
    }
}
