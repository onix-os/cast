use gluon_config::{GluonEngine, Source, SourceRoot};
use stone_recipe::build_policy::{
    AnalyzerKind, ArrayPatch, BuildPolicyConversionError, BuildPolicyPatchSpec, EnvironmentBindingSpec,
    EnvironmentCondition, RetiredTargetPolicySpec, TextSpec, ValuePatch, evaluate_gluon_with, evaluate_patch_gluon,
    evaluate_patch_gluon_with, evaluate_patch_gluon_with_inputs,
};

fn repository_policy() -> stone_recipe::build_policy::BuildPolicySpec {
    let source_root = SourceRoot::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../mason/data/policy")).unwrap();
    let evaluator = GluonEngine::default().with_source_root(source_root.clone());
    let source = source_root
        .load("default.glu", evaluator.limits().max_source_bytes)
        .unwrap();
    evaluate_gluon_with(&evaluator, &source).unwrap().policy
}

fn authored_patch(body: &str) -> Source {
    Source::new(
        "tests/fixtures/build-policy-patch.glu",
        format!("let b = import! cast.build_policy.v5\n{body}\n"),
    )
}

#[test]
fn value_and_array_operations_are_total_and_order_preserving() {
    assert_eq!(ValuePatch::<String>::Keep.apply("current".to_owned()), "current");
    assert_eq!(
        ValuePatch::Set("replacement".to_owned()).apply("current".to_owned()),
        "replacement"
    );

    assert_eq!(
        ArrayPatch::<i32>::Keep.apply_validated_with_limits(vec![1, 2], "values", 4),
        Ok(vec![1, 2])
    );
    assert!(
        ArrayPatch::Replace(Vec::<i32>::new())
            .apply_validated_with_limits(vec![1, 2], "values", 4)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        ArrayPatch::Prepend(vec![0, 1]).apply_validated_with_limits(vec![1, 2], "values", 4),
        Ok(vec![0, 1, 1, 2])
    );
    assert_eq!(
        ArrayPatch::Append(vec![2, 3]).apply_validated_with_limits(vec![1, 2], "values", 4),
        Ok(vec![1, 2, 2, 3])
    );
}

#[test]
fn default_patch_keeps_the_complete_policy() {
    let policy = repository_policy();
    assert_eq!(
        BuildPolicyPatchSpec::default().apply_validated(policy.clone()),
        Ok(policy)
    );
}

#[test]
fn exhaustive_patch_covers_every_build_policy_field() {
    let policy = repository_policy();
    let mut expected = policy.clone();
    expected.build_subdir = "layered-builddir".to_owned();
    expected.retired_targets.clear();
    expected.environment.clear();
    expected.analyzers = vec![AnalyzerKind::Binary, AnalyzerKind::IncludeAny];

    let patch = BuildPolicyPatchSpec {
        build_subdir: ValuePatch::Set(expected.build_subdir.clone()),
        layout: ValuePatch::Set(expected.layout.clone()),
        toolchains: ValuePatch::Set(expected.toolchains.clone()),
        targets: ArrayPatch::Replace(expected.targets.clone()),
        retired_targets: ArrayPatch::Replace(expected.retired_targets.clone()),
        sandbox: ValuePatch::Set(expected.sandbox.clone()),
        build_root: ValuePatch::Set(expected.build_root.clone()),
        sources: ValuePatch::Set(expected.sources.clone()),
        tuning: ValuePatch::Set(expected.tuning.clone()),
        environment: ArrayPatch::Replace(expected.environment.clone()),
        builders: ValuePatch::Set(expected.builders.clone()),
        analyzers: ArrayPatch::Replace(expected.analyzers.clone()),
        pgo: ValuePatch::Set(expected.pgo.clone()),
    };

    assert_eq!(patch.apply_validated(policy).unwrap(), expected);
}

#[test]
fn validation_runs_after_the_patch_is_applied() {
    let patch = BuildPolicyPatchSpec {
        build_subdir: ValuePatch::Set(String::new()),
        ..BuildPolicyPatchSpec::default()
    };

    assert!(matches!(
        patch.apply_validated(repository_policy()),
        Err(BuildPolicyConversionError::Empty { field }) if field == "build_subdir"
    ));
}

#[test]
fn restricted_gluon_bridge_preserves_all_patch_operations() {
    let source = authored_patch(
        r#"
b.policy_patch {
    build_subdir = b.patch.set "layered-builddir",
    targets = b.patch.array.replace [],
    retired_targets = b.patch.array.prepend [b.retired_target {
        name = "removed-test",
        reason = "covered by patch algebra",
    }],
    environment = b.patch.array.append [
        b.env.always "PATCHED" (b.text.literal "yes"),
    ],
    analyzers = b.patch.array.replace [b.analyzer.binary, b.analyzer.include_any],
    .. b.defaults.policy_patch
}
"#,
    );
    let evaluated = evaluate_patch_gluon(&source).unwrap();

    assert!(matches!(evaluated.patch.build_subdir, ValuePatch::Set(ref value) if value == "layered-builddir"));
    assert!(matches!(evaluated.patch.targets, ArrayPatch::Replace(ref values) if values.is_empty()));
    assert!(matches!(
        evaluated.patch.retired_targets,
        ArrayPatch::Prepend(ref values)
            if values == &[RetiredTargetPolicySpec {
                name: "removed-test".to_owned(),
                reason: "covered by patch algebra".to_owned(),
            }]
    ));
    assert!(matches!(
        evaluated.patch.environment,
        ArrayPatch::Append(ref values)
            if values == &[EnvironmentBindingSpec {
                name: "PATCHED".to_owned(),
                value: TextSpec::Literal("yes".to_owned()),
                condition: EnvironmentCondition::Always,
            }]
    ));
    assert!(matches!(evaluated.patch.layout, ValuePatch::Keep));
    assert!(matches!(evaluated.patch.build_root, ValuePatch::Keep));
    assert!(matches!(
        evaluated.patch.analyzers,
        ArrayPatch::Replace(ref values)
            if values == &[AnalyzerKind::Binary, AnalyzerKind::IncludeAny]
    ));

    assert!(matches!(
        evaluated.patch.apply_validated(repository_policy()),
        Err(BuildPolicyConversionError::Empty { field }) if field == "targets"
    ));
}

#[test]
fn patch_bridge_honors_custom_evaluator_and_explicit_identity_inputs() {
    let source = authored_patch("b.defaults.policy_patch");
    let evaluator = GluonEngine::default();
    let plain = evaluate_patch_gluon_with(&evaluator, &source).unwrap();
    let first = evaluate_patch_gluon_with_inputs(&evaluator, &source, b"first").unwrap();
    let second = evaluate_patch_gluon_with_inputs(&evaluator, &source, b"second").unwrap();

    assert_eq!(plain.patch, BuildPolicyPatchSpec::default());
    assert_ne!(first.fingerprint.sha256, second.fingerprint.sha256);
}

#[test]
fn normalized_build_policy_patch_root_matches_the_complete_owned_value() {
    let evaluated = evaluate_patch_gluon(&authored_patch("b.defaults.policy_patch")).unwrap();
    let expected = BuildPolicyPatchSpec {
        build_subdir: ValuePatch::Keep,
        layout: ValuePatch::Keep,
        toolchains: ValuePatch::Keep,
        targets: ArrayPatch::Keep,
        retired_targets: ArrayPatch::Keep,
        sandbox: ValuePatch::Keep,
        build_root: ValuePatch::Keep,
        sources: ValuePatch::Keep,
        tuning: ValuePatch::Keep,
        environment: ArrayPatch::Keep,
        builders: ValuePatch::Keep,
        analyzers: ArrayPatch::Keep,
        pgo: ValuePatch::Keep,
    };

    assert_eq!(evaluated.patch, expected);
}

#[test]
fn analyzer_order_is_preserved_and_participates_in_patch_identity() {
    let first = evaluate_patch_gluon(&authored_patch(
        "b.policy_patch { analyzers = b.patch.array.replace [b.analyzer.binary, b.analyzer.elf, b.analyzer.include_any], .. b.defaults.policy_patch }",
    ))
    .unwrap();
    let second = evaluate_patch_gluon(&authored_patch(
        "b.policy_patch { analyzers = b.patch.array.replace [b.analyzer.elf, b.analyzer.binary, b.analyzer.include_any], .. b.defaults.policy_patch }",
    ))
    .unwrap();

    assert_ne!(first.fingerprint.sha256, second.fingerprint.sha256);
    assert_eq!(
        first.patch.apply_validated(repository_policy()).unwrap().analyzers,
        [AnalyzerKind::Binary, AnalyzerKind::Elf, AnalyzerKind::IncludeAny]
    );
    assert_eq!(
        second.patch.apply_validated(repository_policy()).unwrap().analyzers,
        [AnalyzerKind::Elf, AnalyzerKind::Binary, AnalyzerKind::IncludeAny]
    );
}
