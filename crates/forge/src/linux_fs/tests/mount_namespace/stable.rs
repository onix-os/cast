use std::io;

use super::super::super::mount_namespace::{FixtureMountNamespaceTree, RevalidatedMountNamespaceAnchor};
use super::support::{FixtureEntry, SyntheticMountNamespace};

fn admitted(fixture: &SyntheticMountNamespace) -> io::Result<FixtureMountNamespaceTree> {
    let (parent, tree_name) = fixture.admission()?;
    FixtureMountNamespaceTree::admit(parent, tree_name)
}

fn assert_exact_identities(view: &RevalidatedMountNamespaceAnchor<'_>, namespace: (u64, u64), task_root: (u64, u64)) {
    assert_eq!(view.mount_namespace_device(), namespace.0);
    assert_eq!(view.mount_namespace_inode(), namespace.1);
    assert_eq!(view.task_root_device(), task_root.0);
    assert_eq!(view.task_root_inode(), task_root.1);
    assert_ne!(view.task_root_mount_id(), 0);
}

#[test]
fn stable_fixture_prepares_and_revalidates_exact_capability_identities() {
    let fixture = SyntheticMountNamespace::stable().unwrap();
    let namespace = fixture.identity(FixtureEntry::NamespaceMarker).unwrap();
    let task_root = fixture.identity(FixtureEntry::TaskRoot).unwrap();

    let prepared = admitted(&fixture).unwrap().prepare().unwrap();
    let view = prepared.revalidate().unwrap();

    assert_exact_identities(&view, namespace, task_root);
    fixture.assert_outside_unchanged();
}

#[test]
fn namespace_marker_contents_are_not_semantic_authority() {
    let fixture = SyntheticMountNamespace::stable().unwrap();
    let namespace = fixture.identity(FixtureEntry::NamespaceMarker).unwrap();
    let task_root = fixture.identity(FixtureEntry::TaskRoot).unwrap();
    let prepared = admitted(&fixture).unwrap().prepare().unwrap();

    fixture
        .overwrite_marker_contents(b"changed bytes on the same retained marker inode\n")
        .unwrap();
    let view = prepared.revalidate().unwrap();

    assert_exact_identities(&view, namespace, task_root);
    assert_eq!(fixture.identity(FixtureEntry::NamespaceMarker).unwrap(), namespace);
    fixture.assert_outside_unchanged();
}

#[test]
fn distinct_fixtures_retain_distinct_marker_and_root_identities() {
    let first = SyntheticMountNamespace::stable().unwrap();
    let second = SyntheticMountNamespace::stable().unwrap();
    let first_prepared = admitted(&first).unwrap().prepare().unwrap();
    let second_prepared = admitted(&second).unwrap().prepare().unwrap();

    let first_view = first_prepared.revalidate().unwrap();
    let second_view = second_prepared.revalidate().unwrap();

    assert_ne!(
        (first_view.mount_namespace_device(), first_view.mount_namespace_inode()),
        (
            second_view.mount_namespace_device(),
            second_view.mount_namespace_inode()
        )
    );
    assert_ne!(
        (first_view.task_root_device(), first_view.task_root_inode()),
        (second_view.task_root_device(), second_view.task_root_inode())
    );
    first.assert_outside_unchanged();
    second.assert_outside_unchanged();
}
