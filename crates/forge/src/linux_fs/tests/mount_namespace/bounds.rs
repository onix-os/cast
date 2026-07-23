use std::{
    io,
    time::{Duration, Instant},
};

use super::super::super::mount_namespace::{
    FixtureMountNamespaceCheckpoint, FixtureMountNamespaceLimits, FixtureMountNamespaceTree,
    PreparedMountNamespaceAnchor,
};
use super::support::SyntheticMountNamespace;

#[derive(Clone, Copy)]
enum LimitField {
    Work,
    Descriptors,
}

fn admitted(fixture: &SyntheticMountNamespace) -> io::Result<FixtureMountNamespaceTree> {
    let (parent, tree_name) = fixture.admission()?;
    FixtureMountNamespaceTree::admit(parent, tree_name)
}

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(10)
}

fn with_limit(field: LimitField, value: usize) -> FixtureMountNamespaceLimits {
    let mut limits = FixtureMountNamespaceLimits::default();
    match field {
        LimitField::Work => limits.max_work = value,
        LimitField::Descriptors => limits.max_descriptors = value,
    }
    limits
}

fn prepare_attempt(tree: &FixtureMountNamespaceTree, limits: FixtureMountNamespaceLimits) -> io::Result<()> {
    let mut hook = |_| Ok(());
    tree.prepare_with(limits, deadline(), &mut hook).map(drop)
}

fn revalidate_attempt(prepared: &PreparedMountNamespaceAnchor, limits: FixtureMountNamespaceLimits) -> io::Result<()> {
    let mut hook = |_| Ok(());
    prepared.revalidate_with(limits, deadline(), &mut hook).map(drop)
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
fn zero_limits_and_expired_deadline_fail_before_fixture_hooks() {
    let fixture = SyntheticMountNamespace::stable().unwrap();
    let tree = admitted(&fixture).unwrap();

    for limits in [
        FixtureMountNamespaceLimits {
            max_work: 0,
            ..FixtureMountNamespaceLimits::default()
        },
        FixtureMountNamespaceLimits {
            max_descriptors: 0,
            ..FixtureMountNamespaceLimits::default()
        },
    ] {
        let mut calls = 0usize;
        let result = tree.prepare_with(limits, deadline(), &mut |_| {
            calls += 1;
            Ok(())
        });
        let error = match result {
            Ok(_) => panic!("zero fixture limit unexpectedly produced a namespace anchor"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(calls, 0);
    }

    let mut calls = 0usize;
    let result = tree.prepare_with(
        FixtureMountNamespaceLimits::default(),
        Instant::now() - Duration::from_millis(1),
        &mut |_| {
            calls += 1;
            Ok(())
        },
    );
    let error = match result {
        Ok(_) => panic!("expired fixture deadline unexpectedly produced a namespace anchor"),
        Err(error) => error,
    };
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert_eq!(calls, 0);
    fixture.assert_outside_unchanged();
}

#[test]
fn checkpoint_hooks_are_finite_and_injected_failure_is_propagated() {
    let stable = SyntheticMountNamespace::stable().unwrap();
    let tree = admitted(&stable).unwrap();
    let mut calls = 0usize;
    let mut first_complete = false;
    let mut second_complete = false;
    let mut pass_root_recheck = false;
    let mut pass_namespace_recheck = false;
    let mut terminal_namespace = false;
    let mut terminal_root = false;
    let mut terminal_namespace_recheck = false;
    let mut terminal_root_recheck = false;
    tree.prepare_with(FixtureMountNamespaceLimits::default(), deadline(), &mut |checkpoint| {
        calls += 1;
        if calls > 64 {
            return Err(io::Error::other(
                "fixture checkpoint sequence exceeded its test ceiling",
            ));
        }
        match checkpoint {
            FixtureMountNamespaceCheckpoint::PassComplete { pass: 1 } => first_complete = true,
            FixtureMountNamespaceCheckpoint::PassComplete { pass: 2 } => second_complete = true,
            FixtureMountNamespaceCheckpoint::PassTaskRootRecheck { pass: 2 } => pass_root_recheck = true,
            FixtureMountNamespaceCheckpoint::PassNamespaceRecheck { pass: 2 } => pass_namespace_recheck = true,
            FixtureMountNamespaceCheckpoint::TerminalNamespaceRebind => terminal_namespace = true,
            FixtureMountNamespaceCheckpoint::TerminalTaskRootRebind => terminal_root = true,
            FixtureMountNamespaceCheckpoint::TerminalNamespaceRecheck => terminal_namespace_recheck = true,
            FixtureMountNamespaceCheckpoint::TerminalTaskRootRecheck => terminal_root_recheck = true,
            _ => {}
        }
        Ok(())
    })
    .unwrap();
    assert!((1..=64).contains(&calls));
    assert!(first_complete && second_complete && terminal_namespace && terminal_root);
    assert!(pass_root_recheck && pass_namespace_recheck);
    assert!(terminal_root_recheck && terminal_namespace_recheck);
    stable.assert_outside_unchanged();

    let injected = SyntheticMountNamespace::stable().unwrap();
    let tree = admitted(&injected).unwrap();
    let mut reached = false;
    let result = tree.prepare_with(FixtureMountNamespaceLimits::default(), deadline(), &mut |checkpoint| {
        if checkpoint == (FixtureMountNamespaceCheckpoint::NamespacePinned { pass: 1 }) {
            reached = true;
            return Err(io::Error::other("injected bounded mount-namespace checkpoint failure"));
        }
        Ok(())
    });
    let error = match result {
        Ok(_) => panic!("injected checkpoint failure unexpectedly succeeded"),
        Err(error) => error,
    };
    assert!(reached);
    assert_eq!(error.kind(), io::ErrorKind::Other);
    injected.assert_outside_unchanged();
}

#[test]
fn preparation_work_and_descriptor_budgets_have_exact_boundaries() {
    let fixture = SyntheticMountNamespace::stable().unwrap();
    let tree = admitted(&fixture).unwrap();
    let defaults = FixtureMountNamespaceLimits::default();

    for (field, upper) in [
        (LimitField::Work, defaults.max_work),
        (LimitField::Descriptors, defaults.max_descriptors),
    ] {
        let exact = minimum_accepting(upper, |value| prepare_attempt(&tree, with_limit(field, value)));
        assert!(exact > 1);
        prepare_attempt(&tree, with_limit(field, exact)).unwrap();
        assert_eq!(
            prepare_attempt(&tree, with_limit(field, exact - 1)).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }
    fixture.assert_outside_unchanged();
}

#[test]
fn revalidation_budgets_are_global_across_both_passes_and_terminal_checks() {
    let fixture = SyntheticMountNamespace::stable().unwrap();
    let prepared = admitted(&fixture).unwrap().prepare().unwrap();
    let defaults = FixtureMountNamespaceLimits::default();

    for (field, upper) in [
        (LimitField::Work, defaults.max_work),
        (LimitField::Descriptors, defaults.max_descriptors),
    ] {
        let exact = minimum_accepting(upper, |value| revalidate_attempt(&prepared, with_limit(field, value)));
        assert!(exact > 1);
        revalidate_attempt(&prepared, with_limit(field, exact)).unwrap();
        assert_eq!(
            revalidate_attempt(&prepared, with_limit(field, exact - 1))
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }
    fixture.assert_outside_unchanged();
}
