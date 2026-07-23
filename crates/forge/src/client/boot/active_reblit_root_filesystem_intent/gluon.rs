//! Restricted Gluon boundary for machine-local root-filesystem intent.

use std::time::Duration;

use gluon_config::{EvaluationFingerprint, Evaluator, ImportPolicy, Limits, Source};

use super::{
    ActiveReblitRootFilesystemIntentError, RootFilesystemIntentBudget, RootFilesystemIntentValue, normalization,
};

pub(super) const ROOT_FILESYSTEM_ABI_NAME: &str = "cast.root_filesystem.v1";
pub(super) const ROOT_FILESYSTEM_ABI_VERSION: u32 = 1;
pub(super) const ROOT_FILESYSTEM_ABI: &str = include_str!("../../../../gluon/root_filesystem.glu");
pub(super) const SOURCE_LOGICAL_NAME: &str = "etc/cast/root-filesystem.glu";

const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
const MAX_EVALUATION_TIME: Duration = Duration::from_secs(2);

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonRootFilesystemIntent {
    root: String,
}

pub(super) struct EvaluatedRootFilesystemIntent {
    pub(super) value: RootFilesystemIntentValue,
    pub(super) fingerprint: EvaluationFingerprint,
}

pub(super) fn evaluate(
    source_text: &str,
    budget: &mut RootFilesystemIntentBudget,
) -> Result<EvaluatedRootFilesystemIntent, ActiveReblitRootFilesystemIntentError> {
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
    let evaluator = Evaluator::new(limits).with_import_policy(imports);
    let source = Source::new(SOURCE_LOGICAL_NAME, source_text);
    let evaluation = evaluator.evaluate::<GluonRootFilesystemIntent>(&source)?;
    budget.require_deadline()?;
    require_fingerprint_contract(&evaluation.fingerprint)?;

    let value = normalization::materialize_root_argument(evaluation.value.root, budget)?;
    budget.require_deadline()?;
    Ok(EvaluatedRootFilesystemIntent {
        value,
        fingerprint: evaluation.fingerprint,
    })
}

fn require_fingerprint_contract(
    fingerprint: &EvaluationFingerprint,
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
    if fingerprint.imported_modules.len() != 1
        || fingerprint.imported_modules[0].logical_name != ROOT_FILESYSTEM_ABI_NAME
    {
        return Err(ActiveReblitRootFilesystemIntentError::EvaluationContract {
            reason: "root-filesystem intent must import exactly cast.root_filesystem.v1",
        });
    }
    Ok(())
}
