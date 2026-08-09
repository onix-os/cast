#[test]
fn archived_state_activation_carries_each_generated_snapshot_with_its_usr_tree() {
    let temporary = tempfile::tempdir().unwrap();
    let mut client = stateful_test_client(temporary.path());
    let old_snapshot = generated_system_snapshot("old-package");
    let new_snapshot = generated_system_snapshot("new-package");
    // Both states carry provenance: the coordinated activation route verifies
    // the candidate's stored metadata rather than trusting its absence.
    let old = add_state_with_metadata(&client, "old", &old_snapshot);
    let new = add_state_with_metadata(&client, "new", &new_snapshot);
    client.installation.active_state = Some(old.id);

    let old_encoded = old_snapshot.encoded().to_owned();
    record_state_id(&client.installation.root, old.id).unwrap();
    record_candidate_metadata(&client.installation.root, old_snapshot);

    let archived_new_root = client.installation.root_path(new.id.to_string());
    let new_encoded = new_snapshot.encoded().to_owned();
    record_state_id(&archived_new_root, new.id).unwrap();
    record_candidate_metadata(&archived_new_root, new_snapshot);

    let archived = client.activate_state(new.id, true, true).unwrap();

    assert_eq!(archived, old.id);
    assert_generated_snapshot(
        &system_model::snapshot_path(&client.installation.root),
        &new_encoded,
        "new-package",
    );
    assert_generated_snapshot(
        &system_model::snapshot_path(&client.installation.root_path(old.id.to_string())),
        &old_encoded,
        "old-package",
    );
    assert_eq!(
        fs::read_to_string(client.installation.root.join("usr/.stateID")).unwrap(),
        new.id.to_string()
    );
    assert_eq!(
        fs::read_to_string(client.installation.root_path(old.id.to_string()).join("usr/.stateID")).unwrap(),
        old.id.to_string()
    );
}
