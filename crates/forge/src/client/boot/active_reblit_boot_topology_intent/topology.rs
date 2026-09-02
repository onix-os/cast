//! Engine-neutral boot-topology canonicalization.
//!
//! Each declaration language decodes its own DTOs into raw strings and a
//! [`BootTargetInput`], then calls [`assemble_boot_topology`], so equivalent
//! sources in any language reach the identical validated intent value.

use super::{
    ActiveReblitBootPartitionSelector, ActiveReblitBootTopologyIntentError, ActiveReblitBootTopologyIntentValue,
    ActiveReblitBootTopologyTarget,
};

/// Logical name of the fixed machine-local boot-topology slot.
pub(super) const SOURCE_LOGICAL_NAME: &str = "etc/cast/boot-topology.glu";

const MAX_PARTUUID_DIAGNOSTIC_BYTES: usize = 64;
const MAX_MOUNT_POINT_BYTES: usize = 4_095;
const MAX_MOUNT_POINT_COMPONENTS: usize = 128;
const MAX_MOUNT_POINT_COMPONENT_BYTES: usize = 255;
const MAX_MOUNT_POINT_DIAGNOSTIC_BYTES: usize = 256;


/// Engine-neutral boot destination selection, decoded from either engine before
/// the shared canonicalization and cross-selector checks run.
pub(super) enum BootTargetInput {
    AliasEsp,
    DistinctXbootldr { partuuid: String, mount_point: String },
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
            let xbootldr =
                validated_partition_selector("xbootldr.partuuid", "xbootldr.mount_point", partuuid, mount_point)?;
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
