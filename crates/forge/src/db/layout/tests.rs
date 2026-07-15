use diesel::{
    QueryableByName,
    connection::SimpleConnection,
    sql_query,
    sql_types::{BigInt, Text},
};
use stone::StoneDecodedPayload;

use super::*;

fn regular(path: impl Into<AStr>) -> StonePayloadLayoutRecord {
    StonePayloadLayoutRecord {
        uid: 0,
        gid: 0,
        mode: nix::libc::S_IFREG | 0o644,
        tag: 0,
        file: StonePayloadLayoutFile::Regular(1, path.into()),
    }
}

fn layout_bytes(package: &package::Id, path: &str) -> usize {
    package.as_str().len() + "regular".len() + "1".len() + path.len()
}

#[derive(Debug, QueryableByName)]
struct Count {
    #[diesel(sql_type = BigInt)]
    count: i64,
}

fn package_index_count(connection: &mut SqliteConnection) -> i64 {
    sql_query(format!(
        "SELECT COUNT(*) AS count FROM sqlite_schema WHERE type = 'index' AND name = '{PACKAGE_ID_INDEX}'"
    ))
    .get_result::<Count>(connection)
    .unwrap()
    .count
}

#[test]
fn create_insert_select() {
    let database = Database::new(":memory:").unwrap();

    let bash_completion = include_bytes!("../../../../../tests/fixtures/bash-completion-2.11-1-1-x86_64.stone");

    let mut stone = stone::read_bytes(bash_completion).unwrap();

    let payloads = stone.payloads().unwrap().collect::<Result<Vec<_>, _>>().unwrap();
    let layouts = payloads
        .iter()
        .filter_map(StoneDecodedPayload::layout)
        .flat_map(|p| &p.body)
        .map(|layout| (package::Id::from("test"), layout))
        .collect::<Vec<_>>();

    let count = layouts.len();

    database.batch_add(layouts.iter().map(|(p, l)| (p, *l))).unwrap();

    let all = database.all().unwrap();

    assert_eq!(count, all.len());
}

#[test]
fn bounded_query_admits_n_rows_and_rejects_n_plus_one_before_allocation() {
    let database = Database::new(":memory:").unwrap();
    let package = package::Id::from("bounded-rows");
    let layouts = [
        "share/one",
        "share/two",
        "share/three",
        "share/four",
        "share/five",
        "share/six",
    ]
    .map(|path| StonePayloadLayoutRecord {
        uid: 0,
        gid: 0,
        mode: nix::libc::S_IFREG | 0o644,
        tag: 0,
        file: StonePayloadLayoutFile::Regular(1, path.into()),
    });
    database
        .batch_add(layouts.iter().map(|layout| (&package, layout)))
        .unwrap();

    let complete = database
        .query_bounded(
            slice::from_ref(&package),
            QueryBounds {
                max_rows: layouts.len(),
                max_string_bytes: usize::MAX,
            },
            || true,
        )
        .unwrap();
    assert!(matches!(complete, BoundedQueryOutcome::Complete(rows) if rows.len() == layouts.len()));

    let rejected = database
        .query_bounded(
            slice::from_ref(&package),
            QueryBounds {
                max_rows: 2,
                max_string_bytes: usize::MAX,
            },
            || true,
        )
        .unwrap();
    assert!(matches!(
        rejected,
        BoundedQueryOutcome::RowLimit { limit, actual }
            if limit == 2 && actual == 3
    ));
}

#[test]
fn bounded_query_counts_utf8_storage_bytes_at_the_exact_boundary() {
    let database = Database::new(":memory:").unwrap();
    let package = package::Id::from("bounded-bytes");
    let path = "share/café";
    let layout = StonePayloadLayoutRecord {
        uid: 0,
        gid: 0,
        mode: nix::libc::S_IFREG | 0o644,
        tag: 0,
        file: StonePayloadLayoutFile::Regular(1, path.into()),
    };
    database.add(&package, &layout).unwrap();
    let exact = package.as_str().len() + "regular".len() + "1".len() + path.len();

    let complete = database
        .query_bounded(
            slice::from_ref(&package),
            QueryBounds {
                max_rows: 1,
                max_string_bytes: exact,
            },
            || true,
        )
        .unwrap();
    assert!(matches!(complete, BoundedQueryOutcome::Complete(rows) if rows.len() == 1));

    let rejected = database
        .query_bounded(
            slice::from_ref(&package),
            QueryBounds {
                max_rows: 1,
                max_string_bytes: exact - 1,
            },
            || true,
        )
        .unwrap();
    assert!(matches!(
        rejected,
        BoundedQueryOutcome::StringByteLimit { limit, actual }
            if limit == exact - 1 && actual == exact
    ));
}

#[test]
fn package_index_migration_upgrades_downgrades_and_upgrades_cleanly() {
    let database = Database::new(":memory:").unwrap();
    database.conn.exec(|connection| {
        assert_eq!(package_index_count(connection), 1);
        let reverted = connection.revert_last_migration(MIGRATIONS).unwrap();
        assert_eq!(reverted.to_string(), "20260714120000");
        assert_eq!(package_index_count(connection), 0);
        let applied = connection.run_pending_migrations(MIGRATIONS).unwrap();
        assert_eq!(
            applied.iter().map(ToString::to_string).collect::<Vec<_>>(),
            ["20260714120000"]
        );
        assert_eq!(package_index_count(connection), 1);
    });
}

#[derive(Debug, QueryableByName)]
struct QueryPlanDetail {
    #[diesel(sql_type = Text)]
    detail: String,
}

#[test]
fn both_selected_package_query_plans_use_package_index() {
    let database = Database::new(":memory:").unwrap();
    for pass in [SelectedPass::Preflight, SelectedPass::Materialize] {
        let sql = format!("EXPLAIN QUERY PLAN {}", selected_layout_sql(1, pass));
        let details = database.conn.exec(|connection| {
            sql_query(sql)
                .bind::<Text, _>("selected")
                .bind::<BigInt, _>(2_i64)
                .load::<QueryPlanDetail>(connection)
                .unwrap()
        });
        assert!(
            details.iter().any(|row| {
                row.detail.contains(&format!("USING INDEX {PACKAGE_ID_INDEX}"))
                    || row.detail.contains(&format!("USING COVERING INDEX {PACKAGE_ID_INDEX}"))
            }),
            "{pass:?} query plan did not use {PACKAGE_ID_INDEX}: {details:?}"
        );
    }
}

#[test]
fn bounded_query_package_inputs_accept_n_reject_n_plus_one_and_deduplicate() {
    let database = Database::new(":memory:").unwrap();
    let selected = package::Id::from("selected-once");
    database.add(&selected, &regular("share/once")).unwrap();
    let duplicates = [selected.clone(), selected.clone(), selected.clone()];
    let deduplicated = database
        .query_bounded(
            &duplicates,
            QueryBounds {
                max_rows: 1,
                max_string_bytes: usize::MAX,
            },
            || true,
        )
        .unwrap();
    assert!(matches!(deduplicated, BoundedQueryOutcome::Complete(rows) if rows.len() == 1));

    let exact = (0..MAX_BOUNDED_QUERY_PACKAGES)
        .map(|index| package::Id::from(format!("absent-{index}")))
        .collect::<Vec<_>>();
    assert!(matches!(
        database
            .query_bounded(
                &exact,
                QueryBounds {
                    max_rows: 1,
                    max_string_bytes: usize::MAX,
                },
                || true,
            )
            .unwrap(),
        BoundedQueryOutcome::Complete(rows) if rows.is_empty()
    ));

    let mut over = exact;
    over.push(package::Id::from("absent-over"));
    assert!(matches!(
        database
            .query_bounded(
                &over,
                QueryBounds {
                    max_rows: 1,
                    max_string_bytes: usize::MAX,
                },
                || true,
            )
            .unwrap(),
        BoundedQueryOutcome::PackageLimit { limit, actual }
            if limit == MAX_BOUNDED_QUERY_PACKAGES && actual == limit + 1
    ));
}

#[test]
fn bounded_query_package_id_bytes_accept_n_and_reject_n_plus_one() {
    let database = Database::new(":memory:").unwrap();
    let exact = package::Id::from("x".repeat(MAX_BOUNDED_QUERY_PACKAGE_ID_BYTES));
    assert!(matches!(
        database
            .query_bounded(
                slice::from_ref(&exact),
                QueryBounds {
                    max_rows: 0,
                    max_string_bytes: 0,
                },
                || true,
            )
            .unwrap(),
        BoundedQueryOutcome::Complete(rows) if rows.is_empty()
    ));

    let over = package::Id::from("x".repeat(MAX_BOUNDED_QUERY_PACKAGE_ID_BYTES + 1));
    assert!(matches!(
        database
            .query_bounded(
                slice::from_ref(&over),
                QueryBounds {
                    max_rows: 0,
                    max_string_bytes: 0,
                },
                || true,
            )
            .unwrap(),
        BoundedQueryOutcome::PackageIdByteLimit { limit, actual }
            if limit == MAX_BOUNDED_QUERY_PACKAGE_ID_BYTES && actual == limit + 1
    ));
}

#[test]
fn sqlite_progress_policy_interrupts_selected_package_work() {
    let database = Database::new(":memory:").unwrap();
    let selected = package::Id::from("progress-selected");
    let layouts = (0..4_096)
        .map(|index| regular(format!("share/progress/{index}")))
        .collect::<Vec<_>>();
    database
        .batch_add(layouts.iter().map(|layout| (&selected, layout)))
        .unwrap();

    let outcome = database
        .query_bounded_impl(
            slice::from_ref(&selected),
            QueryBounds {
                max_rows: layouts.len(),
                max_string_bytes: usize::MAX,
            },
            QUERY_PROGRESS_VM_OPS as u64,
            || true,
            |_| {},
        )
        .unwrap();
    assert!(matches!(outcome, BoundedQueryOutcome::Cancelled));
}

#[test]
fn absent_selected_package_does_not_scan_many_unrelated_rows() {
    let database = Database::new(":memory:").unwrap();
    let unrelated = package::Id::from("unrelated");
    let layouts = (0..8_192)
        .map(|index| regular(format!("share/unrelated/{index}")))
        .collect::<Vec<_>>();
    database
        .batch_add(layouts.iter().map(|layout| (&unrelated, layout)))
        .unwrap();
    let absent = package::Id::from("absent");

    let outcome = database
        .query_bounded_impl(
            slice::from_ref(&absent),
            QueryBounds {
                max_rows: 1,
                max_string_bytes: 1,
            },
            2_000,
            || true,
            |_| {},
        )
        .unwrap();
    assert!(matches!(outcome, BoundedQueryOutcome::Complete(rows) if rows.is_empty()));
}

#[test]
fn two_pass_query_keeps_one_snapshot_across_external_commit() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("layout.sqlite");
    let url = path.to_str().unwrap();
    let database = Database::new(url).unwrap();
    database
        .conn
        .exec(|connection| connection.batch_execute("PRAGMA journal_mode = WAL").unwrap());
    let writer = Database::new(url).unwrap();
    let selected = package::Id::from("snapshot-selected");
    let old_path = "share/old";
    let new_path = "share/new";
    database.add(&selected, &regular(old_path)).unwrap();
    let exact = layout_bytes(&selected, old_path);

    let outcome = database
        .query_bounded_impl(
            slice::from_ref(&selected),
            QueryBounds {
                max_rows: 1,
                max_string_bytes: exact,
            },
            MAX_BOUNDED_QUERY_VM_OPS,
            || true,
            |_| writer.add(&selected, &regular(new_path)).unwrap(),
        )
        .unwrap();
    assert!(matches!(
        outcome,
        BoundedQueryOutcome::Complete(rows)
            if matches!(&rows[0].1.file, StonePayloadLayoutFile::Regular(_, path) if path.as_str() == old_path)
    ));
    let current = database.query(slice::from_ref(&selected)).unwrap();
    assert!(matches!(
        &current[0].1.file,
        StonePayloadLayoutFile::Regular(_, path) if path.as_str() == new_path
    ));
}

#[test]
fn materialization_reaccounts_changed_rows_and_rolls_back_test_mutation() {
    let database = Database::new(":memory:").unwrap();
    let selected = package::Id::from("accounting-selected");
    let old_path = "a";
    let changed_path = "a-much-longer-path";
    database.add(&selected, &regular(old_path)).unwrap();
    let exact = layout_bytes(&selected, old_path);

    let outcome = database
        .query_bounded_impl(
            slice::from_ref(&selected),
            QueryBounds {
                max_rows: 1,
                max_string_bytes: exact,
            },
            MAX_BOUNDED_QUERY_VM_OPS,
            || true,
            |connection| {
                raw_execute(
                    connection,
                    &format!("UPDATE layout SET entry_value2 = '{changed_path}'"),
                    "inject accounting test mutation",
                )
                .unwrap();
            },
        )
        .unwrap();
    assert!(matches!(
        outcome,
        BoundedQueryOutcome::StringByteLimit { limit, actual }
            if limit == exact && actual == layout_bytes(&selected, changed_path)
    ));
    let current = database.query(slice::from_ref(&selected)).unwrap();
    assert!(matches!(
        &current[0].1.file,
        StonePayloadLayoutFile::Regular(_, path) if path.as_str() == old_path
    ));
}
