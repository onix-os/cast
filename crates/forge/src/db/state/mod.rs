// SPDX-FileCopyrightText: 2023 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use chrono::{DateTime, Utc};
use diesel::prelude::*;
use diesel::{Connection as _, SqliteConnection};
use diesel_migrations::{EmbeddedMigrations, MigrationHarness, embed_migrations};
use itertools::Itertools;

use super::{Connection, Error, MAX_VARIABLE_NUMBER};
use crate::State;
use crate::state::{self, Id, Selection, TransitionId};

const MIGRATIONS: EmbeddedMigrations = embed_migrations!("src/db/state/migrations");

mod schema;

#[derive(Debug, Clone)]
pub struct Database {
    conn: Connection,
}

impl Database {
    pub fn new(url: &str) -> Result<Self, Error> {
        let mut conn = SqliteConnection::establish(url)?;

        conn.run_pending_migrations(MIGRATIONS).map_err(Error::Migration)?;

        Ok(Database {
            conn: Connection::new(conn),
        })
    }

    pub fn list_ids(&self) -> Result<Vec<(Id, DateTime<Utc>)>, Error> {
        self.conn.exec(|conn| {
            model::state::table
                .select(model::Created::as_select())
                .load_iter(conn)?
                .map(|result| {
                    let row = result?;
                    Ok((row.id.into(), row.created.0))
                })
                .collect()
        })
    }

    pub fn all(&self) -> Result<Vec<State>, Error> {
        self.conn.exec(|conn| {
            let states = model::state::table
                .select(model::State::as_select())
                .load::<model::State>(conn)?;
            let mut selections = model::state_selections::table
                .select(model::Selection::as_select())
                .load::<model::Selection>(conn)?
                .into_iter()
                .map(|row| {
                    (
                        Id::from(row.state_id),
                        Selection {
                            package: row.package_id,
                            explicit: row.explicit,
                            reason: row.reason,
                        },
                    )
                })
                .into_group_map();

            Ok(states
                .into_iter()
                .map(|state| {
                    let id = state.id.into();
                    let selections = selections.remove(&id).unwrap_or_default();
                    State {
                        id,
                        summary: state.summary,
                        description: state.description,
                        selections,
                        created: state.created.0,
                        kind: state.kind,
                    }
                })
                .collect())
        })
    }

    pub fn get(&self, id: Id) -> Result<State, Error> {
        self.conn.exec(|conn| load_state(conn, id))
    }

    /// Look up the fresh state durably correlated with one in-flight
    /// transition.
    #[allow(dead_code)] // consumed by the activation-journal integration slice
    pub(crate) fn get_by_transition(&self, transition_id: &TransitionId) -> Result<Option<State>, Error> {
        self.conn.exec(|conn| {
            model::state::table
                .filter(model::state::transition_id.eq(transition_id.as_str()))
                .select(model::State::as_select())
                .first(conn)
                .optional()?
                .map(|state| load_selected_state(conn, state))
                .transpose()
        })
    }

    pub fn add(
        &self,
        selections: &[Selection],
        summary: Option<&str>,
        description: Option<&str>,
    ) -> Result<State, Error> {
        self.add_inner(selections, summary, description, None)
    }

    /// Atomically insert a fresh state and its transition correlation token.
    ///
    /// Recovery uses [`Self::get_by_transition`] to distinguish a transaction
    /// which never committed from one whose SQLite commit completed before the
    /// activation journal could record the generated state ID.
    #[allow(dead_code)] // consumed by the activation-journal integration slice
    pub(crate) fn add_with_transition(
        &self,
        transition_id: &TransitionId,
        selections: &[Selection],
        summary: Option<&str>,
        description: Option<&str>,
    ) -> Result<State, Error> {
        self.add_inner(selections, summary, description, Some(transition_id))
    }

    fn add_inner(
        &self,
        selections: &[Selection],
        summary: Option<&str>,
        description: Option<&str>,
        transition_id: Option<&TransitionId>,
    ) -> Result<State, Error> {
        self.conn
            .exclusive_tx(|tx| {
                let state = model::NewState {
                    summary,
                    description,
                    kind: state::Kind::Transaction.to_string(),
                    transition_id: transition_id.map(TransitionId::as_str),
                };

                let id = diesel::insert_into(model::state::table)
                    .values(state)
                    .returning(model::state::id)
                    .get_result::<i32>(tx)?;

                let selections = selections
                    .iter()
                    .map(|selection| model::NewSelection {
                        state_id: id,
                        package_id: selection.package.as_str(),
                        explicit: selection.explicit,
                        reason: selection.reason.as_deref(),
                    })
                    .collect::<Vec<_>>();

                for chunk in selections.chunks(MAX_VARIABLE_NUMBER / 4) {
                    diesel::insert_into(model::state_selections::table)
                        .values(chunk)
                        .execute(tx)?;
                }

                Ok(id.into())
            })
            .and_then(|id| self.get(id))
    }

    /// Clear a fresh-state correlation only when both durable identities still
    /// match. A zero-row update is never treated as success.
    #[allow(dead_code)] // consumed by the activation-journal integration slice
    pub(crate) fn clear_transition_if_matches(
        &self,
        state: Id,
        transition_id: &TransitionId,
    ) -> Result<(), TransitionMutationError> {
        let changed = self.conn.exclusive_tx(|tx| {
            diesel::update(
                model::state::table
                    .filter(model::state::id.eq(i32::from(state)))
                    .filter(model::state::transition_id.eq(transition_id.as_str())),
            )
            .set(model::state::transition_id.eq(Option::<&str>::None))
            .execute(tx)
            .map_err(Error::from)
        })?;
        require_one_transition_row(changed, state)
    }

    /// Remove a fresh state only when its state ID and transition correlation
    /// both match the journal's durable evidence.
    #[allow(dead_code)] // consumed by the activation-journal integration slice
    pub(crate) fn remove_transition_if_matches(
        &self,
        state: Id,
        transition_id: &TransitionId,
    ) -> Result<(), TransitionMutationError> {
        let changed = self.conn.exclusive_tx(|tx| {
            diesel::delete(
                model::state::table
                    .filter(model::state::id.eq(i32::from(state)))
                    .filter(model::state::transition_id.eq(transition_id.as_str())),
            )
            .execute(tx)
            .map_err(Error::from)
        })?;
        require_one_transition_row(changed, state)
    }

    pub fn remove(&self, state: &Id) -> Result<(), Error> {
        self.batch_remove(Some(state))
    }

    pub fn batch_remove<'a>(&self, states: impl IntoIterator<Item = &'a Id>) -> Result<(), Error> {
        self.conn.exclusive_tx(|tx| {
            let states = states.into_iter().map(|id| i32::from(*id)).collect::<Vec<_>>();

            for chunk in states.chunks(MAX_VARIABLE_NUMBER) {
                // Cascading wipes other tables
                diesel::delete(model::state::table.filter(model::state::id.eq_any(chunk))).execute(tx)?;
            }

            Ok(())
        })
    }
}

fn load_state(conn: &mut SqliteConnection, id: Id) -> Result<State, Error> {
    let state = model::state::table
        .select(model::State::as_select())
        .find(i32::from(id))
        .first(conn)?;
    load_selected_state(conn, state)
}

fn load_selected_state(conn: &mut SqliteConnection, state: model::State) -> Result<State, Error> {
    let selections = model::Selection::belonging_to(&state)
        .select(model::Selection::as_select())
        .load_iter(conn)?
        .map(|result| {
            let row = result?;
            Ok(Selection {
                package: row.package_id,
                explicit: row.explicit,
                reason: row.reason,
            })
        })
        .collect::<Result<_, Error>>()?;

    Ok(State {
        id: state.id.into(),
        summary: state.summary,
        description: state.description,
        selections,
        created: state.created.0,
        kind: state.kind,
    })
}

#[allow(dead_code)] // consumed by the activation-journal integration slice
fn require_one_transition_row(changed: usize, state: Id) -> Result<(), TransitionMutationError> {
    if changed == 1 {
        Ok(())
    } else {
        Err(TransitionMutationError::Mismatch {
            state_id: i32::from(state),
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum TransitionMutationError {
    #[error("state {state_id} is not correlated with the requested transition")]
    Mismatch { state_id: i32 },
    #[error(transparent)]
    Database(#[from] Error),
}

mod model {
    use astr::AStr;
    use diesel::{
        Selectable,
        associations::{Associations, Identifiable},
        deserialize::Queryable,
        prelude::Insertable,
        sqlite::Sqlite,
    };

    use crate::{db::Timestamp, package, state::Kind};

    pub use super::schema::{state, state_selections};

    #[derive(Queryable, Selectable, Identifiable)]
    #[diesel(table_name = state)]
    #[diesel(check_for_backend(Sqlite))]
    pub struct State {
        pub id: i32,
        #[diesel(deserialize_as = i64)]
        pub created: Timestamp,
        pub summary: Option<String>,
        pub description: Option<String>,
        #[diesel(column_name = "type_", deserialize_as = String)]
        pub kind: Kind,
        pub transition_id: Option<String>,
    }

    #[derive(Queryable, Selectable, Identifiable, Associations)]
    #[diesel(table_name = state_selections)]
    #[diesel(primary_key(state_id, package_id))]
    #[diesel(belongs_to(State))]
    pub struct Selection {
        pub state_id: i32,
        #[diesel(deserialize_as = AStr)]
        pub package_id: package::Id,
        pub explicit: bool,
        pub reason: Option<String>,
    }

    #[derive(Queryable, Selectable, Identifiable)]
    #[diesel(table_name = state)]
    #[diesel(check_for_backend(Sqlite))]
    pub struct Created {
        pub id: i32,
        #[diesel(deserialize_as = i64)]
        pub created: Timestamp,
    }

    #[derive(Insertable)]
    #[diesel(table_name = state)]
    pub struct NewState<'a> {
        pub summary: Option<&'a str>,
        pub description: Option<&'a str>,
        #[diesel(column_name = "type_")]
        pub kind: String,
        pub transition_id: Option<&'a str>,
    }

    #[derive(Insertable)]
    #[diesel(table_name = state_selections)]
    pub struct NewSelection<'a> {
        pub state_id: i32,
        pub package_id: &'a str,
        pub explicit: bool,
        pub reason: Option<&'a str>,
    }
}

#[cfg(test)]
mod test {
    use chrono::Utc;
    use diesel::{RunQueryDsl as _, sql_types::Text};

    use super::*;
    use crate::package;

    fn transition_id(digit: char) -> TransitionId {
        TransitionId::parse(digit.to_string().repeat(TransitionId::TEXT_LENGTH)).unwrap()
    }

    #[test]
    fn create_insert_select() {
        let database = Database::new(":memory:").unwrap();

        let selections = vec![
            Selection::explicit(package::Id::from("pkg a")),
            Selection::explicit(package::Id::from("pkg b")),
            Selection::explicit(package::Id::from("pkg c")),
        ];

        let state = database.add(&selections, Some("test"), Some("test")).unwrap();

        // First record
        assert_eq!(i32::from(state.id), 1);

        // Check created
        let elapsed = Utc::now().signed_duration_since(state.created);
        assert!(elapsed.num_seconds() == 0);
        assert!(!elapsed.is_zero());

        assert_eq!(state.summary.as_deref(), Some("test"));
        assert_eq!(state.description.as_deref(), Some("test"));

        assert_eq!(state.selections, selections);
    }

    #[test]
    fn tokened_state_is_exactly_recoverable_and_ordinary_add_remains_tokenless() {
        let database = Database::new(":memory:").unwrap();
        let ordinary = database.add(&[], Some("ordinary"), None).unwrap();
        let token = transition_id('a');
        assert!(database.get_by_transition(&token).unwrap().is_none());

        let correlated = database
            .add_with_transition(&token, &[], Some("correlated"), Some("in flight"))
            .unwrap();
        assert_ne!(ordinary.id, correlated.id);
        assert_eq!(database.get_by_transition(&token).unwrap(), Some(correlated));
    }

    #[test]
    fn transition_insert_and_selections_commit_as_one_sqlite_transaction() {
        let database = Database::new(":memory:").unwrap();
        let token = transition_id('b');
        let duplicate = Selection::explicit(package::Id::from("duplicate"));

        assert!(
            database
                .add_with_transition(&token, &[duplicate.clone(), duplicate], Some("must roll back"), None)
                .is_err()
        );
        assert!(database.get_by_transition(&token).unwrap().is_none());
        assert!(database.all().unwrap().is_empty());
    }

    #[test]
    fn migration_rejects_noncanonical_and_duplicate_transition_ids() {
        let database = Database::new(":memory:").unwrap();
        let malformed = "g".repeat(TransitionId::TEXT_LENGTH);
        let malformed_insert = database.conn.exec(|conn| {
            diesel::sql_query("INSERT INTO state (\"type\", transition_id) VALUES ('transaction', ?)")
                .bind::<Text, _>(&malformed)
                .execute(conn)
        });
        assert!(malformed_insert.is_err());
        assert!(database.all().unwrap().is_empty());

        // SQLite's text length and GLOB operators stop at U+0000. The CHECK
        // must therefore measure the underlying bytes, or 32 valid digits
        // followed by a NUL and arbitrary suffix would be admitted.
        let nul_suffix_hex = format!("{}0067", "61".repeat(TransitionId::TEXT_LENGTH));
        let nul_suffix_insert = database.conn.exec(|conn| {
            diesel::sql_query(format!(
                "INSERT INTO state (\"type\", transition_id) VALUES ('transaction', CAST(X'{nul_suffix_hex}' AS TEXT))"
            ))
            .execute(conn)
        });
        assert!(nul_suffix_insert.is_err());
        assert!(database.all().unwrap().is_empty());

        let trailing_nul_hex = format!("{}00", "61".repeat(TransitionId::TEXT_LENGTH - 1));
        let trailing_nul_insert = database.conn.exec(|conn| {
            diesel::sql_query(format!(
                "INSERT INTO state (\"type\", transition_id) VALUES ('transaction', CAST(X'{trailing_nul_hex}' AS TEXT))"
            ))
            .execute(conn)
        });
        assert!(trailing_nul_insert.is_err());
        assert!(database.all().unwrap().is_empty());

        let token = transition_id('c');
        let first = database.add_with_transition(&token, &[], Some("first"), None).unwrap();
        assert!(
            database
                .add_with_transition(&token, &[], Some("duplicate"), None)
                .is_err()
        );
        assert_eq!(database.all().unwrap(), vec![first]);
    }

    #[test]
    fn clear_and_remove_require_the_exact_state_and_transition_pair() {
        let database = Database::new(":memory:").unwrap();
        let first_token = transition_id('d');
        let second_token = transition_id('e');
        let first = database
            .add_with_transition(&first_token, &[], Some("first"), None)
            .unwrap();
        let second = database
            .add_with_transition(&second_token, &[], Some("second"), None)
            .unwrap();

        assert!(matches!(
            database.clear_transition_if_matches(first.id, &second_token),
            Err(TransitionMutationError::Mismatch { state_id }) if state_id == i32::from(first.id)
        ));
        assert!(matches!(
            database.remove_transition_if_matches(second.id, &first_token),
            Err(TransitionMutationError::Mismatch { state_id }) if state_id == i32::from(second.id)
        ));
        assert_eq!(database.get_by_transition(&first_token).unwrap(), Some(first.clone()));
        assert_eq!(database.get_by_transition(&second_token).unwrap(), Some(second.clone()));

        database.clear_transition_if_matches(first.id, &first_token).unwrap();
        assert!(database.get_by_transition(&first_token).unwrap().is_none());
        assert_eq!(database.get(first.id).unwrap(), first);
        assert!(matches!(
            database.clear_transition_if_matches(first.id, &first_token),
            Err(TransitionMutationError::Mismatch { .. })
        ));

        database.remove_transition_if_matches(second.id, &second_token).unwrap();
        assert!(database.get_by_transition(&second_token).unwrap().is_none());
        assert!(database.get(second.id).is_err());
        assert!(matches!(
            database.remove_transition_if_matches(second.id, &second_token),
            Err(TransitionMutationError::Mismatch { .. })
        ));
    }
}
