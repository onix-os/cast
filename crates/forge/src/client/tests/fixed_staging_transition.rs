use std::{
    cell::Cell,
    ffi::OsString,
    os::unix::fs::{MetadataExt as _, PermissionsExt as _, symlink},
    rc::Rc,
    sync::mpsc::{self, RecvTimeoutError},
    time::Duration,
};

use fs_err as fs;
use stone::{StonePayloadLayoutFile, StonePayloadLayoutRecord};

use super::*;
use crate::client::fixed_staging::{
    arm_after_fixed_staging_fill, arm_before_candidate_usr_publication, arm_before_coordinator_lock,
    arm_before_fixed_staging_fill, arm_before_legacy_staging_normalization, arm_before_retained_state_metadata,
};

#[test]
fn exact_empty_staging_is_reused_and_returns_the_published_usr_inode() {
    let temporary = tempfile::tempdir().unwrap();
    let client = stateful_test_client(temporary.path());
    let staging = client.installation.staging_dir();
    let wrapper_before = directory_identity(&staging);

    let candidate = client
        .materialize_stateful_candidate(std::iter::empty::<&package::Id>())
        .unwrap();

    assert_eq!(directory_identity(&staging), wrapper_before);
    assert_eq!(
        directory_identity(&staging.join("usr")),
        file_identity(&candidate.candidate_usr)
    );
    assert_eq!(mode(&staging), 0o700);
    assert_eq!(entry_names(&staging), [OsString::from("usr")]);
}

#[test]
fn exact_empty_legacy_staging_is_normalized_without_replacing_its_inode() {
    let temporary = tempfile::tempdir().unwrap();
    let client = stateful_test_client(temporary.path());
    let staging = client.installation.staging_dir();
    fs::set_permissions(&staging, Permissions::from_mode(0o755)).unwrap();
    let before = directory_identity(&staging);
    let normalized = Rc::new(Cell::new(false));
    let observed = Rc::clone(&normalized);
    arm_before_legacy_staging_normalization(move || observed.set(true));

    let candidate = client
        .materialize_stateful_candidate(std::iter::empty::<&package::Id>())
        .unwrap();

    assert!(normalized.get());
    assert_eq!(directory_identity(&staging), before);
    assert_eq!(mode(&staging), 0o700);
    drop(candidate);
}

#[test]
fn nonempty_legacy_staging_residue_is_preserved_byte_for_byte() {
    let temporary = tempfile::tempdir().unwrap();
    let client = stateful_test_client(temporary.path());
    let staging = client.installation.staging_dir();
    fs::set_permissions(&staging, Permissions::from_mode(0o755)).unwrap();
    let residue = staging.join("crash-evidence");
    fs::write(&residue, b"retain me").unwrap();
    let before = directory_identity(&staging);

    let result = client.materialize_stateful_candidate(std::iter::empty::<&package::Id>());

    assert!(matches!(result, Err(Error::StatefulCandidateMaterialization { .. })));
    assert_eq!(directory_identity(&staging), before);
    assert_eq!(mode(&staging), 0o755);
    assert_eq!(fs::read(&residue).unwrap(), b"retain me");
}

#[test]
fn an_entry_inserted_before_fill_is_rejected_without_traversal() {
    let temporary = tempfile::tempdir().unwrap();
    let client = stateful_test_client(temporary.path());
    let staging = client.installation.staging_dir();
    let foreign = staging.join("foreign-before-fill");
    let hook_foreign = foreign.clone();
    arm_before_fixed_staging_fill(move || fs::write(&hook_foreign, b"foreign").unwrap());

    let result = client.materialize_stateful_candidate(std::iter::empty::<&package::Id>());

    assert!(matches!(result, Err(Error::StatefulCandidateMaterialization { .. })));
    assert_eq!(fs::read(&foreign).unwrap(), b"foreign");
    assert_eq!(entry_names(&staging), [OsString::from("foreign-before-fill")]);
}

#[test]
fn candidate_usr_publication_collision_preserves_private_and_public_trees() {
    let temporary = tempfile::tempdir().unwrap();
    let client = stateful_test_client(temporary.path());
    let staging = client.installation.staging_dir();
    let public_usr = staging.join("usr");
    let hook_usr = public_usr.clone();
    arm_before_candidate_usr_publication(move || {
        fs::create_dir(&hook_usr).unwrap();
        fs::set_permissions(&hook_usr, Permissions::from_mode(0o755)).unwrap();
        fs::write(hook_usr.join("foreign"), b"public occupant").unwrap();
    });

    let result = client.materialize_stateful_candidate(std::iter::empty::<&package::Id>());

    assert!(matches!(result, Err(Error::StatefulCandidateMaterialization { .. })));
    assert_eq!(fs::read(public_usr.join("foreign")).unwrap(), b"public occupant");
    let private = private_usr_residue(&staging);
    assert!(entry_names(&private).is_empty());
}

#[test]
fn filled_private_usr_is_not_replaced_by_a_last_moment_public_occupant() {
    let temporary = tempfile::tempdir().unwrap();
    let client = stateful_test_client(temporary.path());
    let package = package::Id::from("retained-fill-proof");
    add_empty_regular(&client, &package, "share/retained-fill-proof");
    let staging = client.installation.staging_dir();
    let public_usr = staging.join("usr");
    let hook_usr = public_usr.clone();
    arm_after_fixed_staging_fill(move || {
        fs::create_dir(&hook_usr).unwrap();
        fs::set_permissions(&hook_usr, Permissions::from_mode(0o755)).unwrap();
        fs::write(hook_usr.join("foreign"), b"late occupant").unwrap();
    });

    let result = client.materialize_stateful_candidate([&package]);

    assert!(matches!(result, Err(Error::StatefulCandidateMaterialization { .. })));
    assert_eq!(fs::read(public_usr.join("foreign")).unwrap(), b"late occupant");
    assert!(
        private_usr_residue(&staging)
            .join("share/retained-fill-proof")
            .is_file()
    );
}

#[test]
fn stateful_candidate_same_digest_modes_and_writes_are_isolated_from_cache() {
    const BYTES: &[u8] = b"stateful independent-copy proof";
    let temporary = tempfile::tempdir().unwrap();
    let client = stateful_test_client(temporary.path());
    let digest = xxhash_rust::xxh3::xxh3_128(BYTES);
    let asset = cache::asset_path(&client.installation, &format!("{digest:02x}"));
    fs::create_dir_all(asset.parent().unwrap()).unwrap();
    fs::write(&asset, BYTES).unwrap();
    fs::set_permissions(&asset, Permissions::from_mode(0o640)).unwrap();
    let asset_before = fs::symlink_metadata(&asset).unwrap();

    let library = package::Id::from("stateful-copy-library");
    let executable = package::Id::from("stateful-copy-executable");
    add_cached_regular(&client, &library, digest, "share/stateful-copy", 0o644);
    add_cached_regular(&client, &executable, digest, "bin/stateful-copy", 0o755);

    let candidate = client.materialize_stateful_candidate([&library, &executable]).unwrap();
    let library_path = client.installation.staging_path("usr/share/stateful-copy");
    let executable_path = client.installation.staging_path("usr/bin/stateful-copy");
    let library_meta = fs::symlink_metadata(&library_path).unwrap();
    let executable_meta = fs::symlink_metadata(&executable_path).unwrap();

    assert_eq!(library_meta.permissions().mode() & 0o7777, 0o644);
    assert_eq!(executable_meta.permissions().mode() & 0o7777, 0o755);
    assert_eq!(library_meta.nlink(), 1);
    assert_eq!(executable_meta.nlink(), 1);
    assert_ne!(
        (library_meta.dev(), library_meta.ino()),
        (executable_meta.dev(), executable_meta.ino())
    );
    assert_ne!(
        (library_meta.dev(), library_meta.ino()),
        (asset_before.dev(), asset_before.ino())
    );
    assert_ne!(
        (executable_meta.dev(), executable_meta.ino()),
        (asset_before.dev(), asset_before.ino())
    );

    fs::write(&executable_path, b"transaction-trigger mutation").unwrap();
    fs::set_permissions(&executable_path, Permissions::from_mode(0o600)).unwrap();
    assert_eq!(fs::read(&asset).unwrap(), BYTES);
    assert_eq!(fs::read(&library_path).unwrap(), BYTES);
    let asset_after = fs::symlink_metadata(&asset).unwrap();
    assert_eq!(
        (asset_after.dev(), asset_after.ino()),
        (asset_before.dev(), asset_before.ino())
    );
    assert_eq!(asset_after.permissions().mode() & 0o7777, 0o640);
    assert_eq!(asset_after.nlink(), 1);
    drop(candidate);
}

#[test]
fn stateful_candidate_rejects_corrupt_cache_bytes_without_publishing_usr() {
    let expected = b"expected stateful bytes";
    let corrupt = b"corrupt! stateful bytes";
    assert_eq!(expected.len(), corrupt.len());
    let temporary = tempfile::tempdir().unwrap();
    let client = stateful_test_client(temporary.path());
    let digest = xxhash_rust::xxh3::xxh3_128(expected);
    let asset = cache::asset_path(&client.installation, &format!("{digest:02x}"));
    fs::create_dir_all(asset.parent().unwrap()).unwrap();
    fs::write(&asset, corrupt).unwrap();
    fs::set_permissions(&asset, Permissions::from_mode(0o640)).unwrap();
    let before = fs::symlink_metadata(&asset).unwrap();
    let package = package::Id::from("stateful-corrupt-cache");
    add_cached_regular(&client, &package, digest, "bin/corrupt-cache", 0o755);

    let result = client.materialize_stateful_candidate([&package]);

    assert!(matches!(result, Err(Error::StatefulCandidateMaterialization { .. })));
    assert!(!client.installation.staging_path("usr").exists());
    assert!(
        !private_usr_residue(&client.installation.staging_dir())
            .join("bin/corrupt-cache")
            .exists()
    );
    let after = fs::symlink_metadata(&asset).unwrap();
    assert_eq!((after.dev(), after.ino()), (before.dev(), before.ino()));
    assert_eq!(after.permissions().mode() & 0o7777, 0o640);
    assert_eq!(after.nlink(), 1);
    assert_eq!(fs::read(&asset).unwrap(), corrupt);
}

#[test]
fn retained_state_id_write_never_targets_a_substituted_usr() {
    let temporary = tempfile::tempdir().unwrap();
    let client = stateful_test_client(temporary.path());
    let candidate = client
        .materialize_stateful_candidate(std::iter::empty::<&package::Id>())
        .unwrap();
    let state = client.state_db.add(&[], Some("retained metadata"), None).unwrap();
    let staging = client.installation.staging_dir();
    let usr = staging.join("usr");
    let retained = staging.join("retained-original-usr");
    let hook_usr = usr.clone();
    let hook_retained = retained.clone();
    arm_before_retained_state_metadata(move || {
        fs::rename(&hook_usr, &hook_retained).unwrap();
        fs::create_dir(&hook_usr).unwrap();
        fs::set_permissions(&hook_usr, Permissions::from_mode(0o755)).unwrap();
        fs::write(hook_usr.join("foreign"), b"replacement").unwrap();
    });

    let result =
        client.apply_stateful_candidate(candidate, &state, None, generated_system_snapshot("retained-metadata"));

    assert!(result.is_err());
    assert_eq!(
        fs::read_to_string(retained.join(".stateID")).unwrap(),
        state.id.to_string()
    );
    assert_eq!(fs::read(usr.join("foreign")).unwrap(), b"replacement");
    assert!(!usr.join(".stateID").exists());
}

#[test]
fn stateful_trigger_preparation_never_follows_a_replaced_isolation_root() {
    let temporary = tempfile::tempdir().unwrap();
    let mut client = stateful_test_client(temporary.path());
    let previous = client.state_db.add(&[], Some("previous"), None).unwrap();
    client.installation.active_state = Some(previous.id);
    record_state_id(&client.installation.root, previous.id).unwrap();
    record_system_snapshot(&client.installation.root, generated_system_snapshot("previous-package")).unwrap();
    let live_usr = client.installation.root.join("usr");
    let live_identity = directory_identity(&live_usr);
    let isolation = client.installation.isolation_dir();
    let detached = client.installation.root_path("detached-isolation-root");
    let victim = client.installation.root.join("foreign-isolation-victim");
    fs::create_dir(&victim).unwrap();
    fs::write(victim.join("sentinel"), b"foreign replacement").unwrap();

    let package = package::Id::from("stateful-isolation-root-race");
    client
        .layout_db
        .add(
            &package,
            &StonePayloadLayoutRecord {
                uid: 0,
                gid: 0,
                mode: nix::libc::S_IFDIR | 0o755,
                tag: 0,
                file: StonePayloadLayoutFile::Directory("share/stateful-isolation-trigger-input".into()),
            },
        )
        .unwrap();
    let candidate = client.materialize_stateful_candidate([&package]).unwrap();
    let candidate_state = client
        .state_db
        .add(
            &[Selection::explicit(package)],
            Some("isolation root race candidate"),
            None,
        )
        .unwrap();
    let candidate_marker = client
        .installation
        .staging_path("usr/share/stateful-isolation-trigger-input");
    let trigger = client
        .installation
        .staging_path("usr/share/cast/triggers/tx.d/isolation-root-race.glu");
    fs::create_dir_all(trigger.parent().unwrap()).unwrap();
    for directory in [
        client.installation.staging_path("usr/share/cast"),
        client.installation.staging_path("usr/share/cast/triggers"),
        client.installation.staging_path("usr/share/cast/triggers/tx.d"),
    ] {
        fs::set_permissions(directory, Permissions::from_mode(0o755)).unwrap();
    }
    fs::write(
        &trigger,
        r#"let cast = import! cast.trigger.v1
let base = cast.trigger "isolation-root-race" "Retained isolation root race proof"
{
    paths = [cast.path
        "/usr/share/stateful-isolation-trigger-input"
        ["delete-marker"]
        (cast.optional.set cast.path_kind.directory)],
    handlers = [cast.handler.named "delete-marker" (cast.handler.delete
        ["/usr/share/stateful-isolation-trigger-input"])],
    .. base
}
"#,
    )
    .unwrap();
    fs::set_permissions(&trigger, Permissions::from_mode(0o644)).unwrap();

    let hook_isolation = isolation.clone();
    let hook_detached = detached.clone();
    let hook_victim = victim.clone();
    arm_after_stateful_isolation_root_retention(move || {
        fs::rename(&hook_isolation, &hook_detached).unwrap();
        symlink(&hook_victim, &hook_isolation).unwrap();
    });

    let error = client
        .apply_stateful_candidate(
            candidate,
            &candidate_state,
            Some(previous.id),
            generated_system_snapshot("candidate-package"),
        )
        .unwrap_err();

    assert!(
        matches!(
            &error,
            Error::StatefulCandidatePreserved { primary, .. }
                if matches!(
                    primary.as_ref(),
                    Error::PostBlit(postblit::Error::PinRetainedTransactionSource {
                        role: "container root",
                        path,
                        ..
                    }) if path == &isolation
                )
        ),
        "unexpected retained isolation failure: {error:#?}"
    );
    assert!(fs::symlink_metadata(&isolation).unwrap().file_type().is_symlink());
    assert_eq!(fs::read_link(&isolation).unwrap(), victim);
    assert_eq!(fs::read(victim.join("sentinel")).unwrap(), b"foreign replacement");
    assert!(
        !victim.join("etc").exists(),
        "stateful trigger preparation followed the replacement symlink"
    );
    assert!(
        !detached.join("etc").exists(),
        "failed preparation must not mutate the detached retained root"
    );
    assert_eq!(directory_identity(&live_usr), live_identity);
    assert_eq!(
        fs::read_to_string(live_usr.join(".stateID")).unwrap(),
        previous.id.to_string()
    );
    let quarantines = fs::read_dir(client.installation.state_quarantine_dir())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    assert_eq!(quarantines.len(), 1, "candidate was not preserved exactly once");
    assert!(
        quarantines[0]
            .join("usr/share/stateful-isolation-trigger-input")
            .is_dir()
    );
    assert!(!candidate_marker.exists());
}

#[test]
fn archived_repair_state_id_write_uses_the_same_retained_usr() {
    let temporary = tempfile::tempdir().unwrap();
    let client = stateful_test_client(temporary.path());
    let repaired = client
        .state_db
        .add(&[], Some("archived retained metadata"), None)
        .unwrap();
    let candidate = client
        .materialize_archived_repair_root(std::iter::empty::<&package::Id>())
        .unwrap();
    let staging = client.installation.staging_dir();
    let usr = staging.join("usr");
    let retained = staging.join("retained-archived-usr");
    let hook_usr = usr.clone();
    let hook_retained = retained.clone();
    arm_before_retained_state_metadata(move || {
        fs::rename(&hook_usr, &hook_retained).unwrap();
        fs::create_dir(&hook_usr).unwrap();
        fs::set_permissions(&hook_usr, Permissions::from_mode(0o755)).unwrap();
    });

    let result = client.repair_archived_state(candidate, &repaired, generated_system_snapshot("archived-retained"));

    assert!(matches!(result, Err(Error::ArchivedStateRepair { .. })));
    assert_eq!(
        fs::read_to_string(retained.join(".stateID")).unwrap(),
        repaired.id.to_string()
    );
    assert!(!usr.join(".stateID").exists());
}

#[test]
fn coordinator_lease_spans_state_allocation_and_retained_identity_preparation() {
    let temporary = tempfile::tempdir().unwrap();
    let client = stateful_test_client(temporary.path());
    let candidate = client
        .materialize_stateful_candidate(std::iter::empty::<&package::Id>())
        .unwrap();
    let (reached_tx, reached_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        arm_before_coordinator_lock(move || reached_tx.send(()).unwrap());
        let _guard = fixed_staging::lock_coordinator().unwrap();
        done_tx.send(()).unwrap();
    });
    reached_rx.recv_timeout(Duration::from_secs(120)).unwrap();
    assert!(matches!(
        done_rx.recv_timeout(Duration::from_millis(100)),
        Err(RecvTimeoutError::Timeout)
    ));

    let state = client.state_db.add(&[], Some("serialized candidate"), None).unwrap();
    record_state_id_retained(&candidate.staging, &candidate.candidate_usr, state.id).unwrap();
    let candidate_path = client.installation.staging_path("usr");
    let identity = StatefulTreeIdentity::prepare_retained_candidate(
        &client.installation,
        &client.state_db,
        &candidate_path,
        &candidate.candidate_usr,
        state.id,
    )
    .unwrap();
    let (retained_usr, diagnostic_path) = identity.retained_candidate_usr();
    assert_eq!(file_identity(retained_usr), file_identity(&candidate.candidate_usr));
    assert_eq!(diagnostic_path, candidate_path);
    identity.verify_candidate_for_activation(&candidate_path).unwrap();
    assert!(matches!(
        done_rx.recv_timeout(Duration::from_millis(100)),
        Err(RecvTimeoutError::Timeout)
    ));

    let retained_state_id = candidate_path.join(".stateID.retained-test-evidence");
    fs::rename(candidate_path.join(".stateID"), &retained_state_id).unwrap();
    fs::write(candidate_path.join(".stateID"), state.id.to_string()).unwrap();
    fs::set_permissions(candidate_path.join(".stateID"), Permissions::from_mode(0o644)).unwrap();
    identity.verify_candidate_for_recovery(&candidate_path).unwrap();
    assert!(identity.verify_candidate_for_activation(&candidate_path).is_err());
    assert_eq!(fs::read_to_string(retained_state_id).unwrap(), state.id.to_string());

    drop(identity);
    drop(candidate);
    done_rx.recv_timeout(Duration::from_secs(120)).unwrap();
    worker.join().unwrap();
}

#[test]
fn public_and_cross_install_blitters_cannot_target_fixed_staging() {
    let temporary = tempfile::tempdir().unwrap();
    let first_root = temporary.path().join("first");
    let second_root = temporary.path().join("second");
    fs::create_dir(&first_root).unwrap();
    fs::create_dir(&second_root).unwrap();
    let first = stateful_test_client(&first_root);
    let second = stateful_test_client(&second_root);
    let second_staging = second.installation.staging_dir();
    let before = directory_identity(&second_staging);
    let tree = vfs(Vec::new()).unwrap();

    assert!(matches!(
        blit_root(&first.installation, &tree, &second_staging),
        Err(Error::EphemeralInstallationRoot)
    ));
    assert_eq!(directory_identity(&second_staging), before);
    assert!(matches!(
        first.ephemeral(&second_staging),
        Err(Error::EphemeralInstallationRoot)
    ));
    assert_eq!(directory_identity(&second_staging), before);
}

#[test]
fn frozen_client_rejects_destination_beneath_installation_root() {
    let temporary = tempfile::tempdir().unwrap();
    let installation_root = temporary.path().join("installation");
    fs::create_dir(&installation_root).unwrap();
    let installation = frozen_test_installation(&installation_root);
    let destination = installation_root.join("frozen-root");
    let root_before = directory_identity(&installation_root);

    let result = Client::frozen(
        "frozen-overlap-rejection",
        installation,
        repository::Map::default(),
        &destination,
    );

    assert!(matches!(result, Err(Error::EphemeralInstallationRoot)));
    assert_eq!(directory_identity(&installation_root), root_before);
    assert!(!destination.exists());
}

#[test]
fn ephemeral_materialization_rechecks_empty_target_under_the_lease() {
    let temporary = tempfile::tempdir().unwrap();
    fs::set_permissions(temporary.path(), Permissions::from_mode(0o700)).unwrap();
    let installation_root = temporary.path().join("installation");
    let destination = temporary.path().join("ephemeral");
    fs::create_dir(&installation_root).unwrap();
    fs::create_dir(&destination).unwrap();
    fs::set_permissions(&destination, Permissions::from_mode(0o700)).unwrap();
    let client = stateful_test_client(&installation_root)
        .ephemeral(&destination)
        .unwrap();
    let foreign = destination.join("appeared-after-construction");
    fs::write(&foreign, b"do not delete").unwrap();

    let result = client.blit_root(std::iter::empty::<&package::Id>());

    assert!(matches!(result, Err(Error::InitialMaterializationTargetChanged { .. })));
    assert_eq!(fs::read(&foreign).unwrap(), b"do not delete");
}

#[test]
fn ephemeral_activate_and_boot_sync_fail_before_fixed_namespace_mutation() {
    let temporary = tempfile::tempdir().unwrap();
    fs::set_permissions(temporary.path(), Permissions::from_mode(0o700)).unwrap();
    let installation_root = temporary.path().join("installation");
    let destination = temporary.path().join("ephemeral");
    fs::create_dir(&installation_root).unwrap();
    fs::create_dir(&destination).unwrap();
    fs::set_permissions(&destination, Permissions::from_mode(0o700)).unwrap();
    let client = stateful_test_client(&installation_root)
        .ephemeral(&destination)
        .unwrap();
    let staging = client.installation.staging_dir();
    let before = directory_identity(&staging);

    assert!(matches!(
        client.activate_state(state::Id::from(999), true, true),
        Err(Error::EphemeralProhibitedOperation)
    ));
    assert!(matches!(
        client.synchronize_boot(),
        Err(Error::EphemeralProhibitedOperation)
    ));
    assert!(matches!(
        client.verify(true, false),
        Err(Error::EphemeralProhibitedOperation)
    ));
    assert!(matches!(client.prune_cache(), Err(Error::EphemeralProhibitedOperation)));
    assert_eq!(directory_identity(&staging), before);
    assert!(entry_names(&staging).is_empty());
}

#[test]
fn frozen_activate_boot_verify_and_prune_fail_before_installation_mutation() {
    let temporary = tempfile::tempdir().unwrap();
    let installation_root = temporary.path().join("installation");
    let destination = temporary.path().join("frozen-root");
    fs::create_dir(&installation_root).unwrap();
    let client = Client::frozen(
        "frozen-scope-gates",
        frozen_test_installation(&installation_root),
        repository::Map::default(),
        &destination,
    )
    .unwrap();
    let before = directory_identity(&installation_root);

    assert!(matches!(
        client.activate_state(state::Id::from(999), true, true),
        Err(Error::FrozenClientProhibitedOperation)
    ));
    assert!(matches!(
        client.synchronize_boot(),
        Err(Error::FrozenClientProhibitedOperation)
    ));
    assert!(matches!(
        client.verify(true, false),
        Err(Error::FrozenClientProhibitedOperation)
    ));
    assert!(matches!(
        client.prune_cache(),
        Err(Error::FrozenClientProhibitedOperation)
    ));
    assert_eq!(directory_identity(&installation_root), before);
    assert!(!destination.exists());
}

fn add_empty_regular(client: &Client, package: &package::Id, path: &str) {
    client
        .layout_db
        .add(
            package,
            &StonePayloadLayoutRecord {
                uid: 0,
                gid: 0,
                mode: nix::libc::S_IFREG | 0o644,
                tag: 0,
                file: StonePayloadLayoutFile::Regular(EMPTY_FILE_DIGEST, path.into()),
            },
        )
        .unwrap();
}

fn add_cached_regular(client: &Client, package: &package::Id, digest: u128, path: &str, mode: u32) {
    client
        .layout_db
        .add(
            package,
            &StonePayloadLayoutRecord {
                uid: 0,
                gid: 0,
                mode: nix::libc::S_IFREG | mode,
                tag: 0,
                file: StonePayloadLayoutFile::Regular(digest, path.into()),
            },
        )
        .unwrap();
}

fn directory_identity(path: &Path) -> (u64, u64) {
    let metadata = fs::symlink_metadata(path).unwrap();
    assert!(metadata.file_type().is_dir());
    (metadata.dev(), metadata.ino())
}

fn file_identity(file: &std::fs::File) -> (u64, u64) {
    let metadata = file.metadata().unwrap();
    (metadata.dev(), metadata.ino())
}

fn mode(path: &Path) -> u32 {
    fs::symlink_metadata(path).unwrap().permissions().mode() & 0o7777
}

fn entry_names(path: &Path) -> Vec<OsString> {
    let mut names = fs::read_dir(path)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    names.sort();
    names
}

fn private_usr_residue(staging: &Path) -> PathBuf {
    let candidates = fs::read_dir(staging)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with(".cast-usr-"))
        })
        .collect::<Vec<_>>();
    assert_eq!(candidates.len(), 1, "expected one private candidate /usr residue");
    candidates.into_iter().next().unwrap()
}
