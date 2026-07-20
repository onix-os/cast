use super::*;
use stone_recipe::derivation::{
    CollectionRulePlan, InputOrigin, JobExecutableRole, JobStepSection, LockedSource, PackageInputSelection,
    PathRuleKind, StepPlan,
};

pub(super) fn assert_semantics(name: &str, declaration: &PackageSpec, plan: &DerivationPlan) {
    match name {
        "conditionals" => assert_conditional_semantics(declaration, plan),
        "custom-steps" => assert_custom_step_semantics(declaration, plan),
        "dependency-roles" => assert_dependency_role_semantics(declaration, plan),
        "external-patch-source" => assert_external_patch_source_semantics(declaration, plan),
        "external-test-vectors" => assert_external_test_vector_semantics(declaration, plan),
        "header-only-library" => assert_header_only_semantics(declaration, plan),
        "multiple-sources" => assert_multiple_source_semantics(declaration, plan),
        "patch-series" => assert_patch_series_semantics(declaration, plan),
        "pgo-workload" => assert_pgo_workload_semantics(declaration, plan),
        "platform-binary-factory" => assert_platform_binary_semantics(declaration, plan),
        "post-install-smoke-test" => assert_post_install_semantics(declaration, plan),
        "profiles-emul32" => assert_profile_semantics(declaration, plan),
        "raw-script-package" => assert_raw_script_semantics(declaration, plan),
        "release-source-factory" => assert_release_source_semantics(declaration, plan),
        "split-outputs" => assert_split_output_semantics(declaration, plan),
        "system-integration-assets" => assert_system_integration_semantics(declaration, plan),
        _ => {}
    }
}

fn extraction_steps(plan: &DerivationPlan) -> Vec<&StepPlan> {
    plan.jobs
        .iter()
        .flat_map(|job| &job.phases)
        .flat_map(|phase| phase.pre.iter().chain(&phase.steps).chain(&phase.post))
        .filter(|step| matches!(step, StepPlan::ExtractArchive { .. }))
        .collect()
}

fn assert_conditional_semantics(declaration: &PackageSpec, plan: &DerivationPlan) {
    assert_eq!(declaration.meta.pname, "conditional-viewer");
    assert!(declaration.build_inputs.is_empty(), "disabled TLS must omit OpenSSL");
    assert_eq!(dependency_names(&declaration.check_inputs), ["binary(xvfb-run)"]);
    assert_eq!(
        declaration.builder.phases.setup.steps,
        [StepSpec::CMakeConfigure {
            flags: vec!["-DBUILD_DOCUMENTATION=ON".to_owned()],
        }]
    );
    assert_eq!(declaration.builder.phases.check.steps, [StepSpec::CMakeTest]);
    assert_eq!(plan.execution.network, NetworkMode::Disabled);
    assert_x86_64_platform(plan);
}

fn assert_dependency_role_semantics(declaration: &PackageSpec, plan: &DerivationPlan) {
    assert_eq!(declaration.meta.pname, "dependency-roles");
    assert_eq!(
        dependency_names(&declaration.native_build_inputs),
        ["binary(pkgconf)", "sysbinary(ldconfig)", "cmake(ZLIB)"]
    );
    assert_eq!(
        dependency_names(&declaration.build_inputs),
        [
            "base-devel",
            "libwidget-devel",
            "pkgconfig(zlib)",
            "pkgconfig32(zlib)",
            "soname(libz.so.1)",
            "python(setuptools)",
            "interpreter(/usr/lib/ld-linux-x86-64.so.2(x86_64))",
        ]
    );
    assert_eq!(dependency_names(&declaration.check_inputs), ["binary(bats)"]);
    assert_eq!(
        dependency_names(&declaration.outputs[0].runtime_inputs),
        ["soname(libc.so.6)", "binary(sh)"]
    );
    assert_eq!(plan.execution.network, NetworkMode::Disabled);
    assert_x86_64_platform(plan);
}

fn assert_external_patch_source_semantics(declaration: &PackageSpec, plan: &DerivationPlan) {
    assert_eq!(declaration.meta.pname, "portable-check");
    assert_eq!(dependency_names(&declaration.native_build_inputs), ["binary(patch)"]);
    let [primary, patch] = declaration.sources.as_slice() else {
        panic!("external patch package must retain two ordered sources");
    };
    assert!(matches!(
        primary,
        UpstreamSpec::Archive {
            rename: Some(rename),
            strip_dirs: Some(1),
            unpack: true,
            unpack_dir: Some(directory),
            ..
        } if rename == "portable-check.tar.xz" && directory == "portable-check"
    ));
    assert!(matches!(
        patch,
        UpstreamSpec::Archive {
            rename: Some(rename),
            strip_dirs: None,
            unpack: false,
            unpack_dir: None,
            ..
        } if rename == "portability.patch"
    ));
    let [
        StepSpec::Shell {
            interpreter,
            declared_programs,
            script,
        },
    ] = declaration.hooks.pre_setup.as_slice()
    else {
        panic!("external patch application must remain one explicit shell hook");
    };
    assert_eq!(interpreter.path, "/usr/bin/bash");
    assert_eq!(
        declared_programs
            .iter()
            .map(|program| program.path.as_str())
            .collect::<Vec<_>>(),
        ["/usr/bin/patch"]
    );
    assert!(script.contains("${CAST_SOURCE_DIR}/portable-check"));
    assert!(script.contains("${CAST_SOURCE_DIR}/portability.patch"));
    assert!(matches!(
        extraction_steps(plan).as_slice(),
        [StepPlan::ExtractArchive { source: 0, .. }]
    ));
    assert_eq!(plan.execution.network, NetworkMode::Disabled);
    assert_x86_64_platform(plan);
}

fn assert_external_test_vector_semantics(declaration: &PackageSpec, plan: &DerivationPlan) {
    const COPY_SCRIPT: &str = r#"test ! -e "${CAST_BUILDER_DIR}/external-test-vectors.json" && cp --preserve=mode,timestamps -- "${CAST_SOURCE_DIR}/frame-codec-conformance.json" "${CAST_BUILDER_DIR}/external-test-vectors.json" && test -s "${CAST_BUILDER_DIR}/external-test-vectors.json""#;

    assert_eq!(declaration.meta.pname, "frame-codec");
    assert_eq!(declaration.architectures, ["x86_64", "aarch64"]);
    assert!(!declaration.options.networking);
    assert_eq!(dependency_names(&declaration.native_build_inputs), ["binary(cp)"]);
    assert_eq!(
        declaration.builder.phases.setup.steps,
        [StepSpec::CMakeConfigure {
            flags: vec![
                "-DBUILD_TESTING=ON".to_owned(),
                "-DFRAME_CODEC_EXTERNAL_VECTOR_FILE=external-test-vectors.json".to_owned(),
            ],
        }]
    );
    assert_eq!(declaration.builder.phases.check.steps, [StepSpec::CMakeTest]);

    let [primary, vectors] = declaration.sources.as_slice() else {
        panic!("external-test-vectors must retain one primary archive and one raw corpus");
    };
    assert!(matches!(
        primary,
        UpstreamSpec::Archive {
            url,
            hash,
            rename: Some(rename),
            strip_dirs: Some(1),
            unpack: true,
            unpack_dir: Some(directory),
        } if url == "https://example.invalid/frame-codec-2.6.1.tar.xz"
            && hash == "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            && rename == "frame-codec.tar.xz"
            && directory == "frame-codec"
    ));
    assert!(matches!(
        vectors,
        UpstreamSpec::Archive {
            url,
            hash,
            rename: Some(rename),
            strip_dirs: None,
            unpack: false,
            unpack_dir: None,
        } if url == "https://example.invalid/frame-codec-conformance-2026-07.json"
            && hash == "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
            && rename == "frame-codec-conformance.json"
    ));

    let [
        StepSpec::Shell {
            interpreter,
            declared_programs,
            script,
        },
    ] = declaration.hooks.pre_check.as_slice()
    else {
        panic!("external-test-vectors must admit its raw corpus through one pre-check copy");
    };
    assert_eq!(interpreter.path, "/usr/bin/bash");
    assert!(matches!(declared_programs.as_slice(), [program] if program.path == "/usr/bin/cp"));
    assert_eq!(script, COPY_SCRIPT);

    assert!(matches!(
        plan.sources.as_slice(),
        [
            LockedSource::Archive {
                order: 0,
                url,
                sha256,
                filename,
                ..
            },
            LockedSource::Archive {
                order: 1,
                url: vector_url,
                sha256: vector_sha256,
                filename: vector_filename,
                ..
            },
        ] if url == "https://example.invalid/frame-codec-2.6.1.tar.xz"
            && sha256 == "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            && filename == "frame-codec.tar.xz"
            && vector_url == "https://example.invalid/frame-codec-conformance-2026-07.json"
            && vector_sha256 == "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
            && vector_filename == "frame-codec-conformance.json"
    ));
    assert!(matches!(
        extraction_steps(plan).as_slice(),
        [StepPlan::ExtractArchive {
            source: 0,
            destination,
            strip_components: 1,
        }] if destination == "frame-codec"
    ));

    let check = plan
        .jobs
        .iter()
        .flat_map(|job| &job.phases)
        .find(|phase| phase.name.eq_ignore_ascii_case("check"))
        .expect("external-test-vectors frozen plan lost its check phase");
    assert!(matches!(
        check.pre.as_slice(),
        [StepPlan::Shell {
            interpreter,
            declared_programs,
            script,
            ..
        }] if interpreter.path == "/usr/bin/bash"
            && matches!(declared_programs.as_slice(), [program] if program.path == "/usr/bin/cp")
            && script == COPY_SCRIPT
    ));
    assert!(matches!(
        check.steps.as_slice(),
        [StepPlan::Run { program, .. }] if program.path == "/usr/bin/ctest"
    ));

    let copy_request = plan
        .build_lock
        .requests
        .iter()
        .find(|request| request.request == "binary(cp)")
        .expect("external-test-vectors frozen closure lost binary(cp)");
    assert_eq!(copy_request.origins.len(), 2);
    assert!(matches!(
        &copy_request.origins[0],
        InputOrigin::NativeBuild {
            selection: PackageInputSelection::Package,
            index: 0,
        }
    ));
    assert!(matches!(
        &copy_request.origins[1],
        InputOrigin::JobExecutable {
            job: 0,
            phase_name,
            section: JobStepSection::Pre,
            step: 0,
            role: JobExecutableRole::ShellDeclaredProgram { index: 0 },
            ..
        } if phase_name.eq_ignore_ascii_case("check")
    ));

    let [output, debugging] = declaration.outputs.as_slice() else {
        panic!("external-test-vectors must publish exactly out and dbginfo");
    };
    assert_eq!(output.name, "out");
    assert!(output.include_in_manifest);
    assert!(matches!(
        output.paths.as_slice(),
        [stone_recipe::PathSpec::Exe { path }] if path == "/usr/bin/frame-codec"
    ));
    assert_eq!(debugging.name, "dbginfo");
    assert!(!debugging.include_in_manifest);
    assert!(matches!(
        debugging.paths.as_slice(),
        [stone_recipe::PathSpec::Any { path }] if path == "/usr/lib/debug"
    ));
    assert_eq!(plan.outputs.len(), 2);
    assert_eq!(plan.outputs[0].name, "out");
    assert_eq!(plan.outputs[1].name, "dbginfo");
    assert!(!plan.outputs[1].include_in_manifest);
    assert_eq!(
        plan.collection_rules,
        [
            CollectionRulePlan {
                output: "out".to_owned(),
                kind: PathRuleKind::Executable,
                pattern: "/usr/bin/frame-codec".to_owned(),
            },
            CollectionRulePlan {
                output: "dbginfo".to_owned(),
                kind: PathRuleKind::Any,
                pattern: "/usr/lib/debug".to_owned(),
            },
        ]
    );
    assert_eq!(plan.execution.network, NetworkMode::Disabled);
    assert_x86_64_platform(plan);
}

fn assert_header_only_semantics(declaration: &PackageSpec, plan: &DerivationPlan) {
    assert_eq!(declaration.meta.pname, "vector-header");
    assert_eq!(
        declaration
            .outputs
            .iter()
            .map(|output| output.name.as_str())
            .collect::<Vec<_>>(),
        ["out", "devel"]
    );
    assert!(
        declaration
            .outputs
            .iter()
            .all(|output| output.runtime_inputs.is_empty())
    );
    assert!(declaration.outputs.iter().all(|output| output.name != "libs"));
    let development = declaration
        .outputs
        .iter()
        .find(|output| output.name == "devel")
        .unwrap();
    assert_eq!(development.paths.len(), 2);
    let [StepSpec::Run { program, args }] = declaration.builder.phases.check.steps.as_slice() else {
        panic!("header-only consumer check must remain one structural compiler step");
    };
    assert_eq!(program.path, "/usr/bin/cc");
    assert_eq!(args.as_slice(), ["-I./include", "-fsyntax-only", "tests/smoke.c"]);
    assert!(matches!(
        extraction_steps(plan).as_slice(),
        [StepPlan::ExtractArchive { source: 0, .. }]
    ));
    assert_eq!(plan.execution.network, NetworkMode::Disabled);
    assert_x86_64_platform(plan);
}

fn assert_multiple_source_semantics(declaration: &PackageSpec, plan: &DerivationPlan) {
    assert_eq!(declaration.meta.pname, "multi-source-app");
    let [archive, git, raw] = declaration.sources.as_slice() else {
        panic!("multiple-sources must retain exactly three ordered sources");
    };
    assert!(matches!(
        archive,
        UpstreamSpec::Archive {
            rename: Some(rename),
            strip_dirs: Some(1),
            unpack: true,
            unpack_dir: Some(unpack_dir),
            ..
        } if rename == "application.tar.xz" && unpack_dir == "application"
    ));
    assert!(matches!(
        git,
        UpstreamSpec::Git {
            git_ref,
            clone_dir: Some(clone_dir),
            ..
        } if git_ref == EXAMPLE_GIT_COMMIT && clone_dir == "vendor-protocol"
    ));
    assert!(matches!(
        raw,
        UpstreamSpec::Archive {
            rename: Some(rename),
            strip_dirs: None,
            unpack: false,
            unpack_dir: None,
            ..
        } if rename == "schema.dat"
    ));
    assert_eq!(plan.sources.len(), 3);
    assert_eq!(plan.execution.network, NetworkMode::Disabled);
    assert_x86_64_platform(plan);
}

fn assert_split_output_semantics(declaration: &PackageSpec, plan: &DerivationPlan) {
    assert_eq!(declaration.meta.pname, "split-demo");
    assert_eq!(
        declaration
            .outputs
            .iter()
            .map(|output| output.name.as_str())
            .collect::<Vec<_>>(),
        ["out", "libs", "devel", "docs"]
    );
    let development = declaration
        .outputs
        .iter()
        .find(|output| output.name == "devel")
        .unwrap();
    assert!(matches!(
        development.runtime_inputs.as_slice(),
        [DependencySpec::Output(reference)]
            if reference.package.name == "split-demo" && reference.output == "libs"
    ));
    let documentation = declaration.outputs.iter().find(|output| output.name == "docs").unwrap();
    assert!(!documentation.include_in_manifest);
    let frozen_development = plan.outputs.iter().find(|output| output.name == "devel").unwrap();
    assert!(matches!(
        frozen_development.runtime_inputs.as_slice(),
        [OutputRelation::Planned { output }] if output == "libs"
    ));
    assert_eq!(plan.execution.network, NetworkMode::Disabled);
    assert_x86_64_platform(plan);
}

fn assert_profile_semantics(declaration: &PackageSpec, plan: &DerivationPlan) {
    assert_eq!(declaration.meta.pname, "profiled-codec");
    assert_eq!(declaration.architectures, ["native", "emul32"]);
    assert!(declaration.emul32);
    assert_eq!(
        declaration
            .profiles
            .iter()
            .map(|profile| profile.name.as_str())
            .collect::<Vec<_>>(),
        ["emul32", "x86_64-v3x"]
    );
    let emul32 = &declaration.profiles[0];
    assert_eq!(dependency_names(&emul32.native_build_inputs), ["binary(nasm)"]);
    assert_eq!(dependency_names(&emul32.build_inputs), ["pkgconfig32(zlib)"]);
    assert_eq!(dependency_names(&emul32.check_inputs), ["binary(file)"]);
    assert_eq!(
        emul32.builder.phases.setup.steps,
        [StepSpec::CMakeConfigure {
            flags: vec![
                "-DCMAKE_INSTALL_LIBDIR=lib32".to_owned(),
                "-DENABLE_TOOLS=OFF".to_owned(),
            ],
        }]
    );
    assert_eq!(plan.execution.network, NetworkMode::Disabled);
    assert_x86_64_platform(plan);
}

fn assert_pgo_workload_semantics(declaration: &PackageSpec, plan: &DerivationPlan) {
    assert_eq!(declaration.meta.pname, "vector-search");
    assert!(declaration.options.cspgo);
    assert!(declaration.options.samplepgo);
    assert!(!declaration.options.debug);
    assert!(declaration.mold);
    assert_eq!(declaration.architectures, ["x86_64", "aarch64"]);
    assert_eq!(
        declaration
            .tuning
            .iter()
            .map(|tuning| tuning.key.as_str())
            .collect::<Vec<_>>(),
        ["harden", "lto", "optimize"]
    );
    let [StepSpec::Shell { script, .. }] = declaration.hooks.pre_workload.as_slice() else {
        panic!("PGO workload must remain one explicit shell hook");
    };
    assert!(script.contains("${CAST_INSTALL_ROOT}${CAST_BINDIR}/vector-search"));
    assert!(script.contains("training/corpus.txt"));
    assert_eq!(plan.execution.network, NetworkMode::Disabled);
    assert_x86_64_platform(plan);
}

fn assert_platform_binary_semantics(declaration: &PackageSpec, plan: &DerivationPlan) {
    assert_eq!(declaration.meta.pname, "orbit-monitor-bin");
    assert_eq!(declaration.architectures, ["x86_64"]);
    assert_eq!(
        dependency_names(&declaration.builder.required_tools),
        ["binary(install)"]
    );
    assert!(matches!(
        declaration.sources.as_slice(),
        [UpstreamSpec::Archive {
            url,
            hash,
            rename: Some(rename),
            strip_dirs: Some(1),
            unpack: true,
            unpack_dir: Some(directory),
        }] if url.ends_with("/orbit-monitor-5.1.0-x86_64.tar.xz")
            && hash == "23456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef01"
            && rename == "orbit-monitor-5.1.0-x86_64.tar.xz"
            && directory == "orbit-monitor"
    ));
    let [output] = declaration.outputs.as_slice() else {
        panic!("platform binary package must publish one output");
    };
    assert_eq!(
        dependency_names(&output.runtime_inputs),
        ["interpreter(/usr/lib/ld-linux-x86-64.so.2(x86_64))"]
    );
    assert!(matches!(
        plan.sources.as_slice(),
        [LockedSource::Archive { filename, .. }] if filename == "orbit-monitor-5.1.0-x86_64.tar.xz"
    ));
    assert_eq!(plan.execution.network, NetworkMode::Disabled);
    assert_x86_64_platform(plan);
}

fn assert_post_install_semantics(declaration: &PackageSpec, plan: &DerivationPlan) {
    assert_eq!(declaration.meta.pname, "staged-probe");
    let [StepSpec::Shell { script, .. }] = declaration.hooks.post_install.as_slice() else {
        panic!("post-install smoke test must remain one explicit shell hook");
    };
    assert!(script.contains("${CAST_INSTALL_ROOT}${CAST_BINDIR}/staged-probe"));
    assert!(script.contains("--self-test"));
    let frozen_post_install = plan
        .jobs
        .iter()
        .flat_map(|job| &job.phases)
        .find(|phase| phase.name.eq_ignore_ascii_case("install"))
        .expect("frozen plan lost install phase");
    assert!(matches!(
        frozen_post_install.post.as_slice(),
        [StepPlan::Shell { script, .. }]
            if script.contains("${CAST_INSTALL_ROOT}${CAST_BINDIR}/staged-probe")
    ));
    assert_eq!(plan.execution.network, NetworkMode::Disabled);
    assert_x86_64_platform(plan);
}

fn assert_patch_series_semantics(declaration: &PackageSpec, plan: &DerivationPlan) {
    assert_eq!(declaration.meta.pname, "patched-archive");
    assert_eq!(dependency_names(&declaration.native_build_inputs), ["binary(patch)"]);
    assert_eq!(dependency_names(&declaration.build_inputs), ["pkgconfig(libarchive)"]);
    assert_eq!(declaration.hooks.pre_setup.len(), 3);
    for (index, step) in declaration.hooks.pre_setup.iter().enumerate() {
        let StepSpec::Run { program, args } = step else {
            panic!("patch series entry {index} must remain a structural run step");
        };
        assert_eq!(program.path, "/usr/bin/patch");
        assert!(matches!(&program.requirement, DependencySpec::Binary(binary) if binary == "patch"));
        assert_eq!(args[0], "-p1");
        assert_eq!(args[1], "-i");
        assert!(args[2].starts_with(&format!("packaging/{:04}-", index + 1)));
    }
    assert_eq!(plan.execution.network, NetworkMode::Disabled);
    assert_x86_64_platform(plan);
}

fn assert_custom_step_semantics(declaration: &PackageSpec, plan: &DerivationPlan) {
    assert_eq!(declaration.meta.pname, "custom-steps");
    assert_eq!(
        dependency_names(&declaration.builder.required_tools),
        [
            "binary(mkdir)",
            "schema-compiler",
            "binary(cc)",
            "binary(install)",
            "binary(ln)",
        ]
    );
    assert_eq!(declaration.builder.phases.build.steps.len(), 2);
    let [StepSpec::RunBuilt { program, args }] = declaration.builder.phases.check.steps.as_slice() else {
        panic!("custom check must execute the typed build-tree artifact");
    };
    assert_eq!(program.path, "build/custom-tool");
    assert!(args.is_empty());
    let [
        StepSpec::Shell {
            interpreter,
            declared_programs,
            script,
        },
    ] = declaration.builder.phases.install.steps.as_slice()
    else {
        panic!("custom install must remain one explicit shell step");
    };
    assert_eq!(interpreter.path, "/usr/bin/bash");
    assert_eq!(
        declared_programs
            .iter()
            .map(|program| program.path.as_str())
            .collect::<Vec<_>>(),
        ["/usr/bin/install", "/usr/bin/ln"]
    );
    assert!(script.contains("custom-tool-compat"));
    let frozen_check = plan
        .jobs
        .iter()
        .flat_map(|job| &job.phases)
        .find(|phase| phase.name.eq_ignore_ascii_case("check"))
        .expect("custom plan lost check phase");
    assert!(matches!(
        frozen_check.steps.as_slice(),
        [StepPlan::RunBuilt { program, args, .. }]
            if program.ends_with("/build/custom-tool") && args.is_empty()
    ));
    assert_eq!(plan.execution.network, NetworkMode::Disabled);
    assert_x86_64_platform(plan);
}

fn assert_raw_script_semantics(declaration: &PackageSpec, plan: &DerivationPlan) {
    assert_eq!(declaration.meta.pname, "net-audit");
    assert!(matches!(
        declaration.sources.as_slice(),
        [UpstreamSpec::Archive {
            rename: Some(rename),
            strip_dirs: None,
            unpack: false,
            unpack_dir: None,
            ..
        }] if rename == "net-audit"
    ));
    assert!(
        extraction_steps(plan).is_empty(),
        "raw script must not gain archive extraction"
    );
    let [output] = declaration.outputs.as_slice() else {
        panic!("raw script package must publish one output");
    };
    assert_eq!(dependency_names(&output.runtime_inputs), ["binary(bash)"]);
    assert!(matches!(
        plan.sources.as_slice(),
        [LockedSource::Archive { filename, .. }] if filename == "net-audit"
    ));
    assert_eq!(plan.execution.network, NetworkMode::Disabled);
    assert_x86_64_platform(plan);
}

fn assert_release_source_semantics(declaration: &PackageSpec, plan: &DerivationPlan) {
    assert_eq!(declaration.meta.pname, "release-probe");
    assert_eq!(declaration.meta.version, "2.7.4");
    assert_eq!(declaration.meta.release, 3);
    assert!(matches!(
        declaration.sources.as_slice(),
        [UpstreamSpec::Archive {
            url,
            hash,
            rename: Some(rename),
            strip_dirs: Some(1),
            unpack: true,
            unpack_dir: Some(directory),
        }] if url.ends_with("/release-probe-2.7.4.tar.xz")
            && hash == "123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0"
            && rename == "release-probe-2.7.4.tar.xz"
            && directory == "release-probe-2.7.4"
    ));
    assert!(matches!(
        plan.sources.as_slice(),
        [LockedSource::Archive { filename, sha256, .. }]
            if filename == "release-probe-2.7.4.tar.xz"
                && sha256 == "123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0"
    ));
    assert_eq!(plan.execution.network, NetworkMode::Disabled);
    assert_x86_64_platform(plan);
}

fn assert_system_integration_semantics(declaration: &PackageSpec, plan: &DerivationPlan) {
    assert_eq!(declaration.meta.pname, "device-broker-integration");
    assert_eq!(declaration.architectures, ["native"]);
    assert_eq!(
        dependency_names(&declaration.builder.required_tools),
        ["binary(install)"]
    );
    let [output] = declaration.outputs.as_slice() else {
        panic!("system integration package must publish exactly one output");
    };
    assert_eq!(
        dependency_names(&output.runtime_inputs),
        ["device-broker", "polkit", "systemd"]
    );
    assert_eq!(output.paths.len(), 5);
    let [
        StepSpec::Shell {
            declared_programs,
            script,
            ..
        },
    ] = declaration.builder.phases.install.steps.as_slice()
    else {
        panic!("system integration install must remain one explicit shell step");
    };
    assert_eq!(
        declared_programs
            .iter()
            .map(|program| program.path.as_str())
            .collect::<Vec<_>>(),
        ["/usr/bin/install"]
    );
    for path in [
        "systemd/system",
        "sysusers.d",
        "tmpfiles.d",
        "udev/rules.d",
        "polkit-1/rules.d",
    ] {
        assert!(script.contains(path), "integration install lost {path}");
    }
    assert_eq!(plan.execution.network, NetworkMode::Disabled);
    assert_x86_64_platform(plan);
}
