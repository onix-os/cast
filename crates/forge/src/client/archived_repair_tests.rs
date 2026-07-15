//! Focused client orchestration proofs for non-active archived-state repair.

use std::{
    collections::BTreeSet,
    ffi::OsString,
    os::{
        fd::AsRawFd as _,
        unix::fs::{MetadataExt as _, PermissionsExt as _},
    },
    path::{Path, PathBuf},
};

use fs_err as fs;

use super::archived_repair::{ArchivedRepairCheckpoint, RepairError};
use super::{Client, Error, Scope, record_state_id, take_observed_trigger_scopes};
use crate::{
    Installation, Provider, State, SystemModel, linux_fs, package, repository, system_model,
    test_support::prepare_private_installation_root, transition_identity::ArchivedStateRepairPublication,
    tree_marker::TreeMarkerStore,
};

mod faults;
mod identity;
mod materialization;
mod metadata;
mod namespace_races;
mod semantics;

struct Fixture {
    _temporary: tempfile::TempDir,
    client: Client,
    active: State,
    repaired: State,
    archived_root: PathBuf,
}

impl Fixture {
    fn new(existing_archive: bool) -> Self {
        let temporary = tempfile::tempdir().unwrap();
        prepare_private_installation_root(temporary.path());
        let installation = Installation::open(temporary.path(), None).unwrap();
        let mut client = Client::builder("archived-repair-test", installation)
            .repositories(repository::Map::default())
            .build()
            .unwrap();
        assert!(matches!(&client.scope, Scope::Stateful));

        let active = client.state_db.add(&[], Some("active"), None).unwrap();
        let repaired = client.state_db.add(&[], Some("repaired"), None).unwrap();
        client.installation.active_state = Some(active.id);
        record_state_id(&client.installation.root, active.id).unwrap();
        prepare_strict_live_tree_marker(&client.installation);
        fs::write(client.installation.root.join("usr/live-sentinel"), b"live").unwrap();
        fs::create_dir(client.installation.root.join("etc")).unwrap();
        fs::write(client.installation.root.join("etc/local-sentinel"), b"local").unwrap();
        fs::create_dir(client.installation.root.join("boot")).unwrap();
        fs::write(client.installation.root.join("boot/boot-sentinel"), b"boot").unwrap();

        let archived_root = client.installation.root_path(repaired.id.to_string());
        if existing_archive {
            fs::create_dir(&archived_root).unwrap();
            fs::set_permissions(&archived_root, std::fs::Permissions::from_mode(0o700)).unwrap();
            fs::create_dir(archived_root.join("usr")).unwrap();
            fs::write(archived_root.join("usr/.stateID"), b"corrupt-old-id").unwrap();
            fs::set_permissions(
                archived_root.join("usr/.stateID"),
                std::fs::Permissions::from_mode(0o644),
            )
            .unwrap();
            fs::write(archived_root.join("opaque-root-sentinel"), b"old-wrapper").unwrap();
        }

        Self {
            _temporary: temporary,
            client,
            active,
            repaired,
            archived_root,
        }
    }

    fn empty_candidate(&self) -> super::archived_repair_materialization::ArchivedRepairCandidate {
        self.client
            .materialize_archived_repair_root(std::iter::empty::<&package::Id>())
            .unwrap()
    }

    fn snapshot(&self, package: &str) -> SystemModel {
        system_model::create(
            repository::Map::default(),
            BTreeSet::from([Provider::package_name(package)]),
        )
    }
}

fn prepare_strict_live_tree_marker(installation: &Installation) {
    let live_path = installation.root.join("usr");
    let live = linux_fs::openat2_file(
        installation.root_directory().as_raw_fd(),
        c"usr",
        nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK,
        0,
        linux_fs::controlled_resolution(),
    )
    .unwrap();
    let store = TreeMarkerStore::open(&live, live_path).unwrap();
    store
        .adopt_or_create_before_journal()
        .unwrap()
        .revalidate(&store)
        .unwrap();
}

#[test]
fn archived_repair_replaces_the_whole_wrapper_and_preserves_old_payload_opaquely() {
    let fixture = Fixture::new(true);
    let old_wrapper = directory_identity(&fixture.archived_root);
    let staging = fixture.client.installation.staging_dir();
    let mut candidate_wrapper = None;

    let publication = fixture
        .client
        .repair_archived_state_with_checkpoint(
            fixture.empty_candidate(),
            &fixture.repaired,
            fixture.snapshot("repaired-package"),
            |point| {
                if point == ArchivedRepairCheckpoint::IdentityPrepared {
                    candidate_wrapper = Some(directory_identity(&staging));
                }
                Ok(())
            },
        )
        .unwrap();

    let ArchivedStateRepairPublication::Replaced { displaced_wrapper } = publication else {
        panic!("existing archive must use whole-wrapper replacement");
    };
    assert_eq!(directory_identity(&fixture.archived_root), candidate_wrapper.unwrap());
    assert_eq!(directory_mode(&fixture.archived_root), 0o700);
    assert_eq!(directory_identity(&displaced_wrapper), old_wrapper);
    assert_eq!(
        fs::read(displaced_wrapper.join("opaque-root-sentinel")).unwrap(),
        b"old-wrapper"
    );
    assert_eq!(
        fs::read(displaced_wrapper.join("usr/.stateID")).unwrap(),
        b"corrupt-old-id"
    );
    assert_eq!(read_entry_names(&fixture.archived_root), [OsString::from("usr")]);
    assert_exact_empty_private_staging(&staging);
    assert_repaired_snapshot(&fixture.archived_root, fixture.repaired.id, "repaired-package");
    assert_eq!(
        fixture.client.state_db.get(fixture.repaired.id).unwrap(),
        fixture.repaired
    );
    assert_eq!(fixture.client.installation.active_state, Some(fixture.active.id));
}

#[test]
fn archived_repair_publishes_missing_wrapper_directly_and_restores_empty_staging() {
    let fixture = Fixture::new(false);
    let staging = fixture.client.installation.staging_dir();
    let mut candidate_wrapper = None;

    let publication = fixture
        .client
        .repair_archived_state_with_checkpoint(
            fixture.empty_candidate(),
            &fixture.repaired,
            fixture.snapshot("missing-package"),
            |point| {
                if point == ArchivedRepairCheckpoint::IdentityPrepared {
                    candidate_wrapper = Some(directory_identity(&staging));
                }
                Ok(())
            },
        )
        .unwrap();

    assert_eq!(publication, ArchivedStateRepairPublication::Published);
    assert_eq!(directory_identity(&fixture.archived_root), candidate_wrapper.unwrap());
    assert_eq!(directory_mode(&fixture.archived_root), 0o700);
    assert_eq!(read_entry_names(&fixture.archived_root), [OsString::from("usr")]);
    assert_exact_empty_private_staging(&staging);
    assert_repaired_snapshot(&fixture.archived_root, fixture.repaired.id, "missing-package");
}

#[test]
fn archived_repair_runs_only_transaction_scope_and_never_mutates_live_namespaces() {
    let fixture = Fixture::new(false);
    assert!(take_observed_trigger_scopes().is_empty());

    fixture
        .client
        .repair_archived_state(
            fixture.empty_candidate(),
            &fixture.repaired,
            fixture.snapshot("scope-package"),
        )
        .unwrap();

    assert_eq!(take_observed_trigger_scopes(), ["retained-transaction"]);
    assert_eq!(
        fs::read(fixture.client.installation.root.join("usr/live-sentinel")).unwrap(),
        b"live"
    );
    assert_eq!(
        fs::read(fixture.client.installation.root.join("etc/local-sentinel")).unwrap(),
        b"local"
    );
    assert_eq!(
        fs::read(fixture.client.installation.root.join("boot/boot-sentinel")).unwrap(),
        b"boot"
    );
    assert_eq!(read_entry_names(&fixture.archived_root), [OsString::from("usr")]);
    for (_, target) in super::ROOT_ABI_LINKS {
        assert!(fixture.client.installation.isolation_dir().join(target).is_symlink());
        assert!(!fixture.archived_root.join(target).exists());
    }
}

#[test]
fn archived_repair_preserves_a_trigger_corrupted_candidate_as_one_opaque_wrapper() {
    let fixture = Fixture::new(true);
    let old_wrapper = directory_identity(&fixture.archived_root);
    let staging = fixture.client.installation.staging_dir();
    let state_id = staging.join("usr/.stateID");

    let error = fixture
        .client
        .repair_archived_state_with_checkpoint(
            fixture.empty_candidate(),
            &fixture.repaired,
            fixture.snapshot("failed-package"),
            |point| {
                if point == ArchivedRepairCheckpoint::AfterTransactionTriggers {
                    fs::write(&state_id, b"trigger-corrupted-state-id").unwrap();
                }
                Ok(())
            },
        )
        .unwrap_err();

    let RepairError::CandidatePreserved { quarantine, .. } = repair_error(error) else {
        panic!("failed candidate must be preserved as a whole wrapper");
    };
    assert_eq!(directory_identity(&fixture.archived_root), old_wrapper);
    assert_eq!(
        fs::read(fixture.archived_root.join("opaque-root-sentinel")).unwrap(),
        b"old-wrapper"
    );
    assert_eq!(
        fs::read(quarantine.join("usr/.stateID")).unwrap(),
        b"trigger-corrupted-state-id"
    );
    assert_eq!(read_entry_names(&quarantine), [OsString::from("usr")]);
    assert_exact_empty_private_staging(&staging);
    assert_eq!(
        fixture.client.state_db.get(fixture.repaired.id).unwrap(),
        fixture.repaired
    );
}

#[test]
fn archived_repair_rejects_a_foreign_canonical_file_without_touching_it() {
    let fixture = Fixture::new(false);
    fs::write(&fixture.archived_root, b"foreign canonical occupant").unwrap();
    let fstree = fixture.empty_candidate();

    let error = fixture
        .client
        .repair_archived_state(fstree, &fixture.repaired, fixture.snapshot("rejected-package"))
        .unwrap_err();
    assert!(matches!(repair_error(error), RepairError::Preparation { .. }));
    assert_eq!(fs::read(&fixture.archived_root).unwrap(), b"foreign canonical occupant");
    assert_eq!(
        read_entry_names(&fixture.client.installation.staging_dir()),
        [OsString::from("usr")]
    );
    assert_eq!(
        fixture.client.state_db.get(fixture.repaired.id).unwrap(),
        fixture.repaired
    );
}

#[test]
fn archived_repair_preserves_an_old_wrapper_with_no_state_id_without_repairing_it() {
    let fixture = Fixture::new(true);
    fs::remove_file(fixture.archived_root.join("usr/.stateID")).unwrap();

    let publication = fixture
        .client
        .repair_archived_state(
            fixture.empty_candidate(),
            &fixture.repaired,
            fixture.snapshot("missing-old-state-id"),
        )
        .unwrap();
    let ArchivedStateRepairPublication::Replaced { displaced_wrapper } = publication else {
        panic!("existing archive must be retained whole");
    };
    assert!(!displaced_wrapper.join("usr/.stateID").exists());
    assert_eq!(
        fs::read(displaced_wrapper.join("opaque-root-sentinel")).unwrap(),
        b"old-wrapper"
    );
}

#[test]
fn archived_repair_detects_active_row_deletion_and_reports_preservation_incomplete() {
    let fixture = Fixture::new(true);
    let active = fixture.active.id;
    let client = &fixture.client;

    let error = client
        .repair_archived_state_with_checkpoint(
            fixture.empty_candidate(),
            &fixture.repaired,
            fixture.snapshot("active-row-race"),
            |point| {
                if point == ArchivedRepairCheckpoint::MetadataRecorded {
                    client.state_db.remove(&active)?;
                }
                Ok(())
            },
        )
        .unwrap_err();

    let RepairError::CandidatePreservationIncomplete { outcome, .. } = repair_error(error) else {
        panic!("post-move semantic failure must not claim complete preservation");
    };
    assert_eq!(outcome, "applied");
    let preserved = archived_repair_quarantine_paths(&fixture);
    assert_eq!(preserved.len(), 1);
    assert_eq!(
        fs::read_to_string(preserved[0].join("usr/.stateID")).unwrap(),
        fixture.repaired.id.to_string()
    );
    assert_exact_empty_private_staging(&fixture.client.installation.staging_dir());
    assert_eq!(
        fixture.client.state_db.get(fixture.repaired.id).unwrap(),
        fixture.repaired
    );
}

#[test]
fn archived_repair_rejects_a_target_selected_active_before_guard_preparation() {
    let mut fixture = Fixture::new(false);
    fixture.client.installation.active_state = Some(fixture.repaired.id);

    let error = fixture
        .client
        .repair_archived_state(
            fixture.empty_candidate(),
            &fixture.repaired,
            fixture.snapshot("became-active"),
        )
        .unwrap_err();
    assert!(matches!(repair_error(error), RepairError::Preparation { .. }));
    assert!(!fixture.archived_root.exists());
    assert_eq!(
        fixture.client.state_db.get(fixture.repaired.id).unwrap(),
        fixture.repaired
    );
}

fn repair_error(error: Error) -> RepairError {
    let Error::ArchivedStateRepair { source } = error else {
        panic!("expected archived-state repair error");
    };
    *source
        .downcast::<RepairError>()
        .expect("archived-state repair source must retain its structured type")
}

fn directory_identity(path: &Path) -> (u64, u64) {
    let metadata = fs::symlink_metadata(path).unwrap();
    assert!(metadata.file_type().is_dir());
    (metadata.dev(), metadata.ino())
}

fn directory_mode(path: &Path) -> u32 {
    fs::symlink_metadata(path).unwrap().permissions().mode() & 0o7777
}

fn read_entry_names(path: &Path) -> Vec<OsString> {
    let mut names = fs::read_dir(path)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    names.sort();
    names
}

fn assert_exact_empty_private_staging(path: &Path) {
    assert!(read_entry_names(path).is_empty());
    assert_eq!(fs::symlink_metadata(path).unwrap().permissions().mode() & 0o7777, 0o700);
}

fn assert_repaired_snapshot(root: &Path, state: crate::state::Id, package: &str) {
    assert_eq!(
        fs::read_to_string(root.join("usr/.stateID")).unwrap(),
        state.to_string()
    );
    let encoded = fs::read_to_string(system_model::snapshot_path(root)).unwrap();
    assert!(encoded.contains(package));
}

fn archived_repair_quarantine_paths(fixture: &Fixture) -> Vec<PathBuf> {
    let prefix = format!("archived-repair-{}-", i32::from(fixture.repaired.id));
    let mut paths = fs::read_dir(fixture.client.installation.state_quarantine_dir())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with(&prefix))
        })
        .collect::<Vec<_>>();
    paths.sort();
    paths
}
