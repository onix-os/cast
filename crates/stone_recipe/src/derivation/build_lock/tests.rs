use super::*;
use declarative_config::{
    DeclarationCodec, DeclarationEvaluationError, DeclarationEvaluator,
    Source,
};

fn encode(lock: &BuildLock) -> String {
    GluonBuildLockCodec::default().encode(lock).unwrap()
}

fn decode(source: &str) -> Result<BuildLock, DeclarationEvaluationError<BuildLockValidationError>> {
    GluonBuildLockCodec::default()
        .evaluate(&Source::new(BUILD_LOCK_FILE_NAME, source))
        .map(|evaluation| evaluation.value)
}

#[test]
fn generated_gluon_round_trips_through_restricted_evaluator() {
    let mut expected = sample_lock();
    expected.normalize();
    expected.validate().unwrap();
    let encoded = encode(&expected);
    let decoded = decode(&encoded).unwrap();

    assert_eq!(
        encoded.as_bytes(),
        include_bytes!("../../../../../tests/fixtures/gluon/goldens/build-lock.glu")
    );
    assert_eq!(decoded, expected);
}

#[test]
fn every_typed_input_origin_round_trips_through_generated_gluon() {
    let mut expected = sample_lock();
    expected.requests[0].origins = vec![
        InputOrigin::BuilderTool {
            selection: PackageInputSelection::Package,
            index: 0,
        },
        InputOrigin::NativeBuild {
            selection: PackageInputSelection::Profile {
                name: "emul32/x86_64".to_owned(),
            },
            index: 1,
        },
        InputOrigin::Build {
            selection: PackageInputSelection::Package,
            index: 2,
        },
        InputOrigin::Check {
            selection: PackageInputSelection::Package,
            index: 3,
        },
        InputOrigin::OutputRuntime {
            output: "dev".to_owned(),
            index: 4,
        },
        InputOrigin::Policy {
            source: "policy.glu".to_owned(),
            field: "build_root.base".to_owned(),
            index: 5,
        },
        InputOrigin::JobExecutable {
            job: 1,
            phase: 2,
            phase_name: "build".to_owned(),
            section: JobStepSection::Steps,
            step: 3,
            role: JobExecutableRole::ShellDeclaredProgram { index: 4 },
        },
        InputOrigin::Analyzer {
            role: AnalyzerRole::Objcopy,
        },
        InputOrigin::CompilerExecutable {
            role: CompilerExecutableRole::Cpp,
        },
        InputOrigin::CompilerCache {
            role: CompilerCacheRole::Sccache,
        },
        InputOrigin::MoldLinker,
    ];
    expected.normalize();

    let encoded = encode(&expected);
    let decoded = decode(&encoded).unwrap();

    assert_eq!(decoded, expected);
    for constructor in [
        "BuilderToolOrigin",
        "NativeBuildOrigin",
        "BuildOrigin",
        "CheckOrigin",
        "OutputRuntimeOrigin",
        "PolicyOrigin",
        "JobExecutableOrigin",
        "AnalyzerOrigin",
        "CompilerExecutableOrigin",
        "CompilerCacheOrigin",
        "MoldLinkerOrigin",
    ] {
        assert!(encoded.contains(constructor), "missing generated {constructor}");
    }
}

#[test]
fn construction_order_does_not_change_encoding_or_digest() {
    let first = sample_lock();
    let mut reordered = first.clone();
    reordered.packages.reverse();
    reordered.packages[0].outputs.reverse();
    reordered.packages[0].dependencies.reverse();

    assert_eq!(encode(&first), encode(&reordered));
    assert_eq!(first.canonical_bytes(), reordered.canonical_bytes());
    assert_eq!(first.digest(), reordered.digest());
}

#[test]
fn string_encoding_is_unambiguous_and_round_trips() {
    let mut lock = sample_lock();
    lock.profile.name = "quote \" slash \\ newline\n".to_owned();
    let decoded = decode(&encode(&lock)).unwrap();

    assert_eq!(decoded.profile.name, lock.profile.name);
}

#[test]
fn mutation_of_every_locked_identity_changes_digest() {
    let original = sample_lock();
    let original_digest = original.digest();
    let mutations: Vec<Box<dyn Fn(&mut BuildLock)>> = vec![
        Box::new(|lock| lock.repositories[0].snapshot.push_str("-changed")),
        Box::new(|lock| lock.requests[0].package_id.push_str("-changed")),
        Box::new(|lock| {
            lock.requests[0].origins[0] = InputOrigin::Check {
                selection: PackageInputSelection::Package,
                index: 0,
            }
        }),
        Box::new(|lock| lock.packages[0].outputs[0].name.push_str("-changed")),
        Box::new(|lock| lock.target_platform.architecture = "aarch64".to_owned()),
        Box::new(|lock| lock.policy.fingerprint.push_str("-changed")),
        Box::new(|lock| lock.target.name.push_str("-changed")),
        Box::new(|lock| lock.target.fingerprint.push_str("-changed")),
        Box::new(|lock| lock.profile.fingerprint.push_str("-changed")),
        Box::new(|lock| lock.toolchain.fingerprint.push_str("-changed")),
        Box::new(|lock| lock.builder.fingerprint.push_str("-changed")),
    ];

    for mutate in mutations {
        let mut changed = original.clone();
        mutate(&mut changed);
        assert_ne!(changed.digest(), original_digest);
    }
}

#[test]
fn validation_rejects_unresolved_output_references() {
    let mut lock = sample_lock();
    lock.packages[1].dependencies[0].output = "missing".to_owned();

    assert!(matches!(
        lock.validate(),
        Err(BuildLockValidationError::UnknownOutput { .. })
    ));
}

#[test]
fn request_origins_are_sorted_deduplicated_and_identity_bearing() {
    let first = RequestedInput {
        request: "binary(tool)".to_owned(),
        origins: vec![
            InputOrigin::Check {
                selection: PackageInputSelection::Package,
                index: 0,
            },
            InputOrigin::BuilderTool {
                selection: PackageInputSelection::Package,
                index: 0,
            },
            InputOrigin::BuilderTool {
                selection: PackageInputSelection::Package,
                index: 0,
            },
        ],
    };
    let reordered = RequestedInput {
        request: first.request.clone(),
        origins: vec![first.origins[1].clone(), first.origins[0].clone()],
    };
    assert_eq!(
        requested_inputs_digest(std::slice::from_ref(&first)),
        requested_inputs_digest(std::slice::from_ref(&reordered))
    );

    let changed = RequestedInput {
        request: first.request.clone(),
        origins: vec![InputOrigin::NativeBuild {
            selection: PackageInputSelection::Package,
            index: 0,
        }],
    };
    assert_ne!(
        requested_inputs_digest(std::slice::from_ref(&first)),
        requested_inputs_digest(std::slice::from_ref(&changed))
    );

    let mut lock = sample_lock();
    lock.requests[0].origins.clear();
    assert!(matches!(
        lock.validate(),
        Err(BuildLockValidationError::MissingInputOrigins { request }) if request == "binary(hello)"
    ));

    let mut duplicate = sample_lock();
    let repeated = duplicate.requests[0].origins[0].clone();
    duplicate.requests[0].origins.push(repeated);
    assert!(matches!(
        duplicate.validate(),
        Err(BuildLockValidationError::DuplicateInputOrigin {
            request,
            first_index: 0,
            duplicate_index: 1,
        }) if request == "binary(hello)"
    ));
}

#[test]
fn validation_rejects_packages_disconnected_from_all_requests() {
    let mut lock = sample_lock();
    lock.packages.push(LockedPackage {
        package_id: "orphan-id".to_owned(),
        name: "orphan".to_owned(),
        version: "1.0.0-1".to_owned(),
        architecture: "x86_64".to_owned(),
        repository: "volatile".to_owned(),
        outputs: vec![LockedOutput { name: "out".to_owned() }],
        dependencies: Vec::new(),
    });

    assert!(matches!(
        lock.validate(),
        Err(BuildLockValidationError::UnreachablePackage { index: 2, package })
            if package == "orphan-id"
    ));
}

#[test]
fn validation_rejects_repository_snapshots_unused_by_the_closure() {
    let mut lock = sample_lock();
    lock.repositories.push(RepositorySnapshot {
        id: "unused".to_owned(),
        index_uri: "https://example.invalid/unused.stone.index".to_owned(),
        snapshot: "unused-snapshot".to_owned(),
    });

    assert!(matches!(
        lock.validate(),
        Err(BuildLockValidationError::UnusedRepository { index: 1, id })
            if id == "unused"
    ));
}

#[test]
fn validation_requires_independent_policy_and_target_identities() {
    let mut missing_policy = sample_lock();
    missing_policy.policy.name.clear();
    assert!(matches!(
        missing_policy.validate(),
        Err(BuildLockValidationError::Empty { field }) if field == "policy.name"
    ));

    let mut missing_target = sample_lock();
    missing_target.target.fingerprint.clear();
    assert!(matches!(
        missing_target.validate(),
        Err(BuildLockValidationError::Empty { field }) if field == "target.fingerprint"
    ));
}

#[test]
fn validation_rejects_dependency_cycles_with_the_closing_edge() {
    let mut lock = sample_lock();
    lock.packages[0].dependencies.push(LockedOutputRef {
        package_id: "hello-id".to_owned(),
        output: "out".to_owned(),
    });

    let error = lock.validate().unwrap_err();
    assert!(matches!(
        &error,
        BuildLockValidationError::DependencyCycle { field, cycle }
            if field == "packages[1].dependencies[0]"
                && cycle.iter().map(String::as_str).eq(["cmake-id", "hello-id", "cmake-id"])
    ));
    assert!(error.to_string().contains("cmake-id -> hello-id -> cmake-id"));
}

#[test]
fn decoder_rejects_unsupported_schema() {
    let encoded = encode(&sample_lock()).replacen("schema_version = 6", "schema_version = 7", 1);
    let error = decode(&encoded).unwrap_err();

    assert!(matches!(
        error,
        DeclarationEvaluationError::Conversion(BuildLockValidationError::UnsupportedSchema { found: 7, .. })
    ));
}

#[test]
fn decoder_rejects_pre_toolchain_command_schema_five() {
    let encoded = encode(&sample_lock()).replacen("schema_version = 6", "schema_version = 5", 1);
    let error = decode(&encoded).unwrap_err();

    assert!(matches!(
        error,
        DeclarationEvaluationError::Conversion(BuildLockValidationError::UnsupportedSchema { found: 5, .. })
    ));
}
