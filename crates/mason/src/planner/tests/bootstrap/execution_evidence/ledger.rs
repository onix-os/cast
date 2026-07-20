//! Canonical digest ledger for independently authenticated bundle snapshots.

use std::collections::BTreeMap;

use sha2::{Digest, Sha256};

use super::{ArtifactEvidence, ArtifactInventory, BundleObservation, checked_sum, digest};

pub(super) const BUNDLE_LEDGER_SCHEMA: &str = "cast.fixtures-ci.bundle.v1";
const BUNDLE_LEDGER_DOMAIN: &[u8] = b"cast.fixtures-ci.bundle.v1\0";
const MAX_ARTIFACT_NAME_BYTES: usize = 255;
const MAX_ARTIFACT_BYTES: u64 = 128 * 1024 * 1024;
const MAX_BUNDLE_BYTES: u64 = 256 * 1024 * 1024;

pub(super) fn capture_inventory(fixture: &str, bundle: &BTreeMap<String, Vec<u8>>) -> ArtifactInventory {
    let (bounded_count, bounded_total) = require_bundle_sizes(bundle);
    let mut entries = Vec::with_capacity(bundle.len());
    let mut stone_count = 0_u64;
    let mut manifest_count = 0_u64;
    for (name, bytes) in bundle {
        let kind = classify_name(name);
        match kind {
            "stone" => stone_count = stone_count.checked_add(1).expect("Stone count overflowed"),
            "manifest-bin" | "manifest-jsonc" => {
                manifest_count = manifest_count.checked_add(1).expect("manifest count overflowed")
            }
            _ => unreachable!("artifact classifier returned an unknown kind"),
        }
        assert!(!bytes.is_empty(), "{fixture}: proof artifact {name:?} is empty");
        entries.push(ArtifactEvidence {
            name: name.clone(),
            kind,
            byte_count: u64::try_from(bytes.len()).expect("artifact size exceeds u64"),
            sha256: digest(bytes),
        });
    }
    assert_eq!(
        manifest_count, 2,
        "{fixture}: proof inventory must contain two manifests"
    );
    assert_eq!(
        stone_count,
        expected_stones(fixture),
        "{fixture}: proof inventory Stone count drifted"
    );
    let artifact_count = u64::try_from(entries.len()).expect("artifact count exceeds u64");
    assert_eq!(artifact_count, bounded_count);
    assert_eq!(artifact_count, stone_count + manifest_count);
    let total_bytes = checked_sum(entries.iter().map(|entry| entry.byte_count), "bundle byte count");
    assert_eq!(total_bytes, bounded_total);
    ArtifactInventory {
        stone_count,
        manifest_count,
        artifact_count,
        total_bytes,
        ledger_sha256: ledger_digest(bundle),
        entries,
    }
}

pub(super) fn observe_bundle(point: &'static str, bundle: &BTreeMap<String, Vec<u8>>) -> BundleObservation {
    let (artifact_count, total_bytes) = require_bundle_sizes(bundle);
    BundleObservation {
        point,
        artifact_count,
        total_bytes,
        ledger_sha256: ledger_digest(bundle),
    }
}

fn require_bundle_sizes(bundle: &BTreeMap<String, Vec<u8>>) -> (u64, u64) {
    let artifact_count = u64::try_from(bundle.len()).expect("bundle artifact count exceeds u64");
    let total_bytes = checked_sum(
        bundle.values().map(|bytes| {
            let byte_count = u64::try_from(bytes.len()).expect("bundle artifact size exceeds u64");
            require_artifact_size(byte_count);
            byte_count
        }),
        "bundle observation byte count",
    );
    require_bundle_size(total_bytes);
    (artifact_count, total_bytes)
}

fn require_artifact_size(byte_count: u64) {
    assert!(byte_count > 0, "proof artifact is empty");
    assert!(
        byte_count <= MAX_ARTIFACT_BYTES,
        "proof artifact exceeds its {MAX_ARTIFACT_BYTES}-byte boundary"
    );
}

fn require_bundle_size(byte_count: u64) {
    assert!(byte_count > 0, "proof bundle is empty");
    assert!(
        byte_count <= MAX_BUNDLE_BYTES,
        "proof bundle exceeds its {MAX_BUNDLE_BYTES}-byte aggregate boundary"
    );
}

pub(super) fn ledger_digest(bundle: &BTreeMap<String, Vec<u8>>) -> String {
    let mut ledger = Sha256::new();
    ledger.update(BUNDLE_LEDGER_DOMAIN);
    ledger.update(
        u64::try_from(bundle.len())
            .expect("bundle count exceeds u64")
            .to_le_bytes(),
    );
    for (name, bytes) in bundle {
        require_safe_name(name);
        ledger.update(
            u64::try_from(name.len())
                .expect("artifact name length exceeds u64")
                .to_le_bytes(),
        );
        ledger.update(name.as_bytes());
        ledger.update(
            u64::try_from(bytes.len())
                .expect("artifact byte length exceeds u64")
                .to_le_bytes(),
        );
        ledger.update(Sha256::digest(bytes));
    }
    hex::encode(ledger.finalize())
}

fn classify_name(name: &str) -> &'static str {
    require_safe_name(name);
    match name {
        "manifest.x86_64.bin" => "manifest-bin",
        "manifest.x86_64.jsonc" => "manifest-jsonc",
        _ if name.ends_with(".stone") => "stone",
        _ => panic!("proof artifact name has no admitted kind: {name:?}"),
    }
}

fn require_safe_name(name: &str) {
    assert!(!name.is_empty(), "proof artifact name is empty");
    assert!(
        name.len() <= MAX_ARTIFACT_NAME_BYTES,
        "proof artifact name exceeds {MAX_ARTIFACT_NAME_BYTES} bytes"
    );
    assert!(
        name.bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'+' | b'-')),
        "proof artifact name is not one safe ASCII component: {name:?}"
    );
    assert!(
        name.as_bytes()[0].is_ascii_alphanumeric(),
        "proof artifact name must start with an ASCII alphanumeric byte: {name:?}"
    );
}

pub(super) fn expected_stones(fixture: &str) -> u64 {
    match fixture {
        "autotools"
        | "autotools-options"
        | "cargo"
        | "cargo-features"
        | "cargo-vendored"
        | "cmake"
        | "custom"
        | "factory-override"
        | "hooks-patch"
        | "meson"
        | "multiple-sources"
        | "post-install-smoke-test" => 9,
        "external-test-vectors" | "header-only-library" => 2,
        "daemon-generated" | "plugin-output" => 3,
        "split" => 5,
        "generated-config"
        | "generated-shell"
        | "desktop-integration"
        | "font-family"
        | "gettext-localization"
        | "go-module"
        | "pgo-workload"
        | "python-module"
        | "relation-policy"
        | "system-integration-assets"
        | "userspace-profile" => 1,
        _ => panic!("unknown execution fixture in proof inventory: {fixture:?}"),
    }
}

#[cfg(test)]
pub(super) fn require_artifact_size_for_test(byte_count: u64) {
    require_artifact_size(byte_count);
}

#[cfg(test)]
pub(super) fn require_bundle_size_for_test(byte_count: u64) {
    require_bundle_size(byte_count);
}
