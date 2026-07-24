//! Restricted Gluon boundary for machine-local boot-topology intent.

use std::time::Duration;

use declarative_config::{
    DeclarationEvaluationError, DeclarationEvaluator, EvaluationDeadline,
    Evaluation as DeclarationEvaluation, LanguageSpec, Limits, Source,
    SourceRoot,
};
use gluon_config::{EvaluationIdentity, GluonEngine, ImportPolicy};

use super::{
    ActiveReblitBootPartitionSelector, ActiveReblitBootTopologyIntentError, ActiveReblitBootTopologyIntentValue,
    ActiveReblitBootTopologyTarget, BootTopologyIntentBudget,
};

pub(super) const BOOT_TOPOLOGY_ABI_NAME: &str = "cast.boot_topology.v2";
pub(super) const BOOT_TOPOLOGY_ABI_VERSION: u32 = 2;
pub(super) const BOOT_TOPOLOGY_ABI: &str = include_str!("../../../../gluon/boot_topology.glu");
pub(super) const SOURCE_LOGICAL_NAME: &str = "etc/cast/boot-topology.glu";

const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
const MAX_EVALUATION_TIME: Duration = Duration::from_secs(2);
const MAX_PARTUUID_DIAGNOSTIC_BYTES: usize = 64;
const MAX_MOUNT_POINT_BYTES: usize = 4_095;
const MAX_MOUNT_POINT_COMPONENTS: usize = 128;
const MAX_MOUNT_POINT_COMPONENT_BYTES: usize = 255;
const MAX_MOUNT_POINT_DIAGNOSTIC_BYTES: usize = 256;

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
    pub(super) fn new(
        budget: &'budget BootTopologyIntentBudget,
    ) -> Result<Self, ActiveReblitBootTopologyIntentError> {
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
        Ok(Self {
            engine: GluonEngine::new(limits).with_import_policy(imports),
            budget,
        })
    }
}

impl DeclarationEvaluator<ActiveReblitBootTopologyIntentValue>
    for GluonBootTopologyIntentEvaluator<'_>
{
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
        DeclarationEvaluation<
            ActiveReblitBootTopologyIntentValue,
            Self::Identity,
        >,
        DeclarationEvaluationError<Self::Error>,
    > {
        let evaluation = self
            .engine
            .evaluate_within::<GluonBootTopologyIntent>(source, deadline)
            .map_err(DeclarationEvaluationError::Evaluation)?;
        self.budget
            .require_deadline()
            .map_err(DeclarationEvaluationError::Conversion)?;
        require_fingerprint_contract(&evaluation.identity)
            .map_err(DeclarationEvaluationError::Conversion)?;

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

fn require_fingerprint_contract(
    fingerprint: &EvaluationIdentity,
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
    if fingerprint.modules.len() != 1 || fingerprint.modules[0].logical_name != BOOT_TOPOLOGY_ABI_NAME
    {
        return Err(ActiveReblitBootTopologyIntentError::EvaluationContract {
            reason: "boot-topology intent must import exactly cast.boot_topology.v2",
        });
    }
    Ok(())
}

/// Engine-neutral boot destination selection, decoded from either engine before
/// the shared canonicalization and cross-selector checks run.
pub(super) enum BootTargetInput {
    AliasEsp,
    DistinctXbootldr { partuuid: String, mount_point: String },
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

/// Shared, engine-neutral assembly: canonicalize the ESP and boot selectors and
/// enforce the distinct-target cross-checks. Both the Gluon and Lua adapters
/// decode their own DTOs into raw strings and a [`BootTargetInput`], then call
/// this so equivalent sources reach the identical validated intent value.
pub(super) fn assemble_boot_topology(
    esp_partuuid: String,
    esp_mount_point: String,
    boot: BootTargetInput,
) -> Result<ActiveReblitBootTopologyIntentValue, ActiveReblitBootTopologyIntentError> {
    let esp = validated_partition_selector("esp.partuuid", "esp.mount_point", esp_partuuid, esp_mount_point)?;
    let boot = match boot {
        BootTargetInput::AliasEsp => ActiveReblitBootTopologyTarget::AliasEsp,
        BootTargetInput::DistinctXbootldr { partuuid, mount_point } => {
            let xbootldr = validated_partition_selector("xbootldr.partuuid", "xbootldr.mount_point", partuuid, mount_point)?;
            if xbootldr.partuuid == esp.partuuid {
                return Err(invalid_partuuid(
                    "xbootldr.partuuid",
                    &xbootldr.partuuid,
                    "distinct ESP and XBOOTLDR PARTUUIDs must not be equal",
                ));
            }
            if xbootldr.mount_point_hint == esp.mount_point_hint {
                return Err(invalid_mount_point_selector(
                    "xbootldr.mount_point",
                    &xbootldr.mount_point_hint,
                    "distinct ESP and XBOOTLDR mount-point selectors must not be equal",
                ));
            }
            ActiveReblitBootTopologyTarget::DistinctXbootldr(xbootldr)
        }
    };
    Ok(ActiveReblitBootTopologyIntentValue { esp, boot })
}

fn validated_partition_selector(
    partuuid_field: &'static str,
    mount_point_field: &'static str,
    partuuid: String,
    mount_point: String,
) -> Result<ActiveReblitBootPartitionSelector, ActiveReblitBootTopologyIntentError> {
    Ok(ActiveReblitBootPartitionSelector {
        partuuid: canonical_partuuid(partuuid_field, partuuid)?,
        mount_point_hint: lexical_mount_point_hint(mount_point_field, mount_point)?,
    })
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

fn lexical_mount_point_hint(
    field: &'static str,
    value: String,
) -> Result<Box<str>, ActiveReblitBootTopologyIntentError> {
    let bytes = value.as_bytes();
    if bytes.len() > MAX_MOUNT_POINT_BYTES {
        return Err(invalid_mount_point_selector(
            field,
            &value,
            "mount-point selector exceeds 4095 bytes",
        ));
    }
    if bytes.first() != Some(&b'/') {
        return Err(invalid_mount_point_selector(
            field,
            &value,
            "mount-point selector must be absolute",
        ));
    }
    if bytes == b"/" {
        return Err(invalid_mount_point_selector(
            field,
            &value,
            "the filesystem root is not a boot destination selector",
        ));
    }
    if bytes.contains(&0) {
        return Err(invalid_mount_point_selector(
            field,
            &value,
            "mount-point selector contains a NUL byte",
        ));
    }

    let mut component_count = 0usize;
    for component in value[1..].split('/') {
        component_count += 1;
        if component_count > MAX_MOUNT_POINT_COMPONENTS {
            return Err(invalid_mount_point_selector(
                field,
                &value,
                "mount-point selector exceeds 128 components",
            ));
        }
        if component.is_empty() {
            return Err(invalid_mount_point_selector(
                field,
                &value,
                "mount-point selector contains an empty component, repeated slash, or trailing slash",
            ));
        }
        if matches!(component, "." | "..") {
            return Err(invalid_mount_point_selector(
                field,
                &value,
                "mount-point selector contains a dot or dot-dot component",
            ));
        }
        if component.len() > MAX_MOUNT_POINT_COMPONENT_BYTES {
            return Err(invalid_mount_point_selector(
                field,
                &value,
                "mount-point selector component exceeds 255 bytes",
            ));
        }
    }

    Ok(value.into_boxed_str())
}

fn invalid_mount_point_selector(
    field: &'static str,
    value: &str,
    reason: &'static str,
) -> ActiveReblitBootTopologyIntentError {
    let mut preview_bytes = value.len().min(MAX_MOUNT_POINT_DIAGNOSTIC_BYTES);
    while !value.is_char_boundary(preview_bytes) {
        preview_bytes -= 1;
    }
    ActiveReblitBootTopologyIntentError::InvalidMountPointSelector {
        field,
        value_preview: value[..preview_bytes].to_owned().into_boxed_str(),
        actual_bytes: value.len(),
        reason,
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
