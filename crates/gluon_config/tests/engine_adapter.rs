use gluon_config::{
    Diagnostic, Evaluator, GluonEngine, ImportPolicy, Limits, Source,
};

fn assert_same_diagnostic(left: &Diagnostic, right: &Diagnostic) {
    assert_eq!(left.category, right.category);
    assert_eq!(left.limit, right.limit);
    assert_eq!(left.source_name, right.source_name);
    assert_eq!(left.span, right.span);
    assert_eq!(left.message, right.message);
}

#[test]
fn gluon_engine_and_compatibility_facade_return_the_same_v1_result() {
    let policy = ImportPolicy::new()
        .with_embedded_module("fixture.answer", "41")
        .unwrap();
    let source = Source::new("root.glu", "import! fixture.answer");
    let engine = GluonEngine::default().with_import_policy(policy.clone());
    let evaluator = Evaluator::default().with_import_policy(policy);

    let direct = engine
        .evaluate_with_inputs::<i64>(&source, b"adapter-input-v1")
        .unwrap();
    let compatible = evaluator
        .evaluate_with_inputs::<i64>(&source, b"adapter-input-v1")
        .unwrap();

    assert_eq!(direct, compatible);
    assert_eq!(direct.value, 41);
    assert_eq!(direct.fingerprint.validate(), Ok(()));
}

#[test]
fn gluon_engine_and_compatibility_facade_preserve_diagnostics() {
    let source = Source::new("invalid.glu", "import! fixture.missing");
    let direct = GluonEngine::default().evaluate::<i64>(&source).unwrap_err();
    let compatible = Evaluator::default().evaluate::<i64>(&source).unwrap_err();

    assert_same_diagnostic(&direct, &compatible);
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
