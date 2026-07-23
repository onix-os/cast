#[cfg(test)]
pub(crate) fn test_derivation_plan() -> stone_recipe::derivation::DerivationPlan {
    static PLAN: std::sync::OnceLock<stone_recipe::derivation::DerivationPlan> = std::sync::OnceLock::new();

    PLAN.get_or_init(build_test_derivation_plan).clone()
}

#[cfg(test)]
pub(crate) fn set_test_compiler_cache(plan: &mut stone_recipe::derivation::DerivationPlan, enabled: bool) {
    use stone_recipe::derivation::{CompilerCacheRole, InputOrigin};

    let program = plan.toolchain_commands.compilers[0].command.program.clone();
    plan.execution.compiler_cache = enabled;
    plan.toolchain_commands.ccache = enabled.then(|| program.clone());
    plan.toolchain_commands.sccache = enabled.then_some(program);

    let request = plan
        .build_lock
        .requests
        .iter_mut()
        .find(|request| request.request == "binary(pkg-config)")
        .expect("test compiler-cache executable must be locked");
    request
        .origins
        .retain(|origin| !matches!(origin, InputOrigin::CompilerCache { .. }));
    if enabled {
        request.origins.extend([
            InputOrigin::CompilerCache {
                role: CompilerCacheRole::Ccache,
            },
            InputOrigin::CompilerCache {
                role: CompilerCacheRole::Sccache,
            },
        ]);
    }
    plan.build_lock.normalize();
}

#[cfg(test)]
fn test_evaluation(logical_name: &str, source: &str, explicit_inputs: &[u8]) -> gluon_config::EvaluationIdentity {
    gluon_config::GluonEngine::default()
        .evaluate_with_inputs::<i64>(&gluon_config::Source::new(logical_name, source), explicit_inputs)
        .expect("test provenance must be a real restricted evaluation")
        .identity
}

#[cfg(test)]
fn build_test_derivation_plan() -> stone_recipe::derivation::DerivationPlan {
    use stone_recipe::build_policy::{AnalyzerKind, layers::BuildPolicyOperation};
    use stone_recipe::derivation::{
        BUILD_LOCK_SCHEMA_VERSION, BuildLock, BuilderLayout, CompilerCommandPlan, DerivationProvenance,
        ExecutableCommandPlan, ExecutablePlan, ExecutionCredentials, InputOrigin, LockedIdentity, LockedOutput,
        LockedPackage, LockedRequest, OutputPlan, PackageIdentity, Platform, PolicyLayerProvenance, PolicyProvenance,
        PolicyTransitionProvenance, ProfileFragmentProvenance, RelationKind, RelationPlan, RepositorySnapshot,
        ToolchainCommandsPlan, policy_composition_identity, profile_aggregate_fingerprint,
    };

    const SOURCE_LOCK_BYTES: &[u8] = b"test source lock bytes";

    let profiles = vec![ProfileFragmentProvenance {
        logical_name: "default".to_owned(),
        evaluation: test_evaluation("profile.d/default.glu", "1", &[]),
    }];
    let layers = vec![PolicyLayerProvenance {
        name: "foundation".to_owned(),
        transitions: vec![PolicyTransitionProvenance {
            operation: BuildPolicyOperation::Add,
            origin: "default.glu".to_owned(),
            evaluation: test_evaluation("default.glu", "2", &[]),
        }],
    }];
    let policy_inputs = policy_composition_identity("aerynos", &layers);
    let provenance = DerivationProvenance {
        recipe: test_evaluation("stone.glu", "3", SOURCE_LOCK_BYTES),
        profiles,
        policy: PolicyProvenance {
            name: "aerynos".to_owned(),
            root: test_evaluation("policy.glu", "4", &policy_inputs),
            layers,
        },
    };
    let platform = Platform {
        architecture: "x86_64".to_owned(),
        vendor: "unknown".to_owned(),
        operating_system: "linux".to_owned(),
        abi: "gnu".to_owned(),
    };
    let identity = |name: &str| LockedIdentity {
        name: name.to_owned(),
        fingerprint: format!("{name}-fingerprint"),
    };
    let mut build_lock = BuildLock {
        schema_version: BUILD_LOCK_SCHEMA_VERSION,
        request_fingerprint: "request-fingerprint".to_owned(),
        repositories: vec![RepositorySnapshot {
            id: "test-repository".to_owned(),
            index_uri: "https://example.invalid/stone.index".to_owned(),
            snapshot: "test-repository-snapshot".to_owned(),
        }],
        requests: [
            "pkg-config",
            "python3",
            "llvm-objcopy",
            "llvm-strip",
            "objcopy",
            "strip",
        ]
        .into_iter()
        .map(|name| {
            let mut origins = vec![InputOrigin::Policy {
                source: "policy.glu".to_owned(),
                field: "build_root.base".to_owned(),
                index: 0,
            }];
            if name == "pkg-config" {
                origins.extend(
                    ToolchainCommandsPlan::COMPILER_ROLES
                        .into_iter()
                        .map(|role| InputOrigin::CompilerExecutable { role }),
                );
            }
            LockedRequest {
                request: format!("binary({name})"),
                package_id: "analyzer-tools-id".to_owned(),
                output: "out".to_owned(),
                origins,
            }
        })
        .collect(),
        packages: vec![LockedPackage {
            package_id: "analyzer-tools-id".to_owned(),
            name: "analyzer-tools".to_owned(),
            version: "1.0.0-1-1".to_owned(),
            architecture: "x86_64".to_owned(),
            repository: "test-repository".to_owned(),
            outputs: vec![LockedOutput { name: "out".to_owned() }],
            dependencies: Vec::new(),
        }],
        build_platform: platform.clone(),
        host_platform: platform.clone(),
        target_platform: platform,
        policy: LockedIdentity {
            name: provenance.policy.name.clone(),
            fingerprint: provenance.policy.root.sha256.clone(),
        },
        target: identity("x86_64"),
        profile: LockedIdentity {
            name: "profile".to_owned(),
            fingerprint: profile_aggregate_fingerprint(&provenance.profiles),
        },
        toolchain: identity("toolchain"),
        builder: identity("builder"),
    };
    build_lock.normalize();
    let mut plan = stone_recipe::derivation::DerivationPlan::new(
        PackageIdentity {
            name: "example".to_owned(),
            version: "1.2.3".to_owned(),
            source_release: 1,
            build_release: 1,
            homepage: "https://example.invalid".to_owned(),
            licenses: vec!["MPL-2.0".to_owned()],
            architecture: "x86_64".to_owned(),
        },
        build_lock,
        provenance,
    );
    plan.cast_version = "test-cast".to_owned();
    plan.cast_fingerprint = "sha256:test-cast-semantics".to_owned();
    plan.execution.executor = LockedIdentity {
        name: "test-executor".to_owned(),
        fingerprint: "test-executor-fingerprint".to_owned(),
    };
    plan.execution.credentials = ExecutionCredentials::IsolatedRoot;
    plan.source_lock_digest = plan.provenance.recipe.explicit_inputs_sha256.clone();
    plan.layout = BuilderLayout {
        hostname: "cast".to_owned(),
        guest_root: "/mason".to_owned(),
        artifacts_dir: "/mason/artefacts".to_owned(),
        build_dir: "/mason/build".to_owned(),
        source_dir: "/mason/sources".to_owned(),
        recipe_dir: "/mason/recipe".to_owned(),
        install_dir: "/mason/install".to_owned(),
        package_dir: "/mason/recipe/pkg".to_owned(),
        ccache_dir: "/mason/ccache".to_owned(),
        sccache_dir: "/mason/sccache".to_owned(),
        go_cache_dir: "/mason/gocache".to_owned(),
        go_mod_cache_dir: "/mason/gomodcache".to_owned(),
        cargo_cache_dir: "/mason/cargocache".to_owned(),
        zig_cache_dir: "/mason/zigcache".to_owned(),
    };
    plan.source_date_epoch = 1_700_000_000;
    plan.analysis.handlers = vec![
        AnalyzerKind::IgnoreBlocked,
        AnalyzerKind::Binary,
        AnalyzerKind::Elf,
        AnalyzerKind::PkgConfig,
        AnalyzerKind::Python,
        AnalyzerKind::CMake,
        AnalyzerKind::CompressMan,
        AnalyzerKind::IncludeAny,
    ];
    let analyzer_tool = |name: &str| ExecutablePlan {
        path: format!("/usr/bin/{name}"),
        requirement: RelationPlan {
            kind: RelationKind::Binary,
            name: name.to_owned(),
        },
    };
    plan.toolchain_commands.compilers = ToolchainCommandsPlan::COMPILER_ROLES
        .into_iter()
        .map(|role| CompilerCommandPlan {
            role,
            command: ExecutableCommandPlan {
                program: analyzer_tool("pkg-config"),
                args: Vec::new(),
            },
        })
        .collect();
    plan.analysis.tools.pkg_config = Some(analyzer_tool("pkg-config"));
    plan.analysis.tools.python = Some(analyzer_tool("python3"));
    plan.analysis.tools.strip = Some(analyzer_tool("llvm-strip"));
    plan.outputs = vec![OutputPlan {
        name: "out".to_owned(),
        package_name: "example".to_owned(),
        include_in_manifest: true,
        summary: None,
        description: None,
        provides_exclude: Vec::new(),
        runtime_exclude: Vec::new(),
        runtime_inputs: Vec::new(),
        conflicts: Vec::new(),
    }];
    plan.validate().unwrap();
    plan
}
