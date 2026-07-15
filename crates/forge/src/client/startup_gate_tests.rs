use std::{fs, os::unix::fs::PermissionsExt as _, path::Path};

use crate::{
    Installation,
    repository::{self, Priority, Repository, Source},
    state::TransitionId,
    test_support::private_installation_tempdir,
    transition_journal::{
        BootId, MountNamespaceIdentity, Operation, Previous, PreviousOrigin, QuarantineName, RuntimeEpoch,
        RuntimeTreeIdentity, StorageError, TransitionJournalStore, TransitionRecord, TreeToken,
    },
};

use super::{Client, Error, startup_gate};

const TRANSITION_ID: &str = "0123456789abcdef0123456789abcdef";

fn transition_id() -> TransitionId {
    TransitionId::parse(TRANSITION_ID).unwrap()
}

fn creation_record() -> TransitionRecord {
    TransitionRecord::preparing(
        transition_id(),
        RuntimeEpoch {
            boot_id: BootId::parse("01234567-89ab-4cde-8f01-23456789abcd").unwrap(),
            mount_namespace: MountNamespaceIdentity { st_dev: 30, inode: 31 },
        },
        Operation::NewState,
        None,
        TreeToken::parse("a".repeat(TreeToken::TEXT_LENGTH)).unwrap(),
        RuntimeTreeIdentity {
            st_dev: 10,
            inode: 10,
            mount_id: 12,
        },
        Previous {
            id: Some(41),
            tree_token: TreeToken::parse("b".repeat(TreeToken::TEXT_LENGTH)).unwrap(),
            usr_runtime_identity: RuntimeTreeIdentity {
                st_dev: 10,
                inode: 20,
                mount_id: 12,
            },
            origin: PreviousOrigin::ActiveState,
        },
        true,
        true,
        QuarantineName::parse("failed-startup-gate-test").unwrap(),
    )
    .unwrap()
}

fn guarded_repositories() -> repository::Map {
    repository::Map::with([(
        repository::Id::new("must-not-open"),
        Repository {
            description: "startup gate ordering sentinel".to_owned(),
            source: Source::DirectIndex("https://packages.invalid/stone.index".parse().unwrap()),
            priority: Priority::new(1),
            active: true,
        },
    )])
}

fn canonical_journal(root: &Path) -> std::path::PathBuf {
    root.join(".cast/journal/state-transition")
}

fn create_journal(installation: &Installation) -> Vec<u8> {
    let store = TransitionJournalStore::open_retained(installation.root_directory(), &installation.root).unwrap();
    store.create(&creation_record()).unwrap();
    drop(store);
    fs::read(canonical_journal(&installation.root)).unwrap()
}

fn expect_startup_gate_error(result: Result<Client, Error>) -> Box<startup_gate::Error> {
    let source = match result {
        Err(Error::SystemStartupGate { source }) => source,
        Err(other) => panic!("expected startup-gate error, got {other:?}"),
        Ok(_) => panic!("startup unexpectedly succeeded"),
    };
    match source.downcast::<startup_gate::Error>() {
        Ok(source) => source,
        Err(source) => panic!("unexpected startup-gate source: {source}"),
    }
}

fn assert_repository_construction_not_started(installation: &Installation) {
    let entries = fs::read_dir(installation.repo_path(""))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert!(
        entries.is_empty(),
        "repository cache was created before the startup gate"
    );
}

#[test]
fn valid_unresolved_journal_precedes_system_intent_and_repository_construction() {
    let temporary = private_installation_tempdir();
    let installation = Installation::open(temporary.path(), None).unwrap();
    let canonical_before = create_journal(&installation);
    let missing_intent = temporary.path().join("must-not-load.gluon");

    let error = expect_startup_gate_error(
        Client::builder("startup-gate-valid-journal", installation.clone())
            .system_intent_path(missing_intent)
            .repositories(guarded_repositories())
            .build(),
    );

    assert!(matches!(
        error.as_ref(),
        startup_gate::Error::UnresolvedJournal { transition } if transition == TRANSITION_ID
    ));
    assert_eq!(fs::read(canonical_journal(temporary.path())).unwrap(), canonical_before);
    assert_repository_construction_not_started(&installation);
}

#[test]
fn corrupt_canonical_journal_blocks_startup_without_rewriting_evidence() {
    let temporary = private_installation_tempdir();
    let installation = Installation::open(temporary.path(), None).unwrap();
    let store = TransitionJournalStore::open_retained(installation.root_directory(), &installation.root).unwrap();
    drop(store);
    let canonical = canonical_journal(temporary.path());
    let corrupt = b"not-a-canonical-transition-record";
    fs::write(&canonical, corrupt).unwrap();
    fs::set_permissions(&canonical, fs::Permissions::from_mode(0o600)).unwrap();

    let error = expect_startup_gate_error(
        Client::builder("startup-gate-corrupt-journal", installation.clone())
            .repositories(guarded_repositories())
            .build(),
    );

    assert!(matches!(
        error.as_ref(),
        startup_gate::Error::Journal(StorageError::Decode(_))
    ));
    assert_eq!(fs::read(&canonical).unwrap(), corrupt);
    assert_repository_construction_not_started(&installation);
}

#[test]
fn orphan_transition_row_blocks_startup_before_repository_construction() {
    let temporary = private_installation_tempdir();
    let installation = Installation::open(temporary.path(), None).unwrap();
    let state_db = crate::db::state::Database::new(installation.db_path("state").to_str().unwrap()).unwrap();
    let orphan = state_db
        .add_with_transition(&transition_id(), &[], Some("orphan"), None)
        .unwrap();
    drop(state_db);

    let error = expect_startup_gate_error(
        Client::builder("startup-gate-orphan", installation.clone())
            .repositories(guarded_repositories())
            .build(),
    );

    assert!(matches!(
        error.as_ref(),
        startup_gate::Error::OrphanTransitionRow { state, transition }
            if *state == i32::from(orphan.id) && transition == TRANSITION_ID
    ));
    assert_repository_construction_not_started(&installation);
}

#[test]
fn frozen_client_ignores_system_journal_and_persistent_transition_rows() {
    let temporary = private_installation_tempdir();
    let installation = Installation::open(temporary.path(), None).unwrap();
    let state_db = crate::db::state::Database::new(installation.db_path("state").to_str().unwrap()).unwrap();
    state_db
        .add_with_transition(&transition_id(), &[], Some("system-only orphan"), None)
        .unwrap();
    drop(state_db);
    let canonical_before = create_journal(&installation);
    drop(installation);

    let frozen_installation = Installation::open_frozen(temporary.path(), None).unwrap();
    let frozen_destination = private_installation_tempdir();
    let client = match Client::frozen(
        "startup-gate-frozen",
        frozen_installation,
        guarded_repositories(),
        frozen_destination.path(),
    ) {
        Ok(client) => client,
        Err(error) => panic!("frozen client unexpectedly consulted system recovery evidence: {error:?}"),
    };
    drop(client);

    assert_eq!(fs::read(canonical_journal(temporary.path())).unwrap(), canonical_before);
}

#[test]
fn system_builder_cannot_use_frozen_discovery_to_bypass_the_startup_gate() {
    let temporary = private_installation_tempdir();
    let frozen_installation = Installation::open_frozen(temporary.path(), None).unwrap();

    assert!(matches!(
        Client::new("startup-gate-frozen-builder", frozen_installation),
        Err(Error::SystemInstallationRequired)
    ));
}
