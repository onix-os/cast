use std::{cell::Cell, time::Duration};

use astr::AStr;
use stone::{StonePayloadLayoutFile, StonePayloadLayoutRecord};

use super::*;
use crate::state::Selection;

fn package(name: &str) -> package::Id {
    package::Id::from(name.to_owned())
}

fn add_state(database: &db::state::Database, packages: &[package::Id], summary: &str) -> State {
    let selections = packages.iter().cloned().map(Selection::explicit).collect::<Vec<_>>();
    database.add(&selections, Some(summary), None).unwrap()
}

fn regular(path: &str) -> StonePayloadLayoutRecord {
    StonePayloadLayoutRecord {
        uid: 0,
        gid: 0,
        mode: nix::libc::S_IFREG | 0o644,
        tag: 0,
        file: StonePayloadLayoutFile::Regular(1, AStr::from(path.to_owned())),
    }
}

fn policy_with(
    max_packages: usize,
    max_package_id_bytes: usize,
    max_rows: usize,
    max_string_bytes: usize,
) -> ProjectionPolicy {
    ProjectionPolicy {
        max_packages,
        max_package_id_bytes,
        layout_bounds: db::layout::QueryBounds {
            max_rows,
            max_string_bytes,
        },
        timeout: Duration::from_secs(5),
    }
}

fn capture_with_database(
    state_db: &db::state::Database,
    layout_db: &db::layout::Database,
    head: state::Id,
    policy: ProjectionPolicy,
) -> Result<ProjectionSnapshot, ActiveReblitBootProjectionError> {
    let deadline = projection_deadline(policy.timeout)?;
    capture_with_layout_query(state_db, head, policy, deadline, |packages, bounds, deadline| {
        layout_db
            .query_bounded(packages, bounds, || Instant::now() <= deadline)
            .map_err(ActiveReblitBootProjectionError::LayoutDatabase)
    })
}

fn selected_fixture() -> (db::state::Database, db::layout::Database, State, package::Id) {
    let state_db = db::state::Database::new(":memory:").unwrap();
    let layout_db = db::layout::Database::new(":memory:").unwrap();
    let selected = package("selected");
    let head = add_state(&state_db, std::slice::from_ref(&selected), "head");
    (state_db, layout_db, head, selected)
}

#[test]
fn preparation_canonicalizes_and_deduplicates_the_selected_package_union() {
    let state_db = db::state::Database::new(":memory:").unwrap();
    let layout_db = db::layout::Database::new(":memory:").unwrap();
    let alpha = package("alpha");
    let beta = package("beta");
    let gamma = package("gamma");

    let head = add_state(&state_db, &[gamma.clone(), alpha.clone()], "head");
    add_state(&state_db, &[beta.clone(), alpha.clone()], "history");
    layout_db.add(&gamma, &regular("usr/gamma")).unwrap();
    layout_db.add(&alpha, &regular("usr/alpha")).unwrap();
    layout_db.add(&beta, &regular("usr/beta")).unwrap();

    let prepared = PreparedActiveReblitBootProjection::prepare(&state_db, &layout_db, head.id).unwrap();
    let package_order = prepared
        .layouts()
        .iter()
        .map(|(package, _)| package.as_str())
        .collect::<Vec<_>>();

    assert_eq!(prepared.head().id, head.id);
    assert_eq!(package_order, ["alpha", "beta", "gamma"]);
}

#[test]
fn reverse_id_head_and_timestamp_ties_have_deterministic_history_order() {
    let state_db = db::state::Database::new(":memory:").unwrap();
    let layout_db = db::layout::Database::new(":memory:").unwrap();
    let selected = package("tie-package");
    let mut equal_timestamp_run = Vec::new();

    for index in 0..64 {
        let state = add_state(&state_db, std::slice::from_ref(&selected), &format!("tie-{index}"));
        if equal_timestamp_run
            .last()
            .is_some_and(|previous: &State| previous.created == state.created)
        {
            equal_timestamp_run.push(state);
        } else {
            equal_timestamp_run.clear();
            equal_timestamp_run.push(state);
        }
        if equal_timestamp_run.len() == 5 {
            break;
        }
    }
    assert_eq!(equal_timestamp_run.len(), 5, "five SQLite-second timestamp ties");

    let head = &equal_timestamp_run[0];
    let prepared = PreparedActiveReblitBootProjection::prepare(&state_db, &layout_db, head.id).unwrap();
    let actual_ids = prepared.states().iter().map(|state| state.id).collect::<Vec<_>>();
    let expected_ids = [
        equal_timestamp_run[0].id,
        equal_timestamp_run[4].id,
        equal_timestamp_run[3].id,
        equal_timestamp_run[2].id,
        equal_timestamp_run[1].id,
    ];

    assert_eq!(actual_ids, expected_ids);
}

#[test]
fn one_capture_performs_exactly_two_bounded_layout_queries() {
    let (state_db, _, head, selected) = selected_fixture();
    let calls = Cell::new(0usize);
    let deadline = projection_deadline(PROJECTION_POLICY.timeout).unwrap();

    let snapshot = capture_with_layout_query(
        &state_db,
        head.id,
        PROJECTION_POLICY,
        deadline,
        |packages, bounds, _| {
            calls.set(calls.get() + 1);
            assert_eq!(packages, std::slice::from_ref(&selected));
            assert_eq!(bounds.max_rows, MAX_PROJECTION_LAYOUT_ROWS);
            assert_eq!(bounds.max_string_bytes, MAX_PROJECTION_LAYOUT_STRING_BYTES);
            Ok(db::layout::BoundedQueryOutcome::Complete(Vec::new()))
        },
    )
    .unwrap();

    assert_eq!(calls.get(), 2);
    assert!(snapshot.layouts.is_empty());
}

#[test]
fn layout_sandwich_rejects_a_mutation_between_bounded_queries() {
    let (state_db, layout_db, head, selected) = selected_fixture();
    layout_db.add(&selected, &regular("usr/before")).unwrap();
    let changed = regular("usr/after");
    let calls = Cell::new(0usize);
    let deadline = projection_deadline(PROJECTION_POLICY.timeout).unwrap();

    let error = capture_with_layout_query(
        &state_db,
        head.id,
        PROJECTION_POLICY,
        deadline,
        |packages, bounds, deadline| {
            let outcome = layout_db
                .query_bounded(packages, bounds, || Instant::now() <= deadline)
                .map_err(ActiveReblitBootProjectionError::LayoutDatabase)?;
            if calls.get() == 0 {
                layout_db.add(&selected, &changed).unwrap();
            }
            calls.set(calls.get() + 1);
            Ok(outcome)
        },
    )
    .err()
    .expect("the two layout snapshots must reject mutation");

    assert!(matches!(error, ActiveReblitBootProjectionError::LayoutSandwichChanged));
    assert_eq!(calls.get(), 2);
}

#[test]
fn state_layout_layout_state_sandwich_rejects_a_mid_query_state_mutation() {
    let (state_db, _, head, _) = selected_fixture();
    let deadline = projection_deadline(PROJECTION_POLICY.timeout).unwrap();

    let error = capture_with_layout_query(&state_db, head.id, PROJECTION_POLICY, deadline, |_, _, _| {
        state_db
            .change_summary_for_test(head.id, Some("changed during layout query"))
            .unwrap();
        Ok(db::layout::BoundedQueryOutcome::Complete(Vec::new()))
    })
    .err()
    .expect("the state sandwich must reject mutation");

    assert!(matches!(error, ActiveReblitBootProjectionError::StateSandwichChanged));
}

#[test]
fn package_count_policy_admits_n_and_rejects_n_plus_one_before_layout_query() {
    let state_db = db::state::Database::new(":memory:").unwrap();
    let selected = [package("one"), package("two"), package("three")];
    let two = add_state(&state_db, &selected[..2], "two packages");
    let policy = policy_with(2, 1024, 16, 1024);
    let calls = Cell::new(0usize);
    let deadline = projection_deadline(policy.timeout).unwrap();
    capture_with_layout_query(&state_db, two.id, policy, deadline, |_, _, _| {
        calls.set(calls.get() + 1);
        Ok(db::layout::BoundedQueryOutcome::Complete(Vec::new()))
    })
    .unwrap();
    assert_eq!(calls.get(), 2);

    let three = add_state(&state_db, &selected, "three packages");
    let deadline = projection_deadline(policy.timeout).unwrap();
    let error = capture_with_layout_query(&state_db, three.id, policy, deadline, |_, _, _| {
        calls.set(calls.get() + 1);
        Ok(db::layout::BoundedQueryOutcome::Complete(Vec::new()))
    })
    .err()
    .expect("N+1 packages must fail before querying layouts");

    assert!(matches!(
        error,
        ActiveReblitBootProjectionError::PackageCountLimit { limit: 2, actual: 3 }
    ));
    assert_eq!(calls.get(), 2);
}

#[test]
fn package_id_byte_policy_accounts_only_the_canonical_unique_union() {
    let state_db = db::state::Database::new(":memory:").unwrap();
    let selected = [package("aa"), package("bbb")];
    let head = add_state(&state_db, &selected, "five unique bytes");
    add_state(&state_db, &[package("aa")], "duplicate in history");
    let admitted = policy_with(4, 5, 16, 1024);
    let deadline = projection_deadline(admitted.timeout).unwrap();
    capture_with_layout_query(&state_db, head.id, admitted, deadline, |packages, _, _| {
        assert_eq!(packages.len(), 2);
        Ok(db::layout::BoundedQueryOutcome::Complete(Vec::new()))
    })
    .unwrap();

    let rejected = policy_with(4, 4, 16, 1024);
    let deadline = projection_deadline(rejected.timeout).unwrap();
    let error = capture_with_layout_query(&state_db, head.id, rejected, deadline, |_, _, _| {
        panic!("layout query must not run after byte-bound rejection")
    })
    .err()
    .expect("N+1 package-ID bytes must be rejected");

    assert!(matches!(
        error,
        ActiveReblitBootProjectionError::PackageIdByteLimit { limit: 4, actual: 5 }
    ));
}

#[test]
fn layout_row_policy_rejects_n_plus_one_rows() {
    let (state_db, layout_db, head, selected) = selected_fixture();
    let first = regular("usr/one");
    let second = regular("usr/two");
    layout_db
        .batch_add([(&selected, &first), (&selected, &second)])
        .unwrap();
    let policy = policy_with(4, 1024, 1, 1024);

    let error = capture_with_database(&state_db, &layout_db, head.id, policy)
        .err()
        .expect("N+1 rows must be rejected");

    assert!(matches!(
        error,
        ActiveReblitBootProjectionError::LayoutRowLimit { limit: 1, actual: 2 }
    ));
}

#[test]
fn layout_string_byte_policy_admits_n_and_rejects_n_plus_one() {
    let (state_db, layout_db, head, selected) = selected_fixture();
    let path = "usr/payload";
    layout_db.add(&selected, &regular(path)).unwrap();
    let exact_bytes = selected.as_str().len() + "regular".len() + "1".len() + path.len();

    let admitted = policy_with(4, 1024, 1, exact_bytes);
    assert_eq!(
        capture_with_database(&state_db, &layout_db, head.id, admitted)
            .unwrap()
            .layouts
            .len(),
        1
    );

    let rejected = policy_with(4, 1024, 1, exact_bytes - 1);
    let error = capture_with_database(&state_db, &layout_db, head.id, rejected)
        .err()
        .expect("N+1 layout string bytes must be rejected");
    assert!(matches!(
        error,
        ActiveReblitBootProjectionError::LayoutStringByteLimit { limit, actual }
            if limit == exact_bytes - 1 && actual == exact_bytes
    ));
}

#[test]
fn expired_deadline_stops_before_any_layout_query() {
    let (state_db, _, head, _) = selected_fixture();
    let calls = Cell::new(0usize);
    let deadline = Instant::now().checked_sub(Duration::from_secs(1)).unwrap();

    let error = capture_with_layout_query(&state_db, head.id, PROJECTION_POLICY, deadline, |_, _, _| {
        calls.set(calls.get() + 1);
        Ok(db::layout::BoundedQueryOutcome::Complete(Vec::new()))
    })
    .err()
    .expect("an expired deadline must fail closed");

    assert!(matches!(
        error,
        ActiveReblitBootProjectionError::DeadlineExceeded { .. }
    ));
    assert_eq!(calls.get(), 0);

    let layout_db = db::layout::Database::new(":memory:").unwrap();
    assert!(matches!(
        PreparedActiveReblitBootProjection::prepare_until(&state_db, &layout_db, head.id, deadline),
        Err(ActiveReblitBootProjectionError::DeadlineExceeded { .. })
    ));

    // Admit every operation through both layout reads and the second state
    // read, then expire only the final post-materialization check.
    let terminal_deadline = Instant::now().checked_add(Duration::from_secs(60)).unwrap();
    let expired_terminal = terminal_deadline.checked_add(Duration::from_nanos(1)).unwrap();
    let clock_calls = Cell::new(0usize);
    let layout_calls = Cell::new(0usize);
    let mut terminal_clock = || {
        let call = clock_calls.get().saturating_add(1);
        clock_calls.set(call);
        if call == 10 {
            expired_terminal
        } else {
            terminal_deadline
        }
    };
    let terminal_error = capture_with_layout_query_and_clock(
        &state_db,
        head.id,
        PROJECTION_POLICY,
        terminal_deadline,
        &mut terminal_clock,
        |_, _, _| {
            layout_calls.set(layout_calls.get().saturating_add(1));
            Ok(db::layout::BoundedQueryOutcome::Complete(Vec::new()))
        },
    )
    .err()
    .expect("the terminal projection deadline check must fail closed");
    assert!(matches!(
        terminal_error,
        ActiveReblitBootProjectionError::DeadlineExceeded { .. }
    ));
    assert_eq!(layout_calls.get(), 2, "both layout reads must complete first");
    assert_eq!(
        clock_calls.get(),
        10,
        "expiry must be observed only at the terminal check"
    );
}

#[test]
fn cancelled_bounded_query_has_a_typed_failure() {
    let (state_db, _, head, _) = selected_fixture();
    let deadline = projection_deadline(PROJECTION_POLICY.timeout).unwrap();
    let error = capture_with_layout_query(&state_db, head.id, PROJECTION_POLICY, deadline, |_, _, _| {
        Ok(db::layout::BoundedQueryOutcome::Cancelled)
    })
    .err()
    .expect("cancelled query must fail closed");

    assert!(matches!(
        error,
        ActiveReblitBootProjectionError::LayoutQueryCancelled { .. }
    ));
}

#[test]
fn revalidation_accepts_unchanged_state_and_layout_evidence() {
    let (state_db, layout_db, head, selected) = selected_fixture();
    layout_db.add(&selected, &regular("usr/original")).unwrap();
    let prepared = PreparedActiveReblitBootProjection::prepare(&state_db, &layout_db, head.id).unwrap();

    prepared.revalidate(&state_db, &layout_db).unwrap();

    // The capture has ten checks for this one-selection fixture. Admit all
    // of them, then expire only the equality-complete revalidation check.
    let terminal_deadline = Instant::now().checked_add(Duration::from_secs(60)).unwrap();
    let expired_terminal = terminal_deadline.checked_add(Duration::from_nanos(1)).unwrap();
    let clock_calls = Cell::new(0usize);
    let mut terminal_clock = || {
        let call = clock_calls.get().saturating_add(1);
        clock_calls.set(call);
        if call == 12 {
            expired_terminal
        } else {
            terminal_deadline
        }
    };
    assert!(matches!(
        prepared.revalidate_until_with_clock(&state_db, &layout_db, terminal_deadline, &mut terminal_clock,),
        Err(ActiveReblitBootProjectionError::DeadlineExceeded { .. })
    ));
    assert_eq!(clock_calls.get(), 12, "expiry must follow complete equality checks");
}

#[test]
fn revalidation_rejects_an_added_history_state() {
    let (state_db, layout_db, head, _) = selected_fixture();
    let prepared = PreparedActiveReblitBootProjection::prepare(&state_db, &layout_db, head.id).unwrap();
    add_state(&state_db, &[package("later")], "new history");

    assert!(matches!(
        prepared.revalidate(&state_db, &layout_db).unwrap_err(),
        ActiveReblitBootProjectionError::StateChanged
    ));
}

#[test]
fn revalidation_rejects_a_removed_history_state() {
    let (state_db, layout_db, head, _) = selected_fixture();
    let history = add_state(&state_db, &[package("history")], "history");
    let prepared = PreparedActiveReblitBootProjection::prepare(&state_db, &layout_db, head.id).unwrap();
    state_db.remove(&history.id).unwrap();

    assert!(matches!(
        prepared.revalidate(&state_db, &layout_db).unwrap_err(),
        ActiveReblitBootProjectionError::StateChanged
    ));
}

#[test]
fn revalidation_rejects_an_exact_state_field_mutation() {
    let (state_db, layout_db, head, _) = selected_fixture();
    let prepared = PreparedActiveReblitBootProjection::prepare(&state_db, &layout_db, head.id).unwrap();
    state_db
        .change_summary_for_test(head.id, Some("changed after preparation"))
        .unwrap();

    assert!(matches!(
        prepared.revalidate(&state_db, &layout_db).unwrap_err(),
        ActiveReblitBootProjectionError::StateChanged
    ));
}

#[test]
fn revalidation_rejects_an_added_selected_package_layout() {
    let (state_db, layout_db, head, selected) = selected_fixture();
    let prepared = PreparedActiveReblitBootProjection::prepare(&state_db, &layout_db, head.id).unwrap();
    layout_db.add(&selected, &regular("usr/added")).unwrap();

    assert!(matches!(
        prepared.revalidate(&state_db, &layout_db).unwrap_err(),
        ActiveReblitBootProjectionError::LayoutChanged
    ));
}

#[test]
fn revalidation_rejects_a_removed_selected_package_layout() {
    let (state_db, layout_db, head, selected) = selected_fixture();
    layout_db.add(&selected, &regular("usr/removed")).unwrap();
    let prepared = PreparedActiveReblitBootProjection::prepare(&state_db, &layout_db, head.id).unwrap();
    layout_db.remove(&selected).unwrap();

    assert!(matches!(
        prepared.revalidate(&state_db, &layout_db).unwrap_err(),
        ActiveReblitBootProjectionError::LayoutChanged
    ));
}

#[test]
fn revalidation_rejects_reordered_layout_records() {
    let (state_db, layout_db, head, selected) = selected_fixture();
    let first = regular("usr/first");
    let second = regular("usr/second");
    layout_db
        .batch_add([(&selected, &first), (&selected, &second)])
        .unwrap();
    let prepared = PreparedActiveReblitBootProjection::prepare(&state_db, &layout_db, head.id).unwrap();

    layout_db
        .batch_add([(&selected, &second), (&selected, &first)])
        .unwrap();

    assert!(matches!(
        prepared.revalidate(&state_db, &layout_db).unwrap_err(),
        ActiveReblitBootProjectionError::LayoutChanged
    ));
}
