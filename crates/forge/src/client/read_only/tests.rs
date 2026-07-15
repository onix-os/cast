use std::{
    collections::BTreeSet,
    fs,
    io::Read as _,
    os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _},
    path::{Path, PathBuf},
};

use stone::{StonePayloadLayoutFile, StonePayloadLayoutRecord};

use crate::{
    Installation, db,
    package::{self, Meta, Name},
    state::{self, Selection, TransitionId},
    test_support::private_installation_tempdir,
    transition_journal::{
        BootId, MountNamespaceIdentity, Operation, Previous, PreviousOrigin, QuarantineName, RuntimeEpoch,
        RuntimeTreeIdentity, TransitionJournalStore, TransitionRecord, TreeToken,
    },
    tree_marker::TreeMarkerStore,
};

use super::{ReadOnlyClient, ReadOnlyClientError};

const TRANSITION_ID: &str = "0123456789abcdef0123456789abcdef";

struct Fixture {
    _temporary: tempfile::TempDir,
    root: PathBuf,
    package: package::Id,
    metadata: Meta,
    state: crate::State,
    layout: StonePayloadLayoutRecord,
}

fn fixture() -> Fixture {
    let temporary = private_installation_tempdir();
    let root = temporary.path().to_owned();
    let installation = Installation::open(&root, None).unwrap();
    let metadata_database = db::meta::Database::new(installation.db_path("install").to_str().unwrap()).unwrap();
    let state_database = db::state::Database::new(installation.db_path("state").to_str().unwrap()).unwrap();
    let layout_database = db::layout::Database::new(installation.db_path("layout").to_str().unwrap()).unwrap();

    let metadata = Meta {
        name: Name::from("alpha".to_owned()),
        version_identifier: "1.0".to_owned(),
        source_release: 1,
        build_release: 2,
        architecture: "x86_64".to_owned(),
        summary: "alpha summary".to_owned(),
        description: "alpha description".to_owned(),
        source_id: "alpha-source".to_owned(),
        homepage: "https://example.invalid/alpha".to_owned(),
        licenses: vec!["MPL-2.0".to_owned()],
        dependencies: BTreeSet::new(),
        providers: BTreeSet::new(),
        conflicts: BTreeSet::new(),
        uri: None,
        hash: None,
        download_size: Some(17),
    };
    let package: package::Id = metadata.id().into();
    metadata_database.add(package.clone(), metadata.clone()).unwrap();
    let state = state_database
        .add(
            &[Selection::explicit(package.clone())],
            Some("selected alpha"),
            Some("read-only client fixture"),
        )
        .unwrap();
    let layout = StonePayloadLayoutRecord {
        uid: 0,
        gid: 0,
        mode: 0o755,
        tag: 0,
        file: StonePayloadLayoutFile::Directory("share/alpha".into()),
    };
    layout_database.add(&package, &layout).unwrap();
    super::super::record_state_id(&root, state.id).unwrap();

    drop(layout_database);
    drop(state_database);
    drop(metadata_database);
    drop(installation);
    Fixture {
        _temporary: temporary,
        root,
        package,
        metadata,
        state,
        layout,
    }
}

fn snapshot(fixture: &Fixture) -> Installation {
    Installation::open_read_only(&fixture.root, None).unwrap()
}

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
        QuarantineName::parse("failed-read-only-client-test").unwrap(),
    )
    .unwrap()
}

fn create_unresolved_journal(fixture: &Fixture) -> Vec<u8> {
    let installation = Installation::open(&fixture.root, None).unwrap();
    let store = TransitionJournalStore::open_retained(installation.root_directory(), &installation.root).unwrap();
    store.create(&creation_record()).unwrap();
    drop(store);
    drop(installation);
    fs::read(fixture.root.join(".cast/journal/state-transition")).unwrap()
}

fn corrupt_live_state(fixture: &Fixture) -> Vec<u8> {
    let path = fixture.root.join("usr/.stateID");
    let bytes = b"malformed-live-state".to_vec();
    fs::write(&path, &bytes).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    bytes
}

#[test]
fn public_client_queries_exact_state_metadata_and_selected_layouts() {
    let fixture = fixture();
    let client = ReadOnlyClient::new(snapshot(&fixture)).unwrap();

    assert_eq!(client.list_state_ids().unwrap(), [fixture.state.id]);
    assert_eq!(client.get_state(fixture.state.id).unwrap(), Some(fixture.state.clone()));
    assert_eq!(client.get_active_state().unwrap(), Some(fixture.state.clone()));
    assert_eq!(
        client.get_package_meta(&fixture.package).unwrap(),
        Some(fixture.metadata.clone())
    );
    assert_eq!(
        client
            .selected_layouts(&[fixture.package.clone(), fixture.package.clone()])
            .unwrap(),
        [(fixture.package.clone(), fixture.layout.clone())]
    );
    assert_eq!(client.get_state(state::Id::from(999)).unwrap(), None);
    assert_eq!(
        client
            .get_package_meta(&package::Id::from("missing-package".to_owned()))
            .unwrap(),
        None
    );
}

#[test]
fn public_client_requires_an_explicit_snapshot_before_recovery_or_database_work() {
    let fixture = fixture();
    let state_before = fs::read(fixture.root.join(".cast/db/state")).unwrap();
    let mutable = Installation::open(&fixture.root, None).unwrap();
    assert!(matches!(
        ReadOnlyClient::new(mutable),
        Err(ReadOnlyClientError::ReadOnlyInstallationRequired)
    ));
    assert!(!fixture.root.join(".cast/journal").exists());
    assert_eq!(fs::read(fixture.root.join(".cast/db/state")).unwrap(), state_before);

    let frozen = Installation::open_frozen(&fixture.root, None).unwrap();
    assert!(matches!(
        ReadOnlyClient::new(frozen),
        Err(ReadOnlyClientError::ReadOnlyInstallationRequired)
    ));
    assert!(!fixture.root.join(".cast/journal").exists());
}

#[test]
fn unresolved_journal_precedes_corrupt_state_database_and_live_selection() {
    let fixture = fixture();
    let canonical = create_unresolved_journal(&fixture);
    fs::write(fixture.root.join(".cast/db/state"), b"not sqlite").unwrap();
    let malformed = corrupt_live_state(&fixture);

    assert!(matches!(
        ReadOnlyClient::new(snapshot(&fixture)),
        Err(ReadOnlyClientError::UnresolvedJournal { transition })
            if transition == TRANSITION_ID
    ));
    assert_eq!(
        fs::read(fixture.root.join(".cast/journal/state-transition")).unwrap(),
        canonical
    );
    assert_eq!(fs::read(fixture.root.join("usr/.stateID")).unwrap(), malformed);
    assert_eq!(fs::read(fixture.root.join(".cast/db/state")).unwrap(), b"not sqlite");
}

#[test]
fn orphan_transition_precedes_live_selection_and_later_database_images() {
    let fixture = fixture();
    let state_database = db::state::Database::new(fixture.root.join(".cast/db/state").to_str().unwrap()).unwrap();
    let orphan = state_database
        .add_with_transition(&transition_id(), &[], Some("orphan"), None)
        .unwrap();
    drop(state_database);
    fs::write(fixture.root.join(".cast/db/install"), b"corrupt metadata").unwrap();
    fs::write(fixture.root.join(".cast/db/layout"), b"corrupt layout").unwrap();
    let malformed = corrupt_live_state(&fixture);

    assert!(matches!(
        ReadOnlyClient::new(snapshot(&fixture)),
        Err(ReadOnlyClientError::OrphanTransitionRow { state, transition })
            if state == orphan.id && transition == TRANSITION_ID
    ));
    assert_eq!(fs::read(fixture.root.join("usr/.stateID")).unwrap(), malformed);
    assert_eq!(
        fs::read(fixture.root.join(".cast/db/install")).unwrap(),
        b"corrupt metadata"
    );
    assert_eq!(
        fs::read(fixture.root.join(".cast/db/layout")).unwrap(),
        b"corrupt layout"
    );
}

#[test]
fn prune_residue_precedes_strict_active_state_and_later_database_images() {
    let fixture = fixture();
    let residue = fixture
        .root
        .join(format!(".cast/quarantine/state-prune-70-{}", "a".repeat(32)));
    fs::create_dir(&residue).unwrap();
    fs::write(residue.join("retained-evidence"), b"unchanged").unwrap();
    fs::write(fixture.root.join(".cast/db/install"), b"corrupt metadata").unwrap();
    let malformed = corrupt_live_state(&fixture);

    assert!(matches!(
        ReadOnlyClient::new(snapshot(&fixture)),
        Err(ReadOnlyClientError::ArchivedPruneResidue { .. })
    ));
    assert_eq!(fs::read(residue.join("retained-evidence")).unwrap(), b"unchanged");
    assert_eq!(fs::read(fixture.root.join("usr/.stateID")).unwrap(), malformed);
    assert_eq!(
        fs::read(fixture.root.join(".cast/db/install")).unwrap(),
        b"corrupt metadata"
    );
}

#[test]
fn strict_active_state_precedes_metadata_and_layout_images() {
    let fixture = fixture();
    fs::write(fixture.root.join(".cast/db/install"), b"corrupt metadata").unwrap();
    fs::write(fixture.root.join(".cast/db/layout"), b"corrupt layout").unwrap();
    let malformed = corrupt_live_state(&fixture);

    assert!(matches!(
        ReadOnlyClient::new(snapshot(&fixture)),
        Err(ReadOnlyClientError::LiveActiveStateProof { .. })
    ));
    assert_eq!(fs::read(fixture.root.join("usr/.stateID")).unwrap(), malformed);
    assert_eq!(
        fs::read(fixture.root.join(".cast/db/install")).unwrap(),
        b"corrupt metadata"
    );
    assert_eq!(
        fs::read(fixture.root.join(".cast/db/layout")).unwrap(),
        b"corrupt layout"
    );
}

#[test]
fn live_selection_must_exist_in_the_captured_state_image() {
    let fixture = fixture();
    let missing = state::Id::from(999);
    super::super::record_state_id(&fixture.root, missing).unwrap();

    assert!(matches!(
        ReadOnlyClient::new(snapshot(&fixture)),
        Err(ReadOnlyClientError::ActiveStateMissing { state }) if state == missing
    ));
}

#[test]
fn public_queries_revalidate_journal_database_and_live_state_namespaces() {
    let journal_fixture = fixture();
    let journal_client = ReadOnlyClient::new(snapshot(&journal_fixture)).unwrap();
    fs::create_dir(journal_fixture.root.join(".cast/journal")).unwrap();
    assert!(matches!(
        journal_client.list_state_ids(),
        Err(ReadOnlyClientError::Journal { .. })
    ));

    let database_fixture = fixture();
    let database_client = ReadOnlyClient::new(snapshot(&database_fixture)).unwrap();
    let state_path = database_fixture.root.join(".cast/db/state");
    let retained = database_fixture.root.join(".cast/db/state-retained");
    fs::rename(&state_path, &retained).unwrap();
    fs::copy(&retained, &state_path).unwrap();
    assert!(matches!(
        database_client.list_state_ids(),
        Err(ReadOnlyClientError::StateSnapshot { .. })
    ));
    assert!(retained.is_file());
    assert!(state_path.is_file());

    let active_fixture = fixture();
    let active_client = ReadOnlyClient::new(snapshot(&active_fixture)).unwrap();
    let state_id = active_fixture.root.join("usr/.stateID");
    let changed = active_fixture.state.id.next().to_string();
    fs::write(&state_id, changed.as_bytes()).unwrap();
    assert!(matches!(
        active_client.get_active_state(),
        Err(ReadOnlyClientError::LiveActiveStateProof { .. })
    ));
}

#[test]
fn trailing_revalidation_detects_post_operation_changes_and_supersedes_operation_errors() {
    let success_fixture = fixture();
    let success_client = ReadOnlyClient::new(snapshot(&success_fixture)).unwrap();
    let success = success_client.query_with_post_operation(
        |_| Ok(()),
        || fs::create_dir(success_fixture.root.join(".cast/journal")).unwrap(),
    );
    assert!(matches!(success, Err(ReadOnlyClientError::Journal { .. })));

    let error_fixture = fixture();
    let error_client = ReadOnlyClient::new(snapshot(&error_fixture)).unwrap();
    let operation_state = state::Id::from(999);
    let error = error_client.query_with_post_operation(
        |_| Err::<(), _>(ReadOnlyClientError::ActiveStateMissing { state: operation_state }),
        || fs::create_dir(error_fixture.root.join(".cast/journal")).unwrap(),
    );
    assert!(matches!(error, Err(ReadOnlyClientError::Journal { .. })));
}

#[derive(Debug, Eq, PartialEq)]
struct Witness {
    device: u64,
    inode: u64,
    owner: u32,
    mode: u32,
    links: u64,
    length: u64,
    accessed_seconds: i64,
    accessed_nanoseconds: i64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
    bytes: Option<Vec<u8>>,
}

fn witness(path: &Path) -> Witness {
    let metadata = fs::symlink_metadata(path).unwrap();
    let bytes = metadata.file_type().is_file().then(|| {
        let mut file = fs::OpenOptions::new()
            .read(true)
            .custom_flags(nix::libc::O_NOATIME | nix::libc::O_CLOEXEC)
            .open(path)
            .unwrap();
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).unwrap();
        bytes
    });
    let metadata = fs::symlink_metadata(path).unwrap();
    Witness {
        device: metadata.dev(),
        inode: metadata.ino(),
        owner: metadata.uid(),
        mode: metadata.mode(),
        links: metadata.nlink(),
        length: metadata.len(),
        accessed_seconds: metadata.atime(),
        accessed_nanoseconds: metadata.atime_nsec(),
        modified_seconds: metadata.mtime(),
        modified_nanoseconds: metadata.mtime_nsec(),
        changed_seconds: metadata.ctime(),
        changed_nanoseconds: metadata.ctime_nsec(),
        bytes,
    }
}

fn age_atime(path: &Path) {
    let metadata = fs::symlink_metadata(path).unwrap();
    let modified = filetime::FileTime::from_last_modification_time(&metadata);
    filetime::set_file_times(path, filetime::FileTime::from_unix_time(1, 0), modified).unwrap();
}

#[test]
fn construction_queries_and_drop_preserve_contents_metadata_and_atimes() {
    let fixture = fixture();
    let paths = [
        fixture.root.join("usr"),
        fixture.root.join("usr/.stateID"),
        fixture.root.join(".cast/quarantine"),
        fixture.root.join(".cast/db/state"),
        fixture.root.join(".cast/db/install"),
        fixture.root.join(".cast/db/layout"),
    ];
    for path in &paths {
        age_atime(path);
    }
    let before = paths.iter().map(|path| witness(path)).collect::<Vec<_>>();

    let client = ReadOnlyClient::new(snapshot(&fixture)).unwrap();
    assert_eq!(client.list_state_ids().unwrap(), [fixture.state.id]);
    assert_eq!(client.get_active_state().unwrap(), Some(fixture.state.clone()));
    assert_eq!(
        client.get_package_meta(&fixture.package).unwrap(),
        Some(fixture.metadata.clone())
    );
    assert_eq!(
        client.selected_layouts(std::slice::from_ref(&fixture.package)).unwrap(),
        [(fixture.package.clone(), fixture.layout.clone())]
    );
    drop(client);

    assert_eq!(paths.iter().map(|path| witness(path)).collect::<Vec<_>>(), before);
}

#[test]
fn marker_only_active_baseline_is_read_without_changing_marker_or_directory_atime() {
    let fixture = fixture();
    let usr = fixture.root.join("usr");
    fs::remove_file(usr.join(".stateID")).unwrap();
    let store = TreeMarkerStore::open_path(&usr).unwrap();
    drop(store.adopt_or_create_before_journal().unwrap());
    drop(store);
    let marker = usr.join(".cast-tree-id");
    age_atime(&usr);
    age_atime(&marker);
    let before = [witness(&usr), witness(&marker)];

    let client = ReadOnlyClient::new(snapshot(&fixture)).unwrap();
    assert_eq!(client.get_active_state().unwrap(), None);
    drop(client);

    assert_eq!([witness(&usr), witness(&marker)], before);
}

#[test]
fn public_client_retains_the_global_shared_lock_until_drop() {
    let fixture = fixture();
    let client = ReadOnlyClient::new(snapshot(&fixture)).unwrap();
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(fixture.root.join(".cast/.cast-lockfile"))
        .unwrap();

    // SAFETY: flock borrows the live descriptor and retains no pointer.
    let contended = unsafe {
        nix::libc::flock(
            std::os::fd::AsRawFd::as_raw_fd(&lock),
            nix::libc::LOCK_EX | nix::libc::LOCK_NB,
        )
    };
    assert_eq!(contended, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(nix::libc::EWOULDBLOCK)
    );

    drop(client);
    // SAFETY: the same live descriptor remains open after the client drops.
    assert_eq!(
        unsafe {
            nix::libc::flock(
                std::os::fd::AsRawFd::as_raw_fd(&lock),
                nix::libc::LOCK_EX | nix::libc::LOCK_NB,
            )
        },
        0
    );
}
