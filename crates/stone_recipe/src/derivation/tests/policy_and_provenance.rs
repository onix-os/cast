#[test]
fn every_allowed_filesystem_policy_change_changes_derivation_identity() {
    let original = sample_plan();
    let original_id = original.derivation_id();

    let mut without_dev = original;
    without_dev.execution.filesystems.dev = DevFilesystem::None;
    assert_ne!(original_id, without_dev.derivation_id());
}

#[test]
fn validation_rejects_enabled_frozen_networking() {
    let mut plan = sample_plan();
    plan.execution.network = NetworkMode::Enabled;

    assert!(matches!(
        plan.validate(),
        Err(DerivationValidationError::NetworkEnabled)
    ));
}

#[test]
fn validation_requires_complete_executor_identity() {
    for (field, clear) in [
        (
            "execution.executor.name",
            Box::new(|plan: &mut DerivationPlan| plan.execution.executor.name.clear())
                as Box<dyn Fn(&mut DerivationPlan)>,
        ),
        (
            "execution.executor.fingerprint",
            Box::new(|plan: &mut DerivationPlan| plan.execution.executor.fingerprint.clear()),
        ),
    ] {
        let mut plan = sample_plan();
        clear(&mut plan);
        assert!(matches!(
            plan.validate(),
            Err(DerivationValidationError::Empty { field: actual }) if actual == field
        ));
    }
}

#[test]
fn validation_rejects_package_manager_state_root_materialization() {
    let mut plan = sample_plan();
    plan.execution.root_materialization = RootMaterializationMode::PackageManagerState;

    assert!(matches!(
        plan.validate(),
        Err(DerivationValidationError::PackageManagerRootMaterialization)
    ));
}

#[test]
fn validation_requires_explicit_isolated_credentials() {
    let mut plan = sample_plan();
    plan.execution.credentials = ExecutionCredentials::Unspecified;

    assert!(matches!(
        plan.validate(),
        Err(DerivationValidationError::UnspecifiedExecutionCredentials)
    ));
}

#[test]
fn complete_evaluation_fingerprint_is_part_of_canonical_identity() {
    let original = sample_plan();
    let original_id = original.derivation_id();
    assert_eq!(original.provenance.recipe.imported_modules.len(), 1);
    assert_eq!(
        original.provenance.recipe.imported_modules[0].logical_name,
        "sample.provenance"
    );
    let mutations: Vec<NamedMutation<EvaluationFingerprint>> = vec![
        (
            "root-logical-name",
            Box::new(|fingerprint| fingerprint.root_logical_name.push_str("-changed")),
        ),
        (
            "root-source-sha256",
            Box::new(|fingerprint| fingerprint.root_source_sha256.push('0')),
        ),
        (
            "import-logical-name",
            Box::new(|fingerprint| fingerprint.imported_modules[0].logical_name.push_str("-changed")),
        ),
        (
            "import-sha256",
            Box::new(|fingerprint| fingerprint.imported_modules[0].sha256.push('0')),
        ),
        (
            "gluon-version",
            Box::new(|fingerprint| fingerprint.gluon_version = "test-gluon-version"),
        ),
        (
            "configuration-abi",
            Box::new(|fingerprint| fingerprint.configuration_abi_version += 1),
        ),
        (
            "evaluator-policy-abi",
            Box::new(|fingerprint| fingerprint.evaluator_policy_version += 1),
        ),
        (
            "explicit-inputs-sha256",
            Box::new(|fingerprint| fingerprint.explicit_inputs_sha256.push('0')),
        ),
        ("aggregate-sha256", Box::new(|fingerprint| fingerprint.sha256.push('0'))),
    ];

    for (name, mutate) in mutations {
        let mut changed = original.clone();
        mutate(&mut changed.provenance.recipe);
        assert_ne!(original_id, changed.derivation_id(), "{name} was not hashed");
    }
}

#[test]
fn nested_provenance_shape_and_order_are_part_of_canonical_identity() {
    let original = sample_plan();
    let original_id = original.derivation_id();
    let mutations: Vec<NamedMutation<DerivationProvenance>> = vec![
        (
            "profile-logical-name",
            Box::new(|provenance| provenance.profiles[0].logical_name.push_str("-changed")),
        ),
        ("profile-order", Box::new(|provenance| provenance.profiles.reverse())),
        (
            "profile-evaluation",
            Box::new(|provenance| provenance.profiles[0].evaluation.root_logical_name.push_str("-changed")),
        ),
        (
            "policy-name",
            Box::new(|provenance| provenance.policy.name.push_str("-changed")),
        ),
        (
            "policy-root",
            Box::new(|provenance| provenance.policy.root.root_logical_name.push_str("-changed")),
        ),
        (
            "empty-policy-layer",
            Box::new(|provenance| {
                provenance.policy.layers.pop();
            }),
        ),
        (
            "policy-layer-order",
            Box::new(|provenance| provenance.policy.layers.reverse()),
        ),
        (
            "policy-layer-name",
            Box::new(|provenance| provenance.policy.layers[0].name.push_str("-changed")),
        ),
        (
            "policy-transition-operation",
            Box::new(|provenance| {
                provenance.policy.layers[0].transitions[0].operation = BuildPolicyOperation::Replace;
            }),
        ),
        (
            "policy-transition-origin",
            Box::new(|provenance| provenance.policy.layers[0].transitions[0].origin.push_str("-changed")),
        ),
        (
            "policy-transition-evaluation",
            Box::new(|provenance| {
                provenance.policy.layers[0].transitions[0]
                    .evaluation
                    .root_logical_name
                    .push_str("-changed");
            }),
        ),
    ];

    for (name, mutate) in mutations {
        let mut changed = original.clone();
        mutate(&mut changed.provenance);
        assert_ne!(original_id, changed.derivation_id(), "{name} was not hashed");
    }
}

#[test]
fn v2_provenance_aggregate_helpers_preserve_nested_semantics() {
    let provenance = sample_provenance();
    let profile_identity = profile_aggregate_fingerprint(&provenance.profiles);
    let policy_identity = policy_composition_identity(&provenance.policy.name, &provenance.policy.layers);

    assert_eq!(profile_identity, profile_aggregate_fingerprint(&provenance.profiles));
    assert_eq!(
        policy_identity,
        policy_composition_identity(&provenance.policy.name, &provenance.policy.layers)
    );

    let mut profiles = provenance.profiles.clone();
    profiles.reverse();
    assert_ne!(profile_identity, profile_aggregate_fingerprint(&profiles));
    profiles = provenance.profiles.clone();
    profiles[0].logical_name.push_str("-changed");
    assert_ne!(profile_identity, profile_aggregate_fingerprint(&profiles));
    profiles = provenance.profiles.clone();
    profiles[0].evaluation.evaluator_policy_version += 1;
    assert_ne!(profile_identity, profile_aggregate_fingerprint(&profiles));

    let mut layers = provenance.policy.layers.clone();
    layers.pop();
    assert_ne!(
        policy_identity,
        policy_composition_identity(&provenance.policy.name, &layers),
        "an empty named layer is semantic"
    );
    layers = provenance.policy.layers.clone();
    layers.reverse();
    assert_ne!(
        policy_identity,
        policy_composition_identity(&provenance.policy.name, &layers)
    );
    layers = provenance.policy.layers.clone();
    layers[0].transitions[0].evaluation.configuration_abi_version += 1;
    assert_ne!(
        policy_identity,
        policy_composition_identity(&provenance.policy.name, &layers)
    );
}

#[test]
fn validation_rejects_invalid_nested_evaluation_fingerprints_at_the_exact_field() {
    let cases: Vec<NamedMutation<DerivationPlan>> = vec![
        (
            "provenance.recipe",
            Box::new(|plan| plan.provenance.recipe.sha256.push('0')),
        ),
        (
            "provenance.profiles[0].evaluation",
            Box::new(|plan| plan.provenance.profiles[0].evaluation.sha256.push('0')),
        ),
        (
            "provenance.policy.root",
            Box::new(|plan| plan.provenance.policy.root.sha256.push('0')),
        ),
        (
            "provenance.policy.layers[0].transitions[0].evaluation",
            Box::new(|plan| {
                plan.provenance.policy.layers[0].transitions[0]
                    .evaluation
                    .sha256
                    .push('0');
            }),
        ),
    ];

    for (expected, corrupt) in cases {
        let mut plan = sample_plan();
        corrupt(&mut plan);
        assert!(matches!(
            plan.validate(),
            Err(DerivationValidationError::InvalidEvaluationFingerprint { field, .. })
                if field == expected
        ));
    }
}

#[test]
fn validation_rejects_ambient_or_non_normalized_provenance_names() {
    let cases: Vec<NamedMutation<DerivationPlan>> = vec![
        (
            "provenance.recipe.root_logical_name",
            Box::new(|plan| plan.provenance.recipe.root_logical_name = "/home/user/stone.glu".to_owned()),
        ),
        (
            "provenance.recipe.imported_modules[0].logical_name",
            Box::new(|plan| {
                plan.provenance.recipe.imported_modules[0].logical_name = "nested/../module.glu".to_owned();
            }),
        ),
        (
            "provenance.profiles[0].logical_name",
            Box::new(|plan| plan.provenance.profiles[0].logical_name = "C:\\profile.glu".to_owned()),
        ),
        (
            "provenance.profiles[0].evaluation.root_logical_name",
            Box::new(|plan| {
                plan.provenance.profiles[0].evaluation.root_logical_name = "./profile.glu".to_owned();
            }),
        ),
        (
            "provenance.policy.root.root_logical_name",
            Box::new(|plan| plan.provenance.policy.root.root_logical_name = "policy//root.glu".to_owned()),
        ),
        (
            "provenance.policy.layers[0].transitions[0].evaluation.root_logical_name",
            Box::new(|plan| {
                plan.provenance.policy.layers[0].transitions[0]
                    .evaluation
                    .root_logical_name = "/etc/policy.glu".to_owned();
            }),
        ),
    ];

    for (expected, corrupt) in cases {
        let mut plan = sample_plan();
        corrupt(&mut plan);
        assert!(matches!(
            plan.validate(),
            Err(DerivationValidationError::InvalidLogicalName { field, .. })
                if field == expected
        ));
    }
}

#[test]
fn validation_binds_recipe_and_profiles_to_their_locked_inputs() {
    let mut recipe = sample_plan();
    recipe.source_lock_digest = sha256(b"different source lock");
    assert!(matches!(
        recipe.validate(),
        Err(DerivationValidationError::RecipeSourceLockDigestMismatch { .. })
    ));

    let mut blank = sample_plan();
    blank.provenance.profiles[0].logical_name = "  ".to_owned();
    assert!(matches!(
        blank.validate(),
        Err(DerivationValidationError::Empty { field })
            if field == "provenance.profiles[0].logical_name"
    ));

    let mut duplicate = sample_plan();
    duplicate.provenance.profiles[1].logical_name = duplicate.provenance.profiles[0].logical_name.clone();
    assert!(matches!(
        duplicate.validate(),
        Err(DerivationValidationError::DuplicateProfileLogicalName {
            first_index: 0,
            duplicate_index: 1,
            ..
        })
    ));

    let mut aggregate = sample_plan();
    aggregate.build_lock.profile.fingerprint.push_str("-changed");
    assert!(matches!(
        aggregate.validate(),
        Err(DerivationValidationError::ProfileAggregateMismatch { .. })
    ));
}

#[test]
fn validation_binds_policy_name_root_and_composition_to_the_build_lock() {
    let mut name = sample_plan();
    name.build_lock.policy.name.push_str("-changed");
    assert!(matches!(
        name.validate(),
        Err(DerivationValidationError::PolicyNameMismatch { .. })
    ));

    let mut root = sample_plan();
    root.build_lock.policy.fingerprint.push_str("-changed");
    assert!(matches!(
        root.validate(),
        Err(DerivationValidationError::PolicyAggregateMismatch { .. })
    ));

    let mut duplicate = sample_plan();
    duplicate.provenance.policy.layers[1].name = duplicate.provenance.policy.layers[0].name.clone();
    assert!(matches!(
        duplicate.validate(),
        Err(DerivationValidationError::DuplicatePolicyLayer {
            first_index: 0,
            duplicate_index: 1,
            ..
        })
    ));

    let mut composition = sample_plan();
    composition.provenance.policy.layers[1].name.push_str("-changed");
    assert!(matches!(
        composition.validate(),
        Err(DerivationValidationError::PolicyCompositionDigestMismatch { .. })
    ));
}

#[test]
fn validation_rejects_non_normalized_policy_origins() {
    for origin in [
        "/absolute.glu",
        "C:\\policy.glu",
        "nested//policy.glu",
        "nested/./policy.glu",
        "nested/../policy.glu",
    ] {
        let mut plan = sample_plan();
        plan.provenance.policy.layers[0].transitions[0].origin = origin.to_owned();
        assert!(matches!(
            plan.validate(),
            Err(DerivationValidationError::InvalidPolicyOrigin { value, .. }) if value == origin
        ));
    }
}

#[test]
fn validation_replays_policy_transition_state() {
    for operation in [BuildPolicyOperation::Replace, BuildPolicyOperation::Modify] {
        let mut missing_initial_state = sample_plan();
        missing_initial_state.provenance.policy.layers[0].transitions[0].operation = operation;
        assert!(matches!(
            missing_initial_state.validate(),
            Err(DerivationValidationError::InvalidPolicyTransition {
                operation: actual,
                ..
            }) if actual == operation
        ));
    }

    let mut second_add = sample_plan();
    let repeated_add = second_add.provenance.policy.layers[0].transitions[0].clone();
    second_add.provenance.policy.layers[0].transitions.push(repeated_add);
    assert!(matches!(
        second_add.validate(),
        Err(DerivationValidationError::InvalidPolicyTransition {
            operation: BuildPolicyOperation::Add,
            ..
        })
    ));

    let mut absent = sample_plan();
    absent.provenance.policy.layers[0].transitions.clear();
    assert!(matches!(
        absent.validate(),
        Err(DerivationValidationError::MissingPolicyState)
    ));
}

#[test]
fn validation_requires_complete_cast_implementation_identity() {
    for (field, clear) in [
        (
            "cast_version",
            Box::new(|plan: &mut DerivationPlan| plan.cast_version.clear()) as Box<dyn Fn(&mut DerivationPlan)>,
        ),
        (
            "cast_fingerprint",
            Box::new(|plan: &mut DerivationPlan| plan.cast_fingerprint.clear()),
        ),
    ] {
        let mut plan = sample_plan();
        clear(&mut plan);
        assert!(matches!(
            plan.validate(),
            Err(DerivationValidationError::Empty { field: actual }) if actual == field
        ));
    }
}

#[test]
fn validation_rejects_artifact_filename_escape_components() {
    for name in [
        "",
        ".",
        "..",
        "/tmp/escape",
        "../../escape",
        "name/child",
        "name\\child",
    ] {
        let mut plan = sample_plan();
        plan.package.name = name.to_owned();
        assert!(matches!(
            plan.validate(),
            Err(DerivationValidationError::InvalidPackageName { field, value })
                if field == "package.name" && value == name
        ));
    }

    for version in ["1/../../escape", "1\\escape", "1\ninvalid"] {
        let mut plan = sample_plan();
        plan.package.version = version.to_owned();
        assert!(matches!(
            plan.validate(),
            Err(DerivationValidationError::InvalidArtifactComponent { field, value })
                if field == "package.version" && value == version
        ));
    }

    let mut non_numeric_version = sample_plan();
    non_numeric_version.package.version = "v1.0".to_owned();
    assert!(matches!(
        non_numeric_version.validate(),
        Err(DerivationValidationError::InvalidPackageVersion { value }) if value == "v1.0"
    ));

    let mut output_name = sample_plan();
    output_name.outputs[0].name = "../escape".to_owned();
    assert!(matches!(
        output_name.validate(),
        Err(DerivationValidationError::InvalidPackageName { field, .. })
            if field == "outputs[0].name"
    ));

    let mut package_name = sample_plan();
    package_name.outputs[0].package_name = "../escape".to_owned();
    assert!(matches!(
        package_name.validate(),
        Err(DerivationValidationError::InvalidPackageName { field, .. })
            if field == "outputs[0].package_name"
    ));
}

