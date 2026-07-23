const HOOKS_ARCHIVE_URL: &str = "https://fixtures.invalid/sources/cast-hooks-fixture-1.0.0.tar.xz";
const HOOKS_PATCH_URL: &str = "https://fixtures.invalid/sources/cast-hooks-fixture-1.0.0-pre-setup.patch";
const HOOKS_PATCH_ARTIFACT: &str = "cast-hooks-fixture-1.0.0-pre-setup.patch";
const HOOKS_PATCH_MATERIALIZATION: &str = "pre-setup.patch";
const HOOKS_PATCH_BYTES: &[u8] = br#"--- a/hello.c
+++ b/hello.c
@@ -6,3 +6,3 @@
 int main(void) {
-    return puts("pre_setup hook missing") == EOF;
+    return puts("pre_setup hook applied") == EOF;
 }
"#;

fn execution_source_cache_path(cache: &Path, url: &str, sha256: &str) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update(url.as_bytes());
    hasher.update(sha256.as_bytes());
    let key = hex::encode(hasher.finalize());
    cache.join("fetched").join(&key[..5]).join(&key[key.len() - 5..]).join(key)
}

#[test]
fn offline_execution_fixture_archives_are_real_locked_and_complete() {
    let temporary = crate::private_tempdir();
    let cache = temporary.path().join("source-cache");
    let shared = temporary.path().join("shared");
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/gluon/execution");
    let packages = root.join("packages");
    let archives = root.join("archives");
    let git_bundles = root.join("git-bundles");
    let git_source_trees = root.join("git-source-trees");
    let source_files = root.join("source-files");
    let source_trees = root.join("source-trees");

    let discovered = [&packages, &source_trees].map(|directory| {
        let mut names = fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap())
            .filter(|entry| entry.file_type().unwrap().is_dir())
            .map(|entry| entry.file_name().into_string().unwrap())
            .collect::<Vec<_>>();
        names.sort();
        names
    });
    assert_eq!(discovered[0], EXECUTION_PACKAGE_DIRECTORIES);
    assert_eq!(
        discovered[1],
        [
            "cast-autotools-fixture-1.0.0",
            "cast-autotools-options-fixture-1.0.0",
            "cast-cargo-features-fixture-1.0.0",
            "cast-cargo-fixture-1.0.0",
            "cast-cargo-vendored-fixture-1.0.0",
            "cast-cmake-fixture-1.0.0",
            "cast-custom-fixture-1.0.0",
            "cast-daemon-fixture-1.0.0",
            "cast-desktop-integration-fixture-1.0.0",
            "cast-external-test-vectors-fixture-1.0.0",
            "cast-factory-override-fixture-1.0.0",
            "cast-font-family-fixture-1.0.0",
            "cast-gettext-localization-fixture-1.0.0",
            "cast-go-module-fixture-1.0.0",
            "cast-header-only-library-fixture-1.0.0",
            "cast-hooks-fixture-1.0.0",
            "cast-meson-fixture-1.0.0",
            "cast-multiple-sources-fixture-1.0.0",
            "cast-pgo-workload-fixture-1.0.0",
            "cast-plugin-output-fixture-1.0.0",
            "cast-post-install-smoke-test-fixture-1.0.0",
            "cast-python-module-fixture-1.0.0",
            "cast-split-fixture-1.0.0",
            "cast-system-integration-assets-fixture-1.0.0",
        ]
    );
    let source_file_names = fs::read_dir(&source_files)
        .unwrap()
        .map(|entry| entry.unwrap())
        .filter(|entry| entry.file_type().unwrap().is_file())
        .map(|entry| entry.file_name().into_string().unwrap())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        source_file_names,
        BTreeSet::from([
            EXTERNAL_TEST_VECTORS_RAW_ARTIFACT.to_owned(),
            HOOKS_PATCH_ARTIFACT.to_owned(),
            MULTIPLE_SOURCES_RAW_ARTIFACT.to_owned(),
        ])
    );
    let git_source_tree_names = fs::read_dir(&git_source_trees)
        .unwrap()
        .map(|entry| entry.unwrap())
        .filter(|entry| entry.file_type().unwrap().is_dir())
        .map(|entry| entry.file_name().into_string().unwrap())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        git_source_tree_names,
        BTreeSet::from(["cast-multiple-sources-protocol-1.0.0".to_owned()])
    );

    let mut admitted_source_artifacts = BTreeSet::new();
    let mut admitted_git_bundles = BTreeSet::<String>::new();
    let mut archive_format_counts = [0_usize; 4];
    let mut locked_source_count = 0_usize;
    let mut sourceful_fixtures = 0_usize;
    let mut source_less_fixtures = 0_usize;
    for name in EXECUTION_FIXTURES {
        let recipe_path = execution_fixture_package_directory(name).join("stone.glu");
        let recipe = crate::Recipe::load_authored(&recipe_path)
            .unwrap_or_else(|error| panic!("{name}: evaluate execution fixture: {error:#}"));
        if name == "autotools" {
            assert_autotools_regeneration_fixture_contract(
                &recipe.declaration,
                &source_trees.join("cast-autotools-fixture-1.0.0"),
            );
        }
        if name == "cmake" {
            assert_cmake_zlib_fixture_contract(
                &recipe.declaration,
                &source_trees.join("cast-cmake-fixture-1.0.0"),
            );
        }
        if name == "desktop-integration" {
            assert_desktop_integration_fixture_contract(
                &recipe.declaration,
                &source_trees.join("cast-desktop-integration-fixture-1.0.0"),
            );
        }
        if name == "font-family" {
            assert_font_family_fixture_contract(
                &recipe.declaration,
                &source_trees.join("cast-font-family-fixture-1.0.0"),
            );
        }
        if name == "header-only-library" {
            assert_header_only_fixture_contract(
                &recipe.declaration,
                &source_trees.join("cast-header-only-library-fixture-1.0.0"),
            );
        }
        if name == "gettext-localization" {
            assert_gettext_fixture_contract(
                &recipe.declaration,
                &source_trees.join("cast-gettext-localization-fixture-1.0.0"),
            );
        }
        if name == "go-module" {
            assert_go_module_fixture_contract(
                &recipe.declaration,
                &source_trees.join("cast-go-module-fixture-1.0.0"),
            );
        }
        if name == "python-module" {
            assert_python_module_fixture_contract(
                &recipe.declaration,
                &source_trees.join("cast-python-module-fixture-1.0.0"),
            );
        }
        if name == "pgo-workload" {
            assert_pgo_workload_fixture_contract(
                &recipe.declaration,
                &source_trees.join("cast-pgo-workload-fixture-1.0.0"),
            );
        }
        if name == "relation-policy" {
            assert_relation_policy_fixture_contract(&recipe.declaration);
        }
        if name == "meson" {
            assert_meson_dependency_role_fixture_contract(
                &recipe.declaration,
                &source_trees.join("cast-meson-fixture-1.0.0"),
            );
        }
        if name == "multiple-sources" {
            assert_multiple_sources_authored_trees(&root);
        }
        if name == "post-install-smoke-test" {
            assert_post_install_smoke_fixture_contract(
                &recipe.declaration,
                &source_trees.join("cast-post-install-smoke-test-fixture-1.0.0"),
            );
        }
        if name == "system-integration-assets" {
            assert_system_integration_assets_fixture_contract(
                &recipe.declaration,
                &source_trees.join("cast-system-integration-assets-fixture-1.0.0"),
            );
        }
        if name == "factory-override" {
            let factory = recipe
                .fingerprint
                .imported_modules
                .iter()
                .find(|module| module.logical_name == "factory.glu")
                .expect("factory-override: local Gluon factory is absent from recipe provenance");
            assert_eq!(
                factory.sha256,
                hex::encode(Sha256::digest(
                    fs::read(packages.join(name).join("factory.glu")).unwrap()
                )),
                "factory-override: recipe provenance does not bind the exact imported factory"
            );
            assert_eq!(recipe.declaration.architectures, ["x86_64"]);
            let [StepSpec::CMakeConfigure { flags }] = recipe.declaration.builder.phases.setup.steps.as_slice() else {
                panic!("factory-override: package patch did not select the CMake builder");
            };
            assert_eq!(flags.as_slice(), ["-DCAST_FACTORY_VARIANT=stone-override"]);
        }
        if name == "autotools-options" {
            let [StepSpec::AutotoolsConfigure { flags }] =
                recipe.declaration.builder.phases.setup.steps.as_slice()
            else {
                panic!("autotools-options: expected one structural configure step");
            };
            assert_eq!(flags.as_slice(), ["--enable-stone-message"]);
            assert!(
                recipe.declaration.builder.phases.check.steps.is_empty(),
                "autotools-options: run_tests=false must remove the typed check step"
            );
        }
        if name == "cargo-features" {
            let [StepSpec::CargoBuild { features }] =
                recipe.declaration.builder.phases.build.steps.as_slice()
            else {
                panic!("cargo-features: expected one structural Cargo build step");
            };
            assert_eq!(features.as_slice(), ["fixture-protocol"]);
            let [StepSpec::CargoInstall { binaries }] =
                recipe.declaration.builder.phases.install.steps.as_slice()
            else {
                panic!("cargo-features: expected one structural Cargo install step");
            };
            assert_eq!(
                binaries.as_slice(),
                ["cast-feature-client", "cast-feature-daemon"]
            );
            let [StepSpec::CargoTest { features }] =
                recipe.declaration.builder.phases.check.steps.as_slice()
            else {
                panic!("cargo-features: expected one structural Cargo test step");
            };
            assert_eq!(features.as_slice(), ["fixture-protocol"]);
        }
        if name == "plugin-output" {
            let source_root = source_trees.join("cast-plugin-output-fixture-1.0.0");
            let host_source = fs::read_to_string(source_root.join("host.c")).unwrap();
            let plugin_source = fs::read_to_string(source_root.join("plugin.c")).unwrap();
            let assert_source_fragment_once = |role: &str, source: &str, fragment: &str| {
                assert_eq!(
                    source.matches(fragment).count(),
                    1,
                    "plugin-output: {role} source must contain its exact semantic fragment once: {fragment:?}"
                );
            };
            for fragment in [
                "static const char expected_identity[] =\n    \"cast plugin output fixture: loaded explicitly\";",
                "handle = dlopen(plugin_path, RTLD_NOW | RTLD_LOCAL);",
                "dlerror();\n    symbol = dlsym(handle, \"cast_plugin_output_identity\");\n    error = dlerror();",
                "_Static_assert(sizeof(identity) == sizeof(symbol),\n                   \"function and object pointers must have equal size\");",
                "memcpy(&identity, &symbol, sizeof(identity));",
                "message = identity();\n    if (message == NULL || strcmp(message, expected_identity) != 0) {\n        (void)dlclose(handle);\n        return 1;\n    }",
                "return dlclose(handle) == 0\n        ? 0\n        : report_loader_error(\"dlclose\", dlerror());",
            ] {
                assert_source_fragment_once("host", &host_source, fragment);
            }
            for fragment in [
                "static const char fixture_identity[] =\n    \"cast plugin output fixture: loaded explicitly\";",
                "const char *cast_plugin_output_identity(void)\n{\n    return fixture_identity;\n}",
            ] {
                assert_source_fragment_once("plugin", &plugin_source, fragment);
            }
            assert_eq!(
                host_source
                    .matches("cast plugin output fixture: loaded explicitly")
                    .count(),
                1,
                "plugin-output: host must bind exactly one expected identity"
            );
            assert_eq!(
                plugin_source
                    .matches("cast plugin output fixture: loaded explicitly")
                    .count(),
                1,
                "plugin-output: plugin must expose exactly one identity"
            );
            assert_eq!(
                dependency_names(&recipe.declaration.builder.required_tools),
                ["binary(mkdir)", "binary(cc)", "binary(dash)", "binary(install)"]
            );
            let [StepSpec::Run { program, args }] = recipe.declaration.builder.phases.setup.steps.as_slice() else {
                panic!("plugin-output: expected one structural setup step");
            };
            assert_eq!(program.path, "/usr/bin/mkdir");
            assert_eq!(args.as_slice(), ["-p", "build"]);
            let [
                StepSpec::Run {
                    program: plugin_cc,
                    args: plugin_args,
                },
                StepSpec::Run {
                    program: host_cc,
                    args: host_args,
                },
            ] = recipe.declaration.builder.phases.build.steps.as_slice()
            else {
                panic!("plugin-output: expected two structural compiler steps");
            };
            assert_eq!(plugin_cc.path, "/usr/bin/cc");
            assert_eq!(host_cc.path, "/usr/bin/cc");
            assert_eq!(
                plugin_args.as_slice(),
                [
                    "-std=c11",
                    "-O2",
                    "-g",
                    "-Wall",
                    "-Wextra",
                    "-Werror",
                    "-fstack-protector-strong",
                    "-D_FORTIFY_SOURCE=3",
                    "-fPIC",
                    "-shared",
                    "plugin.c",
                    "-Wl,-soname,cast-plugin-output.so",
                    "-Wl,--build-id=sha1",
                    "-Wl,-z,relro,-z,now",
                    "-Wl,-z,noexecstack",
                    "-Wl,-z,separate-code",
                    "-Wl,--no-undefined",
                    "-o",
                    "build/cast-plugin-output.so",
                ]
            );
            assert_eq!(
                host_args.as_slice(),
                [
                    "-std=c11",
                    "-O2",
                    "-g",
                    "-Wall",
                    "-Wextra",
                    "-Werror",
                    "-fstack-protector-strong",
                    "-D_FORTIFY_SOURCE=3",
                    "-fPIE",
                    "host.c",
                    "-Wl,-pie",
                    "-Wl,--build-id=sha1",
                    "-Wl,-z,relro,-z,now",
                    "-Wl,-z,noexecstack",
                    "-Wl,-z,separate-code",
                    "-Wl,--as-needed",
                    "-ldl",
                    "-o",
                    "build/cast-plugin-host",
                ]
            );
            let [StepSpec::RunBuilt { program, args }] = recipe.declaration.builder.phases.check.steps.as_slice()
            else {
                panic!("plugin-output: expected one descriptor-executed native check step");
            };
            assert_eq!(program.path, "build/cast-plugin-host");
            assert_eq!(args.as_slice(), ["--plugin", "build/cast-plugin-output.so"]);
            let [StepSpec::Shell {
                interpreter,
                declared_programs,
                script,
            }] = recipe.declaration.builder.phases.install.steps.as_slice()
            else {
                panic!("plugin-output: expected one explicit install shell step");
            };
            assert_eq!(interpreter.path, "/usr/bin/dash");
            assert_eq!(
                declared_programs
                    .iter()
                    .map(|program| program.path.as_str())
                    .collect::<Vec<_>>(),
                ["/usr/bin/install"]
            );
            assert_eq!(
                script,
                r#"
install -Dm755 build/cast-plugin-host \
    "${CAST_INSTALL_ROOT}${CAST_BINDIR}/cast-plugin-host"
install -Dm644 build/cast-plugin-output.so \
    "${CAST_INSTALL_ROOT}${CAST_LIBDIR}/cast/plugins/cast-plugin-output.so"
"#
            );
        }
        let lock_path = recipe_path.with_file_name(SOURCE_LOCK_FILE_NAME);
        if name == "generated-config" {
            source_less_fixtures += 1;
            assert!(
                recipe.declaration.sources.is_empty(),
                "generated-config: authored data must remain source-less"
            );
            assert!(
                !lock_path.exists(),
                "generated-config: a source-less fixture must not gain a source lock"
            );
            assert_eq!(
                dependency_names(&recipe.declaration.builder.required_tools),
                ["binary(bash)", "binary(install)"]
            );
            let [StepSpec::Shell {
                interpreter,
                declared_programs,
                script,
            }] = recipe.declaration.builder.phases.install.steps.as_slice()
            else {
                panic!("generated-config: expected one explicit install shell step");
            };
            assert_eq!(interpreter.path, "/usr/bin/bash");
            assert_eq!(
                declared_programs
                    .iter()
                    .map(|program| program.path.as_str())
                    .collect::<Vec<_>>(),
                ["/usr/bin/install"]
            );
            assert!(script.contains("profile = \"stone-native\""));
            assert!(script.contains("${CAST_INSTALL_ROOT}${CAST_DATADIR}/cast/generated-config.conf"));
            continue;
        }
        if name == "generated-shell" {
            source_less_fixtures += 1;
            assert!(
                recipe.declaration.sources.is_empty(),
                "generated-shell: authored script must remain source-less"
            );
            assert!(
                !lock_path.exists(),
                "generated-shell: a source-less fixture must not gain a source lock"
            );
            assert_eq!(
                dependency_names(&recipe.declaration.builder.required_tools),
                ["binary(bash)", "binary(install)"]
            );
            let [StepSpec::Shell {
                interpreter: build_interpreter,
                declared_programs: build_programs,
                script: build_script,
            }] = recipe.declaration.builder.phases.build.steps.as_slice()
            else {
                panic!("generated-shell: expected one explicit authoring shell step");
            };
            assert_eq!(build_interpreter.path, "/usr/bin/bash");
            assert!(build_programs.is_empty());
            assert!(build_script.contains("'#!/usr/bin/bash'"));
            assert!(build_script.contains("'cast-generated-shell: self-test passed'"));
            assert!(build_script.contains("'    exit 64'"));
            let [StepSpec::Shell {
                interpreter: check_interpreter,
                declared_programs: check_programs,
                script: check_script,
            }] = recipe.declaration.builder.phases.check.steps.as_slice()
            else {
                panic!("generated-shell: expected one explicit Bash check step");
            };
            assert_eq!(check_interpreter.path, "/usr/bin/bash");
            assert!(check_programs.is_empty());
            assert!(check_script.contains("source ./cast-generated-shell --self-test"));
            assert!(check_script.contains("status was %s, expected 64"));
            let [StepSpec::Shell {
                interpreter: install_interpreter,
                declared_programs: install_programs,
                script: install_script,
            }] = recipe.declaration.builder.phases.install.steps.as_slice()
            else {
                panic!("generated-shell: expected one explicit install shell step");
            };
            assert_eq!(install_interpreter.path, "/usr/bin/bash");
            assert_eq!(
                install_programs
                    .iter()
                    .map(|program| program.path.as_str())
                    .collect::<Vec<_>>(),
                ["/usr/bin/install"]
            );
            assert_eq!(
                install_script,
                "install -Dm755 cast-generated-shell \"${CAST_INSTALL_ROOT}${CAST_BINDIR}/cast-generated-shell\""
            );
            let [output] = recipe.declaration.outputs.as_slice() else {
                panic!("generated-shell: declaration must have exactly one output");
            };
            assert_eq!(output.name, "out");
            assert_eq!(dependency_names(&output.runtime_inputs), ["binary(bash)"]);
            continue;
        }
        if name == "userspace-profile" {
            source_less_fixtures += 1;
            assert!(
                recipe.declaration.sources.is_empty(),
                "userspace-profile: declaration must remain source-less"
            );
            assert!(
                !lock_path.exists(),
                "userspace-profile: a source-less fixture must not gain a source lock"
            );
            assert!(
                recipe.declaration.builder.required_tools.is_empty(),
                "userspace-profile: declarative composition must not gain build tools"
            );
            for (phase, steps) in [
                ("setup", &recipe.declaration.builder.phases.setup.steps),
                ("build", &recipe.declaration.builder.phases.build.steps),
                ("install", &recipe.declaration.builder.phases.install.steps),
                ("check", &recipe.declaration.builder.phases.check.steps),
                ("workload", &recipe.declaration.builder.phases.workload.steps),
            ] {
                assert!(steps.is_empty(), "userspace-profile: {phase} must remain empty");
            }
            let [output] = recipe.declaration.outputs.as_slice() else {
                panic!("userspace-profile: declaration must have exactly one output");
            };
            assert_eq!(output.name, "out");
            assert_eq!(
                dependency_names(&output.runtime_inputs),
                ["bash", "uutils-coreutils", "findutils", "ca-certificates", "xz"]
            );
            continue;
        }
        if name == "relation-policy" {
            source_less_fixtures += 1;
            assert!(
                !lock_path.exists(),
                "relation-policy: a source-less fixture must not gain a source lock"
            );
            continue;
        }
        sourceful_fixtures += 1;
        let lock_bytes = fs::read(&lock_path).unwrap();
        let lock = decode_source_lock(SOURCE_LOCK_FILE_NAME, &lock_bytes)
            .unwrap_or_else(|error| panic!("{name}: decode source lock: {error:#}"));
        lock.validate_against(&recipe.declaration.sources)
            .unwrap_or_else(|error| panic!("{name}: validate source lock: {error:#}"));
        assert_eq!(
            lock_bytes,
            encode_source_lock(&lock).into_bytes(),
            "{name}: checked-in source lock is not canonical"
        );
        if name == "external-test-vectors" {
            assert_external_test_vectors_fixture_contract(
                &recipe.declaration,
                &source_trees.join("cast-external-test-vectors-fixture-1.0.0"),
                &source_files.join(EXTERNAL_TEST_VECTORS_RAW_ARTIFACT),
                &lock,
            );
        }

        let expected_sources = match name {
            "external-test-vectors" => 2,
            "hooks-patch" => 2,
            "multiple-sources" => 3,
            _ => 1,
        };
        assert_eq!(
            lock.sources.len(),
            expected_sources,
            "{name}: locked-source cardinality drift"
        );
        if name == "hooks-patch" {
            validate_hooks_patch_source_contract(&recipe.declaration.sources, &lock)
                .unwrap_or_else(|error| panic!("hooks-patch: invalid external patch contract: {error}"));
        }
        if name == "multiple-sources" {
            validate_multiple_sources_contract(&recipe.declaration, &lock)
                .unwrap_or_else(|error| panic!("multiple-sources: invalid source contract: {error}"));
        }

        let mut locked_sources = Vec::with_capacity(lock.sources.len());
        let mut source_artifacts: Vec<Option<(PathBuf, Vec<u8>)>> = Vec::with_capacity(lock.sources.len());
        for (index, (resolution, declaration)) in lock
            .sources
            .iter()
            .zip(&recipe.declaration.sources)
            .enumerate()
        {
            if let SourceResolution::Git(source) = resolution {
                assert_eq!(name, "multiple-sources");
                assert_eq!(index, 1);
                let UpstreamSpec::Git { git_ref, .. } = declaration else {
                    panic!("multiple-sources: Git lock no longer matches a Git declaration");
                };
                let bundle_path = git_bundles.join(MULTIPLE_SOURCES_GIT_BUNDLE);
                let metadata = fs::symlink_metadata(&bundle_path).unwrap();
                assert!(metadata.file_type().is_file() && metadata.nlink() == 1);
                let bytes = fs::read(&bundle_path).unwrap();
                assert_eq!(metadata.len(), u64::try_from(bytes.len()).unwrap());
                assert_eq!(hex::encode(Sha256::digest(&bytes)), MULTIPLE_SOURCES_GIT_BUNDLE_SHA256);
                assert!(admitted_git_bundles.insert(MULTIPLE_SOURCES_GIT_BUNDLE.to_owned()));
                let locked = stone_recipe::derivation::LockedSource::Git {
                    order: source.order,
                    url: source.url.clone(),
                    requested_ref: git_ref.clone(),
                    commit: source.commit.clone(),
                    materialization_sha256: source.materialization_sha256.clone(),
                    directory: declaration.materialization_name().unwrap(),
                };
                crate::upstream::import_locked_git_fixture(
                    &locked,
                    &cache,
                    &bundle_path,
                    SOURCE_DATE_EPOCH,
                )
                .unwrap_or_else(|error| panic!("multiple-sources: import locked Git bundle: {error:?}"));
                locked_sources.push(locked);
                source_artifacts.push(None);
                continue;
            }
            let SourceResolution::Archive(source) = resolution else {
                panic!("{name}: execution source {index} must remain archive-kind");
            };
            let url = Url::parse(&source.url).unwrap();
            assert_eq!(
                url.scheme(),
                "https",
                "{name}: production source policy must remain HTTPS"
            );
            assert_eq!(url.host_str(), Some("fixtures.invalid"));
            let filename = url.path_segments().unwrap().next_back().unwrap();
            let artifact_path = archives.join(filename);
            let metadata = fs::symlink_metadata(&artifact_path).unwrap();
            assert!(
                metadata.file_type().is_file(),
                "{name}: source {index} must be a regular file"
            );
            let bytes = fs::read(&artifact_path).unwrap();
            assert_eq!(metadata.len(), u64::try_from(bytes.len()).unwrap());
            assert!(
                (1..=1024 * 1024).contains(&metadata.len()),
                "{name}: encoded source {index} must remain small and non-empty"
            );
            assert_eq!(hex::encode(Sha256::digest(&bytes)), source.sha256);
            assert!(
                admitted_source_artifacts.insert(filename.to_owned()),
                "duplicate execution source artifact"
            );

            let materialization_name = declaration.materialization_name().unwrap();
            let locked = stone_recipe::derivation::LockedSource::Archive {
                order: u32::try_from(index).unwrap(),
                url: source.url.clone(),
                sha256: source.sha256.clone(),
                filename: materialization_name,
            };
            crate::upstream::import_locked_archive_fixture(&locked, &cache, &artifact_path).unwrap_or_else(|error| {
                panic!("{name}: import locked source {index} into source cache: {error:#}")
            });
            locked_sources.push(locked);
            source_artifacts.push(Some((artifact_path, bytes)));
        }
        if name == "external-test-vectors" {
            let (_, raw_bytes) = source_artifacts[1].as_ref().unwrap();
            assert_eq!(raw_bytes.as_slice(), EXTERNAL_TEST_VECTORS_RAW_BYTES);
            assert_eq!(
                fs::read(source_files.join(EXTERNAL_TEST_VECTORS_RAW_ARTIFACT)).unwrap(),
                EXTERNAL_TEST_VECTORS_RAW_BYTES
            );
        }
        locked_source_count += locked_sources.len();

        let share = shared.join(name);
        crate::upstream::sync_locked(&locked_sources, &cache, &share, SOURCE_DATE_EPOCH).unwrap_or_else(|error| {
            panic!("{name}: share imported fixtures through frozen source path: {error:?}")
        });
        for (index, (locked, artifact)) in locked_sources.iter().zip(&source_artifacts).enumerate() {
            if let stone_recipe::derivation::LockedSource::Git { directory, .. } = locked {
                assert!(artifact.is_none());
                assert_eq!(name, "multiple-sources");
                assert_eq!(directory, "vendor-protocol");
                assert!(share.join(directory).is_dir());
                continue;
            }
            let (artifact_path, bytes) = artifact
                .as_ref()
                .unwrap_or_else(|| panic!("{name}: archive source {index} has no admitted artifact"));
            let stone_recipe::derivation::LockedSource::Archive {
                url,
                sha256,
                filename,
                ..
            } = locked
            else {
                panic!("{name}: locked source {index} stopped being archive-kind");
            };
            let cached = execution_source_cache_path(&cache, url, sha256);
            let build_visible = share.join(filename);
            assert_eq!(fs::read(&cached).unwrap(), *bytes);
            assert_eq!(fs::read(&build_visible).unwrap(), *bytes);

            let fixture_metadata = fs::metadata(artifact_path).unwrap();
            let cached_metadata = fs::metadata(&cached).unwrap();
            let shared_metadata = fs::metadata(&build_visible).unwrap();
            let fixture_inode = (fixture_metadata.dev(), fixture_metadata.ino());
            let cached_inode = (cached_metadata.dev(), cached_metadata.ino());
            let shared_inode = (shared_metadata.dev(), shared_metadata.ino());
            assert_ne!(
                fixture_inode, cached_inode,
                "{name}: source {index} cache entry must not alias the tracked fixture"
            );
            assert_ne!(
                cached_inode, shared_inode,
                "{name}: source {index} build-visible input must not alias the verified cache"
            );
            assert_ne!(
                fixture_inode, shared_inode,
                "{name}: source {index} build-visible input must not alias the tracked fixture"
            );
        }

        if name == "hooks-patch" {
            let (patch_path, patch_bytes) = source_artifacts[1].as_ref().unwrap();
            assert_eq!(patch_path.file_name().unwrap(), HOOKS_PATCH_ARTIFACT);
            assert_eq!(patch_bytes, HOOKS_PATCH_BYTES);
            assert_eq!(
                fs::read(source_files.join(HOOKS_PATCH_ARTIFACT)).unwrap(),
                HOOKS_PATCH_BYTES
            );
            assert!(
                !source_trees
                    .join("cast-hooks-fixture-1.0.0/packaging")
                    .exists(),
                "hooks-patch: removed packaging tree must not affect deterministic archive bytes"
            );
        }

        let SourceResolution::Archive(primary_source) = &lock.sources[0] else {
            panic!("{name}: primary source must remain archive-kind");
        };
        let primary_url = Url::parse(&primary_source.url).unwrap();
        let primary_filename = primary_url.path_segments().unwrap().next_back().unwrap();
        let primary_bytes = &source_artifacts[0].as_ref().unwrap().1;
        let mut decoder: Box<dyn Read + '_> = match name {
            "cargo-vendored" => {
                assert_eq!(primary_filename, "cast-cargo-vendored-fixture-1.0.0.tar.gz");
                assert!(
                    primary_bytes.starts_with(&[0x1f, 0x8b, 0x08]),
                    "{name}: missing gzip magic"
                );
                archive_format_counts[1] += 1;
                Box::new(flate2::read::GzDecoder::new(primary_bytes.as_slice()))
            }
            "hooks-patch" | "multiple-sources" => {
                let expected = if name == "hooks-patch" {
                    "cast-hooks-fixture-1.0.0.tar.xz"
                } else {
                    "cast-multiple-sources-fixture-1.0.0.tar.xz"
                };
                assert_eq!(primary_filename, expected);
                assert!(
                    primary_bytes.starts_with(&[0xfd, b'7', b'z', b'X', b'Z', 0]),
                    "{name}: missing XZ magic"
                );
                archive_format_counts[2] += 1;
                Box::new(xz2::read::XzDecoder::new(primary_bytes.as_slice()))
            }
            "daemon-generated" | "go-module" => {
                let expected = if name == "daemon-generated" {
                    "cast-daemon-fixture-1.0.0.tar.zst"
                } else {
                    "cast-go-module-fixture-1.0.0.tar.zst"
                };
                assert_eq!(primary_filename, expected);
                assert!(
                    primary_bytes.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]),
                    "{name}: missing Zstandard magic"
                );
                archive_format_counts[3] += 1;
                Box::new(zstd::stream::read::Decoder::new(primary_bytes.as_slice()).unwrap())
            }
            _ => {
                assert!(
                    primary_filename.ends_with(".tar"),
                    "{name}: expected a plain .tar fixture"
                );
                archive_format_counts[0] += 1;
                Box::new(std::io::Cursor::new(primary_bytes.as_slice()))
            }
        };
        let mut tar_bytes = Vec::new();
        decoder
            .by_ref()
            .take(1024 * 1024 + 1)
            .read_to_end(&mut tar_bytes)
            .unwrap_or_else(|error| panic!("{name}: decode execution fixture archive: {error}"));
        assert!(
            (10_240..=1024 * 1024).contains(&tar_bytes.len()) && tar_bytes.len() % 512 == 0,
            "{name}: decoded fixture must remain one small block-aligned tar stream"
        );
        assert_eq!(
            &tar_bytes[257..263],
            b"ustar\0",
            "{name}: decoded fixture is not a USTAR archive"
        );
        // Exercise the same structural two-pass extractor and atomic
        // publication path used by a real build. In particular, the four
        // compressed fixtures must not be accepted on filename or magic alone.
        let build = temporary.path().join("extracted").join(name);
        fs::create_dir_all(&build).unwrap();
        let mut archive_session = crate::archive::ArchiveSessionBudget::production();
        let primary_materialization = recipe.declaration.sources[0].materialization_name().unwrap();
        crate::archive::extract_locked_tar(
            &share,
            &primary_materialization,
            &primary_source.sha256,
            &build,
            "source",
            1,
            SOURCE_DATE_EPOCH,
            &mut archive_session,
        )
        .unwrap_or_else(|error| panic!("{name}: structurally extract and publish locked fixture: {error:#}"));
        let published = build.join("source");
        assert!(published.is_dir(), "{name}: extractor did not publish its destination");
        assert!(
            fs::read_dir(&published).unwrap().next().is_some(),
            "{name}: extractor published an empty source tree"
        );
        if name == "hooks-patch" {
            assert!(
                !published.join("packaging").exists(),
                "hooks-patch: primary archive unexpectedly retained the removed packaging tree"
            );
            assert_eq!(fs::read(share.join(HOOKS_PATCH_MATERIALIZATION)).unwrap(), HOOKS_PATCH_BYTES);
        }
        if name == "cmake" {
            assert_cmake_zlib_archive_matches_tracked_sources(
                &source_trees.join("cast-cmake-fixture-1.0.0"),
                &published,
            );
        }
        if name == "desktop-integration" {
            assert_desktop_integration_archive_matches_tracked_sources(
                &source_trees.join("cast-desktop-integration-fixture-1.0.0"),
                &published,
            );
        }
        if name == "external-test-vectors" {
            assert_external_test_vectors_archive_matches_tracked_sources(
                &source_trees.join("cast-external-test-vectors-fixture-1.0.0"),
                &published,
            );
        }
        if name == "font-family" {
            assert_font_family_archive_matches_tracked_sources(
                &source_trees.join("cast-font-family-fixture-1.0.0"),
                &published,
            );
        }
        if name == "autotools" {
            assert_autotools_regeneration_archive_matches_tracked_sources(
                &source_trees.join("cast-autotools-fixture-1.0.0"),
                &published,
            );
        }
        if name == "meson" {
            assert_meson_dependency_role_archive_matches_tracked_sources(
                &source_trees.join("cast-meson-fixture-1.0.0"),
                &published,
            );
        }
        if name == "header-only-library" {
            assert_header_only_archive_matches_tracked_sources(
                &source_trees.join("cast-header-only-library-fixture-1.0.0"),
                &published,
            );
        }
        if name == "post-install-smoke-test" {
            assert_post_install_smoke_archive_matches_tracked_sources(
                &source_trees.join("cast-post-install-smoke-test-fixture-1.0.0"),
                &published,
            );
        }
        if name == "multiple-sources" {
            assert_multiple_sources_materializations(&root, &published, &share);
        }
        if name == "gettext-localization" {
            assert_gettext_archive_matches_tracked_sources(
                &source_trees.join("cast-gettext-localization-fixture-1.0.0"),
                &published,
            );
        }
        if name == "go-module" {
            assert_go_module_archive_matches_tracked_sources(
                &source_trees.join("cast-go-module-fixture-1.0.0"),
                &published,
            );
        }
        if name == "python-module" {
            assert_python_module_archive_matches_tracked_sources(
                &source_trees.join("cast-python-module-fixture-1.0.0"),
                &published,
            );
        }
        if name == "pgo-workload" {
            assert_pgo_workload_archive_matches_tracked_sources(
                &source_trees.join("cast-pgo-workload-fixture-1.0.0"),
                &published,
            );
        }
        if name == "system-integration-assets" {
            assert_system_integration_assets_archive_matches_tracked_sources(
                &source_trees.join("cast-system-integration-assets-fixture-1.0.0"),
                &published,
            );
        }
    }

    let present_source_artifacts = fs::read_dir(archives)
        .unwrap()
        .map(|entry| entry.unwrap())
        .filter(|entry| entry.file_type().unwrap().is_file())
        .map(|entry| entry.file_name().into_string().unwrap())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        present_source_artifacts, admitted_source_artifacts,
        "orphaned execution fixture source artifact"
    );
    let present_git_bundles = fs::read_dir(git_bundles)
        .unwrap()
        .map(|entry| entry.unwrap())
        .filter(|entry| entry.file_type().unwrap().is_file())
        .map(|entry| entry.file_name().into_string().unwrap())
        .collect::<BTreeSet<_>>();
    assert_eq!(present_git_bundles, admitted_git_bundles, "orphaned execution Git bundle");
    assert_eq!(locked_source_count, 28, "locked execution source inventory drift");
    assert_eq!(
        archive_format_counts,
        [19, 1, 2, 2],
        "execution fixtures must cover nineteen plain tar streams, two XZ, one gzip, and two Zstandard"
    );
    assert_eq!(sourceful_fixtures, 24, "execution source inventory drift");
    assert_eq!(source_less_fixtures, 4, "source-less execution fixture inventory drift");
}
