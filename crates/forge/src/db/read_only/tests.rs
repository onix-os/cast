use std::{
    collections::BTreeSet,
    fs,
    io::Read as _,
    os::unix::fs::{FileExt as _, MetadataExt as _, OpenOptionsExt as _, symlink},
    path::{Path, PathBuf},
};

use diesel::{Connection as _, SqliteConnection, connection::SimpleConnection as _};
use stone::{StonePayloadLayoutFile, StonePayloadLayoutRecord};

use crate::{
    Installation, db,
    installation::{self, DatabaseKind},
    package::{self, Meta, Name},
    state::Selection,
    test_support::private_installation_tempdir,
};

use super::{MAX_DATABASE_IMAGE_BYTES, ReadOnlyConnection, ReadOnlyError, Step};

struct Fixture {
    _temporary: tempfile::TempDir,
    root: PathBuf,
    package: package::Id,
    state: crate::State,
    layout: StonePayloadLayoutRecord,
}

fn fixture() -> Fixture {
    let temporary = private_installation_tempdir();
    let root = temporary.path().to_owned();
    let installation = Installation::open(&root, None).unwrap();
    let meta_database = db::meta::Database::new(installation.db_path("install").to_str().unwrap()).unwrap();
    let state_database = db::state::Database::new(installation.db_path("state").to_str().unwrap()).unwrap();
    let layout_database = db::layout::Database::new(installation.db_path("layout").to_str().unwrap()).unwrap();

    let meta = Meta {
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
    let package: package::Id = meta.id().into();
    meta_database.add(package.clone(), meta).unwrap();
    let state = state_database
        .add(
            &[Selection::explicit(package.clone())],
            Some("selected alpha"),
            Some("read-only fixture"),
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

    drop(layout_database);
    drop(state_database);
    drop(meta_database);
    drop(installation);
    Fixture {
        _temporary: temporary,
        root,
        package,
        state,
        layout,
    }
}

fn snapshot_installation(fixture: &Fixture) -> Installation {
    Installation::open_read_only(&fixture.root, None).unwrap()
}

fn state_connection(fixture: &Fixture) -> (Installation, ReadOnlyConnection) {
    let installation = snapshot_installation(fixture);
    let connection = ReadOnlyConnection::open(&installation, DatabaseKind::State).unwrap();
    (installation, connection)
}

fn assert_scalar(connection: &ReadOnlyConnection) {
    let value = connection
        .snapshot(|row| {
            let mut statement = row.prepare(c"SELECT 17")?;
            assert_eq!(statement.step()?, Step::Row);
            let value = statement.i64(0)?;
            assert_eq!(statement.step()?, Step::Done);
            Ok(value)
        })
        .unwrap();
    assert_eq!(value, 17);
}

#[derive(Debug, Eq, PartialEq)]
struct FileSnapshot {
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
    bytes: Vec<u8>,
}

fn file_snapshot(path: &Path) -> FileSnapshot {
    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOATIME | nix::libc::O_CLOEXEC)
        .open(path)
        .unwrap();
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).unwrap();
    let metadata = file.metadata().unwrap();
    FileSnapshot {
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

fn directory_names(path: &Path) -> Vec<String> {
    let mut names = fs::read_dir(path)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect::<Vec<_>>();
    names.sort();
    names
}

fn sparse_witness(path: &Path) -> (u64, i64, i64, i64, i64, Vec<String>, [u8; 2]) {
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOATIME | nix::libc::O_CLOEXEC)
        .open(path)
        .unwrap();
    let metadata = file.metadata().unwrap();
    let mut edges = [0_u8; 2];
    file.read_exact_at(&mut edges[..1], 0).unwrap();
    file.read_exact_at(&mut edges[1..], metadata.len() - 1).unwrap();
    (
        metadata.len(),
        metadata.atime(),
        metadata.atime_nsec(),
        metadata.mtime(),
        metadata.ctime(),
        directory_names(path.parent().unwrap()),
        edges,
    )
}

#[test]
fn deserialized_adapters_query_exact_state_meta_and_selected_layout_without_mutation() {
    let fixture = fixture();
    let state_path = fixture.root.join(".cast/db/state");
    age_atime(&state_path);
    let before_file = file_snapshot(&state_path);
    let before_names = directory_names(state_path.parent().unwrap());
    let installation = snapshot_installation(&fixture);

    let state = db::state::ReadOnlyDatabase::open(&installation).unwrap();
    let meta = db::meta::ReadOnlyDatabase::open(&installation).unwrap();
    let layout = db::layout::ReadOnlyDatabase::open(&installation).unwrap();
    assert_eq!(state.list_ids().unwrap(), [fixture.state.id]);
    assert_eq!(state.get(fixture.state.id).unwrap(), Some(fixture.state.clone()));
    assert_eq!(
        meta.get(&fixture.package).unwrap().unwrap().id(),
        fixture.package.clone().into()
    );
    assert_eq!(
        layout.selected(std::slice::from_ref(&fixture.package)).unwrap(),
        [(fixture.package.clone(), fixture.layout.clone())]
    );
    state.revalidate(&installation).unwrap();
    meta.revalidate(&installation).unwrap();
    layout.revalidate(&installation).unwrap();
    drop(layout);
    drop(meta);
    drop(state);
    drop(installation);

    assert_eq!(file_snapshot(&state_path), before_file);
    assert_eq!(directory_names(state_path.parent().unwrap()), before_names);
}

#[test]
fn authorizer_denies_writes_and_functions_and_connection_remains_clean() {
    let fixture = fixture();
    let (_installation, connection) = state_connection(&fixture);

    assert!(matches!(
        connection.attempt_test_write(),
        Err(ReadOnlyError::Sqlite { status, .. }) if status & 0xff == libsqlite3_sys::SQLITE_AUTH
    ));
    assert_scalar(&connection);
    assert!(matches!(
        connection.attempt_test_function(),
        Err(ReadOnlyError::Sqlite { message, .. }) if message.contains("not authorized")
    ));
    assert_scalar(&connection);
}

#[test]
fn opcode_and_deadline_interruptions_are_deterministic_and_handlers_are_cleared() {
    let fixture = fixture();
    let (_installation, connection) = state_connection(&fixture);

    assert!(matches!(
        connection.attempt_test_opcode_exhaustion(),
        Err(ReadOnlyError::QueryInterrupted {
            reason: "finite SQLite opcode budget exhausted"
        })
    ));
    assert_scalar(&connection);
    assert!(matches!(
        connection.attempt_test_deadline_exhaustion(),
        Err(ReadOnlyError::QueryInterrupted {
            reason: "monotonic SQLite query deadline elapsed"
        })
    ));
    assert_scalar(&connection);
}

#[test]
fn temp_store_is_memory_only_and_ordered_scan_leaves_source_unchanged() {
    let fixture = fixture();
    let path = fixture.root.join(".cast/db/state");
    age_atime(&path);
    let before_file = file_snapshot(&path);
    let before_names = directory_names(path.parent().unwrap());
    let (_installation, connection) = state_connection(&fixture);

    let (compile_mode, runtime_mode) = connection.test_temp_store_modes().unwrap();
    assert!((0..=3).contains(&compile_mode));
    assert_eq!(runtime_mode, 2);
    connection.attempt_test_ordered_scan().unwrap();
    drop(connection);

    assert_eq!(file_snapshot(&path), before_file);
    assert_eq!(directory_names(path.parent().unwrap()), before_names);
}

#[test]
fn sidecar_inode_kinds_fail_closed_and_are_preserved() {
    for (suffix, kind) in [("-journal", "file"), ("-wal", "symlink"), ("-shm", "directory")] {
        let fixture = fixture();
        let sidecar = fixture.root.join(format!(".cast/db/state{suffix}"));
        match kind {
            "file" => fs::write(&sidecar, b"hot journal evidence").unwrap(),
            "symlink" => symlink("outside-target", &sidecar).unwrap(),
            "directory" => fs::create_dir(&sidecar).unwrap(),
            _ => unreachable!(),
        }
        let before = fs::symlink_metadata(&sidecar).unwrap();
        let installation = snapshot_installation(&fixture);
        assert!(matches!(
            ReadOnlyConnection::open(&installation, DatabaseKind::State),
            Err(ReadOnlyError::Installation(installation::Error::ReadOnlyDatabaseSidecar { path }))
                if path == sidecar
        ));
        let after = fs::symlink_metadata(&sidecar).unwrap();
        assert_eq!(
            (after.dev(), after.ino(), after.mode(), after.len()),
            (before.dev(), before.ino(), before.mode(), before.len())
        );
        match kind {
            "file" => assert_eq!(fs::read(&sidecar).unwrap(), b"hot journal evidence"),
            "symlink" => assert_eq!(fs::read_link(&sidecar).unwrap(), PathBuf::from("outside-target")),
            "directory" => assert!(fs::read_dir(&sidecar).unwrap().next().is_none()),
            _ => unreachable!(),
        }
    }
}

#[test]
fn oversized_database_image_fails_before_allocation_without_mutation() {
    let fixture = fixture();
    let path = fixture.root.join(".cast/db/state");
    fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len((MAX_DATABASE_IMAGE_BYTES + 1) as u64)
        .unwrap();
    age_atime(&path);
    let before = sparse_witness(&path);
    let installation = snapshot_installation(&fixture);

    assert!(matches!(
        ReadOnlyConnection::open(&installation, DatabaseKind::State),
        Err(ReadOnlyError::Installation(installation::Error::ReadOnlyDatabaseTooLarge {
            size,
            limit: MAX_DATABASE_IMAGE_BYTES,
            ..
        })) if size == (MAX_DATABASE_IMAGE_BYTES + 1) as u64
    ));
    assert_eq!(sparse_witness(&path), before);
}

#[test]
fn corrupt_database_image_fails_typed_without_mutation() {
    let fixture = fixture();
    let path = fixture.root.join(".cast/db/state");
    fs::write(&path, b"not a SQLite database").unwrap();
    age_atime(&path);
    let before = file_snapshot(&path);
    let installation = snapshot_installation(&fixture);

    assert!(matches!(
        ReadOnlyConnection::open(&installation, DatabaseKind::State),
        Err(ReadOnlyError::CorruptImage { database: "state", .. })
    ));
    assert_eq!(file_snapshot(&path), before);
}

fn assert_version_set_mismatch(statement: &str) {
    let fixture = fixture();
    let path = fixture.root.join(".cast/db/state");
    let mut connection = SqliteConnection::establish(path.to_str().unwrap()).unwrap();
    connection.batch_execute(statement).unwrap();
    drop(connection);
    let installation = snapshot_installation(&fixture);
    assert!(matches!(
        ReadOnlyConnection::open(&installation, DatabaseKind::State),
        Err(ReadOnlyError::MigrationSetMismatch { database: "state", .. })
    ));
}

fn assert_meta_policy(statement: &str, expected: &'static str) {
    let fixture = fixture();
    let path = fixture.root.join(".cast/db/install");
    let mut connection = SqliteConnection::establish(path.to_str().unwrap()).unwrap();
    connection.batch_execute(statement).unwrap();
    drop(connection);
    let installation = snapshot_installation(&fixture);
    let database = db::meta::ReadOnlyDatabase::open(&installation).unwrap();
    assert!(matches!(
        database.get(&fixture.package),
        Err(db::meta::ReadOnlyMetaError::Database(ReadOnlyError::Policy { context }))
            if context == expected
    ));
}

#[test]
fn metadata_reconstructed_id_and_i32_release_corruption_fail_typed() {
    assert_meta_policy(
        "UPDATE meta SET name = 'substituted' WHERE package = 'alpha-1.0-1.x86_64'",
        "stored package identifier does not match reconstructed metadata identifier",
    );
    assert_meta_policy(
        "UPDATE meta SET source_release = 2147483648 WHERE package = 'alpha-1.0-1.x86_64'",
        "invalid source release",
    );
}

#[test]
fn missing_unknown_and_extra_diesel_migrations_fail_typed() {
    assert_version_set_mismatch("DELETE FROM __diesel_schema_migrations WHERE version = '20260714000000'");
    assert_version_set_mismatch(
        "UPDATE __diesel_schema_migrations SET version = '99999999999999' WHERE version = '20260714000000'",
    );
    assert_version_set_mismatch(
        "INSERT INTO __diesel_schema_migrations(version, run_on) VALUES ('99999999999999', CURRENT_TIMESTAMP)",
    );
}

#[test]
fn absent_migration_table_is_version_set_validation_failure_not_migration() {
    let fixture = fixture();
    let path = fixture.root.join(".cast/db/state");
    let mut connection = SqliteConnection::establish(path.to_str().unwrap()).unwrap();
    connection
        .batch_execute("DROP TABLE __diesel_schema_migrations")
        .unwrap();
    drop(connection);
    let installation = snapshot_installation(&fixture);

    assert!(matches!(
        ReadOnlyConnection::open(&installation, DatabaseKind::State),
        Err(ReadOnlyError::MigrationSetValidation { database: "state", .. })
    ));
}
