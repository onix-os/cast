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

/// Decode a minimal-form authored recipe and lower it.
///
/// These tests exercise the authored ABI and the shared lowering, not the
/// fully-lowered decode path, so they evaluate through the authored entry point.
pub(super) fn evaluate_package(
    evaluator: &LuaPackageEvaluator,
    source: &Source,
) -> Result<PackageEvaluation, PackageDeclarationError> {
    evaluator
        .evaluate_authored_within(source, declarative_config::EvaluationDeadline::start(TIMEOUT))
        .map_err(PackageDeclarationError::Evaluation)
}

const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

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
