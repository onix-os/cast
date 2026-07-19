use std::{ffi::CString, fs, io};

use super::super::super::mount_namespace::{
    FIXTURE_MOUNT_NAMESPACE_TYPE, FIXTURE_NSFS_MAGIC, FixtureMountNamespaceTree,
    validate_fixture_namespace_authentication,
};
use super::support::{FixtureEntry, SyntheticMountNamespace};

fn prepare_fixture(fixture: &SyntheticMountNamespace) -> io::Result<()> {
    let (parent, tree_name) = fixture.admission()?;
    FixtureMountNamespaceTree::admit(parent, tree_name)?.prepare().map(drop)
}

fn assert_rejected(result: io::Result<()>) {
    let error = result.expect_err("malformed fixture unexpectedly produced a namespace anchor");
    assert_ne!(error.kind(), io::ErrorKind::TimedOut);
}

#[test]
fn fixture_admission_requires_one_safe_named_ordinary_tree() {
    let fixture = SyntheticMountNamespace::stable().unwrap();
    let (parent, _) = fixture.admission().unwrap();
    for name in ["", ".", "..", "nested/component"] {
        assert_rejected(
            FixtureMountNamespaceTree::admit(
                parent.try_clone().unwrap(),
                CString::new(name).expect("fixed malformed component contains no NUL"),
            )
            .and_then(|tree| tree.prepare())
            .map(drop),
        );
    }

    let regular_parent = fs::File::open(fixture.entry(FixtureEntry::NamespaceMarker)).unwrap();
    let (_, tree_name) = fixture.admission().unwrap();
    assert_rejected(
        FixtureMountNamespaceTree::admit(regular_parent, tree_name)
            .and_then(|tree| tree.prepare())
            .map(drop),
    );
    fixture.assert_outside_unchanged();
}

#[test]
fn namespace_authentication_rejects_wrong_filesystem_magic_and_namespace_type() {
    validate_fixture_namespace_authentication(FIXTURE_NSFS_MAGIC, FIXTURE_MOUNT_NAMESPACE_TYPE).unwrap();

    let wrong_magic =
        validate_fixture_namespace_authentication(FIXTURE_NSFS_MAGIC ^ 1, FIXTURE_MOUNT_NAMESPACE_TYPE).unwrap_err();
    assert_eq!(wrong_magic.kind(), io::ErrorKind::InvalidData);

    let wrong_type =
        validate_fixture_namespace_authentication(FIXTURE_NSFS_MAGIC, FIXTURE_MOUNT_NAMESPACE_TYPE ^ 1).unwrap_err();
    assert_eq!(wrong_type.kind(), io::ErrorKind::InvalidData);

    for (magic, namespace_type) in [(0, FIXTURE_MOUNT_NAMESPACE_TYPE), (FIXTURE_NSFS_MAGIC, 0)] {
        assert_eq!(
            validate_fixture_namespace_authentication(magic, namespace_type)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }
}

#[test]
fn every_required_fixture_entry_must_exist() {
    for entry in [
        FixtureEntry::Tree,
        FixtureEntry::NamespaceDirectory,
        FixtureEntry::NamespaceMarker,
        FixtureEntry::TaskRoot,
    ] {
        let fixture = SyntheticMountNamespace::stable().unwrap();
        fixture.remove(entry).unwrap();
        assert_rejected(prepare_fixture(&fixture));
        fixture.assert_outside_unchanged();
    }
}

#[test]
fn symlink_entries_are_never_followed() {
    for entry in [
        FixtureEntry::Tree,
        FixtureEntry::NamespaceDirectory,
        FixtureEntry::NamespaceMarker,
        FixtureEntry::TaskRoot,
    ] {
        let fixture = SyntheticMountNamespace::stable().unwrap();
        fixture.replace_symlink(entry).unwrap();
        assert_rejected(prepare_fixture(&fixture));
        fixture.assert_outside_unchanged();
    }
}

#[test]
fn fifo_and_wrong_entry_kinds_fail_without_opening_special_files() {
    for entry in [
        FixtureEntry::Tree,
        FixtureEntry::NamespaceDirectory,
        FixtureEntry::NamespaceMarker,
        FixtureEntry::TaskRoot,
    ] {
        let fixture = SyntheticMountNamespace::stable().unwrap();
        fixture.replace_fifo(entry).unwrap();
        assert_rejected(prepare_fixture(&fixture));
        fixture.assert_outside_unchanged();
    }

    for entry in [
        FixtureEntry::Tree,
        FixtureEntry::NamespaceDirectory,
        FixtureEntry::TaskRoot,
    ] {
        let fixture = SyntheticMountNamespace::stable().unwrap();
        fixture.replace_regular(entry, b"not a directory\n").unwrap();
        assert_rejected(prepare_fixture(&fixture));
        fixture.assert_outside_unchanged();
    }

    let marker_directory = SyntheticMountNamespace::stable().unwrap();
    marker_directory
        .replace_directory(FixtureEntry::NamespaceMarker)
        .unwrap();
    assert_rejected(prepare_fixture(&marker_directory));
    marker_directory.assert_outside_unchanged();
}
