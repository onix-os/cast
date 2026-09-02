use declarative_config::{
    DeclarationEvaluationError, DeclarationEvaluator,
    DeclarationInputEvaluator, Evaluation, Source, SourceRoot,
};
use declarative_config::EvaluationIdentity;
use stone_recipe::build_policy::{
    BuildPolicyConversionError, BuildPolicySpec, LuaBuildPolicyEvaluator,
};

type PolicyEvaluation = Evaluation<BuildPolicySpec, EvaluationIdentity>;
type PolicyEvaluationError =
    DeclarationEvaluationError<BuildPolicyConversionError>;

pub(super) fn repository_policy() -> (LuaBuildPolicyEvaluator, Source) {
    let source_root = SourceRoot::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../mason/data/policy"
    ))
    .unwrap();
    let evaluator = DeclarationEvaluator::<BuildPolicySpec>::with_source_root(
        &LuaBuildPolicyEvaluator::default(),
        source_root.clone(),
    );
    let max_source_bytes =
        DeclarationEvaluator::<BuildPolicySpec>::limits(&evaluator)
            .max_source_bytes;
    let source = source_root
        .load("default.lua", max_source_bytes)
        .unwrap();
    (evaluator, source)
}

pub(super) fn evaluate_policy(
    evaluator: &LuaBuildPolicyEvaluator,
    source: &Source,
) -> Result<PolicyEvaluation, PolicyEvaluationError> {
    DeclarationEvaluator::<BuildPolicySpec>::evaluate(evaluator, source)
}

pub(super) fn evaluate_policy_with_inputs(
    evaluator: &LuaBuildPolicyEvaluator,
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
    evaluate_policy(&LuaBuildPolicyEvaluator::default(), source)
}

pub(super) fn repository_policy_value() -> BuildPolicySpec {
    let (evaluator, source) = repository_policy();
    evaluate_policy(&evaluator, &source).unwrap().value
}
