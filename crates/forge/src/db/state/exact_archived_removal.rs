//! Exact, reconciled database removal for descriptor-detached archive trees.

use std::collections::BTreeSet;

use diesel::prelude::*;

use super::{Database, Error, load_selected_state, model};
use crate::State;

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ExactArchivedRemovalFault {
    ReportBeforeTransaction,
    ReportAfterCommit,
    ReportAfterCommitAndRestoreFirst,
}

#[cfg(test)]
std::thread_local! {
    static EXACT_ARCHIVED_REMOVAL_FAULT: std::cell::Cell<Option<ExactArchivedRemovalFault>> =
        const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub(super) fn arm_exact_archived_removal_fault(fault: ExactArchivedRemovalFault) {
    EXACT_ARCHIVED_REMOVAL_FAULT.with(|slot| {
        assert!(
            slot.replace(Some(fault)).is_none(),
            "exact archived removal fault already armed"
        );
    });
}

#[cfg(test)]
fn take_exact_archived_removal_fault() -> Option<ExactArchivedRemovalFault> {
    EXACT_ARCHIVED_REMOVAL_FAULT.with(|slot| slot.take())
}

impl Database {
    /// Remove an exact batch of detached archived states in one transaction.
    ///
    /// Every row and selection must still equal the snapshot authenticated
    /// before namespace detachment, no row may carry a transition correlation,
    /// and every delete must affect exactly one row. A reported transaction
    /// error is reconciled from one SQLite snapshot for the entire batch.
    pub(crate) fn remove_exact_archived(&self, expected: &[State]) -> Result<(), ExactArchivedRemovalError> {
        if expected.is_empty() {
            return Err(ExactArchivedRemovalError::EmptyBatch);
        }
        let mut unique = BTreeSet::new();
        for state in expected {
            if !unique.insert(state.id) {
                return Err(ExactArchivedRemovalError::Duplicate {
                    state_id: i32::from(state.id),
                });
            }
        }
        let remove = || {
            self.conn.exclusive_tx(|tx| {
                for expected in expected {
                    let stored = model::state::table
                        .find(i32::from(expected.id))
                        .select(model::State::as_select())
                        .first::<model::State>(tx)
                        .optional()
                        .map_err(Error::from)?
                        .ok_or(ExactArchivedRemovalError::Changed {
                            state_id: i32::from(expected.id),
                        })?;
                    if stored.transition_id.is_some() {
                        return Err(ExactArchivedRemovalError::TransitionPresent {
                            state_id: i32::from(expected.id),
                        });
                    }
                    let actual = load_selected_state(tx, stored)?;
                    if actual != *expected {
                        return Err(ExactArchivedRemovalError::Changed {
                            state_id: i32::from(expected.id),
                        });
                    }
                    let changed = diesel::delete(
                        model::state::table
                            .filter(model::state::id.eq(i32::from(expected.id)))
                            .filter(model::state::transition_id.is_null()),
                    )
                    .execute(tx)
                    .map_err(Error::from)?;
                    if changed != 1 {
                        return Err(ExactArchivedRemovalError::Changed {
                            state_id: i32::from(expected.id),
                        });
                    }
                }
                Ok(())
            })
        };
        #[cfg(test)]
        let injected = take_exact_archived_removal_fault();
        #[cfg(test)]
        let mut attempt = if injected == Some(ExactArchivedRemovalFault::ReportBeforeTransaction) {
            Err(synthetic_exact_archived_removal_error())
        } else {
            remove()
        };
        #[cfg(not(test))]
        let attempt = remove();
        #[cfg(test)]
        if attempt.is_ok()
            && matches!(
                injected,
                Some(
                    ExactArchivedRemovalFault::ReportAfterCommit
                        | ExactArchivedRemovalFault::ReportAfterCommitAndRestoreFirst
                )
            )
        {
            if injected == Some(ExactArchivedRemovalFault::ReportAfterCommitAndRestoreFirst) {
                self.restore_exact_archived_for_test(&expected[0])?;
            }
            attempt = Err(synthetic_exact_archived_removal_error());
        }
        let Err(source) = attempt else {
            return Ok(());
        };

        // Snapshot and transition mismatches are closure-level validation
        // failures. The exclusive transaction rolls the entire batch back
        // before returning them, so they are definitely not applied and must
        // not be reclassified as ambiguous merely because the caller's
        // intentionally different snapshot is still different on readback.
        if matches!(
            &source,
            ExactArchivedRemovalError::Changed { .. } | ExactArchivedRemovalError::TransitionPresent { .. }
        ) {
            return Err(source);
        }

        let observations = match self.observe_exact_archived_batch(expected) {
            Ok(observations) => observations,
            Err(observation) => {
                return Err(ExactArchivedRemovalError::Reconciliation {
                    source: Box::new(source),
                    observation: Box::new(observation),
                });
            }
        };
        let mut missing = 0usize;
        let mut exact = 0usize;
        for (expected, observation) in expected.iter().zip(observations) {
            match observation {
                ExactArchivedObservation::Exact => exact += 1,
                ExactArchivedObservation::Missing => missing += 1,
                ExactArchivedObservation::Changed => {
                    return Err(ExactArchivedRemovalError::Ambiguous {
                        state_id: i32::from(expected.id),
                        source: Box::new(source),
                    });
                }
            }
        }
        if missing == expected.len() {
            Ok(())
        } else if exact == expected.len() {
            Err(source)
        } else {
            Err(ExactArchivedRemovalError::AmbiguousBatch {
                missing,
                exact,
                source: Box::new(source),
            })
        }
    }

    /// Observe the entire batch under one SQLite transaction/snapshot. Per-row
    /// transactions could synthesize false all-exact or all-missing results if
    /// another connection committed between observations.
    fn observe_exact_archived_batch(&self, expected: &[State]) -> Result<Vec<ExactArchivedObservation>, Error> {
        self.conn.exclusive_tx(|tx| {
            expected
                .iter()
                .map(|expected| {
                    let Some(stored) = model::state::table
                        .find(i32::from(expected.id))
                        .select(model::State::as_select())
                        .first::<model::State>(tx)
                        .optional()?
                    else {
                        return Ok(ExactArchivedObservation::Missing);
                    };
                    if stored.transition_id.is_some() {
                        return Ok(ExactArchivedObservation::Changed);
                    }
                    let actual = load_selected_state(tx, stored)?;
                    Ok(if actual == *expected {
                        ExactArchivedObservation::Exact
                    } else {
                        ExactArchivedObservation::Changed
                    })
                })
                .collect()
        })
    }

    #[cfg(test)]
    fn restore_exact_archived_for_test(&self, expected: &State) -> Result<(), ExactArchivedRemovalError> {
        self.conn.exec(|conn| {
            diesel::insert_into(model::state::table)
                .values((
                    model::state::id.eq(i32::from(expected.id)),
                    model::state::type_.eq(expected.kind.to_string()),
                    model::state::created.eq(expected.created.timestamp()),
                    model::state::summary.eq(expected.summary.clone()),
                    model::state::description.eq(expected.description.clone()),
                    model::state::transition_id.eq::<Option<String>>(None),
                ))
                .execute(conn)?;
            let selections = expected
                .selections
                .iter()
                .map(|selection| model::NewSelection {
                    state_id: i32::from(expected.id),
                    package_id: selection.package.as_str(),
                    explicit: selection.explicit,
                    reason: selection.reason.as_deref(),
                })
                .collect::<Vec<_>>();
            if !selections.is_empty() {
                diesel::insert_into(model::state_selections::table)
                    .values(&selections)
                    .execute(conn)?;
            }
            Ok::<(), Error>(())
        })?;
        Ok(())
    }
}

#[cfg(test)]
fn synthetic_exact_archived_removal_error() -> ExactArchivedRemovalError {
    ExactArchivedRemovalError::Database(Error::Diesel(diesel::result::Error::RollbackTransaction))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ExactArchivedObservation {
    Missing,
    Exact,
    Changed,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ExactArchivedRemovalError {
    #[error("exact archived-state removal requires at least one state")]
    EmptyBatch,
    #[error("archived state {state_id} appears more than once in the exact removal batch")]
    Duplicate { state_id: i32 },
    #[error("archived state {state_id} changed before exact database removal")]
    Changed { state_id: i32 },
    #[error("archived state {state_id} still carries an in-flight transition")]
    TransitionPresent { state_id: i32 },
    #[error("exact archived-state removal is ambiguous at state {state_id}")]
    Ambiguous {
        state_id: i32,
        #[source]
        source: Box<Self>,
    },
    #[error("exact archived-state batch removal is ambiguous ({missing} missing, {exact} exact)")]
    AmbiguousBatch {
        missing: usize,
        exact: usize,
        #[source]
        source: Box<Self>,
    },
    #[error("reconcile a reported exact archived-state removal failure")]
    Reconciliation {
        #[source]
        source: Box<Self>,
        observation: Box<Error>,
    },
    #[error(transparent)]
    Database(#[from] Error),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ExactArchivedRemovalOutcome {
    NotApplied,
    Ambiguous,
}

impl From<diesel::result::Error> for ExactArchivedRemovalError {
    fn from(source: diesel::result::Error) -> Self {
        Self::Database(Error::from(source))
    }
}

impl ExactArchivedRemovalError {
    /// True only when post-error observation proved every requested row still
    /// exists as the exact clean snapshot supplied by the caller.
    pub(crate) fn definitely_not_applied(&self) -> bool {
        self.outcome() == ExactArchivedRemovalOutcome::NotApplied
    }

    pub(crate) fn outcome(&self) -> ExactArchivedRemovalOutcome {
        match self {
            Self::EmptyBatch
            | Self::Duplicate { .. }
            | Self::Changed { .. }
            | Self::TransitionPresent { .. }
            | Self::Database(_) => ExactArchivedRemovalOutcome::NotApplied,
            Self::Ambiguous { .. } | Self::AmbiguousBatch { .. } | Self::Reconciliation { .. } => {
                ExactArchivedRemovalOutcome::Ambiguous
            }
        }
    }
}
