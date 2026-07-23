use declarative_config::{
    DeclarationEvaluationError, DeclarationEvaluator,
    DeclarationInputEvaluator, Evaluation, Source, SourceRoot,
};
use gluon_config::EvaluationFingerprint;
use stone_recipe::build_policy::{
    BuildPolicyConversionError, BuildPolicySpec, GluonBuildPolicyEvaluator,
};

type PolicyEvaluation = Evaluation<BuildPolicySpec, EvaluationFingerprint>;
type PolicyEvaluationError =
    DeclarationEvaluationError<BuildPolicyConversionError>;

pub(super) fn repository_policy() -> (GluonBuildPolicyEvaluator, Source) {
    let source_root = SourceRoot::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../mason/data/policy"
    ))
    .unwrap();
    let evaluator = DeclarationEvaluator::<BuildPolicySpec>::with_source_root(
        &GluonBuildPolicyEvaluator::default(),
        source_root.clone(),
    );
    let max_source_bytes =
        DeclarationEvaluator::<BuildPolicySpec>::limits(&evaluator)
            .max_source_bytes;
    let source = source_root
        .load("default.glu", max_source_bytes)
        .unwrap();
    (evaluator, source)
}

pub(super) fn evaluate_policy(
    evaluator: &GluonBuildPolicyEvaluator,
    source: &Source,
) -> Result<PolicyEvaluation, PolicyEvaluationError> {
    DeclarationEvaluator::<BuildPolicySpec>::evaluate(evaluator, source)
}

pub(super) fn evaluate_policy_with_inputs(
    evaluator: &GluonBuildPolicyEvaluator,
    source: &Source,
    explicit_inputs: &[u8],
) -> Result<PolicyEvaluation, PolicyEvaluationError> {
    DeclarationInputEvaluator::<BuildPolicySpec>::evaluate_with_inputs(
        evaluator,
        source,
        explicit_inputs,
    )
}

pub(super) fn evaluate_default_policy(
    source: &Source,
) -> Result<PolicyEvaluation, PolicyEvaluationError> {
    evaluate_policy(&GluonBuildPolicyEvaluator::default(), source)
}

pub(super) fn repository_policy_value() -> BuildPolicySpec {
    let (evaluator, source) = repository_policy();
    evaluate_policy(&evaluator, &source).unwrap().value
}
