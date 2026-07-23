#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use gluon_config::{EvaluationFingerprint, Evaluator, ImportPolicy, Source};
    use stone_recipe::{
        build_policy::{AnalyzerKind, layers::BuildPolicyOperation},
        derivation::{
            AnalysisPlan, AnalysisToolsPlan, BUILD_LOCK_SCHEMA_VERSION, BuildLock, BuilderLayout, CollectionRulePlan,
            CompilerCommandPlan, CompilerExecutableRole, DERIVATION_PLAN_SCHEMA_VERSION, DerivationProvenance,
            ExecutableCommandPlan, ExecutablePlan, ExecutionCredentials, ExecutionPolicy, InputOrigin,
            JobExecutableRole, JobPlan, JobStepSection, LockedOutput, LockedOutputRef, LockedPackage, LockedRequest,
            OutputPlan, PackageIdentity, PackageInputSelection, PhasePlan, PolicyLayerProvenance, PolicyProvenance,
            PolicyTransitionProvenance, ProfileFragmentProvenance, RepositorySnapshot, RootMaterializationMode,
            ToolchainCommandsPlan, policy_composition_identity, profile_aggregate_fingerprint,
        },
    };

    use super::*;

    struct Fixture {
        plan: DerivationPlan,
    }

    impl Fixture {
        fn render(&self) -> String {
            format(&self.plan)
        }
    }

    fn identity(name: &str) -> LockedIdentity {
        LockedIdentity {
            name: name.to_owned(),
            fingerprint: format!("{name}-fingerprint"),
        }
    }

    fn evaluation(logical_name: &str, source: &str, explicit_inputs: &[u8]) -> EvaluationFingerprint {
        Evaluator::default()
            .evaluate_with_inputs::<i64>(&Source::new(logical_name, source), explicit_inputs)
            .expect("fixture evaluation must succeed")
            .fingerprint
    }

    fn evaluation_with_import(logical_name: &str, explicit_inputs: &[u8]) -> EvaluationFingerprint {
        let policy = ImportPolicy::new()
            .with_embedded_module("fixture.provenance", "41")
            .expect("fixture module name must be valid");
        Evaluator::default()
            .with_import_policy(policy)
            .evaluate_with_inputs::<i64>(
                &Source::new(logical_name, "import! fixture.provenance"),
                explicit_inputs,
            )
            .expect("fixture import evaluation must succeed")
            .fingerprint
    }

    fn fixture() -> Fixture {
        const SOURCE_LOCK_BYTES: &[u8] = b"canonical explanation source lock";

        let profiles = vec![
            ProfileFragmentProvenance {
                logical_name: "vendor/base".to_owned(),
                evaluation: evaluation_with_import("profile.d/base.glu", &[]),
            },
            ProfileFragmentProvenance {
                logical_name: "site/local".to_owned(),
                evaluation: evaluation("profile.d/local.glu", "42", &[]),
            },
        ];
        let layers = vec![
            PolicyLayerProvenance {
                name: "foundation".to_owned(),
                transitions: vec![PolicyTransitionProvenance {
                    operation: BuildPolicyOperation::Add,
                    origin: "default.glu".to_owned(),
                    evaluation: evaluation_with_import("default.glu", &[]),
                }],
            },
            PolicyLayerProvenance {
                name: "site-empty".to_owned(),
                transitions: Vec::new(),
            },
            PolicyLayerProvenance {
                name: "override".to_owned(),
                transitions: vec![PolicyTransitionProvenance {
                    operation: BuildPolicyOperation::Modify,
                    origin: "override.glu".to_owned(),
                    evaluation: evaluation("override.glu", "43", &[]),
                }],
            },
        ];
        let policy_inputs = policy_composition_identity("repository-policy", &layers);
        let provenance = DerivationProvenance {
            recipe: evaluation_with_import("stone.glu", SOURCE_LOCK_BYTES),
            profiles,
            policy: PolicyProvenance {
                name: "repository-policy".to_owned(),
                root: evaluation("policy.glu", "44", &policy_inputs),
                layers,
            },
        };
        let lock = BuildLock {
            schema_version: BUILD_LOCK_SCHEMA_VERSION,
            request_fingerprint: "locked-request-fingerprint".to_owned(),
            repositories: vec![
                RepositorySnapshot {
                    id: "z-repository".to_owned(),
                    index_uri: "https://z.invalid/index".to_owned(),
                    snapshot: "z-snapshot".to_owned(),
                },
                RepositorySnapshot {
                    id: "a-repository".to_owned(),
                    index_uri: "https://a.invalid/index".to_owned(),
                    snapshot: "a-snapshot".to_owned(),
                },
            ],
            requests: vec![
                LockedRequest {
                    request: "pkg(zeta)".to_owned(),
                    package_id: "zeta-id".to_owned(),
                    output: "devel".to_owned(),
                    origins: vec![InputOrigin::BuilderTool {
                        selection: PackageInputSelection::Profile {
                            name: "x86_64".to_owned(),
                        },
                        index: 0,
                    }],
                },
                LockedRequest {
                    request: "binary(alpha)".to_owned(),
                    package_id: "alpha-id".to_owned(),
                    output: "out".to_owned(),
                    origins: {
                        let mut origins = vec![
                            InputOrigin::Build {
                                selection: PackageInputSelection::Package,
                                index: 0,
                            },
                            InputOrigin::OutputRuntime {
                                output: "out".to_owned(),
                                index: 0,
                            },
                            InputOrigin::JobExecutable {
                                job: 0,
                                phase: 1,
                                phase_name: "build".to_owned(),
                                section: JobStepSection::Steps,
                                step: 0,
                                role: JobExecutableRole::ShellDeclaredProgram { index: 0 },
                            },
                            InputOrigin::CompilerCache {
                                role: CompilerCacheRole::Ccache,
                            },
                            InputOrigin::CompilerCache {
                                role: CompilerCacheRole::Sccache,
                            },
                        ];
                        origins.extend(
                            ToolchainCommandsPlan::COMPILER_ROLES
                                .into_iter()
                                .map(|role| InputOrigin::CompilerExecutable { role }),
                        );
                        origins
                    },
                },
                LockedRequest {
                    request: "binary(objcopy)".to_owned(),
                    package_id: "alpha-id".to_owned(),
                    output: "out".to_owned(),
                    origins: vec![
                        InputOrigin::Policy {
                            source: "policy.glu".to_owned(),
                            field: "build_root.analyzer_tools.llvm.objcopy".to_owned(),
                            index: 0,
                        },
                        InputOrigin::Analyzer {
                            role: AnalyzerRole::Objcopy,
                        },
                    ],
                },
                LockedRequest {
                    request: "binary(prepare)".to_owned(),
                    package_id: "alpha-id".to_owned(),
                    output: "out".to_owned(),
                    origins: vec![InputOrigin::JobExecutable {
                        job: 0,
                        phase: 1,
                        phase_name: "build".to_owned(),
                        section: JobStepSection::Pre,
                        step: 0,
                        role: JobExecutableRole::RunProgram,
                    }],
                },
                LockedRequest {
                    request: "binary(bash)".to_owned(),
                    package_id: "alpha-id".to_owned(),
                    output: "out".to_owned(),
                    origins: vec![
                        InputOrigin::Check {
                            selection: PackageInputSelection::Package,
                            index: 0,
                        },
                        InputOrigin::JobExecutable {
                            job: 0,
                            phase: 1,
                            phase_name: "build".to_owned(),
                            section: JobStepSection::Steps,
                            step: 0,
                            role: JobExecutableRole::ShellInterpreter,
                        },
                    ],
                },
                LockedRequest {
                    request: "binary(finish)".to_owned(),
                    package_id: "alpha-id".to_owned(),
                    output: "out".to_owned(),
                    origins: vec![
                        InputOrigin::NativeBuild {
                            selection: PackageInputSelection::Package,
                            index: 0,
                        },
                        InputOrigin::JobExecutable {
                            job: 0,
                            phase: 1,
                            phase_name: "build".to_owned(),
                            section: JobStepSection::Post,
                            step: 0,
                            role: JobExecutableRole::RunProgram,
                        },
                    ],
                },
            ],
            packages: vec![
                LockedPackage {
                    package_id: "zeta-id".to_owned(),
                    name: "zeta".to_owned(),
                    version: "2.0-1-1".to_owned(),
                    architecture: "x86_64".to_owned(),
                    repository: "z-repository".to_owned(),
                    outputs: vec![
                        LockedOutput { name: "out".to_owned() },
                        LockedOutput {
                            name: "devel".to_owned(),
                        },
                    ],
                    dependencies: vec![LockedOutputRef {
                        package_id: "alpha-id".to_owned(),
                        output: "out".to_owned(),
                    }],
                },
                LockedPackage {
                    package_id: "alpha-id".to_owned(),
                    name: "alpha".to_owned(),
                    version: "1.0-1-1".to_owned(),
                    architecture: "x86_64".to_owned(),
                    repository: "a-repository".to_owned(),
                    outputs: vec![LockedOutput { name: "out".to_owned() }],
                    dependencies: Vec::new(),
                },
            ],
            build_platform: Platform {
                architecture: "x86_64".to_owned(),
                vendor: "unknown".to_owned(),
                operating_system: "linux".to_owned(),
                abi: "gnu".to_owned(),
            },
            host_platform: Platform {
                architecture: "x86_64".to_owned(),
                vendor: "aeryn".to_owned(),
                operating_system: "linux".to_owned(),
                abi: "gnu".to_owned(),
            },
            target_platform: Platform {
                architecture: "x86_64".to_owned(),
                vendor: "aeryn".to_owned(),
                operating_system: "linux".to_owned(),
                abi: "stone".to_owned(),
            },
            policy: LockedIdentity {
                name: provenance.policy.name.clone(),
                fingerprint: provenance.policy.root.sha256.clone(),
            },
            target: identity("x86_64"),
            profile: LockedIdentity {
                name: "default-x86_64".to_owned(),
                fingerprint: profile_aggregate_fingerprint(&provenance.profiles),
            },
            toolchain: identity("llvm"),
            builder: identity("cast.builders.cmake.v2"),
        };

        let executable = |name: &str| ExecutablePlan {
            path: format!("/usr/bin/{name}"),
            requirement: RelationPlan {
                kind: RelationKind::Binary,
                name: name.to_owned(),
            },
        };

        let plan = DerivationPlan {
            schema_version: DERIVATION_PLAN_SCHEMA_VERSION,
            cast_version: "0.26.6".to_owned(),
            cast_fingerprint: "sha256:cast".to_owned(),
            package: PackageIdentity {
                name: "demo".to_owned(),
                version: "1.2.3".to_owned(),
                source_release: 4,
                build_release: 5,
                homepage: "https://demo.invalid".to_owned(),
                licenses: vec!["Zlib".to_owned(), "MIT".to_owned()],
                architecture: "x86_64".to_owned(),
            },
            source_lock_digest: provenance.recipe.explicit_inputs_sha256.clone(),
            provenance,
            sources: vec![
                LockedSource::Archive {
                    order: 0,
                    url: "https://src.invalid/demo.tar.xz".to_owned(),
                    sha256: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
                    filename: "demo.tar.xz".to_owned(),
                },
                LockedSource::Git {
                    order: 1,
                    url: "https://git.invalid/demo".to_owned(),
                    requested_ref: "v1.2.3".to_owned(),
                    commit: "1111111111111111111111111111111111111111".to_owned(),
                    materialization_sha256: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
                        .to_owned(),
                    directory: "demo-git".to_owned(),
                },
            ],
            build_lock: lock,
            jobs: vec![JobPlan {
                pgo_stage: Some("one".to_owned()),
                pgo_dir: Some("/sandbox/build/pgo".to_owned()),
                build_dir: "/sandbox/build/job".to_owned(),
                work_dir: "/sandbox/build/job/work".to_owned(),
                phases: vec![
                    PhasePlan {
                        name: "prepare".to_owned(),
                        pre: Vec::new(),
                        steps: vec![StepPlan::ExtractArchive {
                            source: 0,
                            destination: "demo-source".to_owned(),
                            strip_components: 1,
                        }],
                        post: Vec::new(),
                    },
                    PhasePlan {
                        name: "build".to_owned(),
                        pre: vec![StepPlan::Run {
                            program: executable("prepare"),
                            args: vec!["--first".to_owned(), "second value".to_owned()],
                            environment: BTreeMap::from([
                                ("Z_PRE".to_owned(), "z".to_owned()),
                                ("A_PRE".to_owned(), "a".to_owned()),
                            ]),
                            working_dir: "/sandbox/build/job/work".to_owned(),
                        }],
                        steps: vec![StepPlan::Shell {
                            interpreter: executable("bash"),
                            declared_programs: vec![executable("alpha")],
                            script: "printf 'build\\n'".to_owned(),
                            environment: BTreeMap::from([("BUILD_MODE".to_owned(), "release".to_owned())]),
                            working_dir: "/sandbox/build/job/work".to_owned(),
                        }],
                        post: vec![StepPlan::Run {
                            program: executable("finish"),
                            args: Vec::new(),
                            environment: BTreeMap::new(),
                            working_dir: "/sandbox/build/job".to_owned(),
                        }],
                    },
                ],
            }],
            environment: BTreeMap::from([
                ("Z_GLOBAL".to_owned(), "z".to_owned()),
                ("A_GLOBAL".to_owned(), "a".to_owned()),
            ]),
            layout: BuilderLayout {
                hostname: "sandbox-test".to_owned(),
                guest_root: "/sandbox".to_owned(),
                artifacts_dir: "/sandbox/artifacts".to_owned(),
                build_dir: "/sandbox/build".to_owned(),
                source_dir: "/sandbox/sources".to_owned(),
                recipe_dir: "/sandbox/recipe".to_owned(),
                install_dir: "/sandbox/install".to_owned(),
                package_dir: "/sandbox/recipe/pkg".to_owned(),
                ccache_dir: "/sandbox/cache/ccache".to_owned(),
                sccache_dir: "/sandbox/cache/sccache".to_owned(),
                go_cache_dir: "/sandbox/cache/go-build".to_owned(),
                go_mod_cache_dir: "/sandbox/cache/go-mod".to_owned(),
                cargo_cache_dir: "/sandbox/cache/cargo".to_owned(),
                zig_cache_dir: "/sandbox/cache/zig".to_owned(),
            },
            execution: ExecutionPolicy {
                executor: identity("cast-executor-v1"),
                root_materialization: RootMaterializationMode::LockedClosure,
                credentials: ExecutionCredentials::IsolatedRoot,
                network: NetworkMode::Disabled,
                filesystems: Default::default(),
                compiler_cache: true,
                jobs: 8,
            },
            toolchain_commands: ToolchainCommandsPlan {
                compilers: ToolchainCommandsPlan::COMPILER_ROLES
                    .into_iter()
                    .map(|role| CompilerCommandPlan {
                        role,
                        command: ExecutableCommandPlan {
                            program: executable("alpha"),
                            args: (role == CompilerExecutableRole::Cpp)
                                .then(|| vec!["-E".to_owned()])
                                .unwrap_or_default(),
                        },
                    })
                    .collect(),
                ccache: Some(executable("alpha")),
                sccache: Some(executable("alpha")),
                mold: None,
            },
            analysis: AnalysisPlan {
                handlers: vec![AnalyzerKind::Elf, AnalyzerKind::Binary, AnalyzerKind::IncludeAny],
                tools: AnalysisToolsPlan {
                    objcopy: Some(ExecutablePlan {
                        path: "/usr/bin/objcopy".to_owned(),
                        requirement: RelationPlan {
                            kind: RelationKind::Binary,
                            name: "objcopy".to_owned(),
                        },
                    }),
                    ..AnalysisToolsPlan::default()
                },
                debug: true,
                strip: false,
                compress_man: true,
                remove_libtool: false,
            },
            manifest_build_inputs: vec![
                RelationPlan {
                    kind: RelationKind::PackageName,
                    name: "zlib-devel".to_owned(),
                },
                RelationPlan {
                    kind: RelationKind::Binary,
                    name: "cmake".to_owned(),
                },
            ],
            collection_rules: vec![
                CollectionRulePlan {
                    output: "out".to_owned(),
                    kind: PathRuleKind::Executable,
                    pattern: "usr/bin/*".to_owned(),
                },
                CollectionRulePlan {
                    output: "dev".to_owned(),
                    kind: PathRuleKind::Any,
                    pattern: "usr/include/**".to_owned(),
                },
            ],
            outputs: vec![
                OutputPlan {
                    name: "dev".to_owned(),
                    package_name: "demo-devel".to_owned(),
                    include_in_manifest: true,
                    summary: None,
                    description: Some("Development files".to_owned()),
                    provides_exclude: Vec::new(),
                    runtime_exclude: Vec::new(),
                    runtime_inputs: vec![OutputRelation::Planned {
                        output: "out".to_owned(),
                    }],
                    conflicts: Vec::new(),
                },
                OutputPlan {
                    name: "out".to_owned(),
                    package_name: "demo".to_owned(),
                    include_in_manifest: true,
                    summary: Some("Demo summary".to_owned()),
                    description: None,
                    provides_exclude: vec!["z-pattern".to_owned(), "a-pattern".to_owned()],
                    runtime_exclude: vec!["z-runtime".to_owned(), "a-runtime".to_owned()],
                    runtime_inputs: vec![OutputRelation::Locked {
                        relation: RelationPlan {
                            kind: RelationKind::Binary,
                            name: "alpha".to_owned(),
                        },
                        reference: LockedOutputRef {
                            package_id: "alpha-id".to_owned(),
                            output: "out".to_owned(),
                        },
                    }],
                    conflicts: vec![
                        RelationPlan {
                            kind: RelationKind::PackageName,
                            name: "z-conflict".to_owned(),
                        },
                        RelationPlan {
                            kind: RelationKind::Binary,
                            name: "a-conflict".to_owned(),
                        },
                    ],
                },
            ],
            source_date_epoch: 1_700_000_000,
        };

        plan.validate()
            .expect("explanation fixture must be a valid frozen plan");
        Fixture { plan }
    }

    #[test]
    fn complete_explanation_matches_the_golden() {
        let rendered = fixture().render();
        if std::env::var_os("BLESS").is_some() {
            fs_err::write(
                concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden/recipe-explain.txt"),
                rendered,
            )
            .expect("golden explanation must be writable");
            return;
        }

        assert_eq!(rendered, include_str!("../../../../tests/golden/recipe-explain.txt"));
    }

    #[test]
    fn unordered_categories_are_sorted_without_reordering_authored_sequences() {
        let first = fixture();
        let mut second = fixture();
        second.plan.package.licenses.reverse();
        second.plan.sources.reverse();
        second.plan.build_lock.repositories.reverse();
        second.plan.build_lock.requests.reverse();
        second.plan.build_lock.packages.reverse();
        for package in &mut second.plan.build_lock.packages {
            package.outputs.reverse();
            package.dependencies.reverse();
        }
        second.plan.manifest_build_inputs.reverse();
        second.plan.outputs.reverse();
        for output in &mut second.plan.outputs {
            output.provides_exclude.reverse();
            output.runtime_exclude.reverse();
            output.runtime_inputs.reverse();
            output.conflicts.reverse();
        }

        assert_eq!(first.render(), second.render());

        second.plan.analysis.handlers.swap(0, 1);
        assert_ne!(
            first.render(),
            second.render(),
            "analyzer handler precedence must remain visible"
        );
        second.plan.analysis.handlers.swap(0, 1);

        second.plan.collection_rules.reverse();
        assert_ne!(
            first.render(),
            second.render(),
            "collector matching precedence must remain visible"
        );
        second.plan.collection_rules.reverse();

        second.plan.provenance.profiles.swap(0, 1);
        assert_ne!(
            first.render(),
            second.render(),
            "profile fragment precedence must remain visible"
        );
        second.plan.provenance.profiles.swap(0, 1);

        second.plan.provenance.policy.layers.swap(1, 2);
        assert_ne!(
            first.render(),
            second.render(),
            "policy layer order, including empty layers, must remain visible"
        );
    }
}
