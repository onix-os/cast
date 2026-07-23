//! Bounded state queries over a structurally read-only SQLite handle.

use chrono::{DateTime, Utc};
use thiserror::Error;

use crate::{
    Installation, State,
    db::{ReadOnlyConnection, ReadOnlyError, ReadOnlyStep},
    installation::DatabaseKind,
    package,
    state::{self, Id, Selection, TransitionId},
};

use super::{MAX_SELECTION_TEXT_BYTES, MAX_SELECTIONS_PER_STATE, MAX_STATE_DATABASE_TEXT_FIELD_BYTES};

const MAX_STATES: usize = 4_096;

#[derive(Clone, Debug)]
pub(crate) struct ReadOnlyDatabase {
    connection: ReadOnlyConnection,
}

impl ReadOnlyDatabase {
    pub(crate) fn open(installation: &Installation) -> Result<Self, ReadOnlyStateError> {
        Ok(Self {
            connection: ReadOnlyConnection::open(installation, DatabaseKind::State)?,
        })
    }

    pub(crate) fn revalidate(&self, installation: &Installation) -> Result<(), ReadOnlyStateError> {
        installation.revalidate_read_only_database(self.connection.anchor())?;
        Ok(())
    }

    pub(crate) fn list_ids(&self) -> Result<Vec<Id>, ReadOnlyStateError> {
        self.connection
            .snapshot(|row| {
                let mut statement = row.prepare(c"SELECT id FROM state ORDER BY id LIMIT 4097")?;
                let mut states = Vec::new();
                while statement.step()? == ReadOnlyStep::Row {
                    if states.len() == MAX_STATES {
                        return Err(ReadOnlyError::Limit {
                            resource: "states",
                            limit: MAX_STATES,
                        });
                    }
                    states.push(parse_state_id(statement.i64(0)?)?);
                }
                Ok(states)
            })
            .map_err(Into::into)
    }

    pub(crate) fn get(&self, id: Id) -> Result<Option<State>, ReadOnlyStateError> {
        self.connection
            .snapshot(|row| {
                let mut state = row.prepare(
                    c"SELECT id, created, summary, description, \"type\" FROM state WHERE id = ?1 LIMIT 2",
                )?;
                state.bind_i64(1, i64::from(i32::from(id)))?;
                if state.step()? == ReadOnlyStep::Done {
                    return Ok(None);
                }
                let observed = parse_state_id(state.i64(0)?)?;
                if observed != id {
                    return Err(ReadOnlyError::Policy {
                        context: "state primary-key lookup returned another identifier",
                    });
                }
                let created = timestamp(state.i64(1)?)?;
                let summary = state.nullable_text(2, MAX_STATE_DATABASE_TEXT_FIELD_BYTES)?;
                let description = state.nullable_text(3, MAX_STATE_DATABASE_TEXT_FIELD_BYTES)?;
                let kind = state
                    .text(4, MAX_STATE_DATABASE_TEXT_FIELD_BYTES)?
                    .parse::<state::Kind>()
                    .map_err(|_| ReadOnlyError::Policy {
                        context: "state row has an unsupported type",
                    })?;
                if state.step()? != ReadOnlyStep::Done {
                    return Err(ReadOnlyError::Policy {
                        context: "state primary-key lookup returned duplicate rows",
                    });
                }
                drop(state);

                let mut selected = row.prepare(
                    c"SELECT package_id, explicit, reason FROM state_selections WHERE state_id = ?1 ORDER BY package_id LIMIT 32769",
                )?;
                selected.bind_i64(1, i64::from(i32::from(id)))?;
                let mut selections = Vec::new();
                let mut text_bytes = 0usize;
                while selected.step()? == ReadOnlyStep::Row {
                    if selections.len() == MAX_SELECTIONS_PER_STATE {
                        return Err(ReadOnlyError::Limit {
                            resource: "state selections",
                            limit: MAX_SELECTIONS_PER_STATE,
                        });
                    }
                    let package = selected.text(0, MAX_STATE_DATABASE_TEXT_FIELD_BYTES)?;
                    let reason = selected.nullable_text(2, MAX_STATE_DATABASE_TEXT_FIELD_BYTES)?;
                    text_bytes = text_bytes
                        .checked_add(package.len())
                        .and_then(|bytes| bytes.checked_add(reason.as_ref().map_or(0, String::len)))
                        .ok_or(ReadOnlyError::Limit {
                            resource: "state selection text bytes",
                            limit: MAX_SELECTION_TEXT_BYTES,
                        })?;
                    if text_bytes > MAX_SELECTION_TEXT_BYTES {
                        return Err(ReadOnlyError::Limit {
                            resource: "state selection text bytes",
                            limit: MAX_SELECTION_TEXT_BYTES,
                        });
                    }
                    selections.push(Selection {
                        package: package::Id::from(package),
                        explicit: selected.bool(1)?,
                        reason,
                    });
                }

                Ok(Some(State {
                    id,
                    summary,
                    description,
                    selections,
                    created,
                    kind,
                }))
            })
            .map_err(Into::into)
    }

    pub(crate) fn audit_in_flight_transition(&self) -> Result<Option<super::InFlightTransition>, ReadOnlyStateError> {
        self.connection
            .snapshot(|row| {
                let mut statement = row.prepare(
                    c"SELECT id, transition_id FROM state WHERE transition_id IS NOT NULL ORDER BY id LIMIT 2",
                )?;
                let mut transition = None;
                while statement.step()? == ReadOnlyStep::Row {
                    if transition.is_some() {
                        return Err(ReadOnlyError::Policy {
                            context: "state database contains multiple in-flight transitions",
                        });
                    }
                    let state_id = parse_state_id(statement.i64(0)?)?;
                    let transition_id =
                        TransitionId::parse(statement.text(1, TransitionId::TEXT_LENGTH)?).map_err(|_| {
                            ReadOnlyError::Policy {
                                context: "state database contains a noncanonical transition identifier",
                            }
                        })?;
                    transition = Some(super::InFlightTransition {
                        state_id,
                        transition_id,
                    });
                }
                Ok(transition)
            })
            .map_err(Into::into)
    }
}

fn parse_state_id(value: i64) -> Result<Id, ReadOnlyError> {
    let value = i32::try_from(value).map_err(|_| ReadOnlyError::Policy {
        context: "state identifier is outside the supported range",
    })?;
    if value <= 0 {
        return Err(ReadOnlyError::Policy {
            context: "state identifier is not positive",
        });
    }
    Ok(Id::from(value))
}

fn timestamp(value: i64) -> Result<DateTime<Utc>, ReadOnlyError> {
    DateTime::from_timestamp(value, 0).ok_or(ReadOnlyError::Policy {
        context: "state creation timestamp is invalid",
    })
}

#[derive(Debug, Error)]
pub(crate) enum ReadOnlyStateError {
    #[error(transparent)]
    Database(#[from] ReadOnlyError),
    #[error(transparent)]
    Installation(#[from] crate::installation::Error),
}
