fn repository_policy() -> BuildPolicySpec {
    let source_root =
        gluon_config::SourceRoot::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../mason/data/policy")).unwrap();
    let evaluator = gluon_config::Evaluator::default().with_source_root(source_root.clone());
    let source = source_root
        .load("default.glu", evaluator.limits().max_source_bytes)
        .unwrap();
    evaluate_gluon_with(&evaluator, &source).unwrap().policy
}

#[test]
fn text_node_limit_accepts_n_and_rejects_n_plus_one() {
    let mut limits = BuildPolicyValidationLimits::default();
    limits.max_text_nodes = 4;

    let at_limit = TextSpec::Concat(vec![
        TextSpec::Literal("a".to_owned()),
        TextSpec::Literal("b".to_owned()),
        TextSpec::Literal("c".to_owned()),
    ]);
    assert_eq!(at_limit.validate_with_limits(limits), Ok(()));

    let over_limit = TextSpec::Concat(vec![
        TextSpec::Literal("a".to_owned()),
        TextSpec::Literal("b".to_owned()),
        TextSpec::Literal("c".to_owned()),
        TextSpec::Literal("d".to_owned()),
    ]);
    assert_eq!(
        over_limit.validate_with_limits(limits),
        Err(BuildPolicyConversionError::TextNodeLimit {
            field: "text".to_owned(),
            nodes: 5,
            limit: 4,
        })
    );
}

#[test]
fn text_literal_limits_accept_n_and_reject_n_plus_one() {
    let mut limits = BuildPolicyValidationLimits::default();
    limits.max_text_literal_bytes = 3;
    limits.max_text_total_literal_bytes = 3;
    assert_eq!(TextSpec::Literal("abc".to_owned()).validate_with_limits(limits), Ok(()));
    assert_eq!(
        TextSpec::Literal("abcd".to_owned()).validate_with_limits(limits),
        Err(BuildPolicyConversionError::TextLiteralBytesLimit {
            field: "text".to_owned(),
            bytes: 4,
            limit: 3,
        })
    );

    limits.max_text_literal_bytes = 3;
    let over_total = TextSpec::Concat(vec![
        TextSpec::Literal("ab".to_owned()),
        TextSpec::Literal("cd".to_owned()),
    ]);
    assert_eq!(
        over_total.validate_with_limits(limits),
        Err(BuildPolicyConversionError::TextTotalLiteralBytesLimit {
            field: "text".to_owned(),
            bytes: 4,
            limit: 3,
        })
    );
}

#[test]
fn deeply_nested_text_is_rejected_iteratively() {
    let mut value = TextSpec::Literal("x".to_owned());
    for _ in 0..20_000 {
        value = TextSpec::Concat(vec![value]);
    }
    let mut limits = BuildPolicyValidationLimits::default();
    limits.max_text_nodes = 25_000;
    limits.max_text_depth = 64;
    assert_eq!(
        value.validate_with_limits(limits),
        Err(BuildPolicyConversionError::TextDepthLimit {
            field: "text".to_owned(),
            depth: 65,
            limit: 64,
        })
    );
}

#[test]
fn representative_policy_collection_accepts_n_and_rejects_n_plus_one() {
    let policy = repository_policy();
    let mut limits = BuildPolicyValidationLimits::default();
    limits.max_targets = policy.targets.len();
    assert_eq!(policy.validate_with_limits(limits), Ok(()));

    let mut oversized = policy;
    oversized.targets.push(oversized.targets[0].clone());
    assert_eq!(
        oversized.validate_with_limits(limits),
        Err(BuildPolicyConversionError::CollectionLimit {
            field: "targets".to_owned(),
            count: limits.max_targets + 1,
            limit: limits.max_targets,
        })
    );
}

#[test]
fn aggregate_budgets_accept_n_and_reject_n_plus_one() {
    let mut limits = BuildPolicyValidationLimits::default();
    limits.max_total_collection_items = 3;
    let mut at_limit = ResourceValidator::new(limits);
    at_limit.collection("first", 1, 3).unwrap();
    at_limit.collection("second", 2, 3).unwrap();
    assert_eq!(
        at_limit.collection("third", 1, 3),
        Err(BuildPolicyConversionError::TotalCollectionItemsLimit { count: 4, limit: 3 })
    );

    limits.max_total_string_bytes = 3;
    let mut strings = ResourceValidator::new(limits);
    strings.string("first", "a").unwrap();
    strings.string("second", "bc").unwrap();
    assert_eq!(
        strings.string("third", "d"),
        Err(BuildPolicyConversionError::TotalStringBytesLimit { bytes: 4, limit: 3 })
    );

    limits.max_total_text_literal_bytes = 3;
    let mut text = ResourceValidator::new(limits);
    text.text("first", &TextSpec::Literal("a".to_owned())).unwrap();
    text.text("second", &TextSpec::Literal("bc".to_owned())).unwrap();
    assert_eq!(
        text.text("third", &TextSpec::Literal("d".to_owned())),
        Err(BuildPolicyConversionError::TotalTextLiteralBytesLimit { bytes: 4, limit: 3 })
    );

    limits.max_total_text_nodes = 3;
    let mut nodes = ResourceValidator::new(limits);
    nodes.text("first", &TextSpec::Literal("a".to_owned())).unwrap();
    nodes
        .text("second", &TextSpec::Concat(vec![TextSpec::Literal("b".to_owned())]))
        .unwrap();
    assert_eq!(
        nodes.text("third", &TextSpec::Context(ContextValue::Jobs)),
        Err(BuildPolicyConversionError::TotalTextNodesLimit { nodes: 4, limit: 3 })
    );
}

#[test]
fn actual_policy_total_text_nodes_accepts_n_and_rejects_n_plus_one() {
    let policy = repository_policy();
    let mut measured = ResourceValidator::new(BuildPolicyValidationLimits::default());
    measured.policy(&policy).unwrap();
    let total_text_nodes = measured.total_text_nodes;

    let mut limits = BuildPolicyValidationLimits::default();
    limits.max_total_text_nodes = total_text_nodes;
    assert_eq!(policy.validate_with_limits(limits), Ok(()));

    let mut oversized = policy;
    oversized
        .sources
        .git
        .copy
        .args
        .push(TextSpec::Context(ContextValue::Jobs));
    assert_eq!(
        oversized.validate_with_limits(limits),
        Err(BuildPolicyConversionError::TotalTextNodesLimit {
            nodes: total_text_nodes + 1,
            limit: total_text_nodes,
        })
    );
}

#[test]
fn every_dynamic_policy_branch_contributes_to_the_collection_aggregate() {
    let policy = repository_policy();
    let mut measured = ResourceValidator::new(BuildPolicyValidationLimits::default());
    measured.policy(&policy).unwrap();
    let total_collection_items = measured.total_collection_items;
    let mut limits = BuildPolicyValidationLimits::default();
    limits.max_total_collection_items = total_collection_items;

    macro_rules! assert_counted {
        ($mutate:expr) => {{
            let mut oversized = policy.clone();
            $mutate(&mut oversized);
            assert!(matches!(
                oversized.validate_with_limits(limits),
                Err(BuildPolicyConversionError::TotalCollectionItemsLimit { limit, .. })
                    if limit == total_collection_items
            ));
        }};
    }

    assert_counted!(|value: &mut BuildPolicySpec| value.targets[0].environment.push(value.environment[0].clone()));
    assert_counted!(|value: &mut BuildPolicySpec| value.targets[0]
        .architecture_flags
        .common
        .c
        .push(TextSpec::Literal("-fbranch-test".to_owned())));
    assert_counted!(|value: &mut BuildPolicySpec| value.retired_targets.push(value.retired_targets[0].clone()));
    assert_counted!(|value: &mut BuildPolicySpec| value.build_root.base.push(value.build_root.base[0].clone()));
    assert_counted!(|value: &mut BuildPolicySpec| value
        .sources
        .git
        .copy
        .args
        .push(TextSpec::Literal("branch-test".to_owned())));
    assert_counted!(|value: &mut BuildPolicySpec| value
        .tuning
        .default_groups
        .push(value.tuning.default_groups[0].clone()));
    assert_counted!(|value: &mut BuildPolicySpec| value.environment.push(value.environment[0].clone()));
    assert_counted!(|value: &mut BuildPolicySpec| value
        .builders
        .cmake
        .environment
        .push(value.environment[0].clone()));
    assert_counted!(|value: &mut BuildPolicySpec| value.analyzers.push(value.analyzers[0]));
    assert_counted!(|value: &mut BuildPolicySpec| value.pgo.merge_args.push(value.pgo.merge_args[0].clone()));
}

#[test]
fn array_patch_preflights_lengths_and_preserves_order() {
    assert_eq!(
        ArrayPatch::Append(vec![3]).apply_validated_with_limits(vec![1, 2], "values", 3),
        Ok(vec![1, 2, 3])
    );
    assert_eq!(
        ArrayPatch::Prepend(vec![1, 2]).apply_validated_with_limits(vec![3], "values", 3),
        Ok(vec![1, 2, 3])
    );
    assert_eq!(
        ArrayPatch::Replace(vec![1, 2, 3, 4]).apply_validated_with_limits(Vec::new(), "values", 3),
        Err(BuildPolicyConversionError::CollectionLimit {
            field: "values".to_owned(),
            count: 4,
            limit: 3,
        })
    );
    assert_eq!(
        ArrayPatch::Append(vec![3, 4]).apply_validated_with_limits(vec![1, 2], "values", 3),
        Err(BuildPolicyConversionError::CollectionLimit {
            field: "values".to_owned(),
            count: 4,
            limit: 3,
        })
    );
}

#[test]
fn validated_patch_revalidates_scalar_replacements_with_same_limits() {
    let policy = repository_policy();
    let mut layout = policy.layout.clone();
    layout.prefix = TextSpec::Literal("x".repeat(129));
    let patch = BuildPolicyPatchSpec {
        layout: ValuePatch::Set(layout),
        ..BuildPolicyPatchSpec::default()
    };
    let mut limits = BuildPolicyValidationLimits::default();
    limits.max_text_literal_bytes = 128;

    assert_eq!(
        patch.apply_validated_with_limits(policy, limits),
        Err(BuildPolicyConversionError::TextLiteralBytesLimit {
            field: "layout.prefix".to_owned(),
            bytes: 129,
            limit: 128,
        })
    );
}
