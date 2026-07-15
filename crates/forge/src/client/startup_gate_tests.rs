use std::{
    cell::RefCell,
    ffi::OsString,
    fs,
    os::unix::ffi::OsStringExt as _,
    os::unix::fs::{FileTypeExt as _, MetadataExt as _, PermissionsExt as _, symlink},
    path::Path,
    rc::Rc,
};

use crate::{
    Installation, Provider,
    repository::{self, Priority, Repository, Source},
    state::TransitionId,
    test_support::{prepare_private_installation_root, private_installation_tempdir},
    transition_identity::ArchivedStatePruneResidueError,
    transition_journal::{
        BootId, MountNamespaceIdentity, Operation, Previous, PreviousOrigin, QuarantineName, RuntimeEpoch,
        RuntimeTreeIdentity, StorageError, TransitionJournalStore, TransitionRecord, TreeToken,
    },
};

use super::{Client, Error, arm_system_intent_notice_capture, disarm_system_intent_notice_capture, startup_gate};

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

fn prune_residue(root: &Path, state: i32) -> std::path::PathBuf {
    root.join(format!(".cast/quarantine/state-prune-{state}-{}", "a".repeat(32)))
}

fn create_journal(installation: &Installation) -> Vec<u8> {
    let store = TransitionJournalStore::open_retained(installation.root_directory(), &installation.root).unwrap();
    store.create(&creation_record()).unwrap();
    drop(store);
    fs::read(canonical_journal(&installation.root)).unwrap()
}

fn create_malformed_live_state(root: &Path) -> Vec<u8> {
    let usr = root.join("usr");
    fs::create_dir(&usr).unwrap();
    fs::set_permissions(&usr, fs::Permissions::from_mode(0o755)).unwrap();
    let state_id = usr.join(".stateID");
    let malformed = b"malformed";
    fs::write(&state_id, malformed).unwrap();
    fs::set_permissions(&state_id, fs::Permissions::from_mode(0o644)).unwrap();
    malformed.to_vec()
}

fn write_system_intent(root: &Path, package: &str) -> std::path::PathBuf {
    write_system_intent_with_warning(root, package, true)
}

fn write_system_intent_with_warning(root: &Path, package: &str, disable_warning: bool) -> std::path::PathBuf {
    let etc = root.join("etc");
    let cast = etc.join("cast");
    fs::create_dir_all(&cast).unwrap();
    fs::set_permissions(&etc, fs::Permissions::from_mode(0o755)).unwrap();
    fs::set_permissions(&cast, fs::Permissions::from_mode(0o755)).unwrap();
    let path = cast.join("system.glu");
    fs::write(
        &path,
        format!(
            r#"let cast = import! cast.system.v1
{{
    disable_warning = cast.boolean.{},
    packages = ["{package}"],
    .. cast.system
}}
"#,
            if disable_warning { "true" } else { "false" },
        ),
    )
    .unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    path
}

fn write_malformed_system_intent(root: &Path) -> (std::path::PathBuf, Vec<u8>) {
    let path = write_system_intent(root, "will-be-replaced-with-malformed-source");
    let malformed = b"this canonical default must not be evaluated before recovery evidence".to_vec();
    fs::write(&path, &malformed).unwrap();
    (path, malformed)
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

fn assert_system_databases_opened(installation: &Installation) {
    for name in ["install", "state", "layout"] {
        assert!(
            installation.db_path(name).is_file(),
            "{name} database was not opened before the startup gate"
        );
    }
}

#[test]
fn valid_unresolved_journal_precedes_malformed_live_state_system_intent_and_repositories() {
    let temporary = private_installation_tempdir();
    let (intent_path, malformed_intent) = write_malformed_system_intent(temporary.path());
    let installation = Installation::open(temporary.path(), None).unwrap();
    let canonical_before = create_journal(&installation);
    let residue = prune_residue(temporary.path(), 70);
    fs::create_dir(&residue).unwrap();
    let malformed_state = create_malformed_live_state(temporary.path());

    let error = expect_startup_gate_error(
        Client::builder("startup-gate-valid-journal", installation.clone())
            .repositories(guarded_repositories())
            .build(),
    );
    let explicit_error = expect_startup_gate_error(
        Client::builder("startup-gate-explicit-journal", installation.clone())
            .system_intent_path(temporary.path().join("must-not-load-explicit.glu"))
            .repositories(guarded_repositories())
            .build(),
    );

    assert!(matches!(
        error.as_ref(),
        startup_gate::Error::UnresolvedJournal { transition } if transition == TRANSITION_ID
    ));
    assert!(matches!(
        explicit_error.as_ref(),
        startup_gate::Error::UnresolvedJournal { transition } if transition == TRANSITION_ID
    ));
    assert_eq!(fs::read(canonical_journal(temporary.path())).unwrap(), canonical_before);
    assert_eq!(fs::read(intent_path).unwrap(), malformed_intent);
    assert!(residue.is_dir());
    assert_eq!(
        fs::read(temporary.path().join("usr/.stateID")).unwrap(),
        malformed_state
    );
    assert_system_databases_opened(&installation);
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
fn orphan_transition_row_precedes_malformed_live_state_and_repository_construction() {
    let temporary = private_installation_tempdir();
    let (intent_path, malformed_intent) = write_malformed_system_intent(temporary.path());
    let installation = Installation::open(temporary.path(), None).unwrap();
    let state_db = crate::db::state::Database::new(installation.db_path("state").to_str().unwrap()).unwrap();
    let orphan = state_db
        .add_with_transition(&transition_id(), &[], Some("orphan"), None)
        .unwrap();
    drop(state_db);
    let residue = prune_residue(temporary.path(), 71);
    fs::create_dir(&residue).unwrap();
    let malformed_state = create_malformed_live_state(temporary.path());

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
    assert_eq!(
        fs::read(temporary.path().join("usr/.stateID")).unwrap(),
        malformed_state
    );
    assert_eq!(fs::read(intent_path).unwrap(), malformed_intent);
    assert!(residue.is_dir());
    assert_repository_construction_not_started(&installation);
}

#[test]
fn archived_state_prune_residue_types_block_startup_before_live_state_intent_and_repositories() {
    for kind in ["directory", "file", "symlink", "fifo", "invalid-name"] {
        let temporary = private_installation_tempdir();
        let (intent_path, malformed_intent) = write_malformed_system_intent(temporary.path());
        let installation = Installation::open(temporary.path(), None).unwrap();
        let residue = if kind == "invalid-name" {
            let mut name = b"state-prune-80-".to_vec();
            name.extend([0xff, 0xfe]);
            temporary.path().join(".cast/quarantine").join(OsString::from_vec(name))
        } else {
            prune_residue(temporary.path(), 80)
        };
        let outside = temporary.path().join("outside-sentinel");
        fs::write(&outside, b"outside-unchanged").unwrap();
        match kind {
            "directory" => {
                fs::create_dir(&residue).unwrap();
                fs::write(residue.join("retained-evidence"), b"directory-unchanged").unwrap();
            }
            "file" => fs::write(&residue, b"file-unchanged").unwrap(),
            "symlink" => symlink(&outside, &residue).unwrap(),
            "fifo" => nix::unistd::mkfifo(&residue, nix::sys::stat::Mode::from_bits_truncate(0o600)).unwrap(),
            "invalid-name" => fs::write(&residue, b"invalid-name-unchanged").unwrap(),
            _ => unreachable!(),
        }
        let before = fs::symlink_metadata(&residue).unwrap();
        let malformed_state = create_malformed_live_state(temporary.path());

        let error = expect_startup_gate_error(
            Client::builder(format!("startup-gate-prune-residue-{kind}"), installation.clone())
                .repositories(guarded_repositories())
                .build(),
        );

        assert!(matches!(
            error.as_ref(),
            startup_gate::Error::ArchivedStatePruneResidue(ArchivedStatePruneResidueError::Residue { path })
                if path == &residue
        ));
        let after = fs::symlink_metadata(&residue).unwrap();
        assert_eq!((after.dev(), after.ino()), (before.dev(), before.ino()), "{kind}");
        match kind {
            "directory" => assert_eq!(
                fs::read(residue.join("retained-evidence")).unwrap(),
                b"directory-unchanged"
            ),
            "file" => assert_eq!(fs::read(&residue).unwrap(), b"file-unchanged"),
            "symlink" => assert_eq!(fs::read_link(&residue).unwrap(), outside),
            "fifo" => assert!(fs::symlink_metadata(&residue).unwrap().file_type().is_fifo()),
            "invalid-name" => assert_eq!(fs::read(&residue).unwrap(), b"invalid-name-unchanged"),
            _ => unreachable!(),
        }
        assert_eq!(fs::read(&outside).unwrap(), b"outside-unchanged");
        assert_eq!(fs::read(intent_path).unwrap(), malformed_intent);
        assert_eq!(
            fs::read(temporary.path().join("usr/.stateID")).unwrap(),
            malformed_state
        );
        assert_system_databases_opened(&installation);
        assert_repository_construction_not_started(&installation);
    }
}

#[test]
fn archived_state_prune_residue_inserted_between_bounded_scans_blocks_startup() {
    let temporary = private_installation_tempdir();
    let installation = Installation::open(temporary.path(), None).unwrap();
    let residue = prune_residue(temporary.path(), 81);
    let inserted = residue.clone();
    crate::transition_identity::arm_after_archived_state_prune_residue_first_scan(move || {
        fs::create_dir(&inserted).unwrap();
        fs::write(inserted.join("retained-evidence"), b"inserted-unchanged").unwrap();
    });

    let error = expect_startup_gate_error(Client::new("startup-gate-prune-residue-race", installation));

    assert!(matches!(
        error.as_ref(),
        startup_gate::Error::ArchivedStatePruneResidue(ArchivedStatePruneResidueError::Residue { path })
            if path == &residue
    ));
    assert_eq!(
        fs::read(residue.join("retained-evidence")).unwrap(),
        b"inserted-unchanged"
    );
}

#[test]
fn archived_state_prune_residue_audit_rejects_an_oversized_quarantine_without_removing_entries() {
    let temporary = private_installation_tempdir();
    let installation = Installation::open(temporary.path(), None).unwrap();
    let quarantine = temporary.path().join(".cast/quarantine");
    for index in 0..=4_096 {
        fs::write(quarantine.join(format!("unrelated-{index:04}")), b"").unwrap();
    }

    let error = expect_startup_gate_error(Client::new("startup-gate-prune-residue-bound", installation));

    assert!(matches!(
        error.as_ref(),
        startup_gate::Error::ArchivedStatePruneResidue(ArchivedStatePruneResidueError::Namespace(_))
    ));
    assert_eq!(fs::read_dir(&quarantine).unwrap().count(), 4_097);
}

#[test]
fn archived_state_prune_residue_audit_accepts_unrelated_entries_without_changing_them() {
    let temporary = private_installation_tempdir();
    let installation = Installation::open(temporary.path(), None).unwrap();
    let quarantine = temporary.path().join(".cast/quarantine");
    let directory = quarantine.join("unrelated-directory");
    let file = quarantine.join("unrelated-file");
    let link = quarantine.join("unrelated-link");
    let outside = temporary.path().join("unrelated-outside");
    fs::create_dir(&directory).unwrap();
    fs::write(directory.join("payload"), b"directory-unchanged").unwrap();
    fs::write(&file, b"file-unchanged").unwrap();
    fs::write(&outside, b"outside-unchanged").unwrap();
    symlink(&outside, &link).unwrap();
    let quarantine_before = fs::symlink_metadata(&quarantine).unwrap();
    let identities = [&directory, &file, &link].map(|path| {
        let metadata = fs::symlink_metadata(path).unwrap();
        (
            metadata.dev(),
            metadata.ino(),
            metadata.mode(),
            metadata.len(),
            metadata.atime(),
            metadata.atime_nsec(),
            metadata.mtime(),
            metadata.mtime_nsec(),
            metadata.ctime(),
            metadata.ctime_nsec(),
        )
    });

    drop(Client::new("startup-gate-unrelated-quarantine", installation).unwrap());

    for (path, expected) in [&directory, &file, &link].into_iter().zip(identities) {
        let metadata = fs::symlink_metadata(path).unwrap();
        assert_eq!(
            (
                metadata.dev(),
                metadata.ino(),
                metadata.mode(),
                metadata.len(),
                metadata.atime(),
                metadata.atime_nsec(),
                metadata.mtime(),
                metadata.mtime_nsec(),
                metadata.ctime(),
                metadata.ctime_nsec(),
            ),
            expected
        );
    }
    let quarantine_after = fs::symlink_metadata(&quarantine).unwrap();
    assert_eq!(
        (
            quarantine_after.dev(),
            quarantine_after.ino(),
            quarantine_after.mode(),
            quarantine_after.len(),
            quarantine_after.atime(),
            quarantine_after.atime_nsec(),
            quarantine_after.mtime(),
            quarantine_after.mtime_nsec(),
            quarantine_after.ctime(),
            quarantine_after.ctime_nsec(),
        ),
        (
            quarantine_before.dev(),
            quarantine_before.ino(),
            quarantine_before.mode(),
            quarantine_before.len(),
            quarantine_before.atime(),
            quarantine_before.atime_nsec(),
            quarantine_before.mtime(),
            quarantine_before.mtime_nsec(),
            quarantine_before.ctime(),
            quarantine_before.ctime_nsec(),
        )
    );
    assert_eq!(fs::read(directory.join("payload")).unwrap(), b"directory-unchanged");
    assert_eq!(fs::read(file).unwrap(), b"file-unchanged");
    assert_eq!(fs::read_link(link).unwrap(), outside);
    assert_eq!(fs::read(outside).unwrap(), b"outside-unchanged");
}

#[test]
fn archived_state_prune_residue_audit_rejects_root_or_quarantine_substitution() {
    for replace_root in [false, true] {
        let parent = tempfile::tempdir().unwrap();
        prepare_private_installation_root(parent.path());
        let root = parent.path().join("installation");
        fs::create_dir(&root).unwrap();
        prepare_private_installation_root(&root);
        let installation = Installation::open(&root, None).unwrap();
        let original_quarantine = root.join(".cast/quarantine");
        let detached = parent.path().join(if replace_root {
            "detached-installation"
        } else {
            "detached-quarantine"
        });
        let hook_root = root.clone();
        let hook_detached = detached.clone();
        crate::transition_identity::arm_after_archived_state_prune_residue_first_scan(move || {
            if replace_root {
                fs::rename(&hook_root, &hook_detached).unwrap();
                fs::create_dir(&hook_root).unwrap();
                prepare_private_installation_root(&hook_root);
            } else {
                fs::rename(hook_root.join(".cast/quarantine"), &hook_detached).unwrap();
                fs::create_dir(hook_root.join(".cast/quarantine")).unwrap();
                fs::set_permissions(hook_root.join(".cast/quarantine"), fs::Permissions::from_mode(0o700)).unwrap();
            }
        });

        let error = expect_startup_gate_error(Client::new(
            format!("startup-gate-prune-audit-substitution-{replace_root}"),
            installation,
        ));

        assert!(matches!(
            error.as_ref(),
            startup_gate::Error::ArchivedStatePruneResidue(
                ArchivedStatePruneResidueError::Namespace(_) | ArchivedStatePruneResidueError::Installation(_)
            )
        ));
        if replace_root {
            assert!(detached.join(".cast/quarantine").is_dir());
            assert!(root.is_dir());
        } else {
            assert!(detached.is_dir());
            assert!(original_quarantine.is_dir());
        }
    }
}

#[test]
fn clean_startup_loads_the_default_intent_only_after_strict_discovery() {
    let temporary = private_installation_tempdir();
    let intent_path = write_system_intent(temporary.path(), "default-loaded");
    let installation = Installation::open(temporary.path(), None).unwrap();
    assert!(installation.system_model.is_none());

    let client = Client::new("startup-gate-default-intent", installation).unwrap();
    let loaded = client.installation.system_model.as_ref().unwrap();

    assert_eq!(loaded.path(), intent_path);
    assert!(loaded.packages.contains(&Provider::package_name("default-loaded")));
}

#[test]
fn explicit_intent_remains_authoritative_without_loading_the_malformed_default() {
    let temporary = private_installation_tempdir();
    let (default_path, malformed_default) = write_malformed_system_intent(temporary.path());
    let explicit = temporary.path().join("explicit-system.glu");
    fs::write(
        &explicit,
        r#"let cast = import! cast.system.v1
{
    disable_warning = cast.boolean.true,
    packages = ["explicit-loaded"],
    .. cast.system
}
"#,
    )
    .unwrap();
    let installation = Installation::open(temporary.path(), None).unwrap();

    let client = Client::builder("startup-gate-explicit-intent", installation)
        .system_intent_path(&explicit)
        .build()
        .unwrap();
    let loaded = client.installation.system_model.as_ref().unwrap();

    assert_eq!(loaded.path(), explicit);
    assert!(loaded.packages.contains(&Provider::package_name("explicit-loaded")));
    assert_eq!(fs::read(default_path).unwrap(), malformed_default);
}

#[test]
fn cli_notice_preserves_full_verbose_and_failed_startup_semantics() {
    let full_root = private_installation_tempdir();
    let full_path = write_system_intent_with_warning(full_root.path(), "full-notice", false);
    let full_capture = Rc::new(RefCell::new(None));
    let sink = Rc::clone(&full_capture);
    arm_system_intent_notice_capture(move |notice| *sink.borrow_mut() = Some(notice));
    let full_client = Client::builder(
        "startup-gate-full-notice",
        Installation::open(full_root.path(), None).unwrap(),
    )
    .system_intent_notice(false)
    .build()
    .unwrap();
    assert!(full_client.system_intent().is_some());
    assert!(!disarm_system_intent_notice_capture());
    let full_notice = full_capture.borrow_mut().take().unwrap();
    assert!(full_notice.contains(&format!("{full_path:?} is active")));
    assert!(full_notice.contains("Hence:"));
    assert!(full_notice.contains("cast sync"));

    let verbose_root = private_installation_tempdir();
    let verbose_path = write_system_intent(verbose_root.path(), "verbose-first-line");
    let verbose_capture = Rc::new(RefCell::new(None));
    let sink = Rc::clone(&verbose_capture);
    arm_system_intent_notice_capture(move |notice| *sink.borrow_mut() = Some(notice));
    Client::builder(
        "startup-gate-verbose-notice",
        Installation::open(verbose_root.path(), None).unwrap(),
    )
    .system_intent_notice(true)
    .build()
    .unwrap();
    assert!(!disarm_system_intent_notice_capture());
    let verbose_notice = verbose_capture.borrow_mut().take().unwrap();
    assert!(verbose_notice.contains(&format!("{verbose_path:?} is active")));
    assert!(!verbose_notice.contains("Hence:"));
    assert_eq!(verbose_notice.lines().count(), 1);

    let failed_root = private_installation_tempdir();
    write_system_intent_with_warning(failed_root.path(), "must-not-notice", false);
    let installation = Installation::open(failed_root.path(), None).unwrap();
    create_journal(&installation);
    let failed_capture = Rc::new(RefCell::new(None));
    let sink = Rc::clone(&failed_capture);
    arm_system_intent_notice_capture(move |notice| *sink.borrow_mut() = Some(notice));
    expect_startup_gate_error(
        Client::builder("startup-gate-failed-notice", installation)
            .system_intent_notice(true)
            .build(),
    );
    assert!(disarm_system_intent_notice_capture());
    assert!(failed_capture.borrow().is_none());
}

#[test]
fn unsafe_symlink_and_hardlinked_default_sources_fail_unchanged() {
    for kind in ["source-mode", "directory-mode", "symlink", "hardlink"] {
        let temporary = private_installation_tempdir();
        let canonical = write_system_intent(temporary.path(), "must-not-load");
        let cast = canonical.parent().unwrap().to_owned();
        let original = fs::read(&canonical).unwrap();
        let other = cast.join("other.glu");

        match kind {
            "source-mode" => fs::set_permissions(&canonical, fs::Permissions::from_mode(0o666)).unwrap(),
            "directory-mode" => fs::set_permissions(&cast, fs::Permissions::from_mode(0o777)).unwrap(),
            "symlink" => {
                fs::rename(&canonical, &other).unwrap();
                symlink("other.glu", &canonical).unwrap();
            }
            "hardlink" => {
                fs::rename(&canonical, &other).unwrap();
                fs::hard_link(&other, &canonical).unwrap();
            }
            _ => unreachable!(),
        }
        let before = fs::symlink_metadata(&canonical).unwrap();
        let installation = Installation::open(temporary.path(), None).unwrap();
        let error = expect_startup_gate_error(Client::new(format!("startup-gate-unsafe-default-{kind}"), installation));

        assert!(matches!(error.as_ref(), startup_gate::Error::DefaultSystemIntent(_)));
        let after = fs::symlink_metadata(&canonical).unwrap();
        assert_eq!((after.dev(), after.ino()), (before.dev(), before.ino()), "{kind}");
        if kind == "symlink" {
            assert_eq!(fs::read_link(&canonical).unwrap(), Path::new("other.glu"));
            assert_eq!(fs::read(&other).unwrap(), original);
        } else {
            assert_eq!(fs::read(&canonical).unwrap(), original);
        }
    }
}

#[test]
fn default_source_substitution_after_retention_fails_closed() {
    let temporary = private_installation_tempdir();
    let canonical = write_system_intent(temporary.path(), "retained-source");
    let retained = canonical.with_file_name("retained-system.glu");
    let original = fs::read(&canonical).unwrap();
    let hook_canonical = canonical.clone();
    let hook_retained = retained.clone();
    crate::system_model::arm_after_rooted_system_source_retained(move || {
        fs::rename(&hook_canonical, &hook_retained).unwrap();
        fs::write(
            &hook_canonical,
            r#"let cast = import! cast.system.v1
{
    disable_warning = cast.boolean.true,
    packages = ["replacement-injected"],
    .. cast.system
}
"#,
        )
        .unwrap();
        fs::set_permissions(&hook_canonical, fs::Permissions::from_mode(0o644)).unwrap();
    });

    let installation = Installation::open(temporary.path(), None).unwrap();
    let error = expect_startup_gate_error(Client::new("startup-gate-source-substitution", installation));

    assert!(matches!(error.as_ref(), startup_gate::Error::DefaultSystemIntent(_)));
    assert_eq!(fs::read(retained).unwrap(), original);
    assert!(fs::read_to_string(canonical).unwrap().contains("replacement-injected"));
}

#[test]
fn default_intent_root_and_directory_name_substitution_fail_closed() {
    for replace_root in [true, false] {
        let parent = tempfile::tempdir().unwrap();
        prepare_private_installation_root(parent.path());
        let root = parent.path().join("installation");
        fs::create_dir(&root).unwrap();
        prepare_private_installation_root(&root);
        let original = write_system_intent(&root, "retained-original");
        let original_bytes = fs::read(&original).unwrap();
        let installation = Installation::open(&root, None).unwrap();
        let detached = parent.path().join(if replace_root {
            "detached-installation"
        } else {
            "detached-cast"
        });
        let hook_root = root.clone();
        let injected = root.join("etc/cast/system.glu");

        startup_gate::arm_after_default_directory_retained(move || {
            if replace_root {
                fs::rename(&hook_root, &detached).unwrap();
                fs::create_dir(&hook_root).unwrap();
                prepare_private_installation_root(&hook_root);
                write_system_intent(&hook_root, "replacement-injected");
            } else {
                fs::rename(hook_root.join("etc/cast"), &detached).unwrap();
                write_system_intent(&hook_root, "replacement-injected");
            }
        });

        let error = expect_startup_gate_error(Client::new(
            format!("startup-gate-default-substitution-{replace_root}"),
            installation,
        ));
        assert!(matches!(error.as_ref(), startup_gate::Error::DefaultSystemIntent(_)));
        assert!(fs::read_to_string(&injected).unwrap().contains("replacement-injected"));
        let retained_source = if replace_root {
            parent.path().join("detached-installation/etc/cast/system.glu")
        } else {
            parent.path().join("detached-cast/system.glu")
        };
        assert_eq!(fs::read(retained_source).unwrap(), original_bytes);
    }
}

#[test]
fn frozen_client_ignores_system_journal_and_persistent_transition_rows() {
    let temporary = private_installation_tempdir();
    let (intent_path, malformed_intent) = write_malformed_system_intent(temporary.path());
    let installation = Installation::open(temporary.path(), None).unwrap();
    let state_db = crate::db::state::Database::new(installation.db_path("state").to_str().unwrap()).unwrap();
    state_db
        .add_with_transition(&transition_id(), &[], Some("system-only orphan"), None)
        .unwrap();
    drop(state_db);
    let canonical_before = create_journal(&installation);
    let residue = prune_residue(temporary.path(), 90);
    fs::create_dir(&residue).unwrap();
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
    assert!(residue.is_dir());
    assert_eq!(fs::read(intent_path).unwrap(), malformed_intent);
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
