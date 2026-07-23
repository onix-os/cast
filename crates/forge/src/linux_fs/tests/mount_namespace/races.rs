use std::{
    io,
    time::{Duration, Instant},
};

use super::super::super::mount_namespace::{
    FixtureMountNamespaceCheckpoint, FixtureMountNamespaceLimits, FixtureMountNamespaceTree,
};
use super::support::SyntheticMountNamespace;

fn admitted(fixture: &SyntheticMountNamespace) -> io::Result<FixtureMountNamespaceTree> {
    let (parent, tree_name) = fixture.admission()?;
    FixtureMountNamespaceTree::admit(parent, tree_name)
}

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(10)
}

fn assert_prepare_race(
    fixture: &SyntheticMountNamespace,
    mut hook: impl FnMut(FixtureMountNamespaceCheckpoint) -> io::Result<()>,
) {
    let tree = admitted(fixture).unwrap();
    let result = tree.prepare_with(FixtureMountNamespaceLimits::default(), deadline(), &mut hook);
    let error = match result {
        Ok(_) => panic!("replacement race unexpectedly produced a mount-namespace anchor"),
        Err(error) => error,
    };
    assert_ne!(error.kind(), io::ErrorKind::TimedOut);
    fixture.assert_outside_unchanged();
}

#[test]
fn first_pass_tree_namespace_marker_and_root_replacements_fail_closed() {
    let tree = SyntheticMountNamespace::stable().unwrap();
    let mut tree_seen = false;
    assert_prepare_race(&tree, |checkpoint| {
        if checkpoint == FixtureMountNamespaceCheckpoint::TreeRebind && !tree_seen {
            tree_seen = true;
            tree.replace_tree_identity()?;
        }
        Ok(())
    });
    assert!(tree_seen);

    let namespace_directory = SyntheticMountNamespace::stable().unwrap();
    let mut directory_seen = false;
    assert_prepare_race(&namespace_directory, |checkpoint| {
        if checkpoint == (FixtureMountNamespaceCheckpoint::NamespaceDirectoryPinned { pass: 1 }) && !directory_seen {
            directory_seen = true;
            namespace_directory.replace_namespace_directory_identity()?;
        }
        Ok(())
    });
    assert!(directory_seen);

    let namespace = SyntheticMountNamespace::stable().unwrap();
    let mut namespace_seen = false;
    assert_prepare_race(&namespace, |checkpoint| {
        if checkpoint == (FixtureMountNamespaceCheckpoint::NamespacePinned { pass: 1 }) && !namespace_seen {
            namespace_seen = true;
            namespace.replace_namespace_marker_identity()?;
        }
        Ok(())
    });
    assert!(namespace_seen);

    let task_root = SyntheticMountNamespace::stable().unwrap();
    let mut root_seen = false;
    assert_prepare_race(&task_root, |checkpoint| {
        if checkpoint == (FixtureMountNamespaceCheckpoint::TaskRootPinned { pass: 1 }) && !root_seen {
            root_seen = true;
            task_root.replace_task_root_identity()?;
        }
        Ok(())
    });
    assert!(root_seen);
}

#[test]
fn second_pass_descriptor_replacements_fail_closed() {
    let namespace = SyntheticMountNamespace::stable().unwrap();
    let mut namespace_seen = false;
    assert_prepare_race(&namespace, |checkpoint| {
        if checkpoint == (FixtureMountNamespaceCheckpoint::NamespacePinned { pass: 2 }) && !namespace_seen {
            namespace_seen = true;
            namespace.replace_namespace_marker_identity()?;
        }
        Ok(())
    });
    assert!(namespace_seen);

    let task_root = SyntheticMountNamespace::stable().unwrap();
    let mut root_seen = false;
    assert_prepare_race(&task_root, |checkpoint| {
        if checkpoint == (FixtureMountNamespaceCheckpoint::TaskRootPinned { pass: 2 }) && !root_seen {
            root_seen = true;
            task_root.replace_task_root_identity()?;
        }
        Ok(())
    });
    assert!(root_seen);

    let closing_namespace = SyntheticMountNamespace::stable().unwrap();
    let mut namespace_recheck_seen = false;
    assert_prepare_race(&closing_namespace, |checkpoint| {
        if checkpoint == (FixtureMountNamespaceCheckpoint::PassNamespaceRecheck { pass: 2 }) && !namespace_recheck_seen
        {
            namespace_recheck_seen = true;
            closing_namespace.replace_namespace_marker_identity()?;
        }
        Ok(())
    });
    assert!(namespace_recheck_seen);

    let closing_root = SyntheticMountNamespace::stable().unwrap();
    let mut root_recheck_seen = false;
    assert_prepare_race(&closing_root, |checkpoint| {
        if checkpoint == (FixtureMountNamespaceCheckpoint::PassTaskRootRecheck { pass: 2 }) && !root_recheck_seen {
            root_recheck_seen = true;
            closing_root.replace_task_root_identity()?;
        }
        Ok(())
    });
    assert!(root_recheck_seen);
}

#[test]
fn terminal_namespace_and_root_replacements_fail_closed() {
    let tree = SyntheticMountNamespace::stable().unwrap();
    let mut tree_seen = false;
    assert_prepare_race(&tree, |checkpoint| {
        if checkpoint == FixtureMountNamespaceCheckpoint::TerminalTreeRebind && !tree_seen {
            tree_seen = true;
            tree.replace_tree_identity()?;
        }
        Ok(())
    });
    assert!(tree_seen);

    let namespace = SyntheticMountNamespace::stable().unwrap();
    let mut namespace_seen = false;
    assert_prepare_race(&namespace, |checkpoint| {
        if checkpoint == FixtureMountNamespaceCheckpoint::TerminalNamespaceRebind && !namespace_seen {
            namespace_seen = true;
            namespace.replace_namespace_marker_identity()?;
        }
        Ok(())
    });
    assert!(namespace_seen);

    let task_root = SyntheticMountNamespace::stable().unwrap();
    let mut root_seen = false;
    assert_prepare_race(&task_root, |checkpoint| {
        if checkpoint == FixtureMountNamespaceCheckpoint::TerminalTaskRootRebind && !root_seen {
            root_seen = true;
            task_root.replace_task_root_identity()?;
        }
        Ok(())
    });
    assert!(root_seen);

    let closing_namespace = SyntheticMountNamespace::stable().unwrap();
    let mut namespace_recheck_seen = false;
    assert_prepare_race(&closing_namespace, |checkpoint| {
        if checkpoint == FixtureMountNamespaceCheckpoint::TerminalNamespaceRecheck && !namespace_recheck_seen {
            namespace_recheck_seen = true;
            closing_namespace.replace_namespace_marker_identity()?;
        }
        Ok(())
    });
    assert!(namespace_recheck_seen);

    let closing_root = SyntheticMountNamespace::stable().unwrap();
    let mut root_recheck_seen = false;
    assert_prepare_race(&closing_root, |checkpoint| {
        if checkpoint == FixtureMountNamespaceCheckpoint::TerminalTaskRootRecheck && !root_recheck_seen {
            root_recheck_seen = true;
            closing_root.replace_task_root_identity()?;
        }
        Ok(())
    });
    assert!(root_recheck_seen);
}

#[test]
fn prepared_anchor_rejects_across_call_tree_namespace_and_root_replacements() {
    let tree = SyntheticMountNamespace::stable().unwrap();
    let tree_prepared = admitted(&tree).unwrap().prepare().unwrap();
    tree.replace_tree_identity().unwrap();
    assert!(tree_prepared.revalidate().is_err());
    tree.assert_outside_unchanged();

    let namespace = SyntheticMountNamespace::stable().unwrap();
    let namespace_prepared = admitted(&namespace).unwrap().prepare().unwrap();
    namespace.replace_namespace_marker_identity().unwrap();
    assert!(namespace_prepared.revalidate().is_err());
    namespace.assert_outside_unchanged();

    let task_root = SyntheticMountNamespace::stable().unwrap();
    let root_prepared = admitted(&task_root).unwrap().prepare().unwrap();
    task_root.replace_task_root_identity().unwrap();
    assert!(root_prepared.revalidate().is_err());
    task_root.assert_outside_unchanged();
}

#[test]
fn revalidation_repeats_both_complete_passes_and_terminal_rebinds() {
    let fixture = SyntheticMountNamespace::stable().unwrap();
    let prepared = admitted(&fixture).unwrap().prepare().unwrap();
    let mut second_pass_seen = false;

    let result = prepared.revalidate_with(FixtureMountNamespaceLimits::default(), deadline(), &mut |checkpoint| {
        if checkpoint == (FixtureMountNamespaceCheckpoint::NamespacePinned { pass: 2 }) && !second_pass_seen {
            second_pass_seen = true;
            fixture.replace_namespace_marker_identity()?;
        }
        Ok(())
    });

    assert!(result.is_err());
    assert!(second_pass_seen);
    fixture.assert_outside_unchanged();
}
