//! Focused fail-closed tests for strict active-state authority.

use std::{collections::BTreeSet, fs, os::unix::fs::PermissionsExt as _};
use std::{thread, time::Duration};

use super::{Client, Error, Installation, active_state_authority::ActiveStateAuthority, record_state_id};
use crate::{Provider, repository, system_model, test_support::prepare_private_installation_root};

struct PreparedRoot {
    temporary: tempfile::TempDir,
    state: crate::state::Id,
}

fn prepared_root() -> PreparedRoot {
    let temporary = tempfile::tempdir().unwrap();
    prepare_private_installation_root(temporary.path());
    let installation = Installation::open(temporary.path(), None).unwrap();
    let client = Client::builder("active-state-authority-test", installation)
        .repositories(repository::Map::default())
        .build()
        .unwrap();
    let state = client.state_db.add(&[], Some("strict active baseline"), None).unwrap();
    record_state_id(&client.installation.root, state.id).unwrap();
    super::record_system_snapshot(&client.installation.root, snapshot("old-active")).unwrap();
    fs::write(client.installation.root.join("usr/payload"), b"original live tree").unwrap();
    fs::create_dir_all(client.installation.root.join("etc")).unwrap();
    fs::set_permissions(client.installation.root.join("etc"), fs::Permissions::from_mode(0o755)).unwrap();
    drop(client);
    PreparedRoot {
        temporary,
        state: state.id,
    }
}

fn open_client(root: &PreparedRoot) -> Result<Client, Error> {
    let installation = Installation::open(root.temporary.path(), None).unwrap();
    Client::builder("active-state-authority-test", installation)
        .repositories(repository::Map::default())
        .build()
}

fn snapshot(package: &str) -> crate::SystemModel {
    system_model::create(
        repository::Map::default(),
        BTreeSet::from([Provider::package_name(package)]),
    )
}

fn state_id_path(root: &PreparedRoot) -> std::path::PathBuf {
    root.temporary.path().join("usr/.stateID")
}

fn assert_live_proof_error<T>(result: Result<T, Error>) {
    let error = match result {
        Ok(_) => panic!("strict active-state authority unexpectedly succeeded"),
        Err(error) => error,
    };
    assert!(matches!(error, Error::LiveActiveStateProof { .. }), "{error:#?}");
}

#[test]
fn restart_rejects_missing_or_malformed_active_metadata_before_client_construction() {
    for malformed in [false, true] {
        let root = prepared_root();
        let path = state_id_path(&root);
        if malformed {
            fs::write(&path, b"malformed").unwrap();
        } else {
            fs::remove_file(&path).unwrap();
        }

        assert_live_proof_error(open_client(&root));
        assert_eq!(
            fs::read(root.temporary.path().join("usr/payload")).unwrap(),
            b"original live tree"
        );
        if malformed {
            assert_eq!(fs::read(path).unwrap(), b"malformed");
        } else {
            assert!(!path.exists());
        }
    }
}

#[test]
fn public_verify_rejects_damaged_active_metadata_without_repairing_it() {
    for malformed in [false, true] {
        let root = prepared_root();
        let client = open_client(&root).unwrap();
        let path = state_id_path(&root);
        if malformed {
            fs::write(&path, b"malformed").unwrap();
        } else {
            fs::remove_file(&path).unwrap();
        }

        assert_live_proof_error(client.verify(true, false));
        assert_eq!(
            fs::read(root.temporary.path().join("usr/payload")).unwrap(),
            b"original live tree"
        );
        if malformed {
            assert_eq!(fs::read(path).unwrap(), b"malformed");
        } else {
            assert!(!path.exists());
        }
    }
}

#[test]
fn matching_canonical_bytes_with_unsafe_metadata_fail_closed() {
    let root = prepared_root();
    let client = open_client(&root).unwrap();
    let path = state_id_path(&root);
    assert_eq!(fs::read_to_string(&path).unwrap(), root.state.to_string());
    fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).unwrap();

    assert_live_proof_error(client.verify(true, false));
    assert_eq!(fs::read_to_string(&path).unwrap(), root.state.to_string());
    assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o7777, 0o666);
}

#[test]
fn suspended_strict_authority_rejects_same_inode_mutation_before_resume() {
    let root = prepared_root();
    let client = open_client(&root).unwrap();
    let path = state_id_path(&root);
    let authority = ActiveStateAuthority::acquire(&client.installation).unwrap();
    let snapshot = authority.suspend(&client.installation).unwrap();

    thread::sleep(Duration::from_millis(2));
    fs::write(&path, b"malformed").unwrap();
    thread::sleep(Duration::from_millis(2));
    fs::write(&path, root.state.to_string()).unwrap();
    assert_live_proof_error(snapshot.resume(&client.installation));
}
