use std::collections::BTreeSet;

use diesel::{
    QueryableByName, RunQueryDsl as _,
    sql_types::{Integer, Text},
};

use super::*;
use crate::{package, state::Selection};

fn add_at(database: &Database, created: i32, label: &str, selections: &[Selection]) -> State {
    let state = database
        .add(selections, Some(label), Some(&format!("description for {label}")))
        .unwrap();
    database.conn.exec(|connection| {
        diesel::sql_query("UPDATE state SET created = ? WHERE id = ?")
            .bind::<Integer, _>(created)
            .bind::<Integer, _>(i32::from(state.id))
            .execute(connection)
            .unwrap();
    });
    database.get(state.id).unwrap()
}

fn ids(states: &[State]) -> Vec<Id> {
    states.iter().map(|state| state.id).collect()
}

fn packages(state: &State) -> Vec<package::Id> {
    state
        .selections
        .iter()
        .map(|selection| selection.package.clone())
        .collect()
}

#[test]
fn frozen_boot_input_keeps_a_reverse_id_head_first() {
    let database = Database::new(":memory:").unwrap();
    let head = add_at(&database, 100, "head", &[]);
    let newer_first = add_at(&database, 300, "newer first", &[]);
    let newer_second = add_at(&database, 200, "newer second", &[]);

    let input = database.frozen_boot_input(head.id).unwrap();

    assert_eq!(input.head(), &head);
    assert_eq!(ids(input.states()), [head.id, newer_first.id, newer_second.id]);
}

#[test]
fn frozen_boot_history_orders_by_created_then_descending_id() {
    let database = Database::new(":memory:").unwrap();
    let oldest = add_at(&database, 100, "oldest", &[]);
    let tied_low = add_at(&database, 200, "tied low", &[]);
    let tied_high = add_at(&database, 200, "tied high", &[]);
    let newest = add_at(&database, 300, "newest", &[]);
    let head = add_at(&database, 50, "head", &[]);

    let input = database.frozen_boot_input(head.id).unwrap();

    assert_eq!(ids(input.history()), [newest.id, tied_high.id, tied_low.id, oldest.id]);
}

#[test]
fn frozen_boot_history_is_limited_to_four_states() {
    let database = Database::new(":memory:").unwrap();
    let head = add_at(&database, 1, "head", &[]);
    let history = (2..=8)
        .map(|created| add_at(&database, created, &format!("history {created}"), &[]))
        .collect::<Vec<_>>();

    let input = database.frozen_boot_input(head.id).unwrap();

    assert_eq!(input.history().len(), MAX_BOOT_HISTORY_STATES as usize);
    assert_eq!(
        ids(input.history()),
        history.iter().rev().take(4).map(|state| state.id).collect::<Vec<_>>()
    );
}

#[test]
fn frozen_boot_input_captures_head_and_history_selections() {
    let database = Database::new(":memory:").unwrap();
    let head_selections = [Selection::explicit(package::Id::from("head-package"))];
    let history_selections = [
        Selection::explicit(package::Id::from("history-explicit")),
        Selection::transitive(package::Id::from("history-dependency")).reason("required for boot"),
    ];
    let head = add_at(&database, 200, "head", &head_selections);
    let history = add_at(&database, 100, "history", &history_selections);

    let input = database.frozen_boot_input(head.id).unwrap();

    assert_eq!(input.head(), &head);
    assert_eq!(input.history(), [history.clone()]);
    assert_eq!(input.into_states(), [head, history]);
}

#[test]
fn frozen_boot_input_rejects_a_missing_head() {
    let database = Database::new(":memory:").unwrap();
    add_at(&database, 100, "history", &[]);

    assert!(matches!(
        database.frozen_boot_input(Id::from(9_999)),
        Err(FrozenBootInputError::MissingHead { state_id: 9_999 })
    ));
}

#[test]
fn frozen_boot_input_never_duplicates_the_head_or_history() {
    let database = Database::new(":memory:").unwrap();
    let first = add_at(&database, 100, "first", &[]);
    let head = add_at(&database, 500, "head", &[]);
    let third = add_at(&database, 300, "third", &[]);
    let fourth = add_at(&database, 200, "fourth", &[]);
    let fifth = add_at(&database, 100, "fifth", &[]);

    let input = database.frozen_boot_input(head.id).unwrap();
    let unique = input.states().iter().map(|state| state.id).collect::<BTreeSet<_>>();

    assert_eq!(input.states().len(), 5);
    assert_eq!(unique.len(), input.states().len());
    assert_eq!(input.states().iter().filter(|state| state.id == head.id).count(), 1);
    assert_eq!(ids(input.history()), [third.id, fourth.id, fifth.id, first.id]);
}

#[test]
fn frozen_boot_selections_are_canonical_package_order() {
    let database = Database::new(":memory:").unwrap();
    let zeta = package::Id::from("zeta");
    let alpha = package::Id::from("alpha");
    let middle = package::Id::from("middle");
    let head = add_at(
        &database,
        100,
        "head",
        &[
            Selection::explicit(zeta.clone()),
            Selection::transitive(alpha.clone()).reason("first"),
            Selection::explicit(middle.clone()),
        ],
    );

    let input = database.frozen_boot_input(head.id).unwrap();

    assert_eq!(packages(input.head()), [alpha, middle, zeta]);
}

#[test]
fn frozen_boot_selection_count_accepts_n_and_rejects_n_plus_one() {
    let database = Database::new(":memory:").unwrap();
    let limits = FrozenBootSelectionLimits {
        count: 2,
        text_bytes: 1_024,
    };
    let accepted = add_at(
        &database,
        100,
        "accepted",
        &[
            Selection::explicit(package::Id::from("one")),
            Selection::explicit(package::Id::from("two")),
        ],
    );
    assert!(
        database
            .frozen_boot_input_with_limits(accepted.id, limits, BOOT_STATE_TEXT_LIMITS)
            .is_ok()
    );

    let rejected = add_at(
        &database,
        200,
        "rejected",
        &[
            Selection::explicit(package::Id::from("one")),
            Selection::explicit(package::Id::from("two")),
            Selection::explicit(package::Id::from("three")),
        ],
    );
    assert!(matches!(
        database.frozen_boot_input_with_limits(rejected.id, limits, BOOT_STATE_TEXT_LIMITS),
        Err(FrozenBootInputError::SelectionCountLimit { state_id, limit: 2 })
            if state_id == i32::from(rejected.id)
    ));
}

#[test]
fn frozen_boot_selection_text_accepts_n_and_rejects_n_plus_one_utf8_storage_bytes() {
    let database = Database::new(":memory:").unwrap();
    let limits = FrozenBootSelectionLimits {
        count: 4,
        text_bytes: 10,
    };
    let accepted = add_at(
        &database,
        100,
        "accepted",
        &[
            Selection::explicit(package::Id::from("aa")).reason("éx"),
            Selection::explicit(package::Id::from("c")).reason("dddd"),
        ],
    );
    assert!(
        database
            .frozen_boot_input_with_limits(accepted.id, limits, BOOT_STATE_TEXT_LIMITS)
            .is_ok()
    );

    let rejected = add_at(
        &database,
        200,
        "rejected",
        &[
            Selection::explicit(package::Id::from("aa")).reason("éxx"),
            Selection::explicit(package::Id::from("c")).reason("dddd"),
        ],
    );
    assert!(matches!(
        database.frozen_boot_input_with_limits(rejected.id, limits, BOOT_STATE_TEXT_LIMITS),
        Err(FrozenBootInputError::SelectionTextByteLimit {
            state_id,
            limit: 10,
            actual: 11,
        }) if state_id == i32::from(rejected.id)
    ));
}

#[test]
fn frozen_boot_state_text_accepts_n_and_rejects_n_plus_one_utf8_storage_bytes() {
    let database = Database::new(":memory:").unwrap();
    let limits = FrozenBootStateTextLimits { field_bytes: 12 };
    let accepted_summary = "é".repeat(6);
    let accepted = database.add(&[], Some(&accepted_summary), None).unwrap();
    assert!(
        database
            .frozen_boot_input_with_limits(accepted.id, BOOT_SELECTION_LIMITS, limits)
            .is_ok()
    );

    let rejected_summary = format!("{accepted_summary}x");
    let rejected = database.add(&[], Some(&rejected_summary), None).unwrap();
    assert!(matches!(
        database.frozen_boot_input_with_limits(rejected.id, BOOT_SELECTION_LIMITS, limits),
        Err(FrozenBootInputError::StateTextByteLimit {
            state_id,
            field: "summary",
            limit: 12,
            actual: 13,
        }) if state_id == i32::from(rejected.id)
    ));
}

#[derive(Debug, QueryableByName)]
struct QueryPlanDetail {
    #[diesel(sql_type = Text)]
    detail: String,
}

#[test]
fn frozen_boot_history_order_uses_the_bounded_created_id_index() {
    let database = Database::new(":memory:").unwrap();
    let details = database.conn.exec(|connection| {
        diesel::sql_query(
            "EXPLAIN QUERY PLAN SELECT id FROM state WHERE id != ? ORDER BY created DESC, id DESC LIMIT 4",
        )
        .bind::<Integer, _>(1)
        .load::<QueryPlanDetail>(connection)
        .unwrap()
    });

    assert!(
        details.iter().any(|row| {
            row.detail.contains(&format!("USING INDEX {BOOT_HISTORY_ORDER_INDEX}"))
                || row
                    .detail
                    .contains(&format!("USING COVERING INDEX {BOOT_HISTORY_ORDER_INDEX}"))
        }),
        "boot history query did not use {BOOT_HISTORY_ORDER_INDEX}: {details:?}"
    );
}
