use declarative_config::{
    DeclarationEvaluationError, DeclarationEvaluator,
    DeclarationInputEvaluator, Evaluation, Source, SourceRoot,
};
use declarative_config::EvaluationIdentity;
use stone_recipe::build_policy::{
    AnalyzerKind, ArrayPatch, BuildPolicyConversionError, BuildPolicyPatchSpec, BuildPolicySpec,
    EnvironmentBindingSpec, EnvironmentCondition, LuaBuildPolicyEvaluator, RetiredTargetPolicySpec, TextSpec,
    ValuePatch,
};

type PatchEvaluation = Evaluation<BuildPolicyPatchSpec, EvaluationIdentity>;
type PolicyEvaluationError = DeclarationEvaluationError<BuildPolicyConversionError>;

fn repository_policy() -> BuildPolicySpec {
    let source_root = SourceRoot::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../mason/data/policy")).unwrap();
    let evaluator = DeclarationEvaluator::<BuildPolicySpec>::with_source_root(
        &LuaBuildPolicyEvaluator::default(),
        source_root.clone(),
    );
    let source = source_root
        .load(
            "default.lua",
            DeclarationEvaluator::<BuildPolicySpec>::limits(&evaluator).max_source_bytes,
        )
        .unwrap();
    DeclarationEvaluator::<BuildPolicySpec>::evaluate(&evaluator, &source)
        .unwrap()
        .value
}

fn evaluate_patch(
    evaluator: &LuaBuildPolicyEvaluator,
    source: &Source,
) -> Result<PatchEvaluation, PolicyEvaluationError> {
    DeclarationEvaluator::<BuildPolicyPatchSpec>::evaluate(evaluator, source)
}

fn evaluate_default_patch(source: &Source) -> Result<PatchEvaluation, PolicyEvaluationError> {
    evaluate_patch(&LuaBuildPolicyEvaluator::default(), source)
}

fn evaluate_patch_with_inputs(
    evaluator: &LuaBuildPolicyEvaluator,
    source: &Source,
    explicit_inputs: &[u8],
) -> Result<PatchEvaluation, PolicyEvaluationError> {
    DeclarationInputEvaluator::<BuildPolicyPatchSpec>::evaluate_with_inputs(evaluator, source, explicit_inputs)
}

/// A sparse patch: every field defaults to `keep`, and `overrides` names the
/// ones under test.
fn authored_patch(overrides: &[(&str, &str)]) -> Source {
    let mut fields = [
        "build_subdir",
        "layout",
        "toolchains",
        "targets",
        "retired_targets",
        "sandbox",
        "build_root",
        "sources",
        "tuning",
        "environment",
        "builders",
        "analyzers",
        "pgo",
    ]
    .into_iter()
    .map(|field| (field, r#"{ kind = "keep" }"#.to_owned()))
    .collect::<Vec<_>>();
    for (field, value) in overrides {
        let slot = fields
            .iter_mut()
            .find(|(name, _)| name == field)
            .expect("patched field is part of the patch spec");
        slot.1 = (*value).to_owned();
    }
    let body = fields
        .into_iter()
        .map(|(field, value)| format!("{field} = {value}"))
        .collect::<Vec<_>>()
        .join(", ");
    Source::new(
        "tests/fixtures/build-policy-patch.lua",
        format!("return {{ {body} }}\n"),
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
fn the_restricted_declaration_bridge_preserves_all_patch_operations() {
    let source = authored_patch(&[
        ("build_subdir", r#"{ kind = "set", value = "layered-builddir" }"#),
        ("targets", r#"{ kind = "replace", values = {} }"#),
        (
            "retired_targets",
            r#"{ kind = "prepend", values = { { name = "removed-test",
               reason = "covered by patch algebra" } } }"#,
        ),
        (
            "environment",
            r#"{ kind = "append", values = { { name = "PATCHED",
               value = { kind = "literal", value = "yes" }, condition = "always" } } }"#,
        ),
        (
            "analyzers",
            r#"{ kind = "replace", values = { "binary", "include_any" } }"#,
        ),
    ]);
    let evaluated = evaluate_default_patch(&source).unwrap();

    assert!(matches!(evaluated.value.build_subdir, ValuePatch::Set(ref value) if value == "layered-builddir"));
    assert!(matches!(evaluated.value.targets, ArrayPatch::Replace(ref values) if values.is_empty()));
    assert!(matches!(
        evaluated.value.retired_targets,
        ArrayPatch::Prepend(ref values)
            if values == &[RetiredTargetPolicySpec {
                name: "removed-test".to_owned(),
                reason: "covered by patch algebra".to_owned(),
            }]
    ));
    assert!(matches!(
        evaluated.value.environment,
        ArrayPatch::Append(ref values)
            if values == &[EnvironmentBindingSpec {
                name: "PATCHED".to_owned(),
                value: TextSpec::Literal("yes".to_owned()),
                condition: EnvironmentCondition::Always,
            }]
    ));
    assert!(matches!(evaluated.value.layout, ValuePatch::Keep));
    assert!(matches!(evaluated.value.build_root, ValuePatch::Keep));
    assert!(matches!(
        evaluated.value.analyzers,
        ArrayPatch::Replace(ref values)
            if values == &[AnalyzerKind::Binary, AnalyzerKind::IncludeAny]
    ));

    assert!(matches!(
        evaluated.value.apply_validated(repository_policy()),
        Err(BuildPolicyConversionError::Empty { field }) if field == "targets"
    ));
}

#[test]
fn patch_bridge_honors_custom_evaluator_and_explicit_identity_inputs() {
    let source = authored_patch(&[]);
    let evaluator = LuaBuildPolicyEvaluator::default();
    let plain = evaluate_patch(&evaluator, &source).unwrap();
    let first = evaluate_patch_with_inputs(&evaluator, &source, b"first").unwrap();
    let second = evaluate_patch_with_inputs(&evaluator, &source, b"second").unwrap();

    assert_eq!(plain.value, BuildPolicyPatchSpec::default());
    assert_ne!(first.identity.sha256, second.identity.sha256);
}

#[test]
fn normalized_build_policy_patch_root_matches_the_complete_owned_value() {
    let evaluated = evaluate_default_patch(&authored_patch(&[])).unwrap();
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

    assert_eq!(evaluated.value, expected);
}

#[test]
fn analyzer_order_is_preserved_and_participates_in_patch_identity() {
    let replace = |values: &str| {
        authored_patch(&[(
            "analyzers",
            &format!(r#"{{ kind = "replace", values = {{ {values} }} }}"#),
        )])
    };
    let first = evaluate_default_patch(&replace(r#""binary", "elf", "include_any""#)).unwrap();
    let second = evaluate_default_patch(&replace(r#""elf", "binary", "include_any""#)).unwrap();

    assert_ne!(first.identity.sha256, second.identity.sha256);
    assert_eq!(
        first.value.apply_validated(repository_policy()).unwrap().analyzers,
        [AnalyzerKind::Binary, AnalyzerKind::Elf, AnalyzerKind::IncludeAny]
    );
    assert_eq!(
        second.value.apply_validated(repository_policy()).unwrap().analyzers,
        [AnalyzerKind::Elf, AnalyzerKind::Binary, AnalyzerKind::IncludeAny]
    );
}
