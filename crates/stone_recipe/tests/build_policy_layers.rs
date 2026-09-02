use declarative_config::{
    DeclarationEvaluationError, DeclarationEvaluator,
    DeclarationInputEvaluator, Evaluation, Source,
};
use declarative_config::EvaluationIdentity;
use stone_recipe::build_policy::layers::{
    BuildPolicyLayerEntrySpec, BuildPolicyLayerSpec, BuildPolicyOperation, BuildPolicyRootConversionError,
    BuildPolicyRootSpec, LuaBuildPolicyRootEvaluator,
};

type RootEvaluation = Evaluation<BuildPolicyRootSpec, EvaluationIdentity>;
type RootEvaluationError =
    DeclarationEvaluationError<BuildPolicyRootConversionError>;

fn evaluate(source: &Source) -> Result<RootEvaluation, RootEvaluationError> {
    DeclarationEvaluator::<BuildPolicyRootSpec>::evaluate(
        &LuaBuildPolicyRootEvaluator::default(),
        source,
    )
}

fn evaluate_with_inputs(
    source: &Source,
    explicit_inputs: &[u8],
) -> Result<RootEvaluation, RootEvaluationError> {
    DeclarationInputEvaluator::<BuildPolicyRootSpec>::evaluate_with_inputs(
        &LuaBuildPolicyRootEvaluator::default(),
        source,
        explicit_inputs,
    )
}

fn authored(body: &str) -> Source {
    Source::new("policy.lua", format!("return {body}"))
}

/// One layer entry: an operation tag and the module it names.
fn entry(operation: &str, origin: &str) -> String {
    format!(r#"{{ operation = {{ kind = "{operation}" }}, origin = "{origin}" }}"#)
}

/// A layer manifest naming `layers`, each a `(name, entries)` pair.
fn manifest(name: &str, layers: &[(&str, Vec<String>)]) -> String {
    let layers = layers
        .iter()
        .map(|(layer, entries)| {
            format!(r#"{{ name = "{layer}", entries = {{ {} }} }}"#, entries.join(", "))
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!(r#"{{ name = "{name}", layers = {{ {layers} }} }}"#)
}

#[test]
fn ordered_layer_manifest_preserves_every_authored_operation() {
    let evaluated = evaluate(&authored(&manifest(
        "repository",
        &[
            (
                "foundation",
                vec![entry("add", "default.lua"), entry("modify", "layers/local.lua")],
            ),
            ("replacement", vec![entry("replace", "replacement.lua")]),
        ],
    )))
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
                            origin: "default.lua".to_owned(),
                        },
                        BuildPolicyLayerEntrySpec {
                            operation: BuildPolicyOperation::Modify,
                            origin: "layers/local.lua".to_owned(),
                        },
                    ],
                },
                BuildPolicyLayerSpec {
                    name: "replacement".to_owned(),
                    entries: vec![BuildPolicyLayerEntrySpec {
                        operation: BuildPolicyOperation::Replace,
                        origin: "replacement.lua".to_owned(),
                    }],
                },
            ],
        }
    );
}

#[test]
fn manifest_validation_rejects_ambiguous_layers_and_origins() {
    let duplicate = evaluate(&authored(&manifest(
        "repository",
        &[("same", Vec::new()), ("same", Vec::new())],
    )))
    .unwrap_err();
    assert!(matches!(
        duplicate,
        DeclarationEvaluationError::Conversion(BuildPolicyRootConversionError::DuplicateLayer { name })
            if name == "same"
    ));

    for origin in ["", "/absolute.lua", "../escape.lua", "nested//module.lua"] {
        let error = evaluate(&authored(&manifest(
            "repository",
            &[("one", vec![entry("add", origin)])],
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
    let source = authored(&manifest(
        "repository",
        &[("foundation", vec![entry("add", "default.lua")])],
    ));
    let first = evaluate_with_inputs(&source, b"module-a").unwrap();
    let repeated = evaluate_with_inputs(&source, b"module-a").unwrap();
    let changed = evaluate_with_inputs(&source, b"module-b").unwrap();

    assert_eq!(first.identity, repeated.identity);
    assert_ne!(first.identity, changed.identity);
}
