use std::{
    cell::Cell,
    io,
    time::{Duration, Instant},
};

use super::super::super::mount_namespace::{
    FixtureMountNamespaceCheckpoint, FixtureMountNamespaceLimits, FixtureMountNamespaceTree,
};
use super::support::SyntheticMountNamespace;

fn admitted(fixture: &SyntheticMountNamespace) -> FixtureMountNamespaceTree {
    let (parent, tree_name) = fixture.admission().unwrap();
    FixtureMountNamespaceTree::admit(parent, tree_name).unwrap()
}

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(10)
}

#[test]
fn fixture_clock_rejects_prepare_and_revalidate_at_entry() {
    let fixture = SyntheticMountNamespace::stable().unwrap();
    let tree = admitted(&fixture);
    let entry_deadline = deadline();
    let prepare_clock_calls = Cell::new(0usize);
    let prepare_hook_calls = Cell::new(0usize);
    let result = tree.prepare_with_clock(
        FixtureMountNamespaceLimits::default(),
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

    let prepared = tree.prepare().unwrap();
    let revalidate_clock_calls = Cell::new(0usize);
    let revalidate_hook_calls = Cell::new(0usize);
    let result = prepared.revalidate_with_clock(
        FixtureMountNamespaceLimits::default(),
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
        prepared
            .revalidate_until(Instant::now() - Duration::from_nanos(1))
            .unwrap_err()
            .kind(),
        io::ErrorKind::TimedOut
    );
    fixture.assert_outside_unchanged();
}

#[test]
fn fixture_clock_rejects_prepare_and_revalidate_at_explicit_final_checkpoint() {
    let fixture = SyntheticMountNamespace::stable().unwrap();
    let tree = admitted(&fixture);
    let terminal_deadline = deadline();
    let prepare_terminal = Cell::new(false);
    let prepare_terminal_clock_calls = Cell::new(0usize);
    let result = tree.prepare_with_clock(
        FixtureMountNamespaceLimits::default(),
        terminal_deadline,
        &mut |checkpoint| {
            if checkpoint == FixtureMountNamespaceCheckpoint::MountContextComplete {
                prepare_terminal.set(true);
            }
            Ok(())
        },
        &mut || terminal_time(&prepare_terminal, &prepare_terminal_clock_calls, terminal_deadline),
    );
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut);
    assert!(prepare_terminal.get());
    assert_eq!(prepare_terminal_clock_calls.get(), 2);

    let prepared = tree.prepare().unwrap();
    let revalidate_terminal = Cell::new(false);
    let revalidate_terminal_clock_calls = Cell::new(0usize);
    let result = prepared.revalidate_with_clock(
        FixtureMountNamespaceLimits::default(),
        terminal_deadline,
        &mut |checkpoint| {
            if checkpoint == FixtureMountNamespaceCheckpoint::MountContextComplete {
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
