use super::*;

fn residue_path(client: &Client, archived: &State, marker_token: &str) -> PathBuf {
    client.installation.state_quarantine_dir().join(
        archived_state_prune_quarantine_name(archived.id, marker_token)
            .unwrap()
            .to_string_lossy()
            .as_ref(),
    )
}

fn identity(path: &Path) -> (u64, u64, u32) {
    let metadata = fs::symlink_metadata(path).unwrap();
    (metadata.dev(), metadata.ino(), metadata.mode())
}

#[test]
fn prepared_process_loss_blocks_reopened_client_without_changing_evidence() {
    let Fixture {
        _temporary,
        client,
        archived,
        wrapper,
        marker_token,
    } = Fixture::new();
    let prune =
        RetainedArchivedStatePrune::prepare(&client.installation, &client.state_db, &[archived.clone()]).unwrap();
    let residue = residue_path(&client, &archived, &marker_token);
    let residue_identity = identity(&residue);
    let marker = fs::read(wrapper.join("usr/.cast-tree-id")).unwrap();
    assert!(fs::read_dir(&residue).unwrap().next().is_none());
    let root = client.installation.root.clone();

    drop(prune);
    drop(client);
    expect_restart_prune_residue(&root, &residue);

    assert_eq!(identity(&residue), residue_identity);
    assert!(fs::read_dir(&residue).unwrap().next().is_none());
    assert_eq!(fs::read(wrapper.join("usr/.cast-tree-id")).unwrap(), marker);
}

#[test]
fn detached_pre_database_process_loss_blocks_reopened_client_without_changing_evidence() {
    let Fixture {
        _temporary,
        client,
        archived,
        wrapper,
        marker_token,
    } = Fixture::new();
    let mut prune =
        RetainedArchivedStatePrune::prepare(&client.installation, &client.state_db, &[archived.clone()]).unwrap();
    let residue = residue_path(&client, &archived, &marker_token);
    let quarantined_wrapper = prune.detach_all(&client.installation, &client.state_db).unwrap()[0]
        .quarantine
        .clone();
    let residue_identity = identity(&residue);
    let wrapper_identity = identity(&quarantined_wrapper);
    let marker = fs::read(quarantined_wrapper.join("usr/.cast-tree-id")).unwrap();
    assert!(!wrapper.exists());
    assert_eq!(client.state_db.get(archived.id).unwrap(), archived);
    let root = client.installation.root.clone();

    drop(prune);
    drop(client);
    expect_restart_prune_residue(&root, &residue);

    assert_eq!(identity(&residue), residue_identity);
    assert_eq!(identity(&quarantined_wrapper), wrapper_identity);
    assert_eq!(fs::read(quarantined_wrapper.join("usr/.cast-tree-id")).unwrap(), marker);
    assert!(!wrapper.exists());
    let state_db = db::state::Database::new(root.join(".cast/db/state").to_str().unwrap()).unwrap();
    assert_eq!(state_db.get(archived.id).unwrap(), archived);
}

#[test]
fn post_database_process_loss_blocks_reopened_client_without_changing_stranded_evidence() {
    let Fixture {
        _temporary,
        client,
        archived,
        wrapper,
        marker_token,
    } = Fixture::new();
    let mut prune =
        RetainedArchivedStatePrune::prepare(&client.installation, &client.state_db, &[archived.clone()]).unwrap();
    let residue = residue_path(&client, &archived, &marker_token);
    let quarantined_wrapper = prune.detach_all(&client.installation, &client.state_db).unwrap()[0]
        .quarantine
        .clone();
    prune
        .remove_database_rows(&client.installation, &client.state_db)
        .unwrap();
    let residue_identity = identity(&residue);
    let wrapper_identity = identity(&quarantined_wrapper);
    let marker = fs::read(quarantined_wrapper.join("usr/.cast-tree-id")).unwrap();
    assert!(client.state_db.get(archived.id).is_err());
    let root = client.installation.root.clone();

    drop(prune);
    drop(client);
    expect_restart_prune_residue(&root, &residue);

    assert_eq!(identity(&residue), residue_identity);
    assert_eq!(identity(&quarantined_wrapper), wrapper_identity);
    assert_eq!(fs::read(quarantined_wrapper.join("usr/.cast-tree-id")).unwrap(), marker);
    assert!(!wrapper.exists());
    let state_db = db::state::Database::new(root.join(".cast/db/state").to_str().unwrap()).unwrap();
    assert!(state_db.get(archived.id).is_err());
}
