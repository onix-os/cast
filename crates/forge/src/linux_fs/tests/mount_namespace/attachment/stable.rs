use std::io;

use super::super::super::super::mount_namespace::{FixtureMountNamespaceTree, PreparedMountNamespaceAnchor};
use super::super::support::{FixtureEntry, SyntheticMountNamespace};

const NESTED_COMPONENTS: &[&str] = &["alpha", "bravo", "charlie"];
const SINGLE_COMPONENT: &str = concat!("bo", "ot");

fn selector(components: &[&str]) -> String {
    format!("/{}", components.join("/"))
}

fn prepared_anchor(fixture: &SyntheticMountNamespace) -> io::Result<PreparedMountNamespaceAnchor> {
    let (parent, tree_name) = fixture.admission()?;
    FixtureMountNamespaceTree::admit(parent, tree_name)?.prepare()
}

#[test]
fn stable_nested_selector_retains_exact_raw_chain_and_destination_identity() {
    let fixture = SyntheticMountNamespace::with_attachment(NESTED_COMPONENTS).unwrap();
    let anchor = prepared_anchor(&fixture).unwrap();
    let anchor_view = anchor.revalidate().unwrap();
    let authored = selector(NESTED_COMPONENTS);
    let prepared = anchor_view.prepare_task_rooted_attachment(&authored).unwrap();
    let view = prepared.revalidate_against(&anchor).unwrap();

    assert_eq!(view.selector(), authored);
    assert_eq!(view.component_count(), NESTED_COMPONENTS.len());
    for index in 0..NESTED_COMPONENTS.len() {
        let expected = fixture.attachment_identity(NESTED_COMPONENTS, index).unwrap();
        let witness = view.fixture_component_witness(index).unwrap();
        assert_eq!((witness.0, witness.1), expected);
        assert_eq!(witness.2, nix::libc::S_IFDIR);
        assert_eq!(witness.3, witness.1);
    }
    let destination = fixture
        .attachment_identity(NESTED_COMPONENTS, NESTED_COMPONENTS.len() - 1)
        .unwrap();
    assert_eq!((view.destination_device(), view.destination_inode()), destination);
    assert_eq!(view.destination_mount_id(), destination.1);
    fixture.assert_outside_unchanged();
}

#[test]
fn one_component_selector_uses_task_root_as_its_final_parent() {
    let components = &[SINGLE_COMPONENT];
    let fixture = SyntheticMountNamespace::with_attachment(components).unwrap();
    let anchor = prepared_anchor(&fixture).unwrap();
    let authored = selector(components);
    let prepared = anchor
        .revalidate()
        .unwrap()
        .prepare_task_rooted_attachment(&authored)
        .unwrap();
    let view = prepared.revalidate_against(&anchor).unwrap();

    assert_eq!(view.selector(), authored);
    assert_eq!(view.component_count(), 1);
    assert_eq!(
        (view.destination_device(), view.destination_inode()),
        fixture.attachment_identity(components, 0).unwrap()
    );
    fixture.assert_outside_unchanged();
}

#[test]
fn attachment_revalidation_rejects_a_different_mount_context_anchor() {
    let first = SyntheticMountNamespace::with_attachment(NESTED_COMPONENTS).unwrap();
    let second = SyntheticMountNamespace::with_attachment(NESTED_COMPONENTS).unwrap();
    let first_anchor = prepared_anchor(&first).unwrap();
    let second_anchor = prepared_anchor(&second).unwrap();
    let authored = selector(NESTED_COMPONENTS);
    let attachment = first_anchor
        .revalidate()
        .unwrap()
        .prepare_task_rooted_attachment(&authored)
        .unwrap();

    assert!(attachment.revalidate_against(&second_anchor).is_err());
    assert_ne!(
        first.identity(FixtureEntry::TaskRoot).unwrap(),
        second.identity(FixtureEntry::TaskRoot).unwrap()
    );
    first.assert_outside_unchanged();
    second.assert_outside_unchanged();
}

#[test]
fn independently_prepared_anchor_with_the_same_authenticated_snapshot_is_accepted() {
    let fixture = SyntheticMountNamespace::with_attachment(NESTED_COMPONENTS).unwrap();
    let first_anchor = prepared_anchor(&fixture).unwrap();
    let equivalent_anchor = prepared_anchor(&fixture).unwrap();
    let authored = selector(NESTED_COMPONENTS);
    let attachment = first_anchor
        .revalidate()
        .unwrap()
        .prepare_task_rooted_attachment(&authored)
        .unwrap();

    let view = attachment.revalidate_against(&equivalent_anchor).unwrap();
    assert_eq!(view.selector(), authored);
    assert_eq!(view.component_count(), NESTED_COMPONENTS.len());
    fixture.assert_outside_unchanged();
}
