use std::{
    ffi::OsString,
    os::unix::fs::{MetadataExt as _, PermissionsExt as _, symlink},
    path::Path,
};

use fs_err as fs;

use crate::{Installation, installation, test_support::private_installation_tempdir};

use super::{Client, Error, arm_after_system_database_open, startup_gate};

const PRIVATE_DIRECTORY_MODE: u32 = 0o700;

fn create_private_directory(path: &Path) {
    fs::create_dir(path).unwrap();
    fs::set_permissions(path, std::fs::Permissions::from_mode(PRIVATE_DIRECTORY_MODE)).unwrap();
}

fn directory_names(path: &Path) -> Vec<OsString> {
    let mut names = fs::read_dir(path)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    names.sort();
    names
}

fn expect_installation_error(result: Result<Client, Error>) -> installation::Error {
    match result {
        Err(Error::Installation(source)) => source,
        Err(other) => panic!("expected mutable namespace error, got {other}"),
        Ok(_) => panic!("substituted mutable namespace unexpectedly built a client"),
    }
}

#[test]
fn every_system_database_open_is_anchored_and_replacement_directories_remain_untouched() {
    for (label, kind, opened) in [
        ("install", installation::DatabaseKind::Install, &["install"][..]),
        ("state", installation::DatabaseKind::State, &["install", "state"][..]),
        (
            "layout",
            installation::DatabaseKind::Layout,
            &["install", "state", "layout"][..],
        ),
    ] {
        let temporary = private_installation_tempdir();
        let installation = Installation::open(temporary.path(), None).unwrap();
        let database = installation.db_path("");
        let detached = temporary.path().join(format!("detached-database-{label}"));
        let replacement_marker = database.join("foreign-replacement");
        let hook_database = database.clone();
        let hook_detached = detached.clone();
        let hook_marker = replacement_marker.clone();
        arm_after_system_database_open(kind, move || {
            fs::rename(&hook_database, &hook_detached).unwrap();
            create_private_directory(&hook_database);
            fs::write(&hook_marker, b"replacement must remain untouched").unwrap();
        });

        let error = expect_installation_error(Client::new(format!("mutable-database-{label}"), installation));
        assert!(matches!(
            error,
            installation::Error::PrepareDirectory { path, .. } if path == database
        ));

        assert_eq!(
            directory_names(&database),
            vec![OsString::from("foreign-replacement")],
            "{label} replacement received a filesystem mutation"
        );
        assert_eq!(
            fs::read(&replacement_marker).unwrap(),
            b"replacement must remain untouched"
        );
        for name in opened {
            assert!(detached.join(name).is_file(), "{label} did not open anchored {name}");
            assert!(!database.join(name).exists(), "{label} mutated replacement {name}");
        }
        assert!(!temporary.path().join(".cast/journal").exists());
    }
}

#[test]
fn namespace_revalidation_supersedes_a_simultaneous_sqlite_open_failure() {
    let temporary = private_installation_tempdir();
    let installation = Installation::open(temporary.path(), None).unwrap();
    let database = installation.db_path("");
    fs::write(database.join("install"), b"deliberately not a SQLite database").unwrap();
    let detached = temporary.path().join("detached-corrupt-database");
    let replacement_marker = database.join("foreign-replacement");
    let hook_database = database.clone();
    let hook_detached = detached.clone();
    let hook_marker = replacement_marker.clone();
    arm_after_system_database_open(installation::DatabaseKind::Install, move || {
        fs::rename(&hook_database, &hook_detached).unwrap();
        create_private_directory(&hook_database);
        fs::write(&hook_marker, b"replacement must remain untouched").unwrap();
    });

    let error = expect_installation_error(Client::new("mutable-database-error-precedence", installation));
    assert!(matches!(
        error,
        installation::Error::PrepareDirectory { path, .. } if path == database
    ));
    assert_eq!(directory_names(&database), vec![OsString::from("foreign-replacement")]);
    assert_eq!(
        fs::read(&replacement_marker).unwrap(),
        b"replacement must remain untouched"
    );
    assert!(!database.join("install").exists());
    assert!(!temporary.path().join(".cast/journal").exists());
}

#[test]
fn startup_journal_uses_retained_cast_and_never_mutates_its_replacement() {
    let temporary = private_installation_tempdir();
    let installation = Installation::open(temporary.path(), None).unwrap();
    let cast = temporary.path().join(".cast");
    let detached = temporary.path().join("detached-cast");
    let replacement_marker = cast.join("foreign-replacement");
    let hook_cast = cast.clone();
    let hook_detached = detached.clone();
    let hook_marker = replacement_marker.clone();
    startup_gate::arm_after_mutable_namespace_preflight(move || {
        fs::rename(&hook_cast, &hook_detached).unwrap();
        create_private_directory(&hook_cast);
        fs::write(&hook_marker, b"replacement must remain untouched").unwrap();
    });

    let error = expect_installation_error(Client::new("mutable-cast-journal", installation));
    assert!(matches!(
        error,
        installation::Error::PrepareDirectory { path, .. } if path == cast
    ));

    assert_eq!(directory_names(&cast), vec![OsString::from("foreign-replacement")]);
    assert_eq!(
        fs::read(&replacement_marker).unwrap(),
        b"replacement must remain untouched"
    );
    assert!(!cast.join("db").exists());
    assert!(!cast.join("journal").exists());
    for name in ["install", "state", "layout"] {
        assert!(detached.join("db").join(name).is_file(), "missing anchored {name}");
    }
    assert!(detached.join("journal/state-transition.lock").is_file());
}

#[test]
fn startup_namespace_substitution_supersedes_a_simultaneous_journal_open_failure() {
    let temporary = private_installation_tempdir();
    let installation = Installation::open(temporary.path(), None).unwrap();
    let cast = temporary.path().join(".cast");
    let detached = temporary.path().join("detached-cast-with-invalid-journal");
    let external = temporary.path().join("external-journal-target");
    create_private_directory(&external);
    let external_marker = external.join("must-remain-untouched");
    fs::write(&external_marker, b"foreign journal target").unwrap();
    symlink(&external, cast.join("journal")).unwrap();

    let replacement_marker = cast.join("foreign-replacement");
    let hook_cast = cast.clone();
    let hook_detached = detached.clone();
    let hook_marker = replacement_marker.clone();
    startup_gate::arm_after_mutable_namespace_preflight(move || {
        fs::rename(&hook_cast, &hook_detached).unwrap();
        create_private_directory(&hook_cast);
        fs::write(&hook_marker, b"replacement must remain untouched").unwrap();
    });

    let error = expect_installation_error(Client::new("mutable-cast-journal-error-precedence", installation));
    assert!(matches!(
        error,
        installation::Error::PrepareDirectory { path, .. } if path == cast
    ));

    assert_eq!(directory_names(&cast), vec![OsString::from("foreign-replacement")]);
    assert_eq!(
        fs::read(&replacement_marker).unwrap(),
        b"replacement must remain untouched"
    );
    assert!(
        fs::symlink_metadata(detached.join("journal"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read(&external_marker).unwrap(), b"foreign journal target");
}

#[test]
fn replaced_global_lockfile_is_rejected_without_touching_the_foreign_inode() {
    let temporary = private_installation_tempdir();
    let installation = Installation::open(temporary.path(), None).unwrap();
    let lock = temporary.path().join(".cast/.cast-lockfile");
    let detached = temporary.path().join("detached-global-lock");
    let hook_lock = lock.clone();
    let hook_detached = detached.clone();
    arm_after_system_database_open(installation::DatabaseKind::Install, move || {
        fs::rename(&hook_lock, &hook_detached).unwrap();
        fs::write(&hook_lock, b"foreign lock evidence").unwrap();
        fs::set_permissions(&hook_lock, std::fs::Permissions::from_mode(0o600)).unwrap();
    });

    let error = expect_installation_error(Client::new("mutable-lockfile", installation));
    assert!(matches!(
        error,
        installation::Error::PrepareLockfile { path, .. } if path == lock
    ));

    assert_eq!(fs::read(&lock).unwrap(), b"foreign lock evidence");
    assert_ne!(
        fs::metadata(&lock).unwrap().ino(),
        fs::metadata(&detached).unwrap().ino()
    );
    assert_eq!(fs::metadata(&lock).unwrap().permissions().mode() & 0o7777, 0o600);
    assert!(!temporary.path().join(".cast/journal").exists());
}
