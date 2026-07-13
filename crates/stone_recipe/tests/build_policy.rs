// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use gluon_config::Source;
use stone_recipe::build_policy::{
    AnalyzerKind, BUILD_POLICY_ABI_VERSION, BuildPolicyConversionError, BuildPolicySpec, BuildToolSpec, ContextValue,
    EnvironmentBindingSpec, EnvironmentCondition, SandboxCredentialPolicySpec, SandboxDevPolicySpec,
    SandboxSysPolicySpec, SandboxTmpPolicySpec, TargetEmulationSpec, TextSpec, evaluate_gluon,
    evaluate_gluon_with_inputs,
};

fn repository_policy() -> Source {
    Source::new(
        "crates/mason/data/policy/default.glu",
        include_str!("../../mason/data/policy/default.glu"),
    )
}

fn repository_policy_value() -> BuildPolicySpec {
    evaluate_gluon(&repository_policy()).unwrap().policy
}

fn environment_binding<'a>(bindings: &'a [EnvironmentBindingSpec], name: &str) -> &'a EnvironmentBindingSpec {
    bindings.iter().find(|binding| binding.name == name).unwrap()
}

#[test]
fn evaluates_repository_build_policy_as_typed_data() {
    let evaluated = evaluate_gluon(&repository_policy()).unwrap();
    let policy = evaluated.policy;

    assert_eq!(policy.build_subdir, "aerynos-builddir");
    assert_eq!(policy.targets.len(), 6);
    assert_eq!(policy.targets[0].target_triple, "x86_64-unknown-linux-gnu");
    assert_eq!(policy.targets[2].lib_suffix, "32");
    assert_eq!(policy.builders.cmake.setup.program.path, "/usr/bin/cmake");
    assert_eq!(
        policy.builders.cmake.setup.program.requirement,
        BuildToolSpec::Binary("cmake".to_owned())
    );
    assert!(
        policy
            .builders
            .cmake
            .setup
            .args
            .contains(&TextSpec::Context(ContextValue::BuilderDir))
    );
    assert_eq!(policy.pgo.stage_one.finish.as_ref().unwrap().inputs.len(), 1);
    assert_eq!(policy.pgo.stage_two.finish.as_ref().unwrap().inputs.len(), 2);
    assert_eq!(policy.pgo.shell_interpreter.path, "/usr/bin/bash");
    assert_eq!(policy.pgo.merge_program.path, "/usr/bin/llvm-profdata");
    assert_eq!(
        policy.pgo.merge_args,
        [
            TextSpec::Literal("merge".to_owned()),
            TextSpec::Literal("--failure-mode=all".to_owned()),
        ]
    );
    assert_eq!(policy.pgo.copy_program.path, "/usr/bin/cp");
    assert_eq!(policy.pgo.remove_program.path, "/usr/bin/rm");
    assert_eq!(policy.sandbox.filesystems.tmp, SandboxTmpPolicySpec::Empty);
    assert_eq!(policy.sandbox.filesystems.sys, SandboxSysPolicySpec::None);
    assert_eq!(policy.sandbox.filesystems.dev, SandboxDevPolicySpec::Minimal);
    assert_eq!(policy.sandbox.credentials, SandboxCredentialPolicySpec::IsolatedRoot);
    assert_eq!(
        policy.analyzers,
        [
            AnalyzerKind::IgnoreBlocked,
            AnalyzerKind::Binary,
            AnalyzerKind::Elf,
            AnalyzerKind::PkgConfig,
            AnalyzerKind::Python,
            AnalyzerKind::CMake,
            AnalyzerKind::CompressMan,
            AnalyzerKind::IncludeAny,
        ]
    );
    assert_eq!(
        evaluated.fingerprint.imported_modules[0].logical_name,
        "cast.build_policy.v3"
    );
}

#[test]
fn build_policy_v3_is_a_hard_abi_boundary() {
    assert_eq!(BUILD_POLICY_ABI_VERSION, 3);
    for retired in ["boulder.build_policy.v3", "cast.build_policy.v2"] {
        let error = evaluate_gluon(&Source::new("retired-policy.glu", format!("import! {retired}"))).unwrap_err();
        assert!(error.to_string().contains(retired));
    }
}

#[test]
fn restricted_dev_alternative_is_valid_and_changes_policy_identity() {
    let original = evaluate_gluon(&repository_policy()).unwrap();
    let alternative_source = include_str!("../../mason/data/policy/default.glu").replace(
        "dev = p.sandbox_filesystems.dev.minimal",
        "dev = p.sandbox_filesystems.dev.none",
    );
    let alternative = evaluate_gluon(&Source::new("crates/mason/data/policy/default.glu", alternative_source)).unwrap();

    assert_eq!(alternative.policy.sandbox.filesystems.tmp, SandboxTmpPolicySpec::Empty);
    assert_eq!(alternative.policy.sandbox.filesystems.sys, SandboxSysPolicySpec::None);
    assert_eq!(alternative.policy.sandbox.filesystems.dev, SandboxDevPolicySpec::None);
    alternative.policy.validate().unwrap();
    assert_ne!(original.fingerprint.sha256, alternative.fingerprint.sha256);
}

#[test]
fn legacy_read_only_proc_selector_is_not_available() {
    let legacy_source = include_str!("../../mason/data/policy/default.glu").replace(
        "tmp = p.sandbox_filesystems.tmp.empty",
        "proc = p.sandbox_filesystems.proc.read_only,\n        tmp = p.sandbox_filesystems.tmp.empty",
    );

    let error = evaluate_gluon(&Source::new("crates/mason/data/policy/default.glu", legacy_source)).unwrap_err();

    assert!(error.to_string().contains("proc"));
}

#[test]
fn explicit_inputs_participate_in_policy_identity() {
    let source = repository_policy();
    let first = evaluate_gluon_with_inputs(&gluon_config::Evaluator::default(), &source, b"first").unwrap();
    let second = evaluate_gluon_with_inputs(&gluon_config::Evaluator::default(), &source, b"second").unwrap();

    assert_ne!(first.fingerprint.sha256, second.fingerprint.sha256);
}

#[test]
fn duplicate_targets_are_rejected_semantically() {
    let mut policy = repository_policy_value();
    policy.targets.push(policy.targets[0].clone());

    assert!(matches!(
        policy.validate(),
        Err(BuildPolicyConversionError::Duplicate { field, value })
            if field == "targets" && value == "x86_64"
    ));
}

#[test]
fn active_target_catalog_must_not_be_empty() {
    let mut policy = repository_policy_value();
    policy.targets.clear();

    assert!(matches!(
        policy.validate(),
        Err(BuildPolicyConversionError::Empty { field }) if field == "targets"
    ));
}

#[test]
fn analyzer_catalog_must_not_be_empty() {
    let mut policy = repository_policy_value();
    policy.analyzers.clear();

    assert!(matches!(
        policy.validate(),
        Err(BuildPolicyConversionError::Empty { field }) if field == "analyzers"
    ));
}

#[test]
fn analyzer_catalog_is_unique() {
    let mut policy = repository_policy_value();
    policy.analyzers.insert(2, AnalyzerKind::Binary);

    assert!(matches!(
        policy.validate(),
        Err(BuildPolicyConversionError::Duplicate { field, value })
            if field == "analyzers" && value == "Binary"
    ));
}

#[test]
fn analyzer_tools_must_be_safe_executable_capabilities() {
    type SelectTool = fn(&mut BuildPolicySpec) -> &mut BuildToolSpec;
    let tools: [(&str, SelectTool); 6] = [
        ("build_root.analyzer_tools.pkg_config", |policy| {
            &mut policy.build_root.analyzer_tools.pkg_config
        }),
        ("build_root.analyzer_tools.python", |policy| {
            &mut policy.build_root.analyzer_tools.python
        }),
        ("build_root.analyzer_tools.llvm.objcopy", |policy| {
            &mut policy.build_root.analyzer_tools.llvm.objcopy
        }),
        ("build_root.analyzer_tools.llvm.strip", |policy| {
            &mut policy.build_root.analyzer_tools.llvm.strip
        }),
        ("build_root.analyzer_tools.gnu.objcopy", |policy| {
            &mut policy.build_root.analyzer_tools.gnu.objcopy
        }),
        ("build_root.analyzer_tools.gnu.strip", |policy| {
            &mut policy.build_root.analyzer_tools.gnu.strip
        }),
    ];

    for (field, select) in tools {
        let mut package_capability = repository_policy_value();
        *select(&mut package_capability) = BuildToolSpec::Package("tool-package".to_owned());
        assert!(matches!(
            package_capability.validate(),
            Err(BuildPolicyConversionError::AnalyzerToolMustBeExecutable { field: actual })
                if actual == field
        ));

        let mut unsafe_executable = repository_policy_value();
        *select(&mut unsafe_executable) = BuildToolSpec::Binary("../tool".to_owned());
        assert!(matches!(
            unsafe_executable.validate(),
            Err(BuildPolicyConversionError::InvalidAnalyzerExecutable { field: actual, value })
                if actual == field && value == "../tool"
        ));
    }
}

#[test]
fn include_any_is_required_exactly_once_and_last() {
    let mut missing = repository_policy_value();
    missing.analyzers.pop();
    assert!(matches!(
        missing.validate(),
        Err(BuildPolicyConversionError::MissingRequired { field, value })
            if field == "analyzers" && value == "IncludeAny"
    ));

    let mut misplaced = repository_policy_value();
    misplaced.analyzers.swap(0, 7);
    assert!(matches!(
        misplaced.validate(),
        Err(BuildPolicyConversionError::MustBeLast { field, value })
            if field == "analyzers" && value == "IncludeAny"
    ));

    let mut duplicate = repository_policy_value();
    duplicate.analyzers.push(AnalyzerKind::IncludeAny);
    assert!(matches!(
        duplicate.validate(),
        Err(BuildPolicyConversionError::Duplicate { field, value })
            if field == "analyzers" && value == "IncludeAny"
    ));
}

#[test]
fn target_names_accept_normalized_safe_relative_build_paths() {
    for name in ["x86_64", "x86_64-v3x", "emul32/x86_64", "tier/.hidden"] {
        let mut policy = repository_policy_value();
        policy.targets.truncate(1);
        policy.targets[0].name = name.to_owned();

        policy
            .validate()
            .unwrap_or_else(|error| panic!("target name `{name}` was rejected: {error}"));
    }
}

#[test]
fn target_names_reject_unsafe_or_non_normalized_paths() {
    for name in [
        "",
        "/x86_64",
        "//x86_64",
        "emul32//x86_64",
        "emul32/",
        "./x86_64",
        "emul32/./x86_64",
        ".",
        "../x86_64",
        "emul32/../x86_64",
        "..",
    ] {
        let mut policy = repository_policy_value();
        policy.targets.truncate(1);
        policy.targets[0].name = name.to_owned();

        assert!(
            matches!(
                policy.validate(),
                Err(BuildPolicyConversionError::InvalidTargetName { field, value })
                    if field == "targets[0].name" && value == name
            ),
            "target name `{name}` was not rejected as an invalid target path"
        );
    }
}

#[test]
fn unsupported_artifact_architectures_are_rejected_before_planning() {
    let mut policy = repository_policy_value();
    let target = &mut policy.targets[0];
    target.artifact_architecture = "loongarch64".to_owned();
    target.build_platform.architecture = "loongarch64".to_owned();
    target.host_platform.architecture = "loongarch64".to_owned();
    target.target_platform.architecture = "loongarch64".to_owned();

    assert!(matches!(
        policy.validate(),
        Err(BuildPolicyConversionError::UnsupportedArtifactArchitecture {
            field,
            value,
            supported,
        }) if field == "targets[0].artifact_architecture"
            && value == "loongarch64"
            && supported == "x86_64, x86, aarch64, riscv64"
    ));
}

#[test]
fn retired_target_names_obey_the_same_path_invariant() {
    let mut policy = repository_policy_value();
    policy.retired_targets[0].name = "retired/../active".to_owned();

    assert!(matches!(
        policy.validate(),
        Err(BuildPolicyConversionError::InvalidTargetName { field, value })
            if field == "retired_targets[0].name" && value == "retired/../active"
    ));
}

#[test]
fn retired_and_active_target_names_share_one_unique_namespace() {
    let mut policy = repository_policy_value();
    policy.retired_targets[0].name = policy.targets[0].name.clone();

    assert!(matches!(
        policy.validate(),
        Err(BuildPolicyConversionError::Duplicate { field, value })
            if field == "targets" && value == "x86_64"
    ));

    let mut policy = repository_policy_value();
    policy.retired_targets.push(policy.retired_targets[0].clone());

    assert!(matches!(
        policy.validate(),
        Err(BuildPolicyConversionError::Duplicate { field, value })
            if field == "targets" && value == "x86_64-stage1"
    ));
}

#[test]
fn general_tuning_catalog_preserves_all_names_choices_and_defaults() {
    let policy = repository_policy_value();
    let tuning = policy.tuning;

    let flag_names = tuning.flags.iter().map(|flag| flag.name.as_str()).collect::<Vec<_>>();
    assert_eq!(
        flag_names,
        [
            "architecture",
            "base",
            "omit-frame-pointer",
            "no-omit-frame-pointer",
            "bindnow",
            "symbolic-all",
            "symbolic-functions",
            "symbolic-nonweak",
            "fortify-lvl1",
            "fortify-lvl2",
            "fortify-lvl3",
            "harden-none",
            "harden-lvl1",
            "harden-lvl2",
            "control-flow",
            "libstdc-assertions",
            "thread-exceptions",
            "sections",
            "optimize-fast",
            "optimize-generic",
            "optimize-size",
            "optimize-speed",
            "lto-full",
            "lto-thin",
            "ltoextra-full",
            "ltoextra-thin",
            "fat-lto",
            "fat-lto-none",
            "lto-errors",
            "build-id",
            "compress-debug-zstd",
            "compress-debug-zlib",
            "compress-debug-none",
            "icf-all",
            "icf-safe",
            "idae",
            "polly",
            "bolt",
            "common",
            "debug-lines",
            "debug-std",
            "math",
            "noplt",
            "nosemantic",
            "nodaed",
            "avxwidth-128",
            "pch-instantiate",
            "asneeded",
            "runpath",
            "sse2avx",
            "visibility-hidden",
            "visibility-inline",
            "relative-vtables",
            "relr",
            "tls-gnu",
            "version-allow-undefined",
            "version-no-undefined",
            "golang-modflags",
            "golang-ldflags",
        ]
    );

    let group_names = tuning
        .groups
        .iter()
        .map(|group| group.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        group_names,
        [
            "architecture",
            "base",
            "debug",
            "compress-debug",
            "frame-pointer",
            "build-id",
            "bindnow",
            "symbolic",
            "fortify",
            "harden",
            "control-flow",
            "libstdc-assertions",
            "thread-exceptions",
            "optimize",
            "lto",
            "ltoextra",
            "lto-errors",
            "fat-lto",
            "icf",
            "idae",
            "polly",
            "sections",
            "common",
            "math",
            "noplt",
            "nosemantic",
            "nodaed",
            "asneeded",
            "avxwidth",
            "bolt",
            "runpath",
            "sse2avx",
            "pch-instantiate",
            "visibility",
            "relative-vtables",
            "relr",
            "tls-gnu",
            "version-allow-undefined",
            "golang-ldflags",
            "golang-modflags",
        ]
    );

    let choices = tuning
        .groups
        .iter()
        .filter(|group| !group.value.choices.is_empty())
        .map(|group| {
            (
                group.name.as_str(),
                group.value.default.as_deref(),
                group
                    .value
                    .choices
                    .iter()
                    .map(|choice| choice.name.as_str())
                    .collect::<Vec<_>>(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        choices,
        [
            ("debug", Some("std"), vec!["lines", "std"]),
            ("compress-debug", Some("zstd"), vec!["none", "zlib", "zstd"]),
            ("symbolic", Some("functions"), vec!["all", "functions", "nonweak"]),
            ("fortify", Some("lvl3"), vec!["lvl1", "lvl2", "lvl3"]),
            ("harden", Some("lvl1"), vec!["none", "lvl1", "lvl2"]),
            ("optimize", Some("generic"), vec!["fast", "generic", "size", "speed"]),
            ("lto", Some("thin"), vec!["full", "thin"]),
            ("ltoextra", Some("thin"), vec!["full", "thin"]),
            ("icf", Some("safe"), vec!["safe", "all"]),
            ("visibility", Some("inline"), vec!["inline", "hidden"]),
        ]
    );
    assert_eq!(
        tuning.default_groups,
        [
            "asneeded",
            "avxwidth",
            "base",
            "bindnow",
            "build-id",
            "compress-debug",
            "control-flow",
            "debug",
            "fat-lto",
            "fortify",
            "frame-pointer",
            "golang-ldflags",
            "golang-modflags",
            "harden",
            "icf",
            "libstdc-assertions",
            "lto",
            "lto-errors",
            "optimize",
            "relr",
            "symbolic",
            "thread-exceptions",
            "tls-gnu",
            "version-allow-undefined",
        ]
    );
}

#[test]
fn lto_jobs_are_explicit_typed_context_values() {
    let policy = repository_policy_value();
    let lto_full = &policy
        .tuning
        .flags
        .iter()
        .find(|flag| flag.name == "lto-full")
        .unwrap()
        .value
        .gnu;
    let lto_thin = &policy
        .tuning
        .flags
        .iter()
        .find(|flag| flag.name == "lto-thin")
        .unwrap()
        .value
        .gnu;
    let jobs = TextSpec::Concat(vec![
        TextSpec::Literal("-flto=".to_owned()),
        TextSpec::Context(ContextValue::Jobs),
    ]);

    for values in [&lto_full.c, &lto_full.cxx, &lto_full.f, &lto_full.ld] {
        assert_eq!(
            values,
            &[jobs.clone(), TextSpec::Literal("-flto-partition=one".to_owned())]
        );
    }
    for values in [&lto_thin.c, &lto_thin.cxx, &lto_thin.f, &lto_thin.ld] {
        assert_eq!(values, &[jobs.clone()]);
    }
}

#[test]
fn duplicate_tuning_entries_and_choices_are_rejected() {
    let mut policy = repository_policy_value();
    policy.tuning.flags.push(policy.tuning.flags[0].clone());
    assert!(matches!(
        policy.validate(),
        Err(BuildPolicyConversionError::Duplicate { field, value })
            if field == "tuning.flags" && value == "architecture"
    ));

    let mut policy = repository_policy_value();
    let debug = policy
        .tuning
        .groups
        .iter_mut()
        .find(|group| group.name == "debug")
        .unwrap();
    debug.value.choices.push(debug.value.choices[0].clone());
    assert!(matches!(
        policy.validate(),
        Err(BuildPolicyConversionError::Duplicate { field, value })
            if field == "tuning.groups[2].value.choices" && value == "lines"
    ));
}

#[test]
fn unknown_tuning_references_are_rejected() {
    let mut policy = repository_policy_value();
    policy.tuning.groups[0]
        .value
        .base
        .enabled
        .push("missing-flag".to_owned());
    assert!(matches!(
        policy.validate(),
        Err(BuildPolicyConversionError::UnknownReference { field, value })
            if field == "tuning.groups[0].value.base.enabled[1]" && value == "missing-flag"
    ));

    let mut policy = repository_policy_value();
    policy.tuning.default_groups.push("missing-group".to_owned());
    assert!(matches!(
        policy.validate(),
        Err(BuildPolicyConversionError::UnknownReference { field, value })
            if field == "tuning.default_groups[24]" && value == "missing-group"
    ));
}

#[test]
fn invalid_tuning_defaults_are_rejected() {
    let mut policy = repository_policy_value();
    policy.tuning.groups[2].value.default = Some("missing-choice".to_owned());

    assert!(matches!(
        policy.validate(),
        Err(BuildPolicyConversionError::InvalidDefault { field, value })
            if field == "tuning.groups[2].value.default" && value == "missing-choice"
    ));
}

#[test]
fn repository_build_root_inputs_preserve_relation_kinds_and_conditions() {
    let policy = repository_policy_value();
    let root = policy.build_root;

    assert_eq!(
        root.base,
        [
            "bash",
            "cast",
            "coreutils",
            "dash",
            "diffutils",
            "findutils",
            "gawk",
            "glibc-devel",
            "grep",
            "layout",
            "libarchive",
            "linux-headers",
            "os-info",
            "pkgconf",
            "sed",
            "util-linux",
        ]
        .map(|name| BuildToolSpec::Package(name.to_owned()))
        .into_iter()
        .chain(["git", "hx", "less", "nano", "ps", "rg", "vim"].map(|name| BuildToolSpec::Binary(name.to_owned())),)
        .collect::<Vec<_>>()
    );
    assert_eq!(root.toolchains.llvm, [BuildToolSpec::Package("clang".to_owned())]);
    assert_eq!(
        root.toolchains.gnu,
        ["ld.bfd", "gcc", "g++"].map(|name| BuildToolSpec::Binary(name.to_owned()))
    );
    assert_eq!(
        root.emul32.base,
        [BuildToolSpec::Package("glibc-32bit-devel".to_owned())]
    );
    assert_eq!(
        root.emul32.toolchains.llvm,
        [BuildToolSpec::Package("clang-32bit".to_owned())]
    );
    assert_eq!(
        root.emul32.toolchains.gnu,
        [
            BuildToolSpec::Package("gcc-32bit".to_owned()),
            BuildToolSpec::Package("libstdc++-32bit-devel".to_owned()),
        ]
    );
    assert_eq!(
        root.compiler_cache.required_tools,
        [
            BuildToolSpec::Binary("ccache".to_owned()),
            BuildToolSpec::Binary("sccache".to_owned()),
        ]
    );
    assert_eq!(
        root.analyzer_tools.pkg_config,
        BuildToolSpec::Binary("pkg-config".to_owned())
    );
    assert_eq!(root.analyzer_tools.python, BuildToolSpec::Binary("python3".to_owned()));
    assert_eq!(
        root.analyzer_tools.llvm.objcopy,
        BuildToolSpec::Binary("llvm-objcopy".to_owned())
    );
    assert_eq!(
        root.analyzer_tools.llvm.strip,
        BuildToolSpec::Binary("llvm-strip".to_owned())
    );
    assert_eq!(
        root.analyzer_tools.gnu.objcopy,
        BuildToolSpec::Binary("objcopy".to_owned())
    );
    assert_eq!(root.analyzer_tools.gnu.strip, BuildToolSpec::Binary("strip".to_owned()));
}

#[test]
fn repository_sandbox_and_cache_paths_are_explicit_guest_abi() {
    let policy = repository_policy_value();
    assert_eq!(policy.sandbox.hostname, "cast-builder");
    assert_eq!(policy.sandbox.guest_root, "/mason");
    assert_eq!(policy.sandbox.artifacts_dir, "/mason/artefacts");
    assert_eq!(policy.sandbox.build_dir, "/mason/build");
    assert_eq!(policy.sandbox.source_dir, "/mason/sourcedir");
    assert_eq!(policy.sandbox.recipe_dir, "/mason/recipe");
    assert_eq!(policy.sandbox.package_dir, "/mason/recipe/pkg");
    assert_eq!(policy.sandbox.install_dir, "/mason/install");

    let cache = policy.build_root.compiler_cache;
    assert_eq!(cache.default_path, "/usr/bin:/bin");
    assert_eq!(cache.compiler_path, "/usr/lib/ccache/bin:/usr/bin:/bin");
    assert_eq!(cache.ccache_dir, "/mason/ccache");
    assert_eq!(cache.sccache_dir, "/mason/sccache");
    assert_eq!(cache.go_cache_dir, "/mason/gocache");
    assert_eq!(cache.go_mod_cache_dir, "/mason/gomodcache");
    assert_eq!(cache.cargo_cache_dir, "/mason/cargocache");
    assert_eq!(cache.zig_cache_dir, "/mason/zigcache");
    assert_eq!(cache.rustc_wrapper, "/usr/bin/sccache");
}

#[test]
fn repository_source_preparation_is_argv_preserving_policy() {
    let policy = repository_policy_value();
    let archive = policy.sources.archive;
    assert_eq!(archive.create_directory.program.path, "/usr/bin/mkdir");
    assert_eq!(
        archive.create_directory.program.requirement,
        BuildToolSpec::Binary("mkdir".to_owned())
    );
    assert_eq!(
        archive.create_directory.args,
        [
            TextSpec::Literal("-p".to_owned()),
            TextSpec::Context(ContextValue::SourceDestination),
        ]
    );
    assert_eq!(archive.unpack.program.path, "/usr/bin/bsdtar-static");
    assert_eq!(
        archive.unpack.args,
        [
            TextSpec::Literal("xf".to_owned()),
            TextSpec::Context(ContextValue::SourcePath),
            TextSpec::Literal("-C".to_owned()),
            TextSpec::Context(ContextValue::SourceDestination),
            TextSpec::Concat(vec![
                TextSpec::Literal("--strip-components=".to_owned()),
                TextSpec::Context(ContextValue::SourceStripComponents),
            ]),
            TextSpec::Literal("--no-same-owner".to_owned()),
        ]
    );
    let git = policy.sources.git;
    assert_eq!(git.copy.program.path, "/usr/bin/cp");
    assert_eq!(
        git.copy.args,
        [
            TextSpec::Literal("-Ra".to_owned()),
            TextSpec::Literal("--no-preserve=ownership".to_owned()),
            TextSpec::Concat(vec![
                TextSpec::Context(ContextValue::SourcePath),
                TextSpec::Literal("/.".to_owned()),
            ]),
            TextSpec::Context(ContextValue::SourceDestination),
        ]
    );
}

#[test]
fn repository_mold_policy_owns_linker_closure_and_flags() {
    let mold = repository_policy_value().build_root.mold;
    assert_eq!(mold.required_tools, [BuildToolSpec::Binary("mold".to_owned())]);
    assert_eq!(mold.linker, TextSpec::Literal("ld.mold".to_owned()));
    assert_eq!(mold.flags.c, [TextSpec::Literal("-fuse-ld=mold".to_owned())]);
    assert_eq!(mold.flags.cxx, [TextSpec::Literal("-fuse-ld=mold".to_owned())]);
    assert_eq!(
        mold.flags.rust,
        [TextSpec::Literal("-Clink-arg=-fuse-ld=mold".to_owned())]
    );
    assert!(mold.flags.ld.is_empty());
}

#[test]
fn repository_targets_separate_execution_artifact_and_lock_platforms() {
    let policy = repository_policy_value();
    let names = policy
        .targets
        .iter()
        .map(|target| target.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        ["x86_64", "x86_64-v3x", "x86", "emul32/x86_64", "aarch64", "riscv64"]
    );

    let emul32 = policy
        .targets
        .iter()
        .find(|target| target.name == "emul32/x86_64")
        .unwrap();
    assert_eq!(emul32.artifact_architecture, "x86");
    assert_eq!(
        emul32.emulation,
        TargetEmulationSpec::Emul32 {
            host_architecture: "x86_64".to_owned(),
        }
    );
    assert_eq!(emul32.build_platform.architecture, "x86_64");
    assert_eq!(emul32.host_platform.architecture, "x86");
    assert_eq!(emul32.target_platform.architecture, "x86");
    assert_eq!(emul32.target_platform.vendor, "aerynos");

    let aarch64 = policy.targets.iter().find(|target| target.name == "aarch64").unwrap();
    assert!(aarch64.architecture_flags.common.c.is_empty());
    assert_eq!(
        aarch64.architecture_flags.llvm.c,
        [
            TextSpec::Literal("-march=armv8-a+simd+fp+crypto".to_owned()),
            TextSpec::Literal("-mtune=cortex-a72".to_owned()),
        ]
    );

    assert_eq!(policy.retired_targets.len(), 1);
    assert_eq!(policy.retired_targets[0].name, "x86_64-stage1");
    assert!(policy.retired_targets[0].reason.contains("unreachable"));
}

#[test]
fn repository_environment_preserves_base_and_target_specific_values() {
    let policy = repository_policy_value();

    let expected_normal_pkg_config_path = TextSpec::Concat(vec![
        TextSpec::Context(ContextValue::LibDir),
        TextSpec::Literal("/pkgconfig:/usr/share/pkgconfig".to_owned()),
    ]);
    for target_name in ["x86_64", "x86_64-v3x", "x86", "aarch64", "riscv64"] {
        let target = policy.targets.iter().find(|target| target.name == target_name).unwrap();
        assert_eq!(
            environment_binding(&target.environment, "PKG_CONFIG_PATH").value,
            expected_normal_pkg_config_path
        );
    }

    let x86_64 = policy.targets.iter().find(|target| target.name == "x86_64").unwrap();
    assert_eq!(
        environment_binding(&x86_64.environment, "GOAMD64").value,
        TextSpec::Literal("v2".to_owned())
    );
    let x86_64_v3x = policy
        .targets
        .iter()
        .find(|target| target.name == "x86_64-v3x")
        .unwrap();
    assert_eq!(
        environment_binding(&x86_64_v3x.environment, "GOAMD64").value,
        TextSpec::Literal("v3".to_owned())
    );

    let x86 = policy.targets.iter().find(|target| target.name == "x86").unwrap();
    assert!(!x86.environment.iter().any(|binding| binding.name == "GOAMD64"));
    let emul32 = policy
        .targets
        .iter()
        .find(|target| target.name == "emul32/x86_64")
        .unwrap();
    assert_eq!(
        environment_binding(&emul32.environment, "PKG_CONFIG_PATH").value,
        TextSpec::Literal("/usr/lib32/pkgconfig:/usr/share/pkgconfig:/usr/lib/pkgconfig".to_owned())
    );
    assert_eq!(
        environment_binding(&emul32.environment, "GOAMD64").value,
        TextSpec::Literal("v2".to_owned())
    );

    for target in [x86, emul32] {
        for (name, context) in [
            ("CC", ContextValue::Cc),
            ("CXX", ContextValue::Cxx),
            ("CPP", ContextValue::Cpp),
        ] {
            assert_eq!(
                environment_binding(&target.environment, name).value,
                TextSpec::Concat(vec![TextSpec::Context(context), TextSpec::Literal(" -m32".to_owned()),])
            );
        }
    }

    assert!(
        !policy
            .environment
            .iter()
            .any(|binding| binding.name == "PKG_CONFIG_PATH")
    );
    for (name, context) in [
        ("CGO_CFLAGS", ContextValue::CFlags),
        ("CGO_CXXFLAGS", ContextValue::CxxFlags),
    ] {
        let binding = environment_binding(&policy.environment, name);
        assert_eq!(binding.condition, EnvironmentCondition::Always);
        assert_eq!(binding.value, TextSpec::Context(context));
    }
    assert_eq!(
        environment_binding(&policy.environment, "CGO_LDFLAGS").value,
        TextSpec::Concat(vec![
            TextSpec::Context(ContextValue::LdFlags),
            TextSpec::Literal(" -Wl,--no-gc-sections".to_owned()),
        ])
    );
    assert_eq!(
        environment_binding(&policy.environment, "NINJA_STATUS").value,
        TextSpec::Literal("[%f/%t %es (%P)] ".to_owned())
    );
}

#[test]
fn target_environment_bindings_are_validated_at_the_target_path() {
    let mut policy = repository_policy_value();
    let duplicate = policy.targets[0].environment[0].clone();
    policy.targets[0].environment.push(duplicate);
    assert!(matches!(
        policy.validate(),
        Err(BuildPolicyConversionError::Duplicate { field, value })
            if field == "targets[0].environment" && value == "PKG_CONFIG_PATH"
    ));

    let mut policy = repository_policy_value();
    policy.targets[0].environment[0].value = TextSpec::Literal(String::new());
    assert!(matches!(
        policy.validate(),
        Err(BuildPolicyConversionError::Empty { field })
            if field == "targets[0].environment[0].value"
    ));
}

#[test]
fn pgo_command_policy_is_complete_before_lowering() {
    let mut policy = repository_policy_value();
    policy.pgo.merge_args.clear();
    assert!(matches!(
        policy.validate(),
        Err(BuildPolicyConversionError::Empty { field }) if field == "pgo.merge_args"
    ));

    let mut policy = repository_policy_value();
    policy.pgo.copy_program.path.clear();
    assert!(matches!(
        policy.validate(),
        Err(BuildPolicyConversionError::InvalidProgramPath { field, .. }) if field == "pgo.copy_program.path"
    ));
}

#[test]
fn root_source_sandbox_and_platform_semantics_are_rejected_early() {
    let mut policy = repository_policy_value();
    policy.build_root.base.push(policy.build_root.base[0].clone());
    assert!(matches!(
        policy.validate(),
        Err(BuildPolicyConversionError::Duplicate { field, value })
            if field == "build_root.base" && value == "bash"
    ));

    for path in [
        "relative/build",
        "/",
        "/mason/../escape",
        "/mason/./build",
        "/mason//build",
        "/mason/build/",
    ] {
        let mut policy = repository_policy_value();
        policy.sandbox.build_dir = path.to_owned();
        assert!(matches!(
            policy.validate(),
            Err(BuildPolicyConversionError::InvalidGuestPath { field, value })
                if field == "sandbox.build_dir" && value == path
        ));
    }

    let mut policy = repository_policy_value();
    policy.sandbox.build_dir = "/tmp/build".to_owned();
    assert!(matches!(
        policy.validate(),
        Err(BuildPolicyConversionError::GuestPathOutsideRoot { field, value, guest_root })
            if field == "sandbox.build_dir" && value == "/tmp/build" && guest_root == "/mason"
    ));

    let too_long = "x".repeat(65);
    for hostname in ["", "-cast", "cast-", "bad host", "bad/host", too_long.as_str()] {
        let mut policy = repository_policy_value();
        policy.sandbox.hostname = hostname.to_owned();
        assert!(matches!(
            policy.validate(),
            Err(BuildPolicyConversionError::InvalidHostname { field, value })
                if field == "sandbox.hostname" && value == hostname
        ));
    }

    let mut policy = repository_policy_value();
    policy.build_root.compiler_cache.ccache_dir = policy.sandbox.build_dir.clone();
    assert!(matches!(
        policy.validate(),
        Err(BuildPolicyConversionError::OverlappingGuestPath {
            field,
            other_field,
            ..
        }) if field == "build_root.compiler_cache.ccache_dir" && other_field == "sandbox.build_dir"
    ));

    let mut policy = repository_policy_value();
    policy.sandbox.source_dir = format!("{}/sources", policy.sandbox.build_dir);
    assert!(matches!(
        policy.validate(),
        Err(BuildPolicyConversionError::OverlappingGuestPath {
            field,
            other_field,
            ..
        }) if field == "sandbox.source_dir" && other_field == "sandbox.build_dir"
    ));

    let mut policy = repository_policy_value();
    policy.build_root.compiler_cache.zig_cache_dir = "/outside/zig".to_owned();
    assert!(matches!(
        policy.validate(),
        Err(BuildPolicyConversionError::GuestPathOutsideRoot { field, .. })
            if field == "build_root.compiler_cache.zig_cache_dir"
    ));

    let mut policy = repository_policy_value();
    policy.build_root.compiler_cache.sccache_dir = policy.build_root.compiler_cache.ccache_dir.clone();
    assert!(matches!(
        policy.validate(),
        Err(BuildPolicyConversionError::OverlappingGuestPath {
            field,
            other_field,
            ..
        }) if field == "build_root.compiler_cache.sccache_dir"
            && other_field == "build_root.compiler_cache.ccache_dir"
    ));

    let mut policy = repository_policy_value();
    policy.targets[0].target_platform.vendor = "unknown".to_owned();
    assert!(matches!(
        policy.validate(),
        Err(BuildPolicyConversionError::InvalidPlatformComponent { field, value })
            if field == "targets[0].target_platform.vendor" && value == "unknown"
    ));
}
