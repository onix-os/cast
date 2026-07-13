// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use gluon_config::{Evaluator, Source};
use stone_recipe::build_policy::{
    ArrayPatch, BuildPolicyConversionError, BuildPolicyPatchSpec, EnvironmentBindingSpec, EnvironmentCondition,
    RetiredTargetPolicySpec, TextSpec, ValuePatch, evaluate_gluon, evaluate_patch_gluon, evaluate_patch_gluon_with,
    evaluate_patch_gluon_with_inputs,
};

fn repository_policy_source() -> Source {
    Source::new(
        "bin/boulder/data/policy/default.glu",
        include_str!("../../../bin/boulder/data/policy/default.glu"),
    )
}

fn repository_policy() -> stone_recipe::build_policy::BuildPolicySpec {
    evaluate_gluon(&repository_policy_source()).unwrap().policy
}

fn authored_patch(body: &str) -> Source {
    Source::new(
        "tests/fixtures/build-policy-patch.glu",
        format!("let b = import! boulder.build_policy.v1\n{body}\n"),
    )
}

#[test]
fn value_and_array_operations_are_total_and_order_preserving() {
    assert_eq!(ValuePatch::<String>::Keep.apply("current".to_owned()), "current");
    assert_eq!(
        ValuePatch::Set("replacement".to_owned()).apply("current".to_owned()),
        "replacement"
    );

    assert_eq!(ArrayPatch::<i32>::Keep.apply(vec![1, 2]), [1, 2]);
    assert!(ArrayPatch::Replace(Vec::<i32>::new()).apply(vec![1, 2]).is_empty());
    assert_eq!(ArrayPatch::Prepend(vec![0, 1]).apply(vec![1, 2]), [0, 1, 1, 2]);
    assert_eq!(ArrayPatch::Append(vec![2, 3]).apply(vec![1, 2]), [1, 2, 2, 3]);
}

#[test]
fn default_patch_keeps_the_complete_policy() {
    let policy = repository_policy();
    assert_eq!(BuildPolicyPatchSpec::default().apply(policy.clone()), policy);
}

#[test]
fn exhaustive_patch_covers_every_build_policy_field() {
    let policy = repository_policy();
    let mut expected = policy.clone();
    expected.vendor_id = "layered-linux".to_owned();
    expected.build_subdir = "layered-builddir".to_owned();
    expected.retired_targets.clear();
    expected.environment.clear();

    let patch = BuildPolicyPatchSpec {
        vendor_id: ValuePatch::Set(expected.vendor_id.clone()),
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
        pgo: ValuePatch::Set(expected.pgo.clone()),
    };

    assert_eq!(patch.apply_validated(policy).unwrap(), expected);
}

#[test]
fn validation_runs_after_the_patch_is_applied() {
    let patch = BuildPolicyPatchSpec {
        vendor_id: ValuePatch::Set(String::new()),
        ..BuildPolicyPatchSpec::default()
    };

    assert!(matches!(
        patch.apply_validated(repository_policy()),
        Err(BuildPolicyConversionError::Empty { field }) if field == "vendor_id"
    ));
}

#[test]
fn restricted_gluon_bridge_preserves_all_patch_operations() {
    let source = authored_patch(
        r#"
b.policy_patch {
    vendor_id = b.patch.set "layered-linux",
    targets = b.patch.array.replace [],
    retired_targets = b.patch.array.prepend [b.retired_target {
        name = "removed-test",
        reason = "covered by patch algebra",
    }],
    environment = b.patch.array.append [
        b.env.always "PATCHED" (b.text.literal "yes"),
    ],
    .. b.defaults.policy_patch
}
"#,
    );
    let evaluated = evaluate_patch_gluon(&source).unwrap();

    assert!(matches!(evaluated.patch.vendor_id, ValuePatch::Set(ref value) if value == "layered-linux"));
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

    let policy = evaluated.patch.clone().apply(repository_policy());
    assert!(policy.targets.is_empty());
    assert_eq!(policy.retired_targets[0].name, "removed-test");
    assert_eq!(policy.environment.last().unwrap().name, "PATCHED");
    assert!(matches!(
        evaluated.patch.apply_validated(repository_policy()),
        Err(BuildPolicyConversionError::Empty { field }) if field == "targets"
    ));
}

#[test]
fn patch_bridge_honors_custom_evaluator_and_explicit_identity_inputs() {
    let source = authored_patch("b.defaults.policy_patch");
    let evaluator = Evaluator::default();
    let plain = evaluate_patch_gluon_with(&evaluator, &source).unwrap();
    let first = evaluate_patch_gluon_with_inputs(&evaluator, &source, b"first").unwrap();
    let second = evaluate_patch_gluon_with_inputs(&evaluator, &source, b"second").unwrap();

    assert_eq!(plain.patch, BuildPolicyPatchSpec::default());
    assert_ne!(first.fingerprint.sha256, second.fingerprint.sha256);
}
