use std::convert::Infallible;

use declarative_config::{
    DeclarationEvaluationError, DeclarationInputEvaluator,
    Evaluation as DeclarationEvaluation, EvaluationIdentity,
};
use gluon_config::{GluonEngine, ImportPolicy, Limits, Source};

#[test]
fn gluon_engine_returns_a_deterministic_valid_result() {
    let policy = ImportPolicy::new()
        .with_embedded_module("fixture.answer", "41")
        .unwrap();
    let source = Source::new("root.glu", "import! fixture.answer");
    let engine = GluonEngine::default().with_import_policy(policy);

    let first = engine
        .evaluate_with_inputs::<i64>(&source, b"adapter-input-v1")
        .unwrap();
    let repeated = engine
        .evaluate_with_inputs::<i64>(&source, b"adapter-input-v1")
        .unwrap();

    assert_eq!(first, repeated);
    assert_eq!(first.value, 41);
    assert_eq!(first.identity.validate(), Ok(()));
}

#[test]
fn typed_input_role_matches_the_inherent_gluon_pipeline() {
    let policy = ImportPolicy::new()
        .with_embedded_module("fixture.answer", "41")
        .unwrap();
    let source = Source::new("root.glu", "import! fixture.answer");
    let engine = GluonEngine::default().with_import_policy(policy);
    let explicit_inputs = b"adapter-input-v1";

    let inherent = engine
        .evaluate_with_inputs::<i64>(&source, explicit_inputs)
        .unwrap();
    let typed: Result<
        DeclarationEvaluation<i64, EvaluationIdentity>,
        DeclarationEvaluationError<Infallible>,
    > = <GluonEngine as DeclarationInputEvaluator<i64>>::evaluate_with_inputs(
        &engine,
        &source,
        explicit_inputs,
    );
    let typed = typed.unwrap();

    assert_eq!(typed.value, inherent.value);
    assert_eq!(typed.identity, inherent.identity);
    assert_eq!(typed.identity.configuration_abi.name(), "cast.configuration");
    assert_eq!(typed.identity.configuration_abi.version(), "1");
    assert_eq!(typed.identity.evaluator_policy.as_str(), "1");
    assert_eq!(typed.identity.engine.implementation(), "gluon-vm");
    assert_eq!(typed.identity.validate(), Ok(()));

    let changed = <GluonEngine as DeclarationInputEvaluator<i64>>::evaluate_with_inputs(
        &engine,
        &source,
        b"adapter-input-v2",
    )
    .unwrap();
    assert_ne!(typed.identity.sha256, changed.identity.sha256);
}

#[test]
fn gluon_engine_exposes_validated_language_and_builder_policy() {
    let limits = Limits {
        max_imports: 7,
        ..Limits::default()
    };
    let policy = ImportPolicy::new()
        .with_embedded_module("fixture.answer", "42")
        .unwrap();
    let engine = GluonEngine::new(limits).with_import_policy(policy);
    let spec = engine.language_spec();

    assert_eq!(engine.limits(), limits);
    assert!(engine.import_policy().clone().with_embedded_module("fixture.more", "43").is_ok());
    assert_eq!(spec.language().as_str(), "gluon");
    assert_eq!(spec.engine().implementation(), "gluon-vm");
    assert_eq!(spec.engine().version(), "0.18.3");
    assert_eq!(spec.extension(), "glu");
    assert_eq!(spec.source_profile(), "declaration-v1");
}
