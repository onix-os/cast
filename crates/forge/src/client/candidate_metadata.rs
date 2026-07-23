//! Client policy adapter for descriptor-bound candidate metadata.
//!
//! The transition-identity core owns every retained descriptor, namespace
//! operation, publication, and proof. This adapter only derives the semantic
//! `os-release` and system snapshot payloads and preserves the operation-
//! specific client error context for stateful, archived, and ephemeral paths.

use std::{fs::File, path::Path};

use super::Error;
use crate::{
    SystemModel,
    transition_identity::{
        ArchivedStateRepairIdentity, CandidateMetadataError, CandidateMetadataOutputs,
        CandidateMetadataProof as CoreCandidateMetadataProof, CandidateMetadataPublication, RetainedCandidateUsr,
        StatefulTreeIdentity,
    },
};

pub(super) const GENERIC_OS_RELEASE: &str = r#"NAME="Unbranded OS"
VERSION="no-os-info.json"
ID="unbranded-os"
VERSION_CODENAME=no-os-info.json
VERSION_ID="no-os-info.json"
PRETTY_NAME="Unbranded OS no-os-info.json - I forgot to add os-info.json"
HOME_URL="https://github.com/AerynOS/os-info"
BUG_REPORT_URL="https://github.com/AerynOS/os-info/issues""#;

#[derive(Clone, Copy, Debug)]
enum MetadataContext {
    ArchivedRepair,
    Ephemeral,
    Stateful,
}

/// Exact external `/usr` inode retained beneath the already-authenticated
/// ephemeral materialization root. The diagnostic path is never reopened.
#[derive(Debug)]
pub(super) struct RetainedEphemeralUsr {
    core: RetainedCandidateUsr,
}

/// Client-context wrapper around the neutral retained metadata proof.
///
/// The low-level proof owns every capability and has no client dependency;
/// this wrapper remembers only which established client error variant must be
/// restored when later revalidation fails.
#[derive(Debug)]
pub(super) struct CandidateMetadataProof {
    context: MetadataContext,
    core: CoreCandidateMetadataProof,
}

pub(super) fn decorate_archived(
    identity: &ArchivedStateRepairIdentity,
    snapshot: &SystemModel,
) -> Result<CandidateMetadataProof, Error> {
    let (usr, usr_path) = identity.retained_candidate_usr();
    decorate_retained(MetadataContext::ArchivedRepair, usr, usr_path, snapshot)
}

pub(super) fn decorate_stateful(
    identity: &StatefulTreeIdentity,
    snapshot: &SystemModel,
) -> Result<CandidateMetadataProof, Error> {
    let (usr, usr_path) = identity.retained_candidate_usr();
    decorate_retained(MetadataContext::Stateful, usr, usr_path, snapshot)
}

pub(super) fn retain_ephemeral_usr(root: &File, root_path: &Path) -> Result<RetainedEphemeralUsr, Error> {
    RetainedCandidateUsr::retain_under(root, root_path)
        .map(|core| RetainedEphemeralUsr { core })
        .map_err(|source| metadata_error(MetadataContext::Ephemeral, source))
}

pub(super) fn decorate_ephemeral(
    usr: &RetainedEphemeralUsr,
    snapshot: &SystemModel,
) -> Result<CandidateMetadataProof, Error> {
    decorate_retained(MetadataContext::Ephemeral, usr.file(), usr.diagnostic_path(), snapshot)
}

impl RetainedEphemeralUsr {
    pub(super) fn file(&self) -> &File {
        self.core.file()
    }

    pub(super) fn diagnostic_path(&self) -> &Path {
        self.core.diagnostic_path()
    }

    pub(super) fn revalidate_under(&self, root: &File) -> Result<(), Error> {
        self.core
            .revalidate_under(root)
            .map_err(|source| metadata_error(MetadataContext::Ephemeral, source))
    }

    /// Materialization may temporarily widen the retained directory mode and
    /// then apply the declarative final mode. Re-observe that same descriptor
    /// only after the blit has finished; never reacquire `usr` by pathname.
    pub(super) fn refresh_after_materialization(&mut self, root: &File) -> Result<(), Error> {
        self.core
            .refresh_after_materialization(root)
            .map_err(|source| metadata_error(MetadataContext::Ephemeral, source))
    }
}

impl CandidateMetadataProof {
    pub(super) fn revalidate(&self) -> Result<(), Error> {
        self.core
            .revalidate()
            .map_err(|source| metadata_error(self.context, source))
    }

    pub(super) fn diagnostic_path(&self) -> &Path {
        self.core.diagnostic_path()
    }
}

fn decorate_retained(
    context: MetadataContext,
    usr: &File,
    usr_path: &Path,
    snapshot: &SystemModel,
) -> Result<CandidateMetadataProof, Error> {
    let publication =
        CandidateMetadataPublication::begin(usr, usr_path).map_err(|source| metadata_error(context, source))?;
    let os_info = publication
        .read_optional_os_info()
        .map_err(|source| metadata_error(context, source))?;
    let outputs = derive_outputs(os_info.as_deref(), snapshot).map_err(|source| metadata_error(context, source))?;
    publication
        .publish(outputs)
        .map(|core| CandidateMetadataProof { context, core })
        .map_err(|source| metadata_error(context, source))
}

/// Derive the bounded, labeled metadata pair from an already-retained policy
/// input. Descriptor reads and publication stay with the surrounding
/// authority; this function is deterministic and performs no filesystem work.
pub(super) fn derive_outputs(
    os_info: Option<&[u8]>,
    snapshot: &SystemModel,
) -> Result<CandidateMetadataOutputs, CandidateMetadataError> {
    let os_release = render_os_release(os_info);
    CandidateMetadataOutputs::from_policy(os_release.into_bytes(), snapshot.encoded().as_bytes().to_vec())
}

fn render_os_release(os_info: Option<&[u8]>) -> String {
    let Some(bytes) = os_info else {
        return GENERIC_OS_RELEASE.to_owned();
    };
    let Ok(source) = std::str::from_utf8(bytes) else {
        return GENERIC_OS_RELEASE.to_owned();
    };
    let Ok(info) = os_info::load_os_info(source) else {
        return GENERIC_OS_RELEASE.to_owned();
    };
    let release: os_info::OsRelease = (&info).into();
    release.to_string()
}

fn metadata_error(context: MetadataContext, source: CandidateMetadataError) -> Error {
    match context {
        MetadataContext::ArchivedRepair => Error::ArchivedStateRepair {
            source: Box::new(source),
        },
        MetadataContext::Ephemeral => Error::EphemeralCandidateMetadata {
            source: Box::new(source),
        },
        MetadataContext::Stateful => Error::StatefulCandidateMetadata {
            source: Box::new(source),
        },
    }
}

#[cfg(test)]
pub(super) fn arm_applied_private_directory_publication_error(after_parent_sync: impl FnOnce() + 'static) {
    crate::transition_identity::arm_applied_candidate_metadata_directory_publication_error(after_parent_sync);
}

#[cfg(test)]
pub(super) fn arm_candidate_usr_clone_fault() {
    crate::transition_identity::arm_candidate_usr_clone_fault();
}

#[cfg(test)]
pub(super) fn assert_candidate_usr_clone_fault_consumed() {
    crate::transition_identity::assert_candidate_usr_clone_fault_consumed();
}

#[cfg(test)]
pub(super) fn arm_after_first_publication(hook: impl FnOnce() + 'static) {
    crate::transition_identity::arm_after_candidate_metadata_first_publication(hook);
}

#[cfg(test)]
pub(super) fn arm_before_publication(name: &'static str, hook: impl FnOnce() + 'static) {
    crate::transition_identity::arm_before_candidate_metadata_publication(name, hook);
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeSet, fs, os::unix::fs::PermissionsExt as _};

    use super::*;
    use crate::{repository, system_model};

    const VALID_OS_INFO: &[u8] = br#"{
        "os-info-version": "0.1",
        "start_date": "2026-01-01T00:00:00Z",
        "metadata": {
            "identity": {
                "id": "policy-test",
                "id_like": "linux",
                "name": "Policy Test OS",
                "display": "Policy Test OS 1.0",
                "former_identities": []
            },
            "maintainers": {},
            "version": {
                "full": "1.0.0",
                "short": "1.0",
                "build_id": "2026.1",
                "released": "2026-01-01T00:00:00Z"
            }
        },
        "system": {
            "composition": {},
            "features": {
                "atomic_updates": { "strategy": "immediate", "rollback_support": true },
                "boot": {
                    "bootloader": "systemd-boot",
                    "firmware": { "uefi": true, "secure_boot": false, "bios": false }
                },
                "filesystem": { "default": "ext4", "supported": ["ext4"] }
            },
            "kernel": { "type": "monolithic", "name": "linux" },
            "platform": { "architecture": "x86_64", "variant": "test" },
            "update": {
                "strategy": "transactional",
                "cadence": { "type": "rolling" },
                "approach": "full-system"
            }
        },
        "resources": { "websites": {}, "social": {}, "funding": {} }
    }"#;

    const VALID_OS_RELEASE: &str = "NAME=\"Policy Test OS\"\n\
ID=\"policy-test\"\n\
VERSION_ID=\"1.0\"\n\
VERSION=\"1.0.0\"\n\
PRETTY_NAME=\"Policy Test OS 1.0\"\n\
ID_LIKE=\"linux\"\n";

    #[test]
    fn valid_os_info_derives_exact_release_and_snapshot_bytes() {
        assert_policy_outputs(Some(VALID_OS_INFO), VALID_OS_RELEASE.as_bytes());
    }

    #[test]
    fn invalid_os_info_derives_generic_release_and_exact_snapshot_bytes() {
        assert_policy_outputs(Some(br#"{"os-info-version": "invalid"}"#), GENERIC_OS_RELEASE.as_bytes());
    }

    #[test]
    fn non_utf8_os_info_derives_generic_release_and_exact_snapshot_bytes() {
        assert_policy_outputs(Some(b"\xffnot-utf8"), GENERIC_OS_RELEASE.as_bytes());
    }

    #[test]
    fn missing_os_info_derives_generic_release_and_exact_snapshot_bytes() {
        assert_policy_outputs(None, GENERIC_OS_RELEASE.as_bytes());
    }

    #[test]
    fn retained_decoration_publishes_the_same_bytes_as_the_pure_policy() {
        let temporary = tempfile::tempdir().unwrap();
        let usr_path = temporary.path().join("usr");
        fs::create_dir(&usr_path).unwrap();
        fs::create_dir(usr_path.join("lib")).unwrap();
        fs::set_permissions(usr_path.join("lib"), fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(usr_path.join("lib/os-info.json"), VALID_OS_INFO).unwrap();
        fs::set_permissions(
            usr_path.join("lib/os-info.json"),
            fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        let usr = File::open(&usr_path).unwrap();
        let snapshot = snapshot();
        let expected = derive_outputs(Some(VALID_OS_INFO), &snapshot).unwrap();
        let expected_release = expected.os_release().to_vec();
        let expected_snapshot = expected.system_model().to_vec();

        decorate_retained(MetadataContext::Stateful, &usr, &usr_path, &snapshot).unwrap();

        assert_eq!(fs::read(usr_path.join("lib/os-release")).unwrap(), expected_release);
        assert_eq!(
            fs::read(usr_path.join("lib/system-model.glu")).unwrap(),
            expected_snapshot
        );
    }

    fn assert_policy_outputs(os_info: Option<&[u8]>, expected_release: &[u8]) {
        let snapshot = snapshot();
        let outputs = derive_outputs(os_info, &snapshot).unwrap();
        assert_eq!(outputs.os_release(), expected_release);
        assert_eq!(outputs.system_model(), snapshot.encoded().as_bytes());
    }

    fn snapshot() -> SystemModel {
        system_model::create(repository::Map::default(), BTreeSet::new())
    }
}
