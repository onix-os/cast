//! Language-neutral discovery for the fixed build-policy root slot.

use std::path::{Path, PathBuf};

use config::declaration::{
    LoadFixedRootDeclarationError, RootDeclarationSlot,
    TypedDeclarationEvaluatorSet,
    load_required_fixed_root_declaration_from_source_root,
};
use declarative_config::{
    DeclarationEvaluationError, DeclarationEvaluator,
    Evaluation as DeclarationEvaluation, LanguageSpec, Limits, Source,
    SourceRoot,
};
use gluon_config::EvaluationFingerprint;
use stone_recipe::build_policy::layers::{
    BuildPolicyRootConversionError, BuildPolicyRootSpec,
    GluonBuildPolicyRootEvaluator,
};

use super::Error;

const POLICY_ROOT_BASENAME: &str = "policy";
pub(super) const POLICY_ROOT_LOGICAL_NAME: &str = "policy.glu";

/// The exact root bytes and retained directory authority used by the rest of
/// policy composition after language-neutral slot discovery has completed.
pub(super) struct LoadedPolicyRoot {
    pub(super) path: PathBuf,
    pub(super) source_root: SourceRoot,
    pub(super) source: Source,
    pub(super) manifest: BuildPolicyRootSpec,
}

pub(super) fn load(directory: &Path) -> Result<LoadedPolicyRoot, Error> {
    let source_root = SourceRoot::new(directory).map_err(|source| {
        Error::SourceRoot {
            path: directory.to_owned(),
            source: Box::new(source),
        }
    })?;
    let slot = RootDeclarationSlot::new(POLICY_ROOT_BASENAME, POLICY_ROOT_LOGICAL_NAME)
        .expect("the build-policy root slot is canonical");
    let evaluator = PolicyRootDeclarationEvaluator::default();
    let required_language = evaluator.language_spec().clone();
    let evaluators = TypedDeclarationEvaluatorSet::new([evaluator])
        .expect("the build-policy root registers one unique Gluon language");
    let loaded = load_required_fixed_root_declaration_from_source_root(
        directory,
        &source_root,
        &slot,
        &required_language,
        &evaluators,
    )
    .map_err(map_load_error)?;

    Ok(LoadedPolicyRoot {
        path: loaded.path,
        source_root: loaded.value.source_root,
        source: loaded.value.source,
        manifest: loaded.value.manifest,
    })
}

struct PolicyRootDeclaration {
    source_root: SourceRoot,
    source: Source,
    manifest: BuildPolicyRootSpec,
}

struct PolicyRootDeclarationEvaluator {
    evaluator: GluonBuildPolicyRootEvaluator,
    source_root: Option<SourceRoot>,
}

impl Default for PolicyRootDeclarationEvaluator {
    fn default() -> Self {
        Self {
            evaluator: GluonBuildPolicyRootEvaluator::default(),
            source_root: None,
        }
    }
}

impl DeclarationEvaluator<PolicyRootDeclaration> for PolicyRootDeclarationEvaluator {
    type Identity = EvaluationFingerprint;
    type Error = BuildPolicyRootConversionError;

    fn language_spec(&self) -> &LanguageSpec {
        self.evaluator.language_spec()
    }

    fn limits(&self) -> Limits {
        self.evaluator.limits()
    }

    fn with_source_root(&self, source_root: SourceRoot) -> Self {
        Self {
            evaluator: self.evaluator.with_source_root(source_root.clone()),
            source_root: Some(source_root),
        }
    }

    fn evaluate(
        &self,
        source: &Source,
    ) -> Result<
        DeclarationEvaluation<PolicyRootDeclaration, Self::Identity>,
        DeclarationEvaluationError<Self::Error>,
    > {
        let evaluation = self.evaluator.evaluate(source)?;
        let source_root = self
            .source_root
            .clone()
            .expect("the fixed-root loader roots the selected evaluator");

        Ok(DeclarationEvaluation {
            value: PolicyRootDeclaration {
                source_root,
                source: source.clone(),
                manifest: evaluation.value,
            },
            identity: evaluation.identity,
        })
    }
}

fn map_load_error(
    error: LoadFixedRootDeclarationError<BuildPolicyRootConversionError>,
) -> Error {
    match error {
        LoadFixedRootDeclarationError::RetainSourceRoot { path, source } => {
            Error::SourceRoot {
                path,
                source: Box::new(source),
            }
        }
        LoadFixedRootDeclarationError::Read { path, source } => {
            Error::LoadRoot {
                path,
                source: Box::new(source),
            }
        }
        LoadFixedRootDeclarationError::Evaluation { path, source } => {
            Error::EvaluateRoot {
                path,
                source: Box::new(DeclarationEvaluationError::Evaluation(source)),
            }
        }
        LoadFixedRootDeclarationError::Conversion { path, source } => {
            Error::EvaluateRoot {
                path,
                source: Box::new(DeclarationEvaluationError::Conversion(source)),
            }
        }
        error => Error::LoadRootDeclaration(Box::new(error)),
    }
}
