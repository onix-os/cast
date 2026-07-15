//! Focused live active-state discovery and stale-client regression proofs.

use std::{
    fs::Permissions,
    os::unix::fs::{FileTypeExt as _, MetadataExt as _, PermissionsExt as _, symlink},
};

use fs_err as fs;

use super::{Client, Error, Installation, active_state_snapshot::ActiveStateLease, record_state_id};
use crate::{
    Provider, package, repository, state, test_support::prepare_private_installation_root, tree_marker::TreeMarkerStore,
};

fn prepare_root(path: &std::path::Path) {
    prepare_private_installation_root(path);
}

fn empty_installation(path: &std::path::Path) -> Installation {
    prepare_root(path);
    Installation::open(path, None).unwrap()
}

fn installation_with_state(path: &std::path::Path, id: state::Id) -> Installation {
    prepare_root(path);
    record_state_id(path, id).unwrap();
    Installation::open(path, None).unwrap()
}

fn build_client(installation: Installation, name: &str) -> Client {
    Client::builder(name, installation)
        .repositories(repository::Map::default())
        .build()
        .unwrap()
}

fn assert_changed<T>(result: Result<T, Error>, expected: Option<state::Id>, actual: Option<state::Id>) {
    assert!(matches!(
        result,
        Err(Error::ActiveStateSnapshotChanged {
            expected: found_expected,
            actual: found_actual,
        }) if found_expected == expected && found_actual == actual
    ));
}

fn assert_proof_failure<T>(result: Result<T, Error>) {
    assert!(matches!(result, Err(Error::LiveActiveStateProof { .. })));
}

#[test]
fn exact_empty_and_authenticated_marker_only_first_install_baselines_are_accepted() {
    let empty = tempfile::tempdir().unwrap();
    prepare_root(empty.path());
    fs::create_dir(empty.path().join("usr")).unwrap();
    fs::set_permissions(empty.path().join("usr"), Permissions::from_mode(0o755)).unwrap();
    let installation = Installation::open(empty.path(), None).unwrap();
    assert_eq!(ActiveStateLease::acquire(&installation).unwrap().active(), None);

    let marker_only = tempfile::tempdir().unwrap();
    prepare_root(marker_only.path());
    let usr = marker_only.path().join("usr");
    fs::create_dir(&usr).unwrap();
    fs::set_permissions(&usr, Permissions::from_mode(0o755)).unwrap();
    let store = TreeMarkerStore::open_path(&usr).unwrap();
    drop(store.adopt_or_create_before_journal().unwrap());
    drop(store);
    let installation = Installation::open(marker_only.path(), None).unwrap();
    assert_eq!(ActiveStateLease::acquire(&installation).unwrap().active(), None);
}

#[test]
fn missing_state_id_rejects_nonempty_or_unauthenticated_marker_only_usr() {
    let nonempty = tempfile::tempdir().unwrap();
    prepare_root(nonempty.path());
    let usr = nonempty.path().join("usr");
    fs::create_dir(&usr).unwrap();
    fs::set_permissions(&usr, Permissions::from_mode(0o755)).unwrap();
    fs::write(usr.join("foreign"), b"foreign payload").unwrap();
    let installation = Installation::open(nonempty.path(), None).unwrap();
    assert_proof_failure(ActiveStateLease::acquire(&installation));
    assert_eq!(fs::read(usr.join("foreign")).unwrap(), b"foreign payload");

    let fake_marker = tempfile::tempdir().unwrap();
    prepare_root(fake_marker.path());
    let usr = fake_marker.path().join("usr");
    fs::create_dir(&usr).unwrap();
    fs::set_permissions(&usr, Permissions::from_mode(0o755)).unwrap();
    fs::write(usr.join(".cast-tree-id"), b"not an authenticated marker").unwrap();
    fs::set_permissions(usr.join(".cast-tree-id"), Permissions::from_mode(0o444)).unwrap();
    let installation = Installation::open(fake_marker.path(), None).unwrap();
    assert_proof_failure(ActiveStateLease::acquire(&installation));
    assert_eq!(
        fs::read(usr.join(".cast-tree-id")).unwrap(),
        b"not an authenticated marker"
    );
}

#[test]
fn clean_live_selection_changes_are_distinct_from_invalid_evidence() {
    let none_to_some = tempfile::tempdir().unwrap();
    let installation = empty_installation(none_to_some.path());
    let selected = state::Id::from(17);
    record_state_id(none_to_some.path(), selected).unwrap();
    assert_changed(ActiveStateLease::acquire(&installation), None, Some(selected));

    let some_to_none = tempfile::tempdir().unwrap();
    let expected = state::Id::from(18);
    let installation = installation_with_state(some_to_none.path(), expected);
    fs::remove_file(some_to_none.path().join("usr/.stateID")).unwrap();
    assert_changed(ActiveStateLease::acquire(&installation), Some(expected), None);

    let changed = tempfile::tempdir().unwrap();
    let expected = state::Id::from(19);
    let actual = state::Id::from(20);
    let installation = installation_with_state(changed.path(), expected);
    record_state_id(changed.path(), actual).unwrap();
    assert_changed(ActiveStateLease::acquire(&installation), Some(expected), Some(actual));
}

#[test]
fn malformed_and_unsafe_state_ids_fail_closed_instead_of_becoming_absence() {
    for bytes in [
        b"".as_slice(),
        b"0".as_slice(),
        b"01".as_slice(),
        b"-1".as_slice(),
        b"1\n".as_slice(),
        b"2147483648".as_slice(),
        b"12345678901".as_slice(),
    ] {
        let temporary = tempfile::tempdir().unwrap();
        prepare_root(temporary.path());
        let usr = temporary.path().join("usr");
        fs::create_dir(&usr).unwrap();
        fs::set_permissions(&usr, Permissions::from_mode(0o755)).unwrap();
        fs::write(usr.join(".stateID"), bytes).unwrap();
        fs::set_permissions(usr.join(".stateID"), Permissions::from_mode(0o644)).unwrap();
        let installation = Installation::open(temporary.path(), None).unwrap();
        assert_proof_failure(ActiveStateLease::acquire(&installation));
        assert_eq!(fs::read(usr.join(".stateID")).unwrap(), bytes);
    }

    let linked = tempfile::tempdir().unwrap();
    let id = state::Id::from(21);
    let installation = installation_with_state(linked.path(), id);
    fs::hard_link(linked.path().join("usr/.stateID"), linked.path().join("state-id-alias")).unwrap();
    assert_proof_failure(ActiveStateLease::acquire(&installation));
}

#[test]
fn wrong_mode_and_nonregular_state_ids_are_rejected_and_preserved() {
    let wrong_mode = tempfile::tempdir().unwrap();
    let id = state::Id::from(22);
    let installation = installation_with_state(wrong_mode.path(), id);
    let state_path = wrong_mode.path().join("usr/.stateID");
    fs::set_permissions(&state_path, Permissions::from_mode(0o600)).unwrap();
    assert_proof_failure(ActiveStateLease::acquire(&installation));
    assert_eq!(fs::symlink_metadata(&state_path).unwrap().mode() & 0o7777, 0o600);
    assert_eq!(fs::read(&state_path).unwrap(), b"22");

    let directory = tempfile::tempdir().unwrap();
    prepare_root(directory.path());
    let usr = directory.path().join("usr");
    fs::create_dir(&usr).unwrap();
    fs::set_permissions(&usr, Permissions::from_mode(0o755)).unwrap();
    fs::create_dir(usr.join(".stateID")).unwrap();
    let installation = Installation::open(directory.path(), None).unwrap();
    assert_proof_failure(ActiveStateLease::acquire(&installation));
    assert!(usr.join(".stateID").is_dir());

    let fifo = tempfile::tempdir().unwrap();
    let installation = empty_installation(fifo.path());
    let usr = fifo.path().join("usr");
    fs::create_dir(&usr).unwrap();
    fs::set_permissions(&usr, Permissions::from_mode(0o755)).unwrap();
    nix::unistd::mkfifo(&usr.join(".stateID"), nix::sys::stat::Mode::from_bits_truncate(0o644)).unwrap();
    assert_proof_failure(ActiveStateLease::acquire(&installation));
    assert!(
        fs::symlink_metadata(usr.join(".stateID"))
            .unwrap()
            .file_type()
            .is_fifo()
    );
}

#[test]
fn usr_and_state_id_symlinks_are_never_followed() {
    let state_link = tempfile::tempdir().unwrap();
    prepare_root(state_link.path());
    let usr = state_link.path().join("usr");
    fs::create_dir(&usr).unwrap();
    fs::set_permissions(&usr, Permissions::from_mode(0o755)).unwrap();
    let victim = state_link.path().join("victim-state-id");
    fs::write(&victim, b"22").unwrap();
    symlink(&victim, usr.join(".stateID")).unwrap();
    let installation = Installation::open(state_link.path(), None).unwrap();
    assert_proof_failure(ActiveStateLease::acquire(&installation));
    assert_eq!(fs::read(&victim).unwrap(), b"22");

    let usr_link = tempfile::tempdir().unwrap();
    prepare_root(usr_link.path());
    let victim = usr_link.path().join("victim-usr");
    fs::create_dir(&victim).unwrap();
    fs::write(victim.join("proof"), b"untouched").unwrap();
    symlink(&victim, usr_link.path().join("usr")).unwrap();
    let installation = Installation::open(usr_link.path(), None).unwrap();
    assert_proof_failure(ActiveStateLease::acquire(&installation));
    assert_eq!(fs::read(victim.join("proof")).unwrap(), b"untouched");
}

#[test]
fn installation_discovery_is_bounded_and_never_follows_unsafe_state_entries() {
    let fifo = tempfile::tempdir().unwrap();
    prepare_root(fifo.path());
    let usr = fifo.path().join("usr");
    fs::create_dir(&usr).unwrap();
    fs::set_permissions(&usr, Permissions::from_mode(0o755)).unwrap();
    let fifo_path = usr.join(".stateID");
    nix::unistd::mkfifo(&fifo_path, nix::sys::stat::Mode::from_bits_truncate(0o644)).unwrap();
    let fifo_root = fifo.path().to_owned();
    let (finished, completion) = std::sync::mpsc::channel();
    let opener = std::thread::spawn(move || {
        let active = Installation::open(fifo_root, None).unwrap().active_state;
        finished.send(active).unwrap();
    });
    assert_eq!(
        completion
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("installation discovery blocked on a FIFO"),
        None
    );
    opener.join().unwrap();
    assert!(fs::symlink_metadata(&fifo_path).unwrap().file_type().is_fifo());

    let state_link = tempfile::tempdir().unwrap();
    prepare_root(state_link.path());
    let usr = state_link.path().join("usr");
    fs::create_dir(&usr).unwrap();
    fs::set_permissions(&usr, Permissions::from_mode(0o755)).unwrap();
    let victim = state_link.path().join("outside-state-id");
    fs::write(&victim, b"41").unwrap();
    symlink(&victim, usr.join(".stateID")).unwrap();
    assert_eq!(Installation::open(state_link.path(), None).unwrap().active_state, None);
    assert_eq!(fs::read(victim).unwrap(), b"41");

    let usr_link = tempfile::tempdir().unwrap();
    prepare_root(usr_link.path());
    let victim = usr_link.path().join("outside-usr");
    fs::create_dir(&victim).unwrap();
    fs::write(victim.join(".stateID"), b"42").unwrap();
    symlink(&victim, usr_link.path().join("usr")).unwrap();
    assert_eq!(Installation::open(usr_link.path(), None).unwrap().active_state, None);
    assert_eq!(fs::read(victim.join(".stateID")).unwrap(), b"42");

    let oversized = tempfile::tempdir().unwrap();
    prepare_root(oversized.path());
    let usr = oversized.path().join("usr");
    fs::create_dir(&usr).unwrap();
    fs::set_permissions(&usr, Permissions::from_mode(0o755)).unwrap();
    fs::write(usr.join(".stateID"), vec![b'9'; 1024 * 1024]).unwrap();
    fs::set_permissions(usr.join(".stateID"), Permissions::from_mode(0o644)).unwrap();
    assert_eq!(Installation::open(oversized.path(), None).unwrap().active_state, None);
    assert_eq!(fs::metadata(usr.join(".stateID")).unwrap().len(), 1024 * 1024);
}

#[test]
fn state_id_final_name_replacement_is_rejected_with_both_inodes_preserved() {
    let temporary = tempfile::tempdir().unwrap();
    let id = state::Id::from(23);
    let installation = installation_with_state(temporary.path(), id);
    let state_path = temporary.path().join("usr/.stateID");
    let detached = temporary.path().join("usr/.stateID-retained");
    let hook_state = state_path.clone();
    let hook_detached = detached.clone();
    super::active_state_snapshot::arm_after_state_id_read(move || {
        fs::rename(&hook_state, &hook_detached).unwrap();
        fs::write(&hook_state, b"23").unwrap();
        fs::set_permissions(&hook_state, Permissions::from_mode(0o644)).unwrap();
    });

    assert_proof_failure(ActiveStateLease::acquire(&installation));
    assert_eq!(fs::read(&state_path).unwrap(), b"23");
    assert_eq!(fs::read(&detached).unwrap(), b"23");
}

#[test]
fn same_inode_state_id_rewrite_after_first_read_is_rejected() {
    let temporary = tempfile::tempdir().unwrap();
    let id = state::Id::from(28);
    let installation = installation_with_state(temporary.path(), id);
    let state_path = temporary.path().join("usr/.stateID");
    let inode = fs::symlink_metadata(&state_path).unwrap().ino();
    let hook_state = state_path.clone();
    super::active_state_snapshot::arm_after_state_id_read(move || {
        fs::write(&hook_state, b"29").unwrap();
        fs::set_permissions(&hook_state, Permissions::from_mode(0o644)).unwrap();
    });

    assert_proof_failure(ActiveStateLease::acquire(&installation));
    assert_eq!(fs::symlink_metadata(&state_path).unwrap().ino(), inode);
    assert_eq!(fs::read(state_path).unwrap(), b"29");
}

#[test]
fn state_id_insertion_during_absence_proof_is_rejected_untouched() {
    let temporary = tempfile::tempdir().unwrap();
    prepare_root(temporary.path());
    let usr = temporary.path().join("usr");
    fs::create_dir(&usr).unwrap();
    fs::set_permissions(&usr, Permissions::from_mode(0o755)).unwrap();
    let installation = Installation::open(temporary.path(), None).unwrap();
    let inserted = usr.join(".stateID");
    let hook_inserted = inserted.clone();
    super::active_state_snapshot::arm_after_state_id_absence(move || {
        fs::write(&hook_inserted, b"24").unwrap();
        fs::set_permissions(&hook_inserted, Permissions::from_mode(0o644)).unwrap();
    });

    assert_proof_failure(ActiveStateLease::acquire(&installation));
    assert_eq!(fs::read(inserted).unwrap(), b"24");
}

#[test]
fn foreign_entry_inserted_after_first_empty_scan_is_rejected_untouched() {
    let temporary = tempfile::tempdir().unwrap();
    prepare_root(temporary.path());
    let usr = temporary.path().join("usr");
    fs::create_dir(&usr).unwrap();
    fs::set_permissions(&usr, Permissions::from_mode(0o755)).unwrap();
    let installation = Installation::open(temporary.path(), None).unwrap();
    let foreign = usr.join("late-foreign");
    let hook_foreign = foreign.clone();
    super::active_state_snapshot::arm_after_baseline_layout_proof(move || {
        fs::write(&hook_foreign, b"late evidence").unwrap();
    });

    assert_proof_failure(ActiveStateLease::acquire(&installation));
    assert_eq!(fs::read(foreign).unwrap(), b"late evidence");
}

#[test]
fn retained_lease_rejects_same_inode_state_id_aba_after_acquisition() {
    let temporary = tempfile::tempdir().unwrap();
    let selected = state::Id::from(31);
    let installation = installation_with_state(temporary.path(), selected);
    let state_path = temporary.path().join("usr/.stateID");
    let inode = fs::symlink_metadata(&state_path).unwrap().ino();
    let lease = ActiveStateLease::acquire(&installation).unwrap();

    std::thread::sleep(std::time::Duration::from_millis(2));
    fs::write(&state_path, b"32").unwrap();
    fs::write(&state_path, b"31").unwrap();

    assert_eq!(fs::symlink_metadata(&state_path).unwrap().ino(), inode);
    assert_proof_failure(lease.revalidate(&installation));
    assert_eq!(fs::read(&state_path).unwrap(), b"31");
}

#[test]
fn retained_lease_rejects_whole_usr_replacement_and_restore() {
    let temporary = tempfile::tempdir().unwrap();
    let selected = state::Id::from(33);
    let installation = installation_with_state(temporary.path(), selected);
    let usr = temporary.path().join("usr");
    let retained_inode = fs::symlink_metadata(&usr).unwrap().ino();
    let replacement = temporary.path().join("replacement-usr");
    fs::create_dir(&replacement).unwrap();
    fs::set_permissions(&replacement, Permissions::from_mode(0o755)).unwrap();
    fs::write(replacement.join(".stateID"), b"34").unwrap();
    fs::set_permissions(replacement.join(".stateID"), Permissions::from_mode(0o644)).unwrap();
    let retained = temporary.path().join("retained-usr");
    let lease = ActiveStateLease::acquire(&installation).unwrap();

    std::thread::sleep(std::time::Duration::from_millis(2));
    fs::rename(&usr, &retained).unwrap();
    fs::rename(&replacement, &usr).unwrap();
    fs::rename(&usr, &replacement).unwrap();
    fs::rename(&retained, &usr).unwrap();

    assert_eq!(fs::symlink_metadata(&usr).unwrap().ino(), retained_inode);
    assert_eq!(fs::read(usr.join(".stateID")).unwrap(), b"33");
    assert_proof_failure(lease.revalidate(&installation));
}

#[test]
fn stale_builder_rejects_before_opening_database_files() {
    let temporary = tempfile::tempdir().unwrap();
    let installation = empty_installation(temporary.path());
    let db_paths = [
        installation.db_path("install"),
        installation.db_path("state"),
        installation.db_path("layout"),
    ];
    assert!(db_paths.iter().all(|path| !path.exists()));
    let selected = state::Id::from(25);
    record_state_id(temporary.path(), selected).unwrap();

    let result = Client::builder("stale-builder", installation)
        .repositories(repository::Map::default())
        .build();

    assert_changed(result, None, Some(selected));
    assert!(db_paths.iter().all(|path| !path.exists()));
}

#[test]
fn reused_client_rejects_a_second_state_before_database_allocation() {
    let temporary = tempfile::tempdir().unwrap();
    let client = build_client(empty_installation(temporary.path()), "reused-client");
    let first = client.new_state(&[], "first active state").unwrap().unwrap();
    let rows_before = client.state_db.list_ids().unwrap();
    let staging_before = fs::read_dir(client.installation.staging_dir()).unwrap().count();

    assert_changed(client.new_state(&[], "must reopen"), None, Some(first.id));

    assert_eq!(client.state_db.list_ids().unwrap(), rows_before);
    assert_eq!(
        fs::read_dir(client.installation.staging_dir()).unwrap().count(),
        staging_before
    );
    assert_eq!(
        fs::read_to_string(client.installation.root.join("usr/.stateID")).unwrap(),
        first.id.to_string()
    );
    drop(client);

    let reopened = build_client(
        Installation::open(temporary.path(), None).unwrap(),
        "reopened-after-transition",
    );
    assert_eq!(reopened.get_active_state().unwrap().unwrap().id, first.id);
}

#[test]
fn state_id_aba_during_candidate_fill_fails_before_row_allocation_or_tree_identity() {
    let temporary = tempfile::tempdir().unwrap();
    let mut client = build_client(empty_installation(temporary.path()), "aba-candidate");
    let active = client.state_db.add(&[], Some("active baseline"), None).unwrap();
    record_state_id(temporary.path(), active.id).unwrap();
    client.installation.active_state = Some(active.id);
    let marker = TreeMarkerStore::open_path(temporary.path().join("usr")).unwrap();
    drop(marker.adopt_or_create_before_journal().unwrap());
    drop(marker);
    let rows_before = client.state_db.list_ids().unwrap();
    let state_path = temporary.path().join("usr/.stateID");
    let expected = active.id.to_string().into_bytes();
    let temporary_value = if expected == b"1" { b"2".to_vec() } else { b"1".to_vec() };
    let hook_path = state_path.clone();
    let hook_expected = expected.clone();
    super::fixed_staging::arm_after_fixed_staging_fill(move || {
        std::thread::sleep(std::time::Duration::from_millis(2));
        fs::write(&hook_path, &temporary_value).unwrap();
        fs::write(&hook_path, &hook_expected).unwrap();
    });

    let error = client.new_state(&[], "must reject ABA").unwrap_err();
    assert!(matches!(error, Error::LiveActiveStateProof { .. }), "{error:?}");
    assert_eq!(client.state_db.list_ids().unwrap(), rows_before);
    assert_eq!(fs::read(&state_path).unwrap(), expected);
    assert!(!client.installation.staging_path("usr/.cast-tree-id").exists());
}

#[test]
fn stale_cloned_client_cannot_activate_after_a_sibling_transition() {
    let temporary = tempfile::tempdir().unwrap();
    let installation = empty_installation(temporary.path());
    let writer = build_client(installation.clone(), "active-writer");
    let stale = build_client(installation, "stale-activator");
    let first = writer.new_state(&[], "first active state").unwrap().unwrap();

    assert_changed(stale.activate_state(first.id, true, true), None, Some(first.id));
    assert_eq!(
        fs::read_to_string(temporary.path().join("usr/.stateID")).unwrap(),
        first.id.to_string()
    );
}

#[test]
fn stale_verify_prune_boot_and_read_apis_fail_before_authoritative_work() {
    let temporary = tempfile::tempdir().unwrap();
    let installation = empty_installation(temporary.path());
    let verify_client = build_client(installation.clone(), "stale-verify");
    let prune_client = build_client(installation.clone(), "stale-prune");
    let boot_client = build_client(installation.clone(), "stale-boot");
    let read_client = build_client(installation, "stale-read");
    let selected = verify_client.state_db.add(&[], Some("selected"), None).unwrap();
    record_state_id(temporary.path(), selected.id).unwrap();
    fs::write(temporary.path().join("usr/proof"), b"untouched").unwrap();

    assert_changed(verify_client.verify(true, false), None, Some(selected.id));
    assert_changed(
        prune_client.prune_states(super::prune::Strategy::Remove(&[selected.id]), true),
        None,
        Some(selected.id),
    );
    assert_changed(boot_client.synchronize_boot(), None, Some(selected.id));
    assert_changed(read_client.get_active_state(), None, Some(selected.id));
    assert_changed(read_client.export_state(selected.id), None, Some(selected.id));

    assert_eq!(verify_client.state_db.get(selected.id).unwrap(), selected);
    assert_eq!(fs::read(temporary.path().join("usr/proof")).unwrap(), b"untouched");
    assert!(fs::read_dir(temporary.path().join("boot")).is_err());
}

#[test]
fn stale_registry_queries_fail_before_reading_the_construction_time_active_plugin() {
    let temporary = tempfile::tempdir().unwrap();
    let client = build_client(empty_installation(temporary.path()), "stale-registry");
    let selected = client.state_db.add(&[], Some("selected"), None).unwrap();
    record_state_id(temporary.path(), selected.id).unwrap();
    let package = package::Id::from("missing-registry-package");
    let provider = Provider::package_name("missing-registry-package");
    let flags = package::Flags::default();

    assert_changed(client.resolve_package(&package), None, Some(selected.id));
    assert_changed(client.resolve_packages([&package]), None, Some(selected.id));
    assert_changed(
        client.lookup_packages_by_provider(&provider, flags),
        None,
        Some(selected.id),
    );
    assert_changed(client.list_packages(flags), None, Some(selected.id));
    assert_changed(
        client.search_packages("missing-registry-package", flags),
        None,
        Some(selected.id),
    );
}

fn arm_sibling_transition_before_registry_snapshot(writer: Client) {
    super::arm_before_registry_snapshot_acquisition(move || {
        writer
            .new_state(&[], "sibling transition before registry snapshot")
            .unwrap();
    });
}

fn assert_active_state_changed_in_error_chain(error: &(dyn std::error::Error + 'static)) {
    let mut current = Some(error);
    while let Some(source) = current {
        if matches!(
            source.downcast_ref::<Error>(),
            Some(Error::ActiveStateSnapshotChanged {
                expected: None,
                actual: Some(_),
            })
        ) {
            return;
        }
        current = source.source();
    }
    panic!("missing active-state snapshot change in error chain: {error}");
}

#[test]
fn workflow_registry_reads_reject_a_sibling_transition_after_public_preflight() {
    let install_root = tempfile::tempdir().unwrap();
    let installation = empty_installation(install_root.path());
    let writer = build_client(installation.clone(), "install-transition-writer");
    let mut stale = build_client(installation, "stale-install-workflow");
    arm_sibling_transition_before_registry_snapshot(writer);
    let error = match stale.install(&[], true, true) {
        Ok(_) => panic!("stale install crossed its internal registry boundary"),
        Err(error) => error,
    };
    assert_active_state_changed_in_error_chain(&error);

    let remove_root = tempfile::tempdir().unwrap();
    let installation = empty_installation(remove_root.path());
    let writer = build_client(installation.clone(), "remove-transition-writer");
    let mut stale = build_client(installation, "stale-remove-workflow");
    arm_sibling_transition_before_registry_snapshot(writer);
    let error = match stale.remove(&[], true, true) {
        Ok(_) => panic!("stale remove crossed its internal registry boundary"),
        Err(error) => error,
    };
    assert_active_state_changed_in_error_chain(&error);

    let sync_root = tempfile::tempdir().unwrap();
    let installation = empty_installation(sync_root.path());
    let writer = build_client(installation.clone(), "sync-transition-writer");
    let mut stale = build_client(installation, "stale-sync-workflow");
    arm_sibling_transition_before_registry_snapshot(writer);
    let error = match stale.sync(true, true) {
        Ok(_) => panic!("stale sync crossed its internal registry boundary"),
        Err(error) => error,
    };
    assert_active_state_changed_in_error_chain(&error);

    let self_upgrade_root = tempfile::tempdir().unwrap();
    let installation = empty_installation(self_upgrade_root.path());
    let writer = build_client(installation.clone(), "self-upgrade-transition-writer");
    let mut stale = build_client(installation, "stale-self-upgrade-workflow");
    arm_sibling_transition_before_registry_snapshot(writer);
    let error = super::self_upgrade::self_upgrade(&mut stale, true).unwrap_err();
    assert_active_state_changed_in_error_chain(&error);
}

#[test]
fn available_closure_rejects_a_sibling_transition_even_without_requests() {
    let temporary = tempfile::tempdir().unwrap();
    let installation = empty_installation(temporary.path());
    let writer = build_client(installation.clone(), "resolve-transition-writer");
    let stale = build_client(installation, "stale-available-closure");
    arm_sibling_transition_before_registry_snapshot(writer);

    let error = stale.resolve_available_closure(&[]).unwrap_err();
    assert!(matches!(
        error,
        super::resolve::Error::Client(Error::ActiveStateSnapshotChanged {
            expected: None,
            actual: Some(_),
        })
    ));
}

#[test]
fn stale_stateful_candidate_fails_before_fixed_staging_mutation() {
    let temporary = tempfile::tempdir().unwrap();
    let client = build_client(empty_installation(temporary.path()), "stale-candidate");
    let selected = state::Id::from(26);
    record_state_id(temporary.path(), selected).unwrap();
    let staging = client.installation.staging_dir();
    let entries_before = fs::read_dir(&staging).unwrap().count();

    assert_changed(
        client.materialize_stateful_candidate(std::iter::empty::<&package::Id>()),
        None,
        Some(selected),
    );
    assert_eq!(fs::read_dir(staging).unwrap().count(), entries_before);
}

#[test]
fn stale_ephemeral_candidate_fails_before_touching_its_external_target() {
    let temporary = tempfile::tempdir().unwrap();
    fs::set_permissions(temporary.path(), Permissions::from_mode(0o700)).unwrap();
    let installation_root = temporary.path().join("installation");
    fs::create_dir(&installation_root).unwrap();
    let installation = empty_installation(&installation_root);
    let target = temporary.path().join("external-root");
    let client = Client::builder("stale-ephemeral", installation)
        .repositories(repository::Map::default())
        .ephemeral(&target)
        .build()
        .unwrap();
    let selected = state::Id::from(27);
    record_state_id(&installation_root, selected).unwrap();

    assert_changed(
        client.blit_root(std::iter::empty::<&package::Id>()),
        None,
        Some(selected),
    );
    assert!(!target.exists());
}
