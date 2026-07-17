use diesel::{
    ExpressionMethods as _, QueryDsl as _, RunQueryDsl as _,
    connection::SimpleConnection as _,
    sql_types::{BigInt, Binary, Integer, Nullable, Text},
};

use crate::{
    State, package,
    state::{Id, Selection, TransitionId},
};

use super::super::super::{
    Database, ExactFreshTransitionObservation, ExactFreshTransitionPreimage, MetadataProvenance, model,
    schema::{state, state_metadata_provenance, state_selections},
};

pub(super) struct Fixture {
    pub(super) database: Database,
    pub(super) state: State,
    pub(super) transition: TransitionId,
    pub(super) provenance: MetadataProvenance,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct RelationCounts {
    pub(super) states: i64,
    pub(super) selections: i64,
    pub(super) provenance: i64,
}

#[derive(Clone, Copy, Debug)]
pub(super) enum DurableMutation {
    StateId,
    Summary,
    Description,
    Created,
    Kind,
    Selections,
    Provenance,
    Transition,
}

impl DurableMutation {
    pub(super) const ALL: [Self; 8] = [
        Self::StateId,
        Self::Summary,
        Self::Description,
        Self::Created,
        Self::Kind,
        Self::Selections,
        Self::Provenance,
        Self::Transition,
    ];
}

pub(super) fn transition(digit: char) -> TransitionId {
    TransitionId::parse(digit.to_string().repeat(TransitionId::TEXT_LENGTH)).unwrap()
}

pub(super) fn provenance(label: &str) -> MetadataProvenance {
    MetadataProvenance::from_outputs(
        format!("NAME={label}\nID={label}\n").as_bytes(),
        format!("let system = {{ hostname = \"{label}\" }} in system\n").as_bytes(),
    )
}

pub(super) fn fixture(digit: char, label: &str) -> Fixture {
    fixture_in(Database::new(":memory:").unwrap(), digit, label)
}

pub(super) fn fixture_in(database: Database, digit: char, label: &str) -> Fixture {
    let transition = transition(digit);
    let selections = [
        Selection::explicit(package::Id::from(format!("{label}-explicit"))),
        Selection::transitive(package::Id::from(format!("{label}-dependency"))).reason(format!("required by {label}")),
    ];
    let state = database
        .add_with_transition(
            &transition,
            &selections,
            Some(label),
            Some(&format!("complete state for {label}")),
        )
        .unwrap();
    let provenance = provenance(label);
    database
        .insert_fresh_metadata_provenance_if_transition_matches(state.id, &transition, &provenance)
        .unwrap();
    Fixture {
        database,
        state,
        transition,
        provenance,
    }
}

pub(super) fn normalize_created(fixture: &mut Fixture, timestamp: i64) {
    fixture.database.conn.exec(|connection| {
        diesel::sql_query("UPDATE state SET created = ? WHERE id = ?")
            .bind::<BigInt, _>(timestamp)
            .bind::<Integer, _>(i32::from(fixture.state.id))
            .execute(connection)
            .unwrap();
    });
    fixture.state = fixture.database.get(fixture.state.id).unwrap();
}

pub(super) fn present(fixture: &Fixture) -> ExactFreshTransitionPreimage {
    match fixture
        .database
        .inspect_exact_fresh_transition(fixture.state.id, &fixture.transition)
        .unwrap()
    {
        ExactFreshTransitionObservation::Present(preimage) => preimage,
        ExactFreshTransitionObservation::JointlyAbsent(absence) => {
            panic!("fixture state {} was jointly absent", absence.state_id())
        }
    }
}

pub(super) fn relation_counts(database: &Database, state_id: Id) -> RelationCounts {
    database.conn.exec(|connection| {
        let states = state::table
            .filter(state::id.eq(i32::from(state_id)))
            .count()
            .get_result(connection)
            .unwrap();
        let selections = state_selections::table
            .filter(state_selections::state_id.eq(i32::from(state_id)))
            .count()
            .get_result(connection)
            .unwrap();
        let provenance = state_metadata_provenance::table
            .filter(state_metadata_provenance::state_id.eq(i32::from(state_id)))
            .count()
            .get_result(connection)
            .unwrap();
        RelationCounts {
            states,
            selections,
            provenance,
        }
    })
}

pub(super) fn all_relation_counts(database: &Database) -> RelationCounts {
    database.conn.exec(|connection| RelationCounts {
        states: state::table.count().get_result(connection).unwrap(),
        selections: state_selections::table.count().get_result(connection).unwrap(),
        provenance: state_metadata_provenance::table.count().get_result(connection).unwrap(),
    })
}

pub(super) fn delete_provenance(database: &Database, state_id: Id) {
    database
        .conn
        .exclusive_tx(|connection| {
            diesel::delete(
                state_metadata_provenance::table.filter(state_metadata_provenance::state_id.eq(i32::from(state_id))),
            )
            .execute(connection)?;
            Ok::<_, diesel::result::Error>(())
        })
        .unwrap();
}

pub(super) fn delete_state_but_leave_children(database: &Database, state_id: Id) {
    database.conn.exec(|connection| {
        connection.batch_execute("PRAGMA foreign_keys = OFF").unwrap();
        diesel::delete(state::table.filter(state::id.eq(i32::from(state_id))))
            .execute(connection)
            .unwrap();
    });
}

pub(super) fn delete_state_and_selections_but_leave_provenance(database: &Database, state_id: Id) {
    database.conn.exec(|connection| {
        connection.batch_execute("PRAGMA foreign_keys = OFF").unwrap();
        diesel::delete(state_selections::table.filter(state_selections::state_id.eq(i32::from(state_id))))
            .execute(connection)
            .unwrap();
        diesel::delete(state::table.filter(state::id.eq(i32::from(state_id))))
            .execute(connection)
            .unwrap();
    });
}

pub(super) fn disable_foreign_keys(database: &Database) {
    database.conn.exec(|connection| {
        connection.batch_execute("PRAGMA foreign_keys = OFF").unwrap();
    });
}

pub(super) fn delete_state_and_provenance_but_leave_selections(database: &Database, state_id: Id) {
    database.conn.exec(|connection| {
        connection.batch_execute("PRAGMA foreign_keys = OFF").unwrap();
        diesel::delete(
            state_metadata_provenance::table.filter(state_metadata_provenance::state_id.eq(i32::from(state_id))),
        )
        .execute(connection)
        .unwrap();
        diesel::delete(state::table.filter(state::id.eq(i32::from(state_id))))
            .execute(connection)
            .unwrap();
    });
}

pub(super) fn delete_complete_transition(database: &Database, state_id: Id) {
    database.conn.exec(|connection| {
        connection.batch_execute("PRAGMA foreign_keys = OFF").unwrap();
        diesel::delete(state_selections::table.filter(state_selections::state_id.eq(i32::from(state_id))))
            .execute(connection)
            .unwrap();
        diesel::delete(
            state_metadata_provenance::table.filter(state_metadata_provenance::state_id.eq(i32::from(state_id))),
        )
        .execute(connection)
        .unwrap();
        diesel::delete(state::table.filter(state::id.eq(i32::from(state_id))))
            .execute(connection)
            .unwrap();
    });
}

pub(super) fn corrupt_transition_text(database: &Database, state_id: Id) {
    database.conn.exec(|connection| {
        connection
            .batch_execute("PRAGMA ignore_check_constraints = ON")
            .unwrap();
        diesel::sql_query("UPDATE state SET transition_id = 'NOT-CANONICAL' WHERE id = ?")
            .bind::<Integer, _>(i32::from(state_id))
            .execute(connection)
            .unwrap();
        connection
            .batch_execute("PRAGMA ignore_check_constraints = OFF")
            .unwrap();
    });
}

pub(super) fn apply_mutation(fixture: &Fixture, mutation: DurableMutation) {
    let state_id = i32::from(fixture.state.id);
    let replacement_transition = transition('f');
    fixture.database.conn.exec(|connection| match mutation {
        DurableMutation::StateId => {
            connection.batch_execute("PRAGMA foreign_keys = OFF").unwrap();
            diesel::sql_query("UPDATE state SET id = ? WHERE id = ?")
                .bind::<Integer, _>(state_id + 10_000)
                .bind::<Integer, _>(state_id)
                .execute(connection)
                .unwrap();
        }
        DurableMutation::Summary => {
            diesel::sql_query("UPDATE state SET summary = ? WHERE id = ?")
                .bind::<Nullable<Text>, _>(Some("changed summary"))
                .bind::<Integer, _>(state_id)
                .execute(connection)
                .unwrap();
        }
        DurableMutation::Description => {
            diesel::sql_query("UPDATE state SET description = ? WHERE id = ?")
                .bind::<Nullable<Text>, _>(Some("changed description"))
                .bind::<Integer, _>(state_id)
                .execute(connection)
                .unwrap();
        }
        DurableMutation::Created => {
            diesel::sql_query("UPDATE state SET created = created + ? WHERE id = ?")
                .bind::<BigInt, _>(60_i64)
                .bind::<Integer, _>(state_id)
                .execute(connection)
                .unwrap();
        }
        DurableMutation::Kind => {
            diesel::sql_query("UPDATE state SET type = ? WHERE id = ?")
                .bind::<Text, _>("changed-kind")
                .bind::<Integer, _>(state_id)
                .execute(connection)
                .unwrap();
        }
        DurableMutation::Selections => {
            diesel::sql_query(
                "UPDATE state_selections \
                 SET explicit = 0, reason = ? WHERE state_id = ?",
            )
            .bind::<Nullable<Text>, _>(Some("changed reason"))
            .bind::<Integer, _>(state_id)
            .execute(connection)
            .unwrap();
        }
        DurableMutation::Provenance => {
            diesel::sql_query(
                "UPDATE state_metadata_provenance \
                 SET os_release_sha256 = ? WHERE state_id = ?",
            )
            .bind::<Binary, _>([0x5a_u8; 32].as_slice())
            .bind::<Integer, _>(state_id)
            .execute(connection)
            .unwrap();
        }
        DurableMutation::Transition => {
            diesel::sql_query("UPDATE state SET transition_id = ? WHERE id = ?")
                .bind::<Text, _>(replacement_transition.as_str())
                .bind::<Integer, _>(state_id)
                .execute(connection)
                .unwrap();
        }
    });
}

pub(super) fn replace_with_changed_preimage(database: &Database, preimage: &ExactFreshTransitionPreimage) {
    let state = preimage.state();
    let state_id = i32::from(state.id);
    database
        .conn
        .exclusive_tx(|connection| {
            diesel::delete(state_selections::table.filter(state_selections::state_id.eq(state_id)))
                .execute(connection)?;
            diesel::delete(state_metadata_provenance::table.filter(state_metadata_provenance::state_id.eq(state_id)))
                .execute(connection)?;
            diesel::delete(state::table.filter(state::id.eq(state_id))).execute(connection)?;

            diesel::insert_into(state::table)
                .values((
                    state::id.eq(state_id),
                    state::type_.eq(state.kind.to_string()),
                    state::created.eq(state.created.timestamp()),
                    state::summary.eq(Some("replacement state")),
                    state::description.eq(state.description.clone()),
                    state::transition_id.eq(Some(preimage.transition_id().as_str())),
                ))
                .execute(connection)?;
            let selection = model::NewSelection {
                state_id,
                package_id: "replacement-package",
                explicit: true,
                reason: Some("independent replacement"),
            };
            diesel::insert_into(state_selections::table)
                .values(selection)
                .execute(connection)?;
            super::super::super::metadata_provenance::insert_metadata_provenance_row(
                connection,
                state.id,
                preimage.metadata_provenance(),
            )
            .map_err(|source| match source {
                super::super::super::MetadataProvenanceError::Database(source) => source,
                other => panic!("unexpected exact provenance restoration error: {other}"),
            })?;
            Ok::<_, crate::db::Error>(())
        })
        .unwrap();
}
