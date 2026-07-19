use std::{
    io,
    time::{Duration, Instant},
};

use super::super::super::super::mount_namespace::{
    FixtureMountNamespaceCheckpoint, FixtureMountNamespaceTree, FixtureTaskRootedAttachmentLimits,
    PreparedMountNamespaceAnchor, PreparedTaskRootedAttachment, RevalidatedMountNamespaceAnchor,
};
use super::super::support::SyntheticMountNamespace;

const COMPONENTS: &[&str] = &["alpha", "bravo"];

#[derive(Clone, Copy)]
enum LimitField {
    Work,
    Descriptors,
}

fn selector() -> String {
    format!("/{}", COMPONENTS.join("/"))
}

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(10)
}

fn prepared_anchor(fixture: &SyntheticMountNamespace) -> io::Result<PreparedMountNamespaceAnchor> {
    let (parent, tree_name) = fixture.admission()?;
    FixtureMountNamespaceTree::admit(parent, tree_name)?.prepare()
}

fn with_limit(field: LimitField, value: usize) -> FixtureTaskRootedAttachmentLimits {
    let mut limits = FixtureTaskRootedAttachmentLimits::default();
    match field {
        LimitField::Work => limits.max_work = value,
        LimitField::Descriptors => limits.max_descriptors = value,
    }
    limits
}

fn prepare_attempt(
    anchor: &RevalidatedMountNamespaceAnchor<'_>,
    limits: FixtureTaskRootedAttachmentLimits,
) -> io::Result<()> {
    anchor
        .prepare_task_rooted_attachment_with(&selector(), limits, deadline(), &mut |_| Ok(()))
        .map(drop)
}

fn revalidate_attempt(
    attachment: &PreparedTaskRootedAttachment,
    anchor: &PreparedMountNamespaceAnchor,
    limits: FixtureTaskRootedAttachmentLimits,
) -> io::Result<()> {
    attachment
        .revalidate_against_with(anchor, limits, deadline(), &mut |_| Ok(()))
        .map(drop)
}

fn minimum_accepting(upper: usize, mut attempt: impl FnMut(usize) -> io::Result<()>) -> usize {
    assert!(upper > 1);
    attempt(upper).unwrap();
    let mut rejected = 0usize;
    let mut accepted = upper;
    while accepted - rejected > 1 {
        let candidate = rejected + (accepted - rejected) / 2;
        match attempt(candidate) {
            Ok(()) => accepted = candidate,
            Err(error) => {
                assert_eq!(error.kind(), io::ErrorKind::InvalidData);
                rejected = candidate;
            }
        }
    }
    accepted
}

#[test]
fn zero_limits_and_expired_deadline_fail_before_attachment_hooks() {
    let fixture = SyntheticMountNamespace::with_attachment(COMPONENTS).unwrap();
    let anchor = prepared_anchor(&fixture).unwrap();
    let anchor_view = anchor.revalidate().unwrap();

    for limits in [
        FixtureTaskRootedAttachmentLimits {
            max_work: 0,
            ..FixtureTaskRootedAttachmentLimits::default()
        },
        FixtureTaskRootedAttachmentLimits {
            max_descriptors: 0,
            ..FixtureTaskRootedAttachmentLimits::default()
        },
    ] {
        let mut calls = 0usize;
        let result = anchor_view.prepare_task_rooted_attachment_with(&selector(), limits, deadline(), &mut |_| {
            calls += 1;
            Ok(())
        });
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidInput);
        assert_eq!(calls, 0);
    }

    let mut calls = 0usize;
    let result = anchor_view.prepare_task_rooted_attachment_with(
        &selector(),
        FixtureTaskRootedAttachmentLimits::default(),
        Instant::now() - Duration::from_millis(1),
        &mut |_| {
            calls += 1;
            Ok(())
        },
    );
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut);
    assert_eq!(calls, 0);
    fixture.assert_outside_unchanged();
}

#[test]
fn attachment_hooks_are_finite_and_injected_failures_propagate() {
    let fixture = SyntheticMountNamespace::with_attachment(COMPONENTS).unwrap();
    let anchor = prepared_anchor(&fixture).unwrap();
    let anchor_view = anchor.revalidate().unwrap();
    let mut calls = 0usize;
    let mut pass_two = false;
    let mut final_parent = false;
    let mut final_name = false;
    let mut closing_chain = false;
    let mut closing_anchor = false;
    anchor_view
        .prepare_task_rooted_attachment_with(
            &selector(),
            FixtureTaskRootedAttachmentLimits::default(),
            deadline(),
            &mut |checkpoint| {
                calls += 1;
                if calls > 256 {
                    return Err(io::Error::other("attachment hook sequence exceeded finite sentinel"));
                }
                match checkpoint {
                    FixtureMountNamespaceCheckpoint::AttachmentPassComplete { pass: 2 } => pass_two = true,
                    FixtureMountNamespaceCheckpoint::AttachmentTerminalParent => final_parent = true,
                    FixtureMountNamespaceCheckpoint::AttachmentTerminalName => final_name = true,
                    FixtureMountNamespaceCheckpoint::AttachmentTerminalFullChain { round: 2 } => {
                        closing_chain = true;
                    }
                    FixtureMountNamespaceCheckpoint::AttachmentBeforeClosingAnchor => closing_anchor = true,
                    _ => {}
                }
                Ok(())
            },
        )
        .unwrap();
    assert!((1..=256).contains(&calls));
    assert!(pass_two && final_parent && final_name && closing_chain && closing_anchor);

    let mut injected = false;
    let result = anchor_view.prepare_task_rooted_attachment_with(
        &selector(),
        FixtureTaskRootedAttachmentLimits::default(),
        deadline(),
        &mut |checkpoint| {
            if checkpoint == (FixtureMountNamespaceCheckpoint::AttachmentPassComplete { pass: 1 }) {
                injected = true;
                return Err(io::Error::other("injected attachment checkpoint failure"));
            }
            Ok(())
        },
    );
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Other);
    assert!(injected);
    fixture.assert_outside_unchanged();
}

#[test]
fn preparation_work_and_descriptor_budgets_have_exact_adjacent_boundaries() {
    let fixture = SyntheticMountNamespace::with_attachment(COMPONENTS).unwrap();
    let anchor = prepared_anchor(&fixture).unwrap();
    let anchor_view = anchor.revalidate().unwrap();
    let defaults = FixtureTaskRootedAttachmentLimits::default();

    for (field, upper) in [
        (LimitField::Work, defaults.max_work),
        (LimitField::Descriptors, defaults.max_descriptors),
    ] {
        let exact = minimum_accepting(upper, |value| prepare_attempt(&anchor_view, with_limit(field, value)));
        assert!(exact > 1);
        prepare_attempt(&anchor_view, with_limit(field, exact)).unwrap();
        assert_eq!(
            prepare_attempt(&anchor_view, with_limit(field, exact - 1))
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }
    fixture.assert_outside_unchanged();
}

#[test]
fn revalidation_budgets_cover_both_chain_passes_and_both_anchor_edges() {
    let fixture = SyntheticMountNamespace::with_attachment(COMPONENTS).unwrap();
    let anchor = prepared_anchor(&fixture).unwrap();
    let attachment = anchor
        .revalidate()
        .unwrap()
        .prepare_task_rooted_attachment(&selector())
        .unwrap();
    let defaults = FixtureTaskRootedAttachmentLimits::default();

    for (field, upper) in [
        (LimitField::Work, defaults.max_work),
        (LimitField::Descriptors, defaults.max_descriptors),
    ] {
        let exact = minimum_accepting(upper, |value| {
            revalidate_attempt(&attachment, &anchor, with_limit(field, value))
        });
        assert!(exact > 1);
        revalidate_attempt(&attachment, &anchor, with_limit(field, exact)).unwrap();
        assert_eq!(
            revalidate_attempt(&attachment, &anchor, with_limit(field, exact - 1))
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }
    fixture.assert_outside_unchanged();
}
