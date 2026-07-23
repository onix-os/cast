use std::{
    cell::Cell,
    io,
    time::{Duration, Instant},
};

use super::super::super::super::mount_namespace::{
    FixtureMountNamespaceCheckpoint, FixtureMountNamespaceTree, FixtureTaskRootedAttachmentLimits,
};
use super::super::support::SyntheticMountNamespace;

const COMPONENTS: &[&str] = &["alpha", "bravo"];

fn selector() -> String {
    format!("/{}", COMPONENTS.join("/"))
}

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(10)
}

#[test]
fn fixture_clock_rejects_attachment_prepare_and_revalidation_at_entry() {
    let fixture = SyntheticMountNamespace::with_attachment(COMPONENTS).unwrap();
    let (parent, tree_name) = fixture.admission().unwrap();
    let anchor = FixtureMountNamespaceTree::admit(parent, tree_name)
        .unwrap()
        .prepare()
        .unwrap();
    let anchor_view = anchor.revalidate().unwrap();
    let entry_deadline = deadline();
    let prepare_clock_calls = Cell::new(0usize);
    let prepare_hook_calls = Cell::new(0usize);
    let result = anchor_view.prepare_task_rooted_attachment_with_clock(
        &selector(),
        FixtureTaskRootedAttachmentLimits::default(),
        entry_deadline,
        &mut |_| {
            prepare_hook_calls.set(prepare_hook_calls.get() + 1);
            Ok(())
        },
        &mut || {
            prepare_clock_calls.set(prepare_clock_calls.get() + 1);
            entry_deadline + Duration::from_nanos(1)
        },
    );
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut);
    assert_eq!(prepare_clock_calls.get(), 1);
    assert_eq!(prepare_hook_calls.get(), 0);
    assert_eq!(
        anchor_view
            .prepare_task_rooted_attachment_until(&selector(), Instant::now() - Duration::from_nanos(1))
            .unwrap_err()
            .kind(),
        io::ErrorKind::TimedOut
    );

    let attachment = anchor_view.prepare_task_rooted_attachment(&selector()).unwrap();
    let revalidate_clock_calls = Cell::new(0usize);
    let revalidate_hook_calls = Cell::new(0usize);
    let result = attachment.revalidate_against_with_clock(
        &anchor,
        FixtureTaskRootedAttachmentLimits::default(),
        entry_deadline,
        &mut |_| {
            revalidate_hook_calls.set(revalidate_hook_calls.get() + 1);
            Ok(())
        },
        &mut || {
            revalidate_clock_calls.set(revalidate_clock_calls.get() + 1);
            entry_deadline + Duration::from_nanos(1)
        },
    );
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut);
    assert_eq!(revalidate_clock_calls.get(), 1);
    assert_eq!(revalidate_hook_calls.get(), 0);
    assert_eq!(
        attachment
            .revalidate_against_until(&anchor, Instant::now() - Duration::from_nanos(1))
            .unwrap_err()
            .kind(),
        io::ErrorKind::TimedOut
    );
    fixture.assert_outside_unchanged();
}

#[test]
fn fixture_clock_rejects_attachment_prepare_and_revalidation_at_final_checkpoint() {
    let fixture = SyntheticMountNamespace::with_attachment(COMPONENTS).unwrap();
    let (parent, tree_name) = fixture.admission().unwrap();
    let anchor = FixtureMountNamespaceTree::admit(parent, tree_name)
        .unwrap()
        .prepare()
        .unwrap();
    let anchor_view = anchor.revalidate().unwrap();
    let terminal_deadline = deadline();
    let prepare_terminal = Cell::new(false);
    let prepare_terminal_clock_calls = Cell::new(0usize);
    let result = anchor_view.prepare_task_rooted_attachment_with_clock(
        &selector(),
        FixtureTaskRootedAttachmentLimits::default(),
        terminal_deadline,
        &mut |checkpoint| {
            if checkpoint == FixtureMountNamespaceCheckpoint::AttachmentComplete {
                prepare_terminal.set(true);
            }
            Ok(())
        },
        &mut || terminal_time(&prepare_terminal, &prepare_terminal_clock_calls, terminal_deadline),
    );
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut);
    assert!(prepare_terminal.get());
    assert_eq!(prepare_terminal_clock_calls.get(), 2);

    let attachment = anchor_view.prepare_task_rooted_attachment(&selector()).unwrap();
    let revalidate_terminal = Cell::new(false);
    let revalidate_terminal_clock_calls = Cell::new(0usize);
    let result = attachment.revalidate_against_with_clock(
        &anchor,
        FixtureTaskRootedAttachmentLimits::default(),
        terminal_deadline,
        &mut |checkpoint| {
            if checkpoint == FixtureMountNamespaceCheckpoint::AttachmentComplete {
                revalidate_terminal.set(true);
            }
            Ok(())
        },
        &mut || {
            terminal_time(
                &revalidate_terminal,
                &revalidate_terminal_clock_calls,
                terminal_deadline,
            )
        },
    );
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut);
    assert!(revalidate_terminal.get());
    assert_eq!(revalidate_terminal_clock_calls.get(), 2);
    fixture.assert_outside_unchanged();
}

fn terminal_time(terminal: &Cell<bool>, terminal_calls: &Cell<usize>, deadline: Instant) -> Instant {
    if !terminal.get() {
        return deadline;
    }
    let call = terminal_calls.get();
    terminal_calls.set(call + 1);
    if call == 0 {
        deadline
    } else {
        deadline + Duration::from_nanos(1)
    }
}
