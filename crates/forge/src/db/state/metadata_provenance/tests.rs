use diesel::{
    RunQueryDsl as _,
    connection::SimpleConnection as _,
    sql_types::{Binary, Integer, Text},
};
use diesel_migrations::MigrationHarness as _;

use super::*;
use crate::{package, state::Selection};

fn transition(digit: char) -> TransitionId {
    TransitionId::parse(digit.to_string().repeat(TransitionId::TEXT_LENGTH)).unwrap()
}

fn provenance(label: &str) -> MetadataProvenance {
    MetadataProvenance::from_outputs(
        format!("NAME={label}\n").as_bytes(),
        format!("// generated {label}\n").as_bytes(),
    )
}

fn add_correlated(database: &Database, digit: char, label: &str) -> (crate::State, TransitionId, MetadataProvenance) {
    let transition = transition(digit);
    let state = database
        .add_with_transition(
            &transition,
            &[Selection::explicit(package::Id::from(format!("package-{label}")))],
            Some(label),
            None,
        )
        .unwrap();
    let provenance = provenance(label);
    database
        .insert_fresh_metadata_provenance_if_transition_matches(state.id, &transition, &provenance)
        .unwrap();
    (state, transition, provenance)
}

#[test]
fn migration_leaves_preexisting_state_without_provenance() {
    let database = Database::new(":memory:").unwrap();
    database.conn.exec(|connection| {
        connection.revert_last_migration(super::super::MIGRATIONS).unwrap();
        diesel::sql_query("INSERT INTO state (\"type\", summary) VALUES ('transaction', 'pre-provenance state')")
            .execute(connection)
            .unwrap();
        connection.run_pending_migrations(super::super::MIGRATIONS).unwrap();
    });

    let state = database.get(Id::from(1)).unwrap();
    assert_eq!(state.summary.as_deref(), Some("pre-provenance state"));
    assert_eq!(database.metadata_provenance(state.id).unwrap(), None);
    assert!(matches!(
        database.required_metadata_provenance(state.id),
        Err(MetadataProvenanceError::Missing { state_id: 1 })
    ));
}

#[test]
fn provenance_round_trips_as_one_labeled_immutable_pair() {
    let database = Database::new(":memory:").unwrap();
    let (state, _transition, expected) = add_correlated(&database, '1', "round-trip");

    assert_eq!(database.metadata_provenance(state.id).unwrap(), Some(expected));
    assert_eq!(database.required_metadata_provenance(state.id).unwrap(), expected);
    database.require_exact_metadata_provenance(state.id, &expected).unwrap();
    assert!(expected.matches_os_release(b"NAME=round-trip\n"));
    assert!(expected.matches_system_model(b"// generated round-trip\n"));
    assert!(!expected.matches_os_release(b"// generated round-trip\n"));
    assert!(!expected.matches_system_model(b"NAME=round-trip\n"));

    let different = provenance("different");
    assert!(matches!(
        database.require_exact_metadata_provenance(state.id, &different),
        Err(MetadataProvenanceError::Mismatch { state_id }) if state_id == i32::from(state.id)
    ));
}

#[test]
fn fresh_insert_requires_exact_transition_and_never_adopts_or_replaces() {
    let database = Database::new(":memory:").unwrap();
    let expected = provenance("exact-transition");
    let requested = transition('2');

    assert!(matches!(
        database.insert_fresh_metadata_provenance_if_transition_matches(Id::from(9_999), &requested, &expected),
        Err(MetadataProvenanceError::FreshTransitionMismatch {
            ownership: TransitionOwnership::Missing,
            ..
        })
    ));

    let cleared = database.add(&[], Some("cleared"), None).unwrap();
    assert!(matches!(
        database.insert_fresh_metadata_provenance_if_transition_matches(cleared.id, &requested, &expected),
        Err(MetadataProvenanceError::FreshTransitionMismatch {
            ownership: TransitionOwnership::Cleared,
            ..
        })
    ));
    assert_eq!(database.metadata_provenance(cleared.id).unwrap(), None);

    let foreign_transition = transition('3');
    let foreign = database
        .add_with_transition(&foreign_transition, &[], Some("foreign"), None)
        .unwrap();
    assert!(matches!(
        database.insert_fresh_metadata_provenance_if_transition_matches(foreign.id, &requested, &expected),
        Err(MetadataProvenanceError::FreshTransitionMismatch {
            ownership: TransitionOwnership::Foreign,
            ..
        })
    ));
    assert_eq!(database.metadata_provenance(foreign.id).unwrap(), None);

    database
        .insert_fresh_metadata_provenance_if_transition_matches(foreign.id, &foreign_transition, &expected)
        .unwrap();
    for second in [expected, provenance("replacement")] {
        assert!(matches!(
            database.insert_fresh_metadata_provenance_if_transition_matches(
                foreign.id,
                &foreign_transition,
                &second,
            ),
            Err(MetadataProvenanceError::AlreadyExists { state_id }) if state_id == i32::from(foreign.id)
        ));
    }
    assert_eq!(database.required_metadata_provenance(foreign.id).unwrap(), expected);
}

#[test]
fn sql_constraints_reject_partial_non_blob_and_wrong_length_pairs() {
    let database = Database::new(":memory:").unwrap();
    let state = database.add(&[], Some("constraint target"), None).unwrap();
    let state_id = i32::from(state.id);
    let digest = [7_u8; SHA256_BYTES];

    database.conn.exec(|connection| {
        let partial =
            diesel::sql_query("INSERT INTO state_metadata_provenance (state_id, os_release_sha256) VALUES (?, ?)")
                .bind::<Integer, _>(state_id)
                .bind::<Binary, _>(digest.as_slice())
                .execute(connection);
        assert!(partial.is_err());

        let text = "0".repeat(SHA256_BYTES);
        let non_blob = diesel::sql_query(
            "INSERT INTO state_metadata_provenance \
             (state_id, os_release_sha256, system_model_sha256) VALUES (?, ?, ?)",
        )
        .bind::<Integer, _>(state_id)
        .bind::<Text, _>(&text)
        .bind::<Binary, _>(digest.as_slice())
        .execute(connection);
        assert!(non_blob.is_err());

        for wrong in [vec![], vec![1_u8; SHA256_BYTES - 1], vec![1_u8; SHA256_BYTES + 1]] {
            let wrong_length = diesel::sql_query(
                "INSERT INTO state_metadata_provenance \
                 (state_id, os_release_sha256, system_model_sha256) VALUES (?, ?, ?)",
            )
            .bind::<Integer, _>(state_id)
            .bind::<Binary, _>(&wrong)
            .bind::<Binary, _>(digest.as_slice())
            .execute(connection);
            assert!(wrong_length.is_err());
        }
    });
    assert_eq!(database.metadata_provenance(state.id).unwrap(), None);
}

#[test]
fn bypassed_length_constraint_is_rejected_during_typed_decode() {
    let database = Database::new(":memory:").unwrap();
    let state = database.add(&[], Some("corrupt provenance"), None).unwrap();
    database.conn.exec(|connection| {
        connection
            .batch_execute("PRAGMA ignore_check_constraints = ON")
            .unwrap();
        diesel::sql_query(
            "INSERT INTO state_metadata_provenance \
             (state_id, os_release_sha256, system_model_sha256) VALUES (?, ?, ?)",
        )
        .bind::<Integer, _>(i32::from(state.id))
        .bind::<Binary, _>([1_u8; SHA256_BYTES - 1].as_slice())
        .bind::<Binary, _>([2_u8; SHA256_BYTES].as_slice())
        .execute(connection)
        .unwrap();
        connection
            .batch_execute("PRAGMA ignore_check_constraints = OFF")
            .unwrap();
    });

    assert!(matches!(
        database.metadata_provenance(state.id),
        Err(MetadataProvenanceError::InvalidStoredDigestLength {
            state_id,
            field: "os_release_sha256",
            actual: 31,
        }) if state_id == i32::from(state.id)
    ));
}

#[test]
fn every_state_removal_path_explicitly_deletes_provenance() {
    let database = Database::new(":memory:").unwrap();

    let (correlated, transition, _) = add_correlated(&database, '4', "correlated-remove");
    database
        .remove_transition_if_matches(correlated.id, &transition)
        .unwrap();
    assert!(database.get(correlated.id).is_err());
    assert_eq!(database.metadata_provenance(correlated.id).unwrap(), None);

    let (ordinary, ordinary_transition, _) = add_correlated(&database, '5', "ordinary-remove");
    database
        .clear_transition_if_matches(ordinary.id, &ordinary_transition)
        .unwrap();
    database.remove(&ordinary.id).unwrap();
    assert_eq!(database.metadata_provenance(ordinary.id).unwrap(), None);

    let (batch_first, first_transition, _) = add_correlated(&database, '6', "batch-first");
    let (batch_second, second_transition, _) = add_correlated(&database, '7', "batch-second");
    database
        .clear_transition_if_matches(batch_first.id, &first_transition)
        .unwrap();
    database
        .clear_transition_if_matches(batch_second.id, &second_transition)
        .unwrap();
    database.batch_remove([&batch_first.id, &batch_second.id]).unwrap();
    assert_eq!(database.metadata_provenance(batch_first.id).unwrap(), None);
    assert_eq!(database.metadata_provenance(batch_second.id).unwrap(), None);

    let (archived, archived_transition, _) = add_correlated(&database, '8', "exact-archived-remove");
    database
        .clear_transition_if_matches(archived.id, &archived_transition)
        .unwrap();
    database.remove_exact_archived(std::slice::from_ref(&archived)).unwrap();
    assert_eq!(database.metadata_provenance(archived.id).unwrap(), None);
}

#[test]
fn rejected_removal_preserves_the_exact_provenance_pair() {
    let database = Database::new(":memory:").unwrap();
    let (state, exact_transition, expected) = add_correlated(&database, '9', "rejected-remove");

    assert!(
        database
            .remove_transition_if_matches(state.id, &transition('a'))
            .is_err()
    );
    assert_eq!(database.required_metadata_provenance(state.id).unwrap(), expected);

    database
        .clear_transition_if_matches(state.id, &exact_transition)
        .unwrap();
    let mut changed = state.clone();
    changed.summary = Some("changed snapshot".to_owned());
    assert!(database.remove_exact_archived(std::slice::from_ref(&changed)).is_err());
    assert_eq!(database.required_metadata_provenance(state.id).unwrap(), expected);
}

#[test]
fn fault_injection_restores_provenance_with_the_exact_archived_state() {
    let database = Database::new(":memory:").unwrap();
    let (state, transition, expected) = add_correlated(&database, 'b', "restored-exact-archive");
    database.clear_transition_if_matches(state.id, &transition).unwrap();

    super::super::arm_exact_archived_removal_fault(
        super::super::ExactArchivedRemovalFault::ReportAfterCommitAndRestoreFirst,
    );
    let error = database
        .remove_exact_archived(std::slice::from_ref(&state))
        .unwrap_err();

    assert!(error.definitely_not_applied());
    assert_eq!(database.get(state.id).unwrap(), state);
    assert_eq!(database.required_metadata_provenance(state.id).unwrap(), expected);
}
