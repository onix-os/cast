use declarative_config::{
    DeclarationEvaluationError, DeclarationEvaluator,
    DeclarationInputEvaluator, Evaluation, Source, SourceRoot,
};
use declarative_config::EvaluationIdentity;
use stone_recipe::package::{
    LuaPackageEvaluator, PackageConversionError, PackageSpec,
};

pub(super) type PackageEvaluation =
    Evaluation<PackageSpec, EvaluationIdentity>;
pub(super) type PackageDeclarationError =
    DeclarationEvaluationError<PackageConversionError>;

pub(super) fn rooted_package_evaluator(
    source_root: SourceRoot,
) -> LuaPackageEvaluator {
    DeclarationEvaluator::<PackageSpec>::with_source_root(
        &LuaPackageEvaluator::default(),
        source_root,
    )
}

pub(super) fn evaluate_package(
    evaluator: &LuaPackageEvaluator,
    source: &Source,
) -> Result<PackageEvaluation, PackageDeclarationError> {
    DeclarationEvaluator::<PackageSpec>::evaluate(evaluator, source)
}

pub(super) fn evaluate_default_package(
    source: &Source,
) -> Result<PackageEvaluation, PackageDeclarationError> {
    evaluate_package(&LuaPackageEvaluator::default(), source)
}

pub(super) fn evaluate_package_with_inputs(
    evaluator: &LuaPackageEvaluator,
    source: &Source,
    explicit_inputs: &[u8],
) -> Result<PackageEvaluation, PackageDeclarationError> {
    DeclarationInputEvaluator::<PackageSpec>::evaluate_with_inputs(
        evaluator,
        source,
        explicit_inputs,
    )
}
