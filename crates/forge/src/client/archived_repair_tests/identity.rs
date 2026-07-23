//! Strict marker and state-ID race proofs for repaired candidates.

use std::os::unix::fs::PermissionsExt as _;

use fs_err as fs;

use super::*;

#[test]
fn same_content_state_id_inode_replacement_is_preserved_but_never_published() {
    let fixture = Fixture::new(true);
    let old_wrapper = directory_identity(&fixture.archived_root);
    let staging = fixture.client.installation.staging_dir();
    let state_id = staging.join("usr/.stateID");
    let displaced = staging.join("usr/.stateID.displaced");

    let error = fixture
        .client
        .repair_archived_state_with_checkpoint(
            fixture.empty_candidate(),
            &fixture.repaired,
            fixture.snapshot("same-content-state-id-race"),
            |point| {
                if point == ArchivedRepairCheckpoint::MetadataRecorded {
                    fs::rename(&state_id, &displaced).unwrap();
                    fs::write(&state_id, fixture.repaired.id.to_string()).unwrap();
                    fs::set_permissions(&state_id, std::fs::Permissions::from_mode(0o644)).unwrap();
                }
                Ok(())
            },
        )
        .unwrap_err();

    let RepairError::CandidatePreserved { quarantine, .. } = repair_error(error) else {
        panic!("state-ID inode replacement must preserve, not publish, the candidate");
    };
    assert_eq!(directory_identity(&fixture.archived_root), old_wrapper);
    assert_eq!(
        fs::read_to_string(quarantine.join("usr/.stateID")).unwrap(),
        fixture.repaired.id.to_string()
    );
    assert_eq!(
        fs::read_to_string(quarantine.join("usr/.stateID.displaced")).unwrap(),
        fixture.repaired.id.to_string()
    );
    assert_exact_empty_private_staging(&staging);
}

#[test]
fn same_content_tree_marker_inode_replacement_is_preserved_but_never_published() {
    let fixture = Fixture::new(true);
    let old_wrapper = directory_identity(&fixture.archived_root);
    let staging = fixture.client.installation.staging_dir();
    let marker = staging.join("usr/.cast-tree-id");
    let displaced = staging.join("usr/.cast-tree-id.displaced");

    let error = fixture
        .client
        .repair_archived_state_with_checkpoint(
            fixture.empty_candidate(),
            &fixture.repaired,
            fixture.snapshot("same-content-marker-race"),
            |point| {
                if point == ArchivedRepairCheckpoint::MetadataRecorded {
                    let contents = fs::read(&marker).unwrap();
                    fs::rename(&marker, &displaced).unwrap();
                    fs::write(&marker, contents).unwrap();
                    fs::set_permissions(&marker, std::fs::Permissions::from_mode(0o444)).unwrap();
                }
                Ok(())
            },
        )
        .unwrap_err();

    let RepairError::CandidatePreserved { quarantine, .. } = repair_error(error) else {
        panic!("marker inode replacement must preserve, not publish, the candidate");
    };
    assert_eq!(directory_identity(&fixture.archived_root), old_wrapper);
    assert_eq!(
        fs::read(quarantine.join("usr/.cast-tree-id")).unwrap(),
        fs::read(quarantine.join("usr/.cast-tree-id.displaced")).unwrap()
    );
    assert_exact_empty_private_staging(&staging);
}
