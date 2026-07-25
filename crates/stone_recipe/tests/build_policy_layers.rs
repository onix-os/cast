use declarative_config::{
    DeclarationEvaluationError, DeclarationEvaluator,
    DeclarationInputEvaluator, Evaluation, Source,
};
use gluon_config::EvaluationIdentity;
use stone_recipe::build_policy::layers::{
    BuildPolicyLayerEntrySpec, BuildPolicyLayerSpec, BuildPolicyOperation, BuildPolicyRootConversionError,
    BuildPolicyRootSpec, GluonBuildPolicyRootEvaluator,
};

type RootEvaluation = Evaluation<BuildPolicyRootSpec, EvaluationIdentity>;
type RootEvaluationError =
    DeclarationEvaluationError<BuildPolicyRootConversionError>;

fn evaluate(source: &Source) -> Result<RootEvaluation, RootEvaluationError> {
    DeclarationEvaluator::<BuildPolicyRootSpec>::evaluate(
        &GluonBuildPolicyRootEvaluator::default(),
        source,
    )
}

fn evaluate_with_inputs(
    source: &Source,
    explicit_inputs: &[u8],
) -> Result<RootEvaluation, RootEvaluationError> {
    DeclarationInputEvaluator::<BuildPolicyRootSpec>::evaluate_with_inputs(
        &GluonBuildPolicyRootEvaluator::default(),
        source,
        explicit_inputs,
    )
}

fn authored(body: &str) -> Source {
    Source::new(
        "policy.glu",
        format!("let layers = import! cast.build_policy.layers.v1\n{body}"),
    )
}

#[test]
fn retired_boulder_layer_abi_is_not_a_compatibility_alias() {
    let error = evaluate(&Source::new(
        "retired-policy-layers.glu",
        "import! boulder.build_policy.layers.v1",
    ))
    .unwrap_err();

    assert!(error.to_string().contains("boulder.build_policy.layers.v1"));
}

#[test]
fn ordered_layer_manifest_preserves_every_authored_operation() {
    let evaluated = evaluate(&authored(
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

    assert_eq!(
        evaluated.value,
        BuildPolicyRootSpec {
            name: "repository".to_owned(),
            layers: vec![
                BuildPolicyLayerSpec {
                    name: "foundation".to_owned(),
                    entries: vec![
                        BuildPolicyLayerEntrySpec {
                            operation: BuildPolicyOperation::Add,
                            origin: "default.glu".to_owned(),
                        },
                        BuildPolicyLayerEntrySpec {
                            operation: BuildPolicyOperation::Modify,
                            origin: "layers/local.glu".to_owned(),
                        },
                    ],
                },
                BuildPolicyLayerSpec {
                    name: "replacement".to_owned(),
                    entries: vec![BuildPolicyLayerEntrySpec {
                        operation: BuildPolicyOperation::Replace,
                        origin: "replacement.glu".to_owned(),
                    }],
                },
            ],
        }
    );
    assert!(
        evaluated
            .identity
            .modules
            .iter()
            .any(|module| module.logical_name == "cast.build_policy.layers.v1")
    );
}

#[test]
fn manifest_validation_rejects_ambiguous_layers_and_origins() {
    let duplicate = evaluate(&authored(
        r#"layers.policy "repository" [
    layers.layer "same" [],
    layers.layer "same" [],
]"#,
    ))
    .unwrap_err();
    assert!(matches!(
        duplicate,
        DeclarationEvaluationError::Conversion(BuildPolicyRootConversionError::DuplicateLayer { name })
            if name == "same"
    ));

    for origin in ["", "/absolute.glu", "../escape.glu", "nested//module.glu"] {
        let error = evaluate(&authored(&format!(
            "layers.policy \"repository\" [layers.layer \"one\" [layers.add {origin:?}]]"
        )))
        .unwrap_err();
        assert!(matches!(
            error,
            DeclarationEvaluationError::Conversion(
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
    let first = evaluate_with_inputs(&source, b"module-a").unwrap();
    let repeated = evaluate_with_inputs(&source, b"module-a").unwrap();
    let changed = evaluate_with_inputs(&source, b"module-b").unwrap();

    assert_eq!(first.identity, repeated.identity);
    assert_ne!(first.identity, changed.identity);
}
