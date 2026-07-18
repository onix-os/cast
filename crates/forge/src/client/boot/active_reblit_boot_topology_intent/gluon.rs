//! Restricted Gluon boundary for machine-local boot-topology intent.

use std::time::Duration;

use gluon_config::{EvaluationFingerprint, Evaluator, ImportPolicy, Limits, Source};

use super::{
    ActiveReblitBootTopologyIntentError, ActiveReblitBootTopologyIntentValue, ActiveReblitBootTopologyTarget,
    BootTopologyIntentBudget,
};

pub(super) const BOOT_TOPOLOGY_ABI_NAME: &str = "cast.boot_topology.v1";
pub(super) const BOOT_TOPOLOGY_ABI_VERSION: u32 = 1;
pub(super) const BOOT_TOPOLOGY_ABI: &str = include_str!("../../../../gluon/boot_topology.glu");
pub(super) const SOURCE_LOGICAL_NAME: &str = "etc/cast/boot-topology.glu";

const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
const MAX_EVALUATION_TIME: Duration = Duration::from_secs(2);
const MAX_PARTUUID_DIAGNOSTIC_BYTES: usize = 64;

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonBootTopologyIntent {
    esp_partuuid: String,
    boot: GluonBootTarget,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
enum GluonBootTarget {
    AliasEsp,
    DistinctXbootldr(String),
}

pub(super) struct EvaluatedBootTopologyIntent {
    pub(super) value: ActiveReblitBootTopologyIntentValue,
    pub(super) fingerprint: EvaluationFingerprint,
}

pub(super) fn evaluate(
    source_text: &str,
    budget: &BootTopologyIntentBudget,
) -> Result<EvaluatedBootTopologyIntent, ActiveReblitBootTopologyIntentError> {
    budget.require_deadline()?;
    let remaining = budget.remaining_duration()?;
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
    limits.timeout = remaining.min(MAX_EVALUATION_TIME);

    let mut imports = ImportPolicy::new();
    imports.insert_embedded_module(BOOT_TOPOLOGY_ABI_NAME, BOOT_TOPOLOGY_ABI)?;
    let evaluator = Evaluator::new(limits).with_import_policy(imports);
    let source = Source::new(SOURCE_LOGICAL_NAME, source_text);
    let evaluation = evaluator.evaluate::<GluonBootTopologyIntent>(&source)?;
    budget.require_deadline()?;
    require_fingerprint_contract(&evaluation.fingerprint)?;

    let value = ActiveReblitBootTopologyIntentValue::try_from(evaluation.value)?;
    Ok(EvaluatedBootTopologyIntent {
        value,
        fingerprint: evaluation.fingerprint,
    })
}

fn require_fingerprint_contract(
    fingerprint: &EvaluationFingerprint,
) -> Result<(), ActiveReblitBootTopologyIntentError> {
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
    if fingerprint.imported_modules.len() != 1 || fingerprint.imported_modules[0].logical_name != BOOT_TOPOLOGY_ABI_NAME
    {
        return Err(ActiveReblitBootTopologyIntentError::EvaluationContract {
            reason: "boot-topology intent must import exactly cast.boot_topology.v1",
        });
    }
    Ok(())
}

impl TryFrom<GluonBootTopologyIntent> for ActiveReblitBootTopologyIntentValue {
    type Error = ActiveReblitBootTopologyIntentError;

    fn try_from(value: GluonBootTopologyIntent) -> Result<Self, Self::Error> {
        let esp_partuuid = canonical_partuuid("esp_partuuid", value.esp_partuuid)?;
        let boot = match value.boot {
            GluonBootTarget::AliasEsp => ActiveReblitBootTopologyTarget::AliasEsp,
            GluonBootTarget::DistinctXbootldr(partuuid) => {
                let partuuid = canonical_partuuid("xbootldr_partuuid", partuuid)?;
                if partuuid == esp_partuuid {
                    return Err(invalid_partuuid(
                        "xbootldr_partuuid",
                        &partuuid,
                        "distinct ESP and XBOOTLDR PARTUUIDs must not be equal",
                    ));
                }
                ActiveReblitBootTopologyTarget::DistinctXbootldr(partuuid)
            }
        };
        Ok(Self { esp_partuuid, boot })
    }
}

fn canonical_partuuid(field: &'static str, value: String) -> Result<Box<str>, ActiveReblitBootTopologyIntentError> {
    let bytes = value.as_bytes();
    let canonical = bytes.len() == 36
        && bytes.iter().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                *byte == b'-'
            } else {
                byte.is_ascii_digit() || (b'a'..=b'f').contains(byte)
            }
        });
    if !canonical {
        return Err(invalid_partuuid(
            field,
            &value,
            "expected one lowercase canonical 8-4-4-4-12 UUID",
        ));
    }
    if bytes.iter().filter(|byte| **byte != b'-').all(|byte| *byte == b'0') {
        return Err(invalid_partuuid(
            field,
            &value,
            "the nil UUID is not a partition identity",
        ));
    }
    Ok(value.into_boxed_str())
}

fn invalid_partuuid(field: &'static str, value: &str, reason: &'static str) -> ActiveReblitBootTopologyIntentError {
    let mut preview_bytes = value.len().min(MAX_PARTUUID_DIAGNOSTIC_BYTES);
    while !value.is_char_boundary(preview_bytes) {
        preview_bytes -= 1;
    }
    ActiveReblitBootTopologyIntentError::InvalidPartUuid {
        field,
        value_preview: value[..preview_bytes].to_owned().into_boxed_str(),
        actual_bytes: value.len(),
        reason,
    }
}
