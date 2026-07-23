use gluon_config::Source;
use stone_recipe::build_policy::layers::{
    BuildPolicyOperation, BuildPolicyRootConversionError, BuildPolicyRootEvaluationError, evaluate_gluon,
    evaluate_gluon_with_inputs,
};

fn authored(body: &str) -> Source {
    Source::new(
        "policy.glu",
        format!("let layers = import! cast.build_policy.layers.v1\n{body}"),
    )
}

#[test]
fn retired_boulder_layer_abi_is_not_a_compatibility_alias() {
    let error = evaluate_gluon(&Source::new(
        "retired-policy-layers.glu",
        "import! boulder.build_policy.layers.v1",
    ))
    .unwrap_err();

    assert!(error.to_string().contains("boulder.build_policy.layers.v1"));
}

#[test]
fn ordered_layer_manifest_preserves_every_authored_operation() {
    let evaluated = evaluate_gluon(&authored(
        r#"layers.policy "repository" [
    layers.layer "foundation" [
        layers.add "default.glu",
        layers.modify "layers/local.glu",
    ],
    layers.layer "replacement" [
        layers.replace "replacement.glu",
    ],
]"#,
    ))
    .unwrap();

    assert_eq!(evaluated.root.name, "repository");
    assert_eq!(
        evaluated
            .root
            .layers
            .iter()
            .map(|layer| layer.name.as_str())
            .collect::<Vec<_>>(),
        ["foundation", "replacement"]
    );
    assert_eq!(
        evaluated.root.layers[0]
            .entries
            .iter()
            .map(|entry| (entry.operation, entry.origin.as_str()))
            .collect::<Vec<_>>(),
        [
            (BuildPolicyOperation::Add, "default.glu"),
            (BuildPolicyOperation::Modify, "layers/local.glu"),
        ]
    );
    assert_eq!(
        evaluated.root.layers[1].entries[0].operation,
        BuildPolicyOperation::Replace
    );
    assert!(
        evaluated
            .fingerprint
            .imported_modules
            .iter()
            .any(|module| module.logical_name == "cast.build_policy.layers.v1")
    );
}

#[test]
fn manifest_validation_rejects_ambiguous_layers_and_origins() {
    let duplicate = evaluate_gluon(&authored(
        r#"layers.policy "repository" [
    layers.layer "same" [],
    layers.layer "same" [],
]"#,
    ))
    .unwrap_err();
    assert!(matches!(
        duplicate,
        BuildPolicyRootEvaluationError::Conversion(BuildPolicyRootConversionError::DuplicateLayer { name })
            if name == "same"
    ));

    for origin in ["", "/absolute.glu", "../escape.glu", "nested//module.glu"] {
        let error = evaluate_gluon(&authored(&format!(
            "layers.policy \"repository\" [layers.layer \"one\" [layers.add {origin:?}]]"
        )))
        .unwrap_err();
        assert!(matches!(
            error,
            BuildPolicyRootEvaluationError::Conversion(
                BuildPolicyRootConversionError::Empty { .. } | BuildPolicyRootConversionError::InvalidOrigin { .. }
            )
        ));
    }
}

#[test]
fn composed_module_input_changes_the_manifest_fingerprint() {
    let source = authored(
        r#"layers.policy "repository" [
    layers.layer "foundation" [layers.add "default.glu"],
]"#,
    );
    let first = evaluate_gluon_with_inputs(&gluon_config::Evaluator::default(), &source, b"module-a").unwrap();
    let repeated = evaluate_gluon_with_inputs(&gluon_config::Evaluator::default(), &source, b"module-a").unwrap();
    let changed = evaluate_gluon_with_inputs(&gluon_config::Evaluator::default(), &source, b"module-b").unwrap();

    assert_eq!(first.fingerprint, repeated.fingerprint);
    assert_ne!(first.fingerprint, changed.fingerprint);
}
