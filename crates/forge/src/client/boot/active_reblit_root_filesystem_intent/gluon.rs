//! Restricted Gluon boundary for machine-local root-filesystem intent.

use std::{
    cell::RefCell,
    rc::Rc,
    time::Duration,
};

use declarative_config::{
    DeclarationEvaluationError, DeclarationEvaluator, EvaluationDeadline,
    Evaluation as DeclarationEvaluation, LanguageSpec, Limits, Source,
    SourceRoot,
};
use gluon_config::{EvaluationIdentity, GluonEngine, ImportPolicy};

use super::{
    ActiveReblitRootFilesystemIntentError, RootFilesystemIntentBudget, RootFilesystemIntentValue, normalization,
};

pub(super) const ROOT_FILESYSTEM_ABI_NAME: &str = "cast.root_filesystem.v1";
pub(super) const ROOT_FILESYSTEM_ABI_VERSION: u32 = 1;
pub(super) const ROOT_FILESYSTEM_ABI: &str = include_str!("../../../../gluon/root_filesystem.glu");
pub(super) const SOURCE_LOGICAL_NAME: &str = "etc/cast/root-filesystem.glu";

const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
const MAX_EVALUATION_TIME: Duration = Duration::from_secs(2);

pub(super) fn language_spec() -> LanguageSpec {
    GluonEngine::default().language_spec().clone()
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonRootFilesystemIntent {
    root: String,
}

/// Stateful Gluon adapter for the closed root-filesystem declaration.
///
/// The shared same-thread budget keeps normalization work and every deadline
/// checkpoint in the existing ActiveReblit authority. The adapter never opens
/// the fixed path; its caller supplies bytes read from the retained inode.
pub(super) struct GluonRootFilesystemIntentEvaluator<'budget> {
    engine: GluonEngine,
    budget: Rc<RefCell<&'budget mut RootFilesystemIntentBudget>>,
}

impl<'budget> GluonRootFilesystemIntentEvaluator<'budget> {
    pub(super) fn new(
        budget: &'budget mut RootFilesystemIntentBudget,
    ) -> Result<Self, ActiveReblitRootFilesystemIntentError> {
        budget.require_deadline()?;
        let remaining = budget.remaining_duration()?;
        let mut limits = Limits::default();
        limits.max_source_bytes = budget.policy.max_source_bytes;
        limits.max_explicit_input_bytes = 0;
        limits.max_imported_file_bytes = ROOT_FILESYSTEM_ABI.len();
        limits.max_imports = 1;
        limits.max_import_graph_bytes = budget
            .policy
            .max_source_bytes
            .checked_add(ROOT_FILESYSTEM_ABI.len())
            .ok_or(ActiveReblitRootFilesystemIntentError::EvaluationContract {
                reason: "source and embedded ABI byte bound overflowed",
            })?;
        limits.timeout = remaining.min(MAX_EVALUATION_TIME);

        let mut imports = ImportPolicy::new();
        imports.insert_embedded_module(ROOT_FILESYSTEM_ABI_NAME, ROOT_FILESYSTEM_ABI)?;
        Ok(Self {
            engine: GluonEngine::new(limits).with_import_policy(imports),
            budget: Rc::new(RefCell::new(budget)),
        })
    }
}

impl DeclarationEvaluator<RootFilesystemIntentValue>
    for GluonRootFilesystemIntentEvaluator<'_>
{
    type Identity = EvaluationIdentity;
    type Error = ActiveReblitRootFilesystemIntentError;

    fn language_spec(&self) -> &LanguageSpec {
        self.engine.language_spec()
    }

    fn limits(&self) -> Limits {
        self.engine.limits()
    }

    fn with_source_root(&self, source_root: SourceRoot) -> Self {
        Self {
            engine: self.engine.clone().with_source_root(source_root),
            budget: Rc::clone(&self.budget),
        }
    }

    fn evaluate_within(
        &self,
        source: &Source,
        deadline: EvaluationDeadline,
    ) -> Result<
        DeclarationEvaluation<RootFilesystemIntentValue, Self::Identity>,
        DeclarationEvaluationError<Self::Error>,
    > {
        let evaluation = self
            .engine
            .evaluate_within::<GluonRootFilesystemIntent>(source, deadline)
            .map_err(DeclarationEvaluationError::Evaluation)?;
        let mut budget = self.budget.borrow_mut();
        budget
            .require_deadline()
            .map_err(DeclarationEvaluationError::Conversion)?;
        require_fingerprint_contract(&evaluation.identity)
            .map_err(DeclarationEvaluationError::Conversion)?;

        let value = normalization::materialize_root_argument(
            evaluation.value.root,
            &mut budget,
        )
        .map_err(DeclarationEvaluationError::Conversion)?;
        budget
            .require_deadline()
            .map_err(DeclarationEvaluationError::Conversion)?;
        Ok(DeclarationEvaluation {
            value,
            identity: evaluation.identity,
        })
    }
}

fn require_fingerprint_contract(
    fingerprint: &EvaluationIdentity,
) -> Result<(), ActiveReblitRootFilesystemIntentError> {
    fingerprint.validate()?;
    if fingerprint.root_logical_name != SOURCE_LOGICAL_NAME {
        return Err(ActiveReblitRootFilesystemIntentError::EvaluationContract {
            reason: "evaluation fingerprint does not bind the fixed root-filesystem source name",
        });
    }
    if fingerprint.explicit_inputs_sha256 != EMPTY_SHA256 {
        return Err(ActiveReblitRootFilesystemIntentError::EvaluationContract {
            reason: "root-filesystem evaluation admitted explicit external inputs",
        });
    }
    if fingerprint.modules.len() != 1
        || fingerprint.modules[0].logical_name != ROOT_FILESYSTEM_ABI_NAME
    {
        return Err(ActiveReblitRootFilesystemIntentError::EvaluationContract {
            reason: "root-filesystem intent must import exactly cast.root_filesystem.v1",
        });
    }
    Ok(())
}
