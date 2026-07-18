//! One-snapshot state input for boot projection.

use diesel::{
    SqliteConnection,
    dsl::sql,
    prelude::*,
    sql_types::{BigInt, Nullable},
};

use super::{
    Database, Error, Id, MAX_SELECTION_TEXT_BYTES, MAX_SELECTIONS_PER_STATE, MAX_STATE_DATABASE_TEXT_FIELD_BYTES,
    Selection, State, model,
};

const MAX_BOOT_HISTORY_STATES: usize = 4;
const BOOT_SELECTION_LIMITS: FrozenBootSelectionLimits = FrozenBootSelectionLimits {
    count: MAX_SELECTIONS_PER_STATE,
    text_bytes: MAX_SELECTION_TEXT_BYTES,
};
const BOOT_STATE_TEXT_LIMITS: FrozenBootStateTextLimits = FrozenBootStateTextLimits {
    field_bytes: MAX_STATE_DATABASE_TEXT_FIELD_BYTES,
};

#[cfg(test)]
const BOOT_HISTORY_ORDER_INDEX: &str = "state_boot_history_created_id";

#[derive(Clone, Copy)]
struct FrozenBootSelectionLimits {
    count: usize,
    text_bytes: usize,
}

#[derive(Clone, Copy)]
struct FrozenBootStateTextLimits {
    field_bytes: usize,
}

/// The exact active head followed by a bounded, deterministic history.
///
/// Construction is private so callers cannot manufacture an input whose head
/// is absent from the first position or duplicated in the history.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct FrozenBootInput {
    states: Vec<State>,
}

impl FrozenBootInput {
    pub(crate) fn head(&self) -> &State {
        self.states
            .first()
            .expect("a frozen boot input is constructed with one exact head")
    }

    pub(crate) fn history(&self) -> &[State] {
        &self.states[1..]
    }

    pub(crate) fn states(&self) -> &[State] {
        &self.states
    }

    pub(crate) fn into_states(self) -> Vec<State> {
        self.states
    }
}

impl Database {
    /// Freeze the exact head and bounded rollback history in one SQLite read
    /// transaction.
    ///
    /// History never assumes that a greater state ID is the active head. It
    /// excludes the requested head explicitly and uses creation time followed
    /// by state ID as a total, deterministic order backed by the matching
    /// `state_boot_history_created_id` index. State text and selection text are
    /// admitted by numeric byte-length preflights before any of those strings
    /// are materialized, and selections are returned in package-ID order.
    pub(crate) fn frozen_boot_input(&self, head: Id) -> Result<FrozenBootInput, FrozenBootInputError> {
        self.frozen_boot_input_with_limits(head, BOOT_SELECTION_LIMITS, BOOT_STATE_TEXT_LIMITS)
    }

    fn frozen_boot_input_with_limits(
        &self,
        head: Id,
        selection_limits: FrozenBootSelectionLimits,
        state_text_limits: FrozenBootStateTextLimits,
    ) -> Result<FrozenBootInput, FrozenBootInputError> {
        self.conn.exec(|conn| {
            conn.transaction(|tx| {
                let head_id = i32::from(head);
                let head_id = model::state::table
                    .find(head_id)
                    .select(model::state::id)
                    .first(tx)
                    .map_err(|error| match error {
                        diesel::result::Error::NotFound => FrozenBootInputError::MissingHead { state_id: head_id },
                        error => FrozenBootInputError::from(error),
                    })?;

                let history = model::state::table
                    .filter(model::state::id.ne(head_id))
                    .order((model::state::created.desc(), model::state::id.desc()))
                    .limit(MAX_BOOT_HISTORY_STATES as i64)
                    .select(model::state::id)
                    .load::<i32>(tx)?;

                let mut states = Vec::with_capacity(1 + history.len());
                states.push(load_bounded_state(tx, head_id, selection_limits, state_text_limits)?);
                for state_id in history {
                    states.push(load_bounded_state(tx, state_id, selection_limits, state_text_limits)?);
                }

                Ok(FrozenBootInput { states })
            })
        })
    }
}

fn load_bounded_state(
    connection: &mut SqliteConnection,
    state_id: i32,
    selection_limits: FrozenBootSelectionLimits,
    text_limits: FrozenBootStateTextLimits,
) -> Result<State, FrozenBootInputError> {
    let (summary_bytes, description_bytes, kind_bytes) = model::state::table
        .find(state_id)
        .select((
            sql::<Nullable<BigInt>>("length(CAST(summary AS BLOB))"),
            sql::<Nullable<BigInt>>("length(CAST(description AS BLOB))"),
            sql::<BigInt>("length(CAST(\"type\" AS BLOB))"),
        ))
        .first::<(Option<i64>, Option<i64>, i64)>(connection)?;
    validate_state_text_length(state_id, "summary", summary_bytes, text_limits)?;
    validate_state_text_length(state_id, "description", description_bytes, text_limits)?;
    validate_state_text_length(state_id, "type", Some(kind_bytes), text_limits)?;

    let (created, summary, description, kind) = model::state::table
        .find(state_id)
        .select((
            model::state::created,
            model::state::summary,
            model::state::description,
            model::state::type_,
        ))
        .first::<(i64, Option<String>, Option<String>, String)>(connection)?;
    let created = crate::db::Timestamp::try_from(created)?.0;
    let kind = kind
        .parse::<crate::state::Kind>()
        .map_err(|_| FrozenBootInputError::InvalidStateKind { state_id })?;
    let selections = load_bounded_selections(connection, state_id, selection_limits)?;

    Ok(State {
        id: state_id.into(),
        summary,
        description,
        selections,
        created,
        kind,
    })
}

fn validate_state_text_length(
    state_id: i32,
    field: &'static str,
    length: Option<i64>,
    limits: FrozenBootStateTextLimits,
) -> Result<(), FrozenBootInputError> {
    let Some(length) = length else {
        return Ok(());
    };
    let actual = usize::try_from(length).map_err(|_| FrozenBootInputError::InvalidStateTextLength {
        state_id,
        field,
        length,
    })?;
    if actual > limits.field_bytes {
        return Err(FrozenBootInputError::StateTextByteLimit {
            state_id,
            field,
            limit: limits.field_bytes,
            actual,
        });
    }
    Ok(())
}

fn load_bounded_selections(
    connection: &mut SqliteConnection,
    state_id: i32,
    limits: FrozenBootSelectionLimits,
) -> Result<Vec<Selection>, FrozenBootInputError> {
    let probe_limit = limits
        .count
        .checked_add(1)
        .and_then(|limit| i64::try_from(limit).ok())
        .expect("internal boot selection count limit fits SQLite");
    let lengths = model::state_selections::table
        .filter(model::state_selections::state_id.eq(state_id))
        .order(model::state_selections::package_id.asc())
        .limit(probe_limit)
        .select((
            sql::<BigInt>("length(CAST(package_id AS BLOB))"),
            sql::<Nullable<BigInt>>("length(CAST(reason AS BLOB))"),
        ))
        .load::<(i64, Option<i64>)>(connection)?;

    if lengths.len() > limits.count {
        return Err(FrozenBootInputError::SelectionCountLimit {
            state_id,
            limit: limits.count,
        });
    }

    let mut text_bytes = 0usize;
    for (package_bytes, reason_bytes) in lengths {
        let package_bytes = selection_text_length(state_id, package_bytes)?;
        let reason_bytes = reason_bytes
            .map(|bytes| selection_text_length(state_id, bytes))
            .transpose()?
            .unwrap_or_default();
        text_bytes = text_bytes
            .checked_add(package_bytes)
            .and_then(|bytes| bytes.checked_add(reason_bytes))
            .ok_or(FrozenBootInputError::SelectionTextByteLimit {
                state_id,
                limit: limits.text_bytes,
                actual: usize::MAX,
            })?;
        if text_bytes > limits.text_bytes {
            return Err(FrozenBootInputError::SelectionTextByteLimit {
                state_id,
                limit: limits.text_bytes,
                actual: text_bytes,
            });
        }
    }

    let selection_limit = i64::try_from(limits.count).expect("internal boot selection count limit fits SQLite");
    let selections = model::state_selections::table
        .filter(model::state_selections::state_id.eq(state_id))
        .order(model::state_selections::package_id.asc())
        .limit(selection_limit)
        .select(model::Selection::as_select())
        .load::<model::Selection>(connection)?
        .into_iter()
        .map(|row| Selection {
            package: row.package_id,
            explicit: row.explicit,
            reason: row.reason,
        })
        .collect();
    Ok(selections)
}

fn selection_text_length(state_id: i32, length: i64) -> Result<usize, FrozenBootInputError> {
    usize::try_from(length).map_err(|_| FrozenBootInputError::InvalidSelectionTextLength { state_id, length })
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum FrozenBootInputError {
    #[error("active boot head state {state_id} is absent")]
    MissingHead { state_id: i32 },
    #[error("state {state_id} exceeds the frozen boot selection limit of {limit}")]
    SelectionCountLimit { state_id: i32, limit: usize },
    #[error("state {state_id} selection text exceeds {limit} bytes (got {actual})")]
    SelectionTextByteLimit { state_id: i32, limit: usize, actual: usize },
    #[error("state {state_id} has an invalid SQLite selection text length {length}")]
    InvalidSelectionTextLength { state_id: i32, length: i64 },
    #[error("state {state_id} {field} text exceeds {limit} bytes (got {actual})")]
    StateTextByteLimit {
        state_id: i32,
        field: &'static str,
        limit: usize,
        actual: usize,
    },
    #[error("state {state_id} {field} has an invalid SQLite text length {length}")]
    InvalidStateTextLength {
        state_id: i32,
        field: &'static str,
        length: i64,
    },
    #[error("state {state_id} has an unsupported type")]
    InvalidStateKind { state_id: i32 },
    #[error(transparent)]
    Database(#[from] Error),
}

impl From<diesel::result::Error> for FrozenBootInputError {
    fn from(source: diesel::result::Error) -> Self {
        Self::Database(Error::from(source))
    }
}

#[cfg(test)]
mod tests;
