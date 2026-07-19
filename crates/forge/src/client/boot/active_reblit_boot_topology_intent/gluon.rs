//! Restricted Gluon boundary for machine-local boot-topology intent.

use std::time::Duration;

use gluon_config::{EvaluationFingerprint, Evaluator, ImportPolicy, Limits, Source};

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
    budget.require_deadline()?;
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
            reason: "boot-topology intent must import exactly cast.boot_topology.v2",
        });
    }
    Ok(())
}

impl TryFrom<GluonBootTopologyIntent> for ActiveReblitBootTopologyIntentValue {
    type Error = ActiveReblitBootTopologyIntentError;

    fn try_from(value: GluonBootTopologyIntent) -> Result<Self, Self::Error> {
        let esp = validated_partition_selector("esp.partuuid", "esp.mount_point", value.esp)?;
        let boot = match value.boot {
            GluonBootTarget::AliasEsp => ActiveReblitBootTopologyTarget::AliasEsp,
            GluonBootTarget::DistinctXbootldr(selector) => {
                let xbootldr = validated_partition_selector("xbootldr.partuuid", "xbootldr.mount_point", selector)?;
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
        Ok(Self { esp, boot })
    }
}

fn validated_partition_selector(
    partuuid_field: &'static str,
    mount_point_field: &'static str,
    value: GluonPartitionSelector,
) -> Result<ActiveReblitBootPartitionSelector, ActiveReblitBootTopologyIntentError> {
    Ok(ActiveReblitBootPartitionSelector {
        partuuid: canonical_partuuid(partuuid_field, value.partuuid)?,
        mount_point_hint: lexical_mount_point_hint(mount_point_field, value.mount_point)?,
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
