// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use gluon_config::Source;
use stone_recipe::build_policy::{
    BuildPolicyConversionError, BuildToolSpec, ContextValue, TargetEmulationSpec, TextSpec, evaluate_gluon,
    evaluate_gluon_with_inputs,
};

fn repository_policy() -> Source {
    Source::new(
        "bin/boulder/data/policy/default.glu",
        include_str!("../../../bin/boulder/data/policy/default.glu"),
    )
}

fn repository_policy_value() -> stone_recipe::build_policy::BuildPolicySpec {
    evaluate_gluon(&repository_policy()).unwrap().policy
}

#[test]
fn evaluates_repository_build_policy_as_typed_data() {
    let evaluated = evaluate_gluon(&repository_policy()).unwrap();
    let policy = evaluated.policy;

    assert_eq!(policy.vendor_id, "aerynos-linux");
    assert_eq!(policy.build_subdir, "aerynos-builddir");
    assert_eq!(policy.targets.len(), 6);
    assert_eq!(policy.targets[0].target_triple, "x86_64-unknown-linux-gnu");
    assert_eq!(policy.targets[2].lib_suffix, "32");
    assert_eq!(
        policy.builders.cmake.required_tools,
        [
            BuildToolSpec::Binary("cmake".to_owned()),
            BuildToolSpec::Binary("ninja".to_owned()),
            BuildToolSpec::Binary("ctest".to_owned()),
        ]
    );
    assert_eq!(
        policy.builders.cmake.setup.program,
        TextSpec::Literal("cmake".to_owned())
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
    assert_eq!(
        evaluated.fingerprint.imported_modules[0].logical_name,
        "boulder.build_policy.v1"
    );
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
            "boulder",
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
}

#[test]
fn repository_sandbox_and_cache_paths_are_explicit_guest_abi() {
    let policy = repository_policy_value();
    assert_eq!(policy.sandbox.guest_root, "/mason");
    assert_eq!(policy.sandbox.artifacts_dir, "/mason/artefacts");
    assert_eq!(policy.sandbox.build_dir, "/mason/build");
    assert_eq!(policy.sandbox.source_dir, "/mason/sourcedir");
    assert_eq!(policy.sandbox.recipe_dir, "/mason/recipe");
    assert_eq!(policy.sandbox.package_dir, "/mason/recipe/pkg");
    assert_eq!(policy.sandbox.install_dir, "/mason/install");
    assert_eq!(policy.sandbox.verify_dir, "/mason/verify");

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
    assert_eq!(
        archive.required_tools,
        [
            BuildToolSpec::Binary("mkdir".to_owned()),
            BuildToolSpec::Binary("bsdtar-static".to_owned()),
        ]
    );
    assert_eq!(archive.create_directory.program, TextSpec::Literal("mkdir".to_owned()));
    assert_eq!(
        archive.create_directory.args,
        [
            TextSpec::Literal("-p".to_owned()),
            TextSpec::Context(ContextValue::SourceDestination),
        ]
    );
    assert_eq!(archive.unpack.program, TextSpec::Literal("bsdtar-static".to_owned()));
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
    assert_eq!(archive.tool_rules.len(), 2);
    assert_eq!(archive.tool_rules[0].extensions, ["rpm"]);
    assert_eq!(
        archive.tool_rules[0].required_tools,
        [
            BuildToolSpec::Binary("rpm2cpio".to_owned()),
            BuildToolSpec::Package("cpio".to_owned()),
        ]
    );
    assert_eq!(archive.tool_rules[1].extensions, ["deb"]);
    assert_eq!(
        archive.tool_rules[1].required_tools,
        [BuildToolSpec::Binary("ar".to_owned())]
    );

    let git = policy.sources.git;
    assert_eq!(git.copy.program, TextSpec::Literal("cp".to_owned()));
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
fn root_source_sandbox_and_platform_semantics_are_rejected_early() {
    let mut policy = repository_policy_value();
    policy.build_root.base.push(policy.build_root.base[0].clone());
    assert!(matches!(
        policy.validate(),
        Err(BuildPolicyConversionError::Duplicate { field, value })
            if field == "build_root.base" && value == "bash"
    ));

    let mut policy = repository_policy_value();
    policy.sources.archive.tool_rules[1].extensions[0] = "rpm".to_owned();
    assert!(matches!(
        policy.validate(),
        Err(BuildPolicyConversionError::Duplicate { field, value })
            if field == "sources.archive.tool_rules.extensions" && value == "rpm"
    ));

    let mut policy = repository_policy_value();
    policy.sandbox.build_dir = "/tmp/build".to_owned();
    assert!(matches!(
        policy.validate(),
        Err(BuildPolicyConversionError::GuestPathOutsideRoot { field, value, guest_root })
            if field == "sandbox.build_dir" && value == "/tmp/build" && guest_root == "/mason"
    ));

    let mut policy = repository_policy_value();
    policy.targets[0].target_platform.vendor = "unknown".to_owned();
    assert!(matches!(
        policy.validate(),
        Err(BuildPolicyConversionError::InvalidPlatformComponent { field, value })
            if field == "targets[0].target_platform.vendor" && value == "unknown"
    ));
}
