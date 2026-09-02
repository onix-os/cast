//! Restricted Gluon boundary for machine-local boot-topology intent.

use std::time::Duration;

use declarative_config::{
    DeclarationEvaluationError, DeclarationEvaluator, Evaluation as DeclarationEvaluation, EvaluationDeadline,
    LanguageSpec, Limits, Source, SourceRoot,
};
use gluon_config::{EvaluationIdentity, GluonEngine, ImportPolicy};

use super::topology::{BootTargetInput, SOURCE_LOGICAL_NAME, assemble_boot_topology};
use super::{
    ActiveReblitBootPartitionSelector, ActiveReblitBootTopologyIntentError, ActiveReblitBootTopologyIntentValue,
    ActiveReblitBootTopologyTarget, BootTopologyIntentBudget,
};

pub(super) const BOOT_TOPOLOGY_ABI_NAME: &str = "cast.boot_topology.v2";
pub(super) const BOOT_TOPOLOGY_ABI_VERSION: u32 = 2;
pub(super) const BOOT_TOPOLOGY_ABI: &str = include_str!("../../../../gluon/boot_topology.glu");

const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
const MAX_EVALUATION_TIME: Duration = Duration::from_secs(2);

pub(super) fn language_spec() -> LanguageSpec {
    GluonEngine::default().language_spec().clone()
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonBootTopologyIntent {
    esp: GluonPartitionSelector,
    boot: GluonBootTarget,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
enum GluonBootTarget {
    AliasEsp,
    DistinctXbootldr(GluonPartitionSelector),
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonPartitionSelector {
    partuuid: String,
    mount_point: String,
}

/// Stateful Gluon adapter for the closed boot-topology declaration.
///
/// The adapter borrows the caller-owned absolute budget so the neutral typed
/// evaluation boundary cannot replace ActiveReblit's deadline with a fresh
/// relative timeout. Descriptor retention and source revalidation remain in
/// the fixed-path loader which owns that stronger authority.
pub(super) struct GluonBootTopologyIntentEvaluator<'budget> {
    engine: GluonEngine,
    budget: &'budget BootTopologyIntentBudget,
}

impl<'budget> GluonBootTopologyIntentEvaluator<'budget> {
    pub(super) fn new(budget: &'budget BootTopologyIntentBudget) -> Result<Self, ActiveReblitBootTopologyIntentError> {
        budget.require_deadline()?;
        // Called for its check, not its value: it fails if the caller's absolute
        // deadline has already passed. The duration itself must not reach
        // `limits` — see the timeout comment below.
        budget.remaining_duration()?;
        let mut limits = Limits::default();
        limits.max_source_bytes = budget.policy.max_source_bytes;
        limits.max_explicit_input_bytes = 0;
        limits.max_imported_file_bytes = BOOT_TOPOLOGY_ABI.len();
        limits.max_imports = 1;
        limits.max_import_graph_bytes = budget
            .policy
            .max_source_bytes
            .checked_add(BOOT_TOPOLOGY_ABI.len())
            .ok_or(ActiveReblitBootTopologyIntentError::EvaluationContract {
                reason: "source and embedded ABI byte bound overflowed",
            })?;
        // A fixed policy value, never the remaining budget. This timeout is
        // hashed into `resource_policy_sha256` and therefore into
        // `EvaluationIdentity`, so deriving it from wall-clock remaining time
        // makes the identity of *identical source* vary between evaluations.
        // Whenever `remaining` exceeds this constant the `min` clamps and hides
        // the bug; once evaluation runs long enough for `remaining` to drop
        // below it, the preparing and revalidating evaluations hash differently
        // and revalidation fails with "typed value or evaluation fingerprint
        // changed" on source that never changed.
        //
        // The absolute deadline is not weakened: it is owned by the caller's
        // budget and enforced by `require_deadline` here and at every
        // surrounding checkpoint, which is the stronger authority this adapter
        // exists to preserve.
        limits.timeout = MAX_EVALUATION_TIME;

        let mut imports = ImportPolicy::new();
        imports.insert_embedded_module(BOOT_TOPOLOGY_ABI_NAME, BOOT_TOPOLOGY_ABI)?;
        Ok(Self {
            engine: GluonEngine::new(limits).with_import_policy(imports),
            budget,
        })
    }
}

impl DeclarationEvaluator<ActiveReblitBootTopologyIntentValue> for GluonBootTopologyIntentEvaluator<'_> {
    type Identity = EvaluationIdentity;
    type Error = ActiveReblitBootTopologyIntentError;

    fn language_spec(&self) -> &LanguageSpec {
        self.engine.language_spec()
    }

    fn limits(&self) -> Limits {
        self.engine.limits()
    }

    fn with_source_root(&self, source_root: SourceRoot) -> Self {
        Self {
            engine: self.engine.clone().with_source_root(source_root),
            budget: self.budget,
        }
    }

    fn evaluate_within(
        &self,
        source: &Source,
        deadline: EvaluationDeadline,
    ) -> Result<
        DeclarationEvaluation<ActiveReblitBootTopologyIntentValue, Self::Identity>,
        DeclarationEvaluationError<Self::Error>,
    > {
        let evaluation = self
            .engine
            .evaluate_within::<GluonBootTopologyIntent>(source, deadline)
            .map_err(DeclarationEvaluationError::Evaluation)?;
        self.budget
            .require_deadline()
            .map_err(DeclarationEvaluationError::Conversion)?;
        require_fingerprint_contract(&evaluation.identity).map_err(DeclarationEvaluationError::Conversion)?;

        let value = ActiveReblitBootTopologyIntentValue::try_from(evaluation.value)
            .map_err(DeclarationEvaluationError::Conversion)?;
        self.budget
            .require_deadline()
            .map_err(DeclarationEvaluationError::Conversion)?;
        Ok(DeclarationEvaluation {
            value,
            identity: evaluation.identity,
        })
    }
}

fn require_fingerprint_contract(fingerprint: &EvaluationIdentity) -> Result<(), ActiveReblitBootTopologyIntentError> {
    fingerprint.validate()?;
    if fingerprint.root_logical_name != SOURCE_LOGICAL_NAME {
        return Err(ActiveReblitBootTopologyIntentError::EvaluationContract {
            reason: "evaluation fingerprint does not bind the fixed topology-intent source name",
        });
    }
    if fingerprint.explicit_inputs_sha256 != EMPTY_SHA256 {
        return Err(ActiveReblitBootTopologyIntentError::EvaluationContract {
            reason: "boot-topology evaluation admitted explicit external inputs",
        });
    }
    if fingerprint.modules.len() != 1 || fingerprint.modules[0].logical_name != BOOT_TOPOLOGY_ABI_NAME {
        return Err(ActiveReblitBootTopologyIntentError::EvaluationContract {
            reason: "boot-topology intent must import exactly cast.boot_topology.v2",
        });
    }
    Ok(())
}

impl TryFrom<GluonBootTopologyIntent> for ActiveReblitBootTopologyIntentValue {
    type Error = ActiveReblitBootTopologyIntentError;

    fn try_from(value: GluonBootTopologyIntent) -> Result<Self, Self::Error> {
        let boot = match value.boot {
            GluonBootTarget::AliasEsp => BootTargetInput::AliasEsp,
            GluonBootTarget::DistinctXbootldr(selector) => BootTargetInput::DistinctXbootldr {
                partuuid: selector.partuuid,
                mount_point: selector.mount_point,
            },
        };
        assemble_boot_topology(value.esp.partuuid, value.esp.mount_point, boot)
    }
}


#[cfg(test)]
pub(super) fn gluon_value_for_test(
    esp_partuuid: &str,
    esp_mount_point: &str,
    xbootldr: Option<(&str, &str)>,
) -> Result<ActiveReblitBootTopologyIntentValue, ActiveReblitBootTopologyIntentError> {
    let intent = GluonBootTopologyIntent {
        esp: GluonPartitionSelector {
            partuuid: esp_partuuid.to_owned(),
            mount_point: esp_mount_point.to_owned(),
        },
        boot: match xbootldr {
            None => GluonBootTarget::AliasEsp,
            Some((partuuid, mount_point)) => GluonBootTarget::DistinctXbootldr(GluonPartitionSelector {
                partuuid: partuuid.to_owned(),
                mount_point: mount_point.to_owned(),
            }),
        },
    };
    ActiveReblitBootTopologyIntentValue::try_from(intent)
}
