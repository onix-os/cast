use std::{
    io,
    time::{Duration, Instant},
};

use super::super::super::super::mount_namespace::{
    FixtureMountNamespaceCheckpoint, FixtureMountNamespaceTree, FixtureTaskRootedAttachmentLimits,
};
use super::super::support::SyntheticMountNamespace;

const COMPONENTS: &[&str] = &["alpha", "bravo", "charlie"];

fn selector() -> String {
    format!("/{}", COMPONENTS.join("/"))
}

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(10)
}

fn assert_prepare_race(
    fixture: &SyntheticMountNamespace,
    mut hook: impl FnMut(FixtureMountNamespaceCheckpoint) -> io::Result<()>,
) {
    let (parent, tree_name) = fixture.admission().unwrap();
    let anchor = FixtureMountNamespaceTree::admit(parent, tree_name)
        .unwrap()
        .prepare()
        .unwrap();
    let anchor_view = anchor.revalidate().unwrap();
    let result = anchor_view.prepare_task_rooted_attachment_with(
        &selector(),
        FixtureTaskRootedAttachmentLimits::default(),
        deadline(),
        &mut hook,
    );
    let error = match result {
        Ok(_) => panic!("attachment replacement race unexpectedly succeeded"),
        Err(error) => error,
    };
    assert_ne!(error.kind(), io::ErrorKind::TimedOut);
    fixture.assert_outside_unchanged();
}

#[test]
fn every_component_replacement_in_either_complete_pass_fails_closed() {
    for pass in 1..=2 {
        for index in 0..COMPONENTS.len() {
            let fixture = SyntheticMountNamespace::with_attachment(COMPONENTS).unwrap();
            let mut reached = false;
            assert_prepare_race(&fixture, |checkpoint| {
                if checkpoint == (FixtureMountNamespaceCheckpoint::AttachmentComponentPinned { pass, index })
                    && !reached
                {
                    reached = true;
                    fixture.replace_attachment_identity(COMPONENTS, index)?;
                }
                Ok(())
            });
            assert!(reached);
        }
    }
}

#[test]
fn public_task_root_replacement_is_rejected_by_both_anchor_sandwich_edges() {
    for target in [
        FixtureMountNamespaceCheckpoint::AttachmentAnchorOpened,
        FixtureMountNamespaceCheckpoint::AttachmentBeforeClosingAnchor,
    ] {
        let fixture = SyntheticMountNamespace::with_attachment(COMPONENTS).unwrap();
        let mut reached = false;
        assert_prepare_race(&fixture, |checkpoint| {
            if checkpoint == target && !reached {
                reached = true;
                fixture.replace_task_root_identity()?;
            }
            Ok(())
        });
        assert!(reached);
    }
}

#[test]
fn mount_namespace_replacement_is_rejected_by_both_anchor_sandwich_edges() {
    for target in [
        FixtureMountNamespaceCheckpoint::AttachmentAnchorOpened,
        FixtureMountNamespaceCheckpoint::AttachmentBeforeClosingAnchor,
    ] {
        let fixture = SyntheticMountNamespace::with_attachment(COMPONENTS).unwrap();
        let mut reached = false;
        assert_prepare_race(&fixture, |checkpoint| {
            if checkpoint == target && !reached {
                reached = true;
                fixture.replace_namespace_marker_identity()?;
            }
            Ok(())
        });
        assert!(reached);
    }
}

#[test]
fn terminal_parent_final_name_and_late_full_chain_replacements_fail_closed() {
    let parent_rebind = SyntheticMountNamespace::with_attachment(COMPONENTS).unwrap();
    let mut parent_rebind_seen = false;
    assert_prepare_race(&parent_rebind, |checkpoint| {
        if checkpoint == FixtureMountNamespaceCheckpoint::AttachmentTerminalParent && !parent_rebind_seen {
            parent_rebind_seen = true;
            parent_rebind.replace_attachment_identity(COMPONENTS, COMPONENTS.len() - 2)?;
        }
        Ok(())
    });
    assert!(parent_rebind_seen);

    let parent_after_pin = SyntheticMountNamespace::with_attachment(COMPONENTS).unwrap();
    let mut parent_after_pin_seen = false;
    assert_prepare_race(&parent_after_pin, |checkpoint| {
        if checkpoint == FixtureMountNamespaceCheckpoint::AttachmentTerminalName && !parent_after_pin_seen {
            parent_after_pin_seen = true;
            parent_after_pin.replace_attachment_identity(COMPONENTS, COMPONENTS.len() - 2)?;
        }
        Ok(())
    });
    assert!(parent_after_pin_seen);

    let final_name = SyntheticMountNamespace::with_attachment(COMPONENTS).unwrap();
    let mut name_seen = false;
    assert_prepare_race(&final_name, |checkpoint| {
        if checkpoint == FixtureMountNamespaceCheckpoint::AttachmentTerminalName && !name_seen {
            name_seen = true;
            final_name.replace_attachment_identity(COMPONENTS, COMPONENTS.len() - 1)?;
        }
        Ok(())
    });
    assert!(name_seen);

    let late = SyntheticMountNamespace::with_attachment(COMPONENTS).unwrap();
    let mut late_seen = false;
    assert_prepare_race(&late, |checkpoint| {
        if checkpoint == (FixtureMountNamespaceCheckpoint::AttachmentTerminalFullChain { round: 2 }) && !late_seen {
            late_seen = true;
            late.replace_attachment_identity(COMPONENTS, COMPONENTS.len() - 1)?;
        }
        Ok(())
    });
    assert!(late_seen);
}

#[test]
fn prepared_attachment_rejects_across_call_component_and_root_replacements() {
    for index in 0..COMPONENTS.len() {
        let component = SyntheticMountNamespace::with_attachment(COMPONENTS).unwrap();
        let (parent, tree_name) = component.admission().unwrap();
        let component_anchor = FixtureMountNamespaceTree::admit(parent, tree_name)
            .unwrap()
            .prepare()
            .unwrap();
        let component_attachment = component_anchor
            .revalidate()
            .unwrap()
            .prepare_task_rooted_attachment(&selector())
            .unwrap();
        component.replace_attachment_identity(COMPONENTS, index).unwrap();
        assert!(component_attachment.revalidate_against(&component_anchor).is_err());
        component.assert_outside_unchanged();
    }

    let root = SyntheticMountNamespace::with_attachment(COMPONENTS).unwrap();
    let (parent, tree_name) = root.admission().unwrap();
    let root_anchor = FixtureMountNamespaceTree::admit(parent, tree_name)
        .unwrap()
        .prepare()
        .unwrap();
    let root_attachment = root_anchor
        .revalidate()
        .unwrap()
        .prepare_task_rooted_attachment(&selector())
        .unwrap();
    root.replace_task_root_identity().unwrap();
    assert!(root_attachment.revalidate_against(&root_anchor).is_err());
    root.assert_outside_unchanged();
}

#[test]
fn revalidation_repeats_second_pass_and_closing_anchor_checks() {
    let fixture = SyntheticMountNamespace::with_attachment(COMPONENTS).unwrap();
    let (parent, tree_name) = fixture.admission().unwrap();
    let anchor = FixtureMountNamespaceTree::admit(parent, tree_name)
        .unwrap()
        .prepare()
        .unwrap();
    let attachment = anchor
        .revalidate()
        .unwrap()
        .prepare_task_rooted_attachment(&selector())
        .unwrap();
    let mut reached = false;
    let result = attachment.revalidate_against_with(
        &anchor,
        FixtureTaskRootedAttachmentLimits::default(),
        deadline(),
        &mut |checkpoint| {
            if checkpoint
                == (FixtureMountNamespaceCheckpoint::AttachmentComponentPinned {
                    pass: 2,
                    index: COMPONENTS.len() - 1,
                })
                && !reached
            {
                reached = true;
                fixture.replace_attachment_identity(COMPONENTS, COMPONENTS.len() - 1)?;
            }
            Ok(())
        },
    );
    assert!(result.is_err());
    assert!(reached);
    fixture.assert_outside_unchanged();

    let closing_fixture = SyntheticMountNamespace::with_attachment(COMPONENTS).unwrap();
    let (parent, tree_name) = closing_fixture.admission().unwrap();
    let closing_anchor = FixtureMountNamespaceTree::admit(parent, tree_name)
        .unwrap()
        .prepare()
        .unwrap();
    let closing_attachment = closing_anchor
        .revalidate()
        .unwrap()
        .prepare_task_rooted_attachment(&selector())
        .unwrap();
    let mut closing_reached = false;
    let closing_result = closing_attachment.revalidate_against_with(
        &closing_anchor,
        FixtureTaskRootedAttachmentLimits::default(),
        deadline(),
        &mut |checkpoint| {
            if checkpoint == FixtureMountNamespaceCheckpoint::AttachmentBeforeClosingAnchor && !closing_reached {
                closing_reached = true;
                closing_fixture.replace_task_root_identity()?;
            }
            Ok(())
        },
    );
    assert!(closing_result.is_err());
    assert!(closing_reached);
    closing_fixture.assert_outside_unchanged();
}
