// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use gluon_config::Source;
use stone_recipe::build_policy::{
    BuildPolicyConversionError, BuildToolSpec, ContextValue, TextSpec, evaluate_gluon, evaluate_gluon_with_inputs,
};

fn repository_policy() -> Source {
    Source::new(
        "bin/boulder/data/policy/default.glu",
        include_str!("../../../bin/boulder/data/policy/default.glu"),
    )
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
    let mut policy = evaluate_gluon(&repository_policy()).unwrap().policy;
    policy.targets.push(policy.targets[0].clone());

    assert!(matches!(
        policy.validate(),
        Err(BuildPolicyConversionError::Duplicate { field, value })
            if field == "targets" && value == "x86_64"
    ));
}
