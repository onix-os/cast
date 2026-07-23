use declarative_config::{
    DeclarationEvaluationError, DeclarationEvaluator,
    DeclarationInputEvaluator, Evaluation, Source, SourceRoot,
};
use gluon_config::EvaluationFingerprint;
use stone_recipe::package::{
    GluonPackageEvaluator, PackageConversionError, PackageSpec,
};

pub(super) type PackageEvaluation =
    Evaluation<PackageSpec, EvaluationFingerprint>;
pub(super) type PackageDeclarationError =
    DeclarationEvaluationError<PackageConversionError>;

pub(super) fn rooted_package_evaluator(
    source_root: SourceRoot,
) -> GluonPackageEvaluator {
    DeclarationEvaluator::<PackageSpec>::with_source_root(
        &GluonPackageEvaluator::default(),
        source_root,
    )
}

pub(super) fn evaluate_package(
    evaluator: &GluonPackageEvaluator,
    source: &Source,
) -> Result<PackageEvaluation, PackageDeclarationError> {
    DeclarationEvaluator::<PackageSpec>::evaluate(evaluator, source)
}

pub(super) fn evaluate_default_package(
    source: &Source,
) -> Result<PackageEvaluation, PackageDeclarationError> {
    evaluate_package(&GluonPackageEvaluator::default(), source)
}

pub(super) fn evaluate_package_with_inputs(
    evaluator: &GluonPackageEvaluator,
    source: &Source,
    explicit_inputs: &[u8],
) -> Result<PackageEvaluation, PackageDeclarationError> {
    DeclarationInputEvaluator::<PackageSpec>::evaluate_with_inputs(
        evaluator,
        source,
        explicit_inputs,
    )
}
