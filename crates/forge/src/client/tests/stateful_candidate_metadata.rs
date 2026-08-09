//! Adversarial proofs for descriptor-bound stateful candidate metadata.

use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

use super::*;

#[test]
fn candidate_usr_clone_failure_precedes_all_metadata_decoration() {
    let fixture = stateful_transition_fixture(false);
    let candidate_usr = fixture.client.installation.staging_path("usr");
    let identity = fixture
        .client
        .prepare_stateful_tree_identity(&candidate_usr, fixture.candidate.id)
        .unwrap();
    candidate_metadata::arm_candidate_usr_clone_fault();

    let error = candidate_metadata::decorate_stateful(&identity, &generated_system_snapshot("clone-fault-candidate"))
        .unwrap_err();

    candidate_metadata::assert_candidate_usr_clone_fault_consumed();
    assert!(matches!(error, Error::StatefulCandidateMetadata { .. }), "{error:#?}");
    assert!(!candidate_usr.join("lib").exists());
    assert!(!candidate_usr.join("lib/os-release").exists());
    assert!(!candidate_usr.join("lib/system-model.glu").exists());
}

#[test]
fn owned_metadata_proof_outlives_source_identity_and_rejects_named_substitution() {
    let fixture = stateful_transition_fixture(false);
    let candidate_usr = fixture.client.installation.staging_path("usr");
    let identity = fixture
        .client
        .prepare_stateful_tree_identity(&candidate_usr, fixture.candidate.id)
        .unwrap();
    let proof =
        candidate_metadata::decorate_stateful(&identity, &generated_system_snapshot("owned-proof-candidate")).unwrap();

    drop(identity);
    proof.revalidate().unwrap();

    let canonical = candidate_usr.join("lib/system-model.glu");
    let displaced = candidate_usr.join("lib/displaced-system-model.glu");
    let expected = fs::read(&canonical).unwrap();
    let original_identity = inode_identity(&canonical);
    fs::rename(&canonical, &displaced).unwrap();
    write_canonical_candidate_file(&canonical, &expected);
    let replacement_identity = inode_identity(&canonical);
    assert_ne!(replacement_identity, original_identity);

    let error = proof.revalidate().unwrap_err();
    let Error::StatefulCandidateMetadata { source } = error else {
        panic!("named metadata substitution returned the wrong error: {error:#?}");
    };
    assert_eq!(
        source.to_string(),
        format!(
            "candidate metadata inode changed while retained at `{}`",
            canonical.display()
        )
    );
    assert_eq!(inode_identity(&displaced), original_identity);
    assert_eq!(inode_identity(&canonical), replacement_identity);
    assert_eq!(fs::read(&displaced).unwrap(), expected);
    assert_eq!(fs::read(&canonical).unwrap(), expected);
}

fn inode_identity(path: &Path) -> (u64, u64) {
    let metadata = fs::symlink_metadata(path).unwrap();
    (metadata.dev(), metadata.ino())
}

fn write_canonical_candidate_file(path: &Path, contents: impl AsRef<[u8]>) {
    fs::write(path, contents).unwrap();
    fs::set_permissions(path, Permissions::from_mode(0o644)).unwrap();
}
