//! Database and live active-selection race proofs for archived repair.

use std::os::unix::fs::PermissionsExt as _;

use fs_err as fs;

use super::*;

#[test]
fn live_active_selection_change_is_detected_without_trusting_the_cached_field() {
    let fixture = Fixture::new(true);
    let target = fixture.repaired.id;
    let live_state_id = fixture.client.installation.root.join("usr/.stateID");

    let error = fixture
        .client
        .repair_archived_state_with_checkpoint(
            fixture.empty_candidate(),
            &fixture.repaired,
            fixture.snapshot("live-active-selection-race"),
            |point| {
                if point == ArchivedRepairCheckpoint::MetadataRecorded {
                    fs::write(&live_state_id, target.to_string()).unwrap();
                }
                Ok(())
            },
        )
        .unwrap_err();

    let RepairError::CandidatePreservationIncomplete { outcome, .. } = repair_error(error) else {
        panic!("a changed live selection must remain a structured applied preservation failure");
    };
    assert_eq!(outcome, "applied");
    assert_eq!(fs::read_to_string(live_state_id).unwrap(), target.to_string());
    assert_eq!(archived_repair_quarantine_paths(&fixture).len(), 1);
    assert_exact_empty_private_staging(&fixture.client.installation.staging_dir());
}

#[test]
fn repaired_target_row_deletion_is_detected_and_the_candidate_is_still_preserved() {
    let fixture = Fixture::new(true);
    let target = fixture.repaired.id;
    let client = &fixture.client;

    let error = client
        .repair_archived_state_with_checkpoint(
            fixture.empty_candidate(),
            &fixture.repaired,
            fixture.snapshot("target-row-race"),
            |point| {
                if point == ArchivedRepairCheckpoint::MetadataRecorded {
                    client.state_db.remove(&target)?;
                }
                Ok(())
            },
        )
        .unwrap_err();

    let RepairError::CandidatePreservationIncomplete { outcome, .. } = repair_error(error) else {
        panic!("a deleted target row must not allow publication or erase the candidate");
    };
    assert_eq!(outcome, "applied");
    let preserved = archived_repair_quarantine_paths(&fixture);
    assert_eq!(preserved.len(), 1);
    assert_eq!(
        fs::read_to_string(preserved[0].join("usr/.stateID")).unwrap(),
        target.to_string()
    );
    assert_exact_empty_private_staging(&fixture.client.installation.staging_dir());
}

#[test]
fn same_content_live_state_id_inode_replacement_fails_closed() {
    let fixture = Fixture::new(true);
    let old_wrapper = directory_identity(&fixture.archived_root);
    let live_state_id = fixture.client.installation.root.join("usr/.stateID");
    let displaced_state_id = fixture
        .client
        .installation
        .root
        .join("usr/.stateID.displaced-during-repair");
    let active = fixture.active.id;

    let error = fixture
        .client
        .repair_archived_state_with_checkpoint(
            fixture.empty_candidate(),
            &fixture.repaired,
            fixture.snapshot("live-state-id-inode-race"),
            |point| {
                if point == ArchivedRepairCheckpoint::MetadataRecorded {
                    fs::rename(&live_state_id, &displaced_state_id).unwrap();
                    fs::write(&live_state_id, active.to_string()).unwrap();
                    fs::set_permissions(&live_state_id, std::fs::Permissions::from_mode(0o644)).unwrap();
                }
                Ok(())
            },
        )
        .unwrap_err();

    let RepairError::CandidatePreservationIncomplete { outcome, .. } = repair_error(error) else {
        panic!("same-content live state-ID replacement must fail closed after preserving the candidate");
    };
    assert_eq!(outcome, "applied");
    assert_eq!(directory_identity(&fixture.archived_root), old_wrapper);
    assert_eq!(fs::read_to_string(&live_state_id).unwrap(), active.to_string());
    assert_eq!(fs::read_to_string(&displaced_state_id).unwrap(), active.to_string());
    assert_eq!(archived_repair_quarantine_paths(&fixture).len(), 1);
    assert_exact_empty_private_staging(&fixture.client.installation.staging_dir());
}

#[test]
fn same_content_live_tree_marker_inode_replacement_fails_closed() {
    let fixture = Fixture::new(true);
    let old_wrapper = directory_identity(&fixture.archived_root);
    let live_marker = fixture.client.installation.root.join("usr/.cast-tree-id");
    let displaced_marker = fixture
        .client
        .installation
        .root
        .join("usr/.cast-tree-id.displaced-during-repair");

    let error = fixture
        .client
        .repair_archived_state_with_checkpoint(
            fixture.empty_candidate(),
            &fixture.repaired,
            fixture.snapshot("live-tree-marker-inode-race"),
            |point| {
                if point == ArchivedRepairCheckpoint::MetadataRecorded {
                    let contents = fs::read(&live_marker).unwrap();
                    fs::rename(&live_marker, &displaced_marker).unwrap();
                    fs::write(&live_marker, contents).unwrap();
                    fs::set_permissions(&live_marker, std::fs::Permissions::from_mode(0o444)).unwrap();
                }
                Ok(())
            },
        )
        .unwrap_err();

    let RepairError::CandidatePreservationIncomplete { outcome, .. } = repair_error(error) else {
        panic!("same-content live tree-marker replacement must fail closed after preserving the candidate");
    };
    assert_eq!(outcome, "applied");
    assert_eq!(directory_identity(&fixture.archived_root), old_wrapper);
    assert_eq!(fs::read(&live_marker).unwrap(), fs::read(&displaced_marker).unwrap());
    assert_eq!(archived_repair_quarantine_paths(&fixture).len(), 1);
    assert_exact_empty_private_staging(&fixture.client.installation.staging_dir());
}

#[test]
fn same_content_whole_live_usr_replacement_fails_closed() {
    let fixture = Fixture::new(true);
    let old_wrapper = directory_identity(&fixture.archived_root);
    let live_usr = fixture.client.installation.root.join("usr");
    let displaced_usr = fixture.client.installation.root.join("usr.displaced-during-repair");
    let active = fixture.active.id;

    let error = fixture
        .client
        .repair_archived_state_with_checkpoint(
            fixture.empty_candidate(),
            &fixture.repaired,
            fixture.snapshot("whole-live-usr-race"),
            |point| {
                if point == ArchivedRepairCheckpoint::MetadataRecorded {
                    let state_id = fs::read(live_usr.join(".stateID")).unwrap();
                    let tree_marker = fs::read(live_usr.join(".cast-tree-id")).unwrap();
                    fs::rename(&live_usr, &displaced_usr).unwrap();
                    fs::create_dir(&live_usr).unwrap();
                    fs::set_permissions(&live_usr, std::fs::Permissions::from_mode(0o755)).unwrap();
                    fs::write(live_usr.join(".stateID"), state_id).unwrap();
                    fs::set_permissions(live_usr.join(".stateID"), std::fs::Permissions::from_mode(0o644)).unwrap();
                    fs::write(live_usr.join(".cast-tree-id"), tree_marker).unwrap();
                    fs::set_permissions(live_usr.join(".cast-tree-id"), std::fs::Permissions::from_mode(0o444))
                        .unwrap();
                    fs::write(live_usr.join("live-sentinel"), b"live").unwrap();
                }
                Ok(())
            },
        )
        .unwrap_err();

    let RepairError::CandidatePreservationIncomplete { outcome, .. } = repair_error(error) else {
        panic!("whole live-/usr replacement must fail closed after preserving the candidate");
    };
    assert_eq!(outcome, "applied");
    assert_eq!(directory_identity(&fixture.archived_root), old_wrapper);
    assert_eq!(
        fs::read_to_string(live_usr.join(".stateID")).unwrap(),
        active.to_string()
    );
    assert_eq!(
        fs::read_to_string(displaced_usr.join(".stateID")).unwrap(),
        active.to_string()
    );
    assert_ne!(directory_identity(&live_usr), directory_identity(&displaced_usr));
    assert_eq!(archived_repair_quarantine_paths(&fixture).len(), 1);
    assert_exact_empty_private_staging(&fixture.client.installation.staging_dir());
}
