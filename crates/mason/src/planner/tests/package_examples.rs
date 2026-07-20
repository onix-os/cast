fn assert_documented_factory_semantics(name: &str, declaration: &PackageSpec, plan: &DerivationPlan) {
    match name {
        "backend-choice-factory" => documented_variants::assert_semantics(declaration, plan),
        "explicit-git-subprojects" => documented_git_subprojects::assert_semantics(declaration, plan),
        "explicit-package-scope" => documented_scopes::assert_semantics(declaration, plan),
        "explicit-package-set-extension" => documented_composition::assert_package_set_extension(declaration, plan),
        "factory-override" => assert_factory_override_semantics(declaration, plan),
        "gettext-catalogs" => assert_gettext_catalog_semantics(declaration, plan),
        "go-module" => assert_go_module_semantics(declaration, plan),
        "kernel-module-factory" => assert_kernel_module_factory_semantics(declaration, plan),
        "layered-overrides" => assert_layered_override_semantics(declaration, plan),
        "locked-template-substitution" => assert_locked_template_substitution_semantics(declaration, plan),
        "maven-application" => assert_maven_application_semantics(declaration, plan),
        "native-codegen-target-library" => documented_code_generation::assert_semantics(declaration, plan),
        "nodejs-vendored-application" => assert_nodejs_vendored_application_semantics(declaration, plan),
        "optional-component-source-graph" => documented_sources::assert_semantics(declaration, plan),
        "output-policy-factory" => assert_output_policy_factory_semantics(declaration, plan),
        "platform-factory" => assert_platform_factory_semantics(declaration, plan),
        "release-override" => documented_overrides::assert_semantics(declaration, plan),
        "service-family-factory" => documented_composition::assert_service_family(declaration, plan),
        "shared-capability-origins" => documented_dependencies::assert_semantics(declaration, plan),
        "source-less-generated-config" => documented_generated::assert_semantics(declaration, plan),
        "target-profile-specialization" => documented_profiles::assert_semantics(declaration, plan),
        "typed-output-routing" => documented_outputs::assert_semantics(declaration, plan),
        "userspace-role-factory" => documented_composition::assert_userspace_role(declaration, plan),
        "variant-matrix-factory" => documented_composition::assert_variant_matrix(declaration, plan),
        "zig-project" => assert_zig_project_semantics(declaration, plan),
        _ => documented_semantics::assert_semantics(name, declaration, plan),
    }
}

fn assert_locked_template_substitution_semantics(declaration: &PackageSpec, plan: &DerivationPlan) {
    assert_eq!(declaration.meta.pname, "session-index-config");
    assert_eq!(
        dependency_names(&declaration.builder.required_tools),
        ["binary(sed)", "binary(grep)", "binary(bash)", "binary(install)"]
    );

    let [StepSpec::Run { program, args }] = declaration.builder.phases.setup.steps.as_slice() else {
        panic!("locked-template-substitution must retain one structural substitution step");
    };
    assert_eq!(program.path, "/usr/bin/sed");
    assert_eq!(
        args.as_slice(),
        [
            "-i",
            "-e",
            "s|@SERVICE_NAME@|session-index|g",
            "-e",
            "s|@SOCKET_PATH@|/run/session-index/control.sock|g",
            "-e",
            "s|@WORKER_COUNT@|4|g",
            "-e",
            "s|@ACCESS_MODE@|read-only|g",
            "config/session-index.conf.in",
        ]
    );

    let expected = [
        "service_name = session-index",
        "socket_path = /run/session-index/control.sock",
        "worker_count = 4",
        "access_mode = read-only",
    ];
    assert_eq!(declaration.builder.phases.check.steps.len(), expected.len());
    for (step, expected_line) in declaration.builder.phases.check.steps.iter().zip(expected) {
        let StepSpec::Run { program, args } = step else {
            panic!("locked-template-substitution checks must remain structural Run steps");
        };
        assert_eq!(program.path, "/usr/bin/grep");
        assert_eq!(
            args.as_slice(),
            ["-Fqx", expected_line, "config/session-index.conf.in"]
        );
    }

    let [StepSpec::Shell {
        interpreter,
        declared_programs,
        script,
    }] = declaration.builder.phases.install.steps.as_slice()
    else {
        panic!("locked-template-substitution must retain one explicit install step");
    };
    assert_eq!(interpreter.path, "/usr/bin/bash");
    assert_eq!(
        declared_programs.iter().map(|program| program.path.as_str()).collect::<Vec<_>>(),
        ["/usr/bin/install"]
    );
    assert!(script.contains("${CAST_INSTALL_ROOT}${CAST_DATADIR}/session-index/session-index.conf"));
    assert!(declaration.outputs[0].paths.iter().any(|path| {
        matches!(path, stone_recipe::PathSpec::Any { path }
            if path == "/usr/share/session-index/session-index.conf")
    }));
    assert!(matches!(declaration.sources.as_slice(), [UpstreamSpec::Archive { .. }]));
    assert_eq!(plan.execution.network, NetworkMode::Disabled);
    assert_x86_64_platform(plan);
}

fn assert_kernel_module_factory_semantics(declaration: &PackageSpec, plan: &DerivationPlan) {
    assert_eq!(declaration.meta.pname, "atlas-sensor-module");
    assert_eq!(declaration.architectures, ["x86_64", "aarch64"]);
    assert!(matches!(
        declaration.native_build_inputs.as_slice(),
        [DependencySpec::Output(output)]
            if output.package.name == "linux-lts" && output.output == "devel"
    ));
    assert_eq!(
        dependency_names(&declaration.builder.required_tools),
        ["binary(make)", "binary(modinfo)", "binary(bash)", "binary(install)"]
    );
    let [StepSpec::Run { args, .. }] = declaration.builder.phases.build.steps.as_slice() else {
        panic!("kernel-module-factory must retain one structural make step");
    };
    assert_eq!(
        args.as_slice(),
        [
            "KERNEL_RELEASE=6.12.28-onix1",
            "KERNEL_DIR=/usr/lib/modules/6.12.28-onix1/build",
            "modules",
        ]
    );
    let root = declaration.outputs.iter().find(|output| output.name == "out").unwrap();
    assert!(root.paths.iter().any(|path| {
        matches!(path, stone_recipe::PathSpec::Any { path }
            if path == "/usr/lib/modules/6.12.28-onix1/extra/atlas-sensor.ko")
    }));
    assert_eq!(plan.execution.network, NetworkMode::Disabled);
    assert_x86_64_platform(plan);
}

fn assert_layered_override_semantics(declaration: &PackageSpec, plan: &DerivationPlan) {
    assert_eq!(declaration.meta.pname, "layered-proxy");
    assert_eq!(
        dependency_names(&declaration.native_build_inputs),
        ["binary(hardening-check)"]
    );
    assert_eq!(
        dependency_names(&declaration.build_inputs),
        ["pkgconfig(zlib)", "pkgconfig(libarchive)"]
    );
    assert_eq!(
        declaration
            .outputs
            .iter()
            .map(|output| output.name.as_str())
            .collect::<Vec<_>>(),
        [
            "out",
            "docs",
            "devel",
            "dbginfo",
            "libs",
            "32bit",
            "32bit-devel",
            "32bit-dbginfo",
            "demos",
            "tools",
        ]
    );
    assert_eq!(
        declaration
            .tuning
            .iter()
            .map(|tuning| tuning.key.as_str())
            .collect::<Vec<_>>(),
        ["harden", "optimize"]
    );
    assert!(!declaration.options.debug);
    assert!(declaration.options.strip);
    assert!(declaration.options.compressman);
    assert!(!declaration.options.networking);
    assert_eq!(plan.execution.network, NetworkMode::Disabled);
    assert_x86_64_platform(plan);
}

fn assert_maven_application_semantics(declaration: &PackageSpec, plan: &DerivationPlan) {
    assert_eq!(declaration.meta.pname, "pulse-router");
    assert_eq!(
        dependency_names(&declaration.builder.required_tools),
        ["binary(mvn)", "binary(install)"]
    );
    for phase in [&declaration.builder.phases.build, &declaration.builder.phases.check] {
        let [StepSpec::Shell { script, .. }] = phase.steps.as_slice() else {
            panic!("maven-application build and check phases must remain explicit shell steps");
        };
        for required in ["--offline", "-Dmaven.repo.local=", "-Dproject.build.outputTimestamp="] {
            assert!(
                script.contains(required),
                "maven-application lost offline setting {required}"
            );
        }
    }
    assert!(!declaration.options.networking);
    assert!(matches!(declaration.sources.as_slice(), [UpstreamSpec::Archive { .. }]));
    assert_eq!(plan.execution.network, NetworkMode::Disabled);
    assert_x86_64_platform(plan);
}

fn assert_nodejs_vendored_application_semantics(declaration: &PackageSpec, plan: &DerivationPlan) {
    assert_eq!(declaration.meta.pname, "nodejs-nebula-lint");
    assert_eq!(
        dependency_names(&declaration.builder.required_tools),
        ["binary(node)", "binary(install)", "binary(cp)"]
    );
    for phase in [&declaration.builder.phases.build, &declaration.builder.phases.check] {
        let [StepSpec::Shell { script, .. }] = phase.steps.as_slice() else {
            panic!("nodejs-vendored-application build and check phases must remain explicit shell steps");
        };
        assert!(script.contains("NODE_PATH=\"${CAST_SOURCE_DIR}/vendor/node_modules\""));
        assert!(!script.contains("npm"));
    }
    assert!(!declaration.options.networking);
    assert!(matches!(declaration.sources.as_slice(), [UpstreamSpec::Archive { .. }]));
    assert_eq!(plan.execution.network, NetworkMode::Disabled);
    assert_x86_64_platform(plan);
}

fn assert_gettext_catalog_semantics(declaration: &PackageSpec, plan: &DerivationPlan) {
    assert_eq!(declaration.meta.pname, "orbit-catalogs");
    assert_eq!(
        dependency_names(&declaration.builder.required_tools),
        ["binary(mkdir)", "binary(msgfmt)", "binary(install)"]
    );
    assert_eq!(declaration.builder.phases.build.steps.len(), 2);
    assert_eq!(declaration.builder.phases.check.steps.len(), 2);
    assert_eq!(declaration.outputs.len(), 2);
    assert!(declaration.outputs[0].paths.iter().any(|path| {
        matches!(path, stone_recipe::PathSpec::Any { path } if path == "/usr/share/locale/*/LC_MESSAGES/orbit.mo")
    }));
    assert!(matches!(declaration.sources.as_slice(), [UpstreamSpec::Archive { .. }]));
    assert_eq!(plan.execution.network, NetworkMode::Disabled);
    assert_x86_64_platform(plan);
}

fn assert_go_module_semantics(declaration: &PackageSpec, plan: &DerivationPlan) {
    assert_eq!(declaration.meta.pname, "go-glyph");
    assert_eq!(
        dependency_names(&declaration.builder.required_tools),
        ["binary(mkdir)", "binary(go)", "binary(install)"]
    );
    let [StepSpec::Shell { script: build, .. }] = declaration.builder.phases.build.steps.as_slice() else {
        panic!("go-module must retain one explicit shell build step");
    };
    for required in [
        "HOME=\"${CAST_BUILD_ROOT}/home\"",
        "GOROOT=/usr/lib/golang",
        "GOCACHE=\"${CAST_BUILD_ROOT}/go-cache\"",
        "GOMODCACHE=\"${CAST_BUILD_ROOT}/go-mod-cache\"",
        "GOENV=off",
        "GOWORK=off",
        "GOTOOLCHAIN=local",
        "GOPROXY=off",
        "GOSUMDB=off",
        "GONOSUMDB='*'",
        "GONOPROXY='*'",
        "GOFLAGS=",
        "GO111MODULE=on",
        "CGO_ENABLED=0",
        "go telemetry off",
        "-mod=vendor",
        "-trimpath",
        "-buildvcs=false",
    ] {
        assert!(
            build.contains(required),
            "go-module build lost offline/reproducible setting {required}"
        );
    }
    let [StepSpec::Shell { script: check, .. }] = declaration.builder.phases.check.steps.as_slice() else {
        panic!("go-module must retain one explicit shell check step");
    };
    for required in [
        "HOME=\"${CAST_BUILD_ROOT}/home\"",
        "GOROOT=/usr/lib/golang",
        "GOCACHE=\"${CAST_BUILD_ROOT}/go-cache\"",
        "GOMODCACHE=\"${CAST_BUILD_ROOT}/go-mod-cache\"",
        "GOENV=off",
        "GOWORK=off",
        "GOTOOLCHAIN=local",
        "GOPROXY=off",
        "GOSUMDB=off",
        "GONOSUMDB='*'",
        "GONOPROXY='*'",
        "GOFLAGS=",
        "GO111MODULE=on",
        "CGO_ENABLED=0",
        "go telemetry off",
        "-mod=vendor",
        "-trimpath",
    ] {
        assert!(
            check.contains(required),
            "go-module check lost offline/reproducible setting {required}"
        );
    }
    assert!(!declaration.options.networking);
    assert_eq!(plan.execution.network, NetworkMode::Disabled);
    assert_x86_64_platform(plan);
}

fn assert_zig_project_semantics(declaration: &PackageSpec, plan: &DerivationPlan) {
    assert_eq!(declaration.meta.pname, "zig-vector");
    assert_eq!(dependency_names(&declaration.builder.required_tools), ["binary(zig)"]);
    for phase in [
        &declaration.builder.phases.build,
        &declaration.builder.phases.check,
        &declaration.builder.phases.install,
    ] {
        let [StepSpec::Shell { script, .. }] = phase.steps.as_slice() else {
            panic!("zig-project phases must remain explicit shell steps");
        };
        assert!(script.contains("ZIG_GLOBAL_CACHE_DIR=\"${CAST_BUILD_ROOT}/zig-global-cache\""));
        assert!(script.contains("ZIG_LOCAL_CACHE_DIR=\"${CAST_BUILD_ROOT}/zig-local-cache\""));
    }
    let root = declaration.outputs.iter().find(|output| output.name == "out").unwrap();
    let development = declaration
        .outputs
        .iter()
        .find(|output| output.name == "devel")
        .unwrap();
    for output in [root, development] {
        assert!(matches!(
            output.runtime_inputs.as_slice(),
            [DependencySpec::Output(reference)]
                if reference.package.name == "zig-vector" && reference.output == "libs"
        ));
    }
    assert!(!declaration.options.networking);
    assert_eq!(plan.execution.network, NetworkMode::Disabled);
    assert_x86_64_platform(plan);
}

fn assert_factory_override_semantics(declaration: &PackageSpec, plan: &DerivationPlan) {
    assert_eq!(declaration.meta.pname, "override-client");
    assert_eq!(
        dependency_names(&declaration.build_inputs),
        ["pkgconfig(zlib)", "pkgconfig(libressl)"],
        "the explicit TLS argument must replace the factory's OpenSSL default"
    );
    assert_eq!(
        declaration
            .outputs
            .iter()
            .map(|output| output.name.as_str())
            .collect::<Vec<_>>(),
        [
            "out",
            "docs",
            "devel",
            "dbginfo",
            "libs",
            "32bit",
            "32bit-devel",
            "32bit-dbginfo",
            "demos",
            "tools",
        ],
        "the output patch must append tools without disturbing the base output order"
    );
    let tools = declaration.outputs.last().expect("factory override appends tools");
    assert!(
        matches!(
            tools.runtime_inputs.as_slice(),
            [DependencySpec::Output(output)]
                if output.package.name == "override-client" && output.output == "out"
        ),
        "the appended tools output must depend on the package's exact root output"
    );
    assert_eq!(declaration.architectures, ["x86_64"]);
    assert_eq!(
        declaration.builder.phases.setup.steps,
        [StepSpec::CMakeConfigure {
            flags: vec!["-DUSE_SYSTEM_LIBRARIES=ON".to_owned()],
        }]
    );
    assert_eq!(
        plan.manifest_build_inputs
            .iter()
            .map(|dependency| dependency.canonical_name())
            .collect::<Vec<_>>(),
        ["binary(ninja)", "pkgconfig(zlib)", "pkgconfig(libressl)"]
    );
    let frozen_tools = plan
        .outputs
        .iter()
        .find(|output| output.name == "tools")
        .expect("the appended tools output reaches the frozen plan");
    assert!(matches!(
        frozen_tools.runtime_inputs.as_slice(),
        [OutputRelation::Planned { output }] if output == "out"
    ));
    assert_locked_request_origin(
        plan,
        "pkgconfig(zlib)",
        InputOrigin::Build {
            selection: PackageInputSelection::Package,
            index: 0,
        },
    );
    assert_locked_request_origin(
        plan,
        "pkgconfig(libressl)",
        InputOrigin::Build {
            selection: PackageInputSelection::Package,
            index: 1,
        },
    );
    assert!(
        plan.build_lock
            .requests
            .iter()
            .all(|request| request.request != "pkgconfig(openssl)"),
        "the replaced OpenSSL default must not leak into the frozen closure"
    );
    assert_x86_64_platform(plan);
}

fn assert_output_policy_factory_semantics(declaration: &PackageSpec, plan: &DerivationPlan) {
    assert_eq!(declaration.meta.pname, "telemetry-runtime");
    assert!(
        declaration.native_build_inputs.is_empty(),
        "disabled documentation policy must omit its generator capability"
    );
    assert_eq!(
        dependency_names(&declaration.build_inputs),
        ["pkgconfig(zlib)", "pkgconfig(libressl)"]
    );
    assert_eq!(
        declaration
            .outputs
            .iter()
            .map(|output| output.name.as_str())
            .collect::<Vec<_>>(),
        ["out", "libs", "devel", "tools"],
        "one policy value must select the exact published output graph"
    );
    let [root, libraries, development, tools] = declaration.outputs.as_slice() else {
        panic!("output policy must return exactly four outputs");
    };
    assert!(libraries.runtime_inputs.is_empty());
    for (output, name) in [(root, "out"), (development, "devel")] {
        assert!(
            matches!(
                output.runtime_inputs.as_slice(),
                [DependencySpec::Output(reference)]
                    if reference.package.name == "telemetry-runtime" && reference.output == "libs"
            ),
            "{name} must retain its exact local library-output relation"
        );
    }
    assert!(matches!(
        tools.runtime_inputs.as_slice(),
        [DependencySpec::Output(libraries), DependencySpec::Package(trust_store)]
            if libraries.package.name == "telemetry-runtime"
                && libraries.output == "libs"
                && trust_store.name == "ca-certificates"
    ));
    assert_eq!(
        declaration.builder.phases.setup.steps,
        [StepSpec::CMakeConfigure {
            flags: vec![
                "-DBUILD_COMMAND_LINE_TOOLS=ON".to_owned(),
                "-DBUILD_DOCUMENTATION=OFF".to_owned(),
            ],
        }]
    );
    assert_eq!(declaration.builder.phases.check.steps, [StepSpec::CMakeTest]);
    assert_eq!(
        declaration
            .tuning
            .iter()
            .map(|tuning| (tuning.key.as_str(), &tuning.value))
            .collect::<Vec<_>>(),
        [
            ("harden", &TuningSpec::Enable),
            (
                "optimize",
                &TuningSpec::Config {
                    value: "size".to_owned(),
                },
            ),
        ]
    );
    assert_eq!(
        plan.manifest_build_inputs
            .iter()
            .map(|dependency| dependency.canonical_name())
            .collect::<Vec<_>>(),
        ["binary(ninja)", "pkgconfig(zlib)", "pkgconfig(libressl)"]
    );
    for output in ["out", "devel"] {
        let frozen = plan
            .outputs
            .iter()
            .find(|candidate| candidate.name == output)
            .unwrap_or_else(|| panic!("missing frozen {output} output"));
        assert!(matches!(
            frozen.runtime_inputs.as_slice(),
            [OutputRelation::Planned { output }] if output == "libs"
        ));
    }
    let frozen_tools = plan
        .outputs
        .iter()
        .find(|output| output.name == "tools")
        .expect("policy-selected tools output reaches the frozen plan");
    assert!(matches!(
        frozen_tools.runtime_inputs.as_slice(),
        [
            OutputRelation::Planned { output },
            OutputRelation::Locked { relation, reference },
        ] if output == "libs"
            && relation.canonical_name() == "ca-certificates"
            && reference.output == "out"
    ));
    for (request, origin) in [
        (
            "pkgconfig(zlib)",
            InputOrigin::Build {
                selection: PackageInputSelection::Package,
                index: 0,
            },
        ),
        (
            "pkgconfig(libressl)",
            InputOrigin::Build {
                selection: PackageInputSelection::Package,
                index: 1,
            },
        ),
        (
            "ca-certificates",
            InputOrigin::OutputRuntime {
                output: "tools".to_owned(),
                index: 1,
            },
        ),
    ] {
        assert_locked_request_origin(plan, request, origin);
    }
    assert!(
        plan.build_lock
            .requests
            .iter()
            .all(|request| request.request != "binary(doxygen)"),
        "disabled documentation tooling must not leak into the frozen closure"
    );
    assert_x86_64_platform(plan);
}

fn assert_platform_factory_semantics(declaration: &PackageSpec, plan: &DerivationPlan) {
    assert_eq!(declaration.meta.pname, "relay-engine");
    assert_eq!(
        dependency_names(&declaration.native_build_inputs),
        ["binary(protocol-compiler)"]
    );
    assert_eq!(
        dependency_names(&declaration.build_inputs),
        ["pkgconfig(zlib)", "pkgconfig(openssl)", "pkgconfig(liburing)"],
        "the selected platform must supply liburing after the reusable dependencies"
    );
    assert_eq!(declaration.architectures, ["x86_64"]);
    assert_eq!(
        declaration.builder.phases.setup.steps,
        [StepSpec::CMakeConfigure {
            flags: vec![
                "-DENABLE_PORTABLE_DISPATCH=ON".to_owned(),
                "-DENABLE_SERVER=OFF".to_owned(),
                "-DUSE_IO_URING=ON".to_owned(),
            ],
        }]
    );
    assert_eq!(declaration.builder.phases.check.steps, [StepSpec::CMakeTest]);
    assert_eq!(
        declaration
            .tuning
            .iter()
            .map(|tuning| (tuning.key.as_str(), &tuning.value))
            .collect::<Vec<_>>(),
        [
            ("harden", &TuningSpec::Enable),
            (
                "lto",
                &TuningSpec::Config {
                    value: "thin".to_owned(),
                },
            ),
            (
                "optimize",
                &TuningSpec::Config {
                    value: "speed".to_owned(),
                },
            ),
        ]
    );
    assert_eq!(
        plan.manifest_build_inputs
            .iter()
            .map(|dependency| dependency.canonical_name())
            .collect::<Vec<_>>(),
        [
            "binary(ninja)",
            "binary(protocol-compiler)",
            "pkgconfig(zlib)",
            "pkgconfig(openssl)",
            "pkgconfig(liburing)",
        ]
    );
    for (request, origin) in [
        (
            "binary(protocol-compiler)",
            InputOrigin::NativeBuild {
                selection: PackageInputSelection::Package,
                index: 0,
            },
        ),
        (
            "pkgconfig(zlib)",
            InputOrigin::Build {
                selection: PackageInputSelection::Package,
                index: 0,
            },
        ),
        (
            "pkgconfig(openssl)",
            InputOrigin::Build {
                selection: PackageInputSelection::Package,
                index: 1,
            },
        ),
        (
            "pkgconfig(liburing)",
            InputOrigin::Build {
                selection: PackageInputSelection::Package,
                index: 2,
            },
        ),
    ] {
        assert_locked_request_origin(plan, request, origin);
    }
    assert_x86_64_platform(plan);
}

fn assert_factory_override_changes_frozen_identity(matrix: &PackageExampleMatrix) {
    let example = matrix
        .examples
        .iter()
        .find(|example| example.name == "factory-override")
        .expect("the explicit example inventory contains factory-override");
    let original = plan_for_build(matrix.env(), matrix.request(example, false), &matrix.output_dir)
        .expect("reuse the original factory-override build lock");
    let original_source = fs::read_to_string(&example.recipe_path).unwrap();
    const OVERRIDE: &str = "b.dep.pkgconfig \"libressl\"";
    const CHANGED_OVERRIDE: &str = "b.dep.pkgconfig \"openssl\"";
    assert_eq!(
        original_source.matches(OVERRIDE).count(),
        1,
        "the fingerprint proof must mutate exactly one explicit factory argument"
    );
    let changed_source = original_source.replacen(OVERRIDE, CHANGED_OVERRIDE, 1);
    fs::write(&example.recipe_path, changed_source).unwrap();

    let changed_evaluation = matrix.builder(example);
    assert_eq!(
        dependency_names(&changed_evaluation.recipe.declaration.build_inputs),
        ["pkgconfig(zlib)", "pkgconfig(openssl)"]
    );
    let changed = plan_for_build(matrix.env(), matrix.request(example, true), &matrix.output_dir)
        .expect("freeze the changed factory override");

    assert_eq!(changed.lock_outcome, Some(WriteOutcome::Written));
    assert_eq!(changed.plan.provenance.recipe, changed_evaluation.recipe.fingerprint);
    assert_ne!(
        original.plan.provenance.recipe.sha256, changed.plan.provenance.recipe.sha256,
        "changing a factory argument must invalidate the complete evaluation fingerprint"
    );
    assert_ne!(
        original.plan.canonical_bytes(),
        changed.plan.canonical_bytes(),
        "changing a factory argument must change the frozen plan"
    );
    assert_ne!(
        original.plan.derivation_id(),
        changed.plan.derivation_id(),
        "changing a factory argument must change derivation identity"
    );
    assert_locked_request_origin(
        &changed.plan,
        "pkgconfig(openssl)",
        InputOrigin::Build {
            selection: PackageInputSelection::Package,
            index: 1,
        },
    );
    assert!(
        changed
            .plan
            .build_lock
            .requests
            .iter()
            .all(|request| request.request != "pkgconfig(libressl)"),
        "the old override must not survive in the changed frozen closure"
    );
}

#[test]
fn checked_in_package_examples_freeze_hermetically_and_reuse_exact_build_locks() {
    let matrix = PackageExampleMatrix::new();
    let repository_uri = Url::from_file_path(&matrix.repository_index).unwrap().to_string();

    for example in &matrix.examples {
        let evaluated = matrix.builder(example);
        let first = plan_for_build(matrix.env(), matrix.request(example, true), &matrix.output_dir)
            .unwrap_or_else(|error| panic!("{}: freeze example plan: {error:#}", example.name));
        first
            .plan
            .validate()
            .unwrap_or_else(|error| panic!("{}: validate frozen example plan: {error:#}", example.name));
        assert_eq!(
            first.plan.provenance.recipe, evaluated.recipe.fingerprint,
            "{}: the frozen plan must retain the exact public recipe evaluation fingerprint",
            example.name
        );
        assert_documented_factory_semantics(&example.name, &evaluated.recipe.declaration, &first.plan);
        assert_eq!(
            first.lock_outcome,
            Some(WriteOutcome::Written),
            "{}: first freeze must create a fresh build lock",
            example.name
        );
        assert_eq!(
            first.plan.sources.len(),
            example.source_count,
            "{}: every authored source must reach the derivation plan",
            example.name
        );
        assert!(
            !first.plan.build_lock.repositories.is_empty()
                && first
                    .plan
                    .build_lock
                    .repositories
                    .iter()
                    .all(|repository| repository.index_uri == repository_uri),
            "{}: dependency resolution must use only the temporary local file repository",
            example.name
        );
        assert!(
            first
                .plan
                .build_lock
                .repositories
                .iter()
                .all(|repository| Url::parse(&repository.index_uri)
                    .is_ok_and(|uri| uri.scheme() == "file" && uri.to_file_path().is_ok())),
            "{}: the temporary repository must remain a valid file URL",
            example.name
        );

        let first_plan_bytes = first.plan.canonical_bytes();
        let first_derivation_id = first.plan.derivation_id();
        let first_lock_bytes = fs::read(&first.lock_path).unwrap();
        assert_eq!(
            first_lock_bytes,
            encode_build_lock(&first.plan.build_lock).into_bytes(),
            "{}: the on-disk build lock must be the canonical encoding of the frozen lock",
            example.name
        );
        match &example.source_lock_bytes {
            Some(expected) => assert_eq!(
                fs::read(example.recipe_path.with_file_name(SOURCE_LOCK_FILE_NAME)).unwrap(),
                *expected,
                "{}: planning must not rewrite the synthetic canonical source lock",
                example.name
            ),
            None => assert!(
                !example.recipe_path.with_file_name(SOURCE_LOCK_FILE_NAME).exists(),
                "{}: source-less examples must not gain a synthetic source lock",
                example.name
            ),
        }

        let locked = plan_for_build(matrix.env(), matrix.request(example, false), &matrix.output_dir)
            .unwrap_or_else(|error| panic!("{}: plan from written build lock: {error:#}", example.name));
        assert_eq!(
            locked.lock_outcome, None,
            "{}: the second plan must consume, not regenerate, build.lock.glu",
            example.name
        );
        assert_eq!(
            locked.plan.canonical_bytes(),
            first_plan_bytes,
            "{}: canonical plan bytes changed when reusing the build lock",
            example.name
        );
        assert_eq!(
            locked.plan.derivation_id(),
            first_derivation_id,
            "{}: derivation identity changed when reusing the build lock",
            example.name
        );
        assert_eq!(
            fs::read(&locked.lock_path).unwrap(),
            first_lock_bytes,
            "{}: consuming the build lock changed its canonical bytes",
            example.name
        );
    }

    assert_factory_override_changes_frozen_identity(&matrix);
}
