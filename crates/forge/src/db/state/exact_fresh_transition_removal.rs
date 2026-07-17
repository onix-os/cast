//! Exact, reconciled removal of one fresh transition row and its provenance.

use diesel::prelude::*;
use diesel::sqlite::SqliteConnection;

use super::{Database, Error, load_selected_state, metadata_provenance, model, parse_transition_evidence};
use crate::{
    State,
    state::{Id, TransitionId},
};

/// One exact observation of the fresh state row and its provenance from a
/// single exclusive SQLite snapshot.
///
/// Both payloads are opaque. Safe callers can compare and move the evidence,
/// but cannot construct or duplicate a preimage independently of the database
/// snapshot which established it.
#[derive(Debug, Eq, PartialEq)]
pub(crate) enum ExactFreshTransitionObservation {
    Present(ExactFreshTransitionPreimage),
    JointlyAbsent(ExactFreshTransitionAbsence),
}

/// Complete compare-and-delete preimage for one fresh transition.
#[derive(Debug)]
pub(crate) struct ExactFreshTransitionPreimage {
    database: Database,
    state: State,
    transition_id: TransitionId,
    metadata_provenance: metadata_provenance::MetadataProvenance,
}

impl PartialEq for ExactFreshTransitionPreimage {
    fn eq(&self, other: &Self) -> bool {
        self.database.same_instance(&other.database)
            && self.state == other.state
            && self.transition_id == other.transition_id
            && self.metadata_provenance == other.metadata_provenance
    }
}

impl Eq for ExactFreshTransitionPreimage {}

impl ExactFreshTransitionPreimage {
    pub(crate) fn state(&self) -> &State {
        &self.state
    }

    pub(crate) fn transition_id(&self) -> &TransitionId {
        &self.transition_id
    }

    pub(crate) fn metadata_provenance(&self) -> &metadata_provenance::MetadataProvenance {
        &self.metadata_provenance
    }
}

/// Source-database-bound proof that the exact queried state, its selections,
/// and its provenance are absent while no transition-bearing row exists
/// globally. The transition identity is retained so this proof cannot be
/// confused with an observation made for another recovery record or database.
#[derive(Debug)]
pub(crate) struct ExactFreshTransitionAbsence {
    database: Database,
    state_id: Id,
    transition_id: TransitionId,
}

impl PartialEq for ExactFreshTransitionAbsence {
    fn eq(&self, other: &Self) -> bool {
        self.database.same_instance(&other.database)
            && self.state_id == other.state_id
            && self.transition_id == other.transition_id
    }
}

impl Eq for ExactFreshTransitionAbsence {}

impl ExactFreshTransitionAbsence {
    pub(crate) fn state_id(&self) -> Id {
        self.state_id
    }

    pub(crate) fn transition_id(&self) -> &TransitionId {
        &self.transition_id
    }
}

impl Database {
    /// Inspect one exact fresh transition and its required provenance in a
    /// single exclusive SQLite snapshot.
    ///
    /// Absence is returned only when the state, its provenance, its selections,
    /// and every global in-flight transition are absent. Every asymmetric,
    /// foreign, cleared, malformed, or otherwise unobservable state is an
    /// error rather than a permissive absence inference. Both outcomes remain
    /// bound to this exact in-process database capability.
    pub(crate) fn inspect_exact_fresh_transition(
        &self,
        state_id: Id,
        transition_id: &TransitionId,
    ) -> Result<ExactFreshTransitionObservation, ExactFreshTransitionInspectionError> {
        self.conn
            .exclusive_tx(|tx| inspect_exact_fresh_transition_impl(tx, self, state_id, transition_id))
    }

    /// Consume one complete fresh-transition preimage and atomically remove
    /// its exact provenance and state row.
    ///
    /// The method makes one transaction attempt and never retries it. Whatever
    /// that attempt reports, a fresh exclusive snapshot reconciles the net
    /// result: joint absence is success, a complete retained preimage after an
    /// attempt proven not-started or rolled-back is definitely not applied,
    /// and every changed, partial, unobservable, commit-uncertain, or
    /// post-success reappearance is ambiguous.
    pub(crate) fn remove_exact_fresh_transition(
        &self,
        preimage: ExactFreshTransitionPreimage,
    ) -> Result<ExactFreshTransitionAbsence, ExactFreshTransitionRemovalError> {
        reset_exact_fresh_transition_removal_transaction_attempts();
        let state_id = preimage.state.id;
        if !preimage.database.same_instance(self) {
            return Err(ExactFreshTransitionRemovalError::not_applied_error(
                state_id,
                ExactFreshTransitionAttemptError::DatabaseInstanceMismatch {
                    state_id: i32::from(state_id),
                },
            ));
        }
        let transition_id = preimage.transition_id.clone();

        let attempt = if exact_fresh_transition_removal_fault(ExactFreshTransitionRemovalFault::BeforeTransaction) {
            Err(ExactFreshTransitionAttemptError::FaultInjected {
                point: ExactFreshTransitionRemovalFault::BeforeTransaction,
            })
        } else {
            increment_exact_fresh_transition_removal_transaction_attempts();
            self.remove_exact_fresh_transition_once(&preimage)
        };

        let attempt = match attempt {
            Ok(()) if exact_fresh_transition_removal_fault(ExactFreshTransitionRemovalFault::AfterCommit) => {
                Err(ExactFreshTransitionAttemptError::FaultInjected {
                    point: ExactFreshTransitionRemovalFault::AfterCommit,
                })
            }
            Ok(())
                if exact_fresh_transition_removal_fault(
                    ExactFreshTransitionRemovalFault::AfterCommitWithPartialRestoration,
                ) =>
            {
                let restoration = self.restore_exact_fresh_transition_for_test(&preimage, false, false);
                match restoration {
                    Ok(()) => Err(ExactFreshTransitionAttemptError::FaultInjected {
                        point: ExactFreshTransitionRemovalFault::AfterCommitWithPartialRestoration,
                    }),
                    Err(source) => Err(ExactFreshTransitionAttemptError::Restoration {
                        source: Box::new(source),
                    }),
                }
            }
            Ok(())
                if exact_fresh_transition_removal_fault(
                    ExactFreshTransitionRemovalFault::AfterCommitWithChangedRestoration,
                ) =>
            {
                let restoration = self.restore_exact_fresh_transition_for_test(&preimage, true, true);
                match restoration {
                    Ok(()) => Err(ExactFreshTransitionAttemptError::FaultInjected {
                        point: ExactFreshTransitionRemovalFault::AfterCommitWithChangedRestoration,
                    }),
                    Err(source) => Err(ExactFreshTransitionAttemptError::Restoration {
                        source: Box::new(source),
                    }),
                }
            }
            Ok(())
                if exact_fresh_transition_removal_fault(
                    ExactFreshTransitionRemovalFault::AfterCommitWithExactRestoration,
                ) =>
            {
                self.restore_exact_fresh_transition_for_test(&preimage, true, false)
                    .map_err(|source| ExactFreshTransitionAttemptError::Restoration {
                        source: Box::new(source),
                    })
            }
            attempt => attempt,
        };

        let attempt_error = attempt.err();
        let observation = self.inspect_exact_fresh_transition(state_id, &transition_id);
        match observation {
            Ok(ExactFreshTransitionObservation::JointlyAbsent(absence)) => Ok(absence),
            Ok(ExactFreshTransitionObservation::Present(actual)) if actual == preimage => {
                // For supported writers, AUTOINCREMENT state IDs are not
                // reissued and each coordinator TransitionId is single-use.
                // Even so, an exact tuple restored after reported success or
                // an uncertain commit is modeled as ABA and remains ambiguous;
                // the premise supports NotApplied only after an attempt whose
                // error proves it never started or was rolled back.
                if attempt_error
                    .as_ref()
                    .is_some_and(ExactFreshTransitionAttemptError::rolled_back_or_not_started)
                {
                    Err(ExactFreshTransitionRemovalError::not_applied_error(
                        state_id,
                        attempt_error.expect("the classified attempt error is present"),
                    ))
                } else {
                    Err(ExactFreshTransitionRemovalError::ambiguous(
                        state_id,
                        attempt_error,
                        ExactFreshTransitionReconciliation::ExactPreimageAfterUncertainAttempt,
                    ))
                }
            }
            Ok(ExactFreshTransitionObservation::Present(_)) => Err(ExactFreshTransitionRemovalError::ambiguous(
                state_id,
                attempt_error,
                ExactFreshTransitionReconciliation::ChangedPreimage,
            )),
            Err(source) => Err(ExactFreshTransitionRemovalError::ambiguous(
                state_id,
                attempt_error,
                ExactFreshTransitionReconciliation::Unobservable(Box::new(source)),
            )),
        }
    }

    fn remove_exact_fresh_transition_once(
        &self,
        preimage: &ExactFreshTransitionPreimage,
    ) -> Result<(), ExactFreshTransitionAttemptError> {
        self.conn.exclusive_tx(|tx| {
            let observed = inspect_exact_fresh_transition_impl(tx, self, preimage.state.id, &preimage.transition_id)?;
            if !matches!(
                observed,
                ExactFreshTransitionObservation::Present(actual) if actual == *preimage
            ) {
                return Err(ExactFreshTransitionAttemptError::PreimageChanged {
                    state_id: i32::from(preimage.state.id),
                });
            }

            let provenance_changed = metadata_provenance::delete_exact_metadata_provenance(
                tx,
                preimage.state.id,
                &preimage.metadata_provenance,
            )?;
            if provenance_changed != 1 {
                return Err(ExactFreshTransitionAttemptError::AffectedRows {
                    relation: "state_metadata_provenance",
                    state_id: i32::from(preimage.state.id),
                    expected: 1,
                    actual: provenance_changed,
                });
            }

            if exact_fresh_transition_removal_fault(ExactFreshTransitionRemovalFault::BetweenProvenanceAndStateDelete) {
                return Err(ExactFreshTransitionAttemptError::FaultInjected {
                    point: ExactFreshTransitionRemovalFault::BetweenProvenanceAndStateDelete,
                });
            }

            let selections_changed = diesel::delete(
                model::state_selections::table
                    .filter(model::state_selections::state_id.eq(i32::from(preimage.state.id))),
            )
            .execute(tx)
            .map_err(Error::from)?;
            if selections_changed != preimage.state.selections.len() {
                return Err(ExactFreshTransitionAttemptError::AffectedRows {
                    relation: "state_selections",
                    state_id: i32::from(preimage.state.id),
                    expected: preimage.state.selections.len(),
                    actual: selections_changed,
                });
            }

            let state_changed = diesel::delete(
                model::state::table
                    .filter(model::state::id.eq(i32::from(preimage.state.id)))
                    .filter(model::state::transition_id.eq(preimage.transition_id.as_str())),
            )
            .execute(tx)
            .map_err(Error::from)?;
            if state_changed != 1 {
                return Err(ExactFreshTransitionAttemptError::AffectedRows {
                    relation: "state",
                    state_id: i32::from(preimage.state.id),
                    expected: 1,
                    actual: state_changed,
                });
            }

            if exact_fresh_transition_removal_fault(ExactFreshTransitionRemovalFault::BeforeCommit) {
                return Err(ExactFreshTransitionAttemptError::FaultInjected {
                    point: ExactFreshTransitionRemovalFault::BeforeCommit,
                });
            }

            Ok(())
        })
    }

    #[cfg(test)]
    fn restore_exact_fresh_transition_for_test(
        &self,
        preimage: &ExactFreshTransitionPreimage,
        restore_provenance: bool,
        change_summary: bool,
    ) -> Result<(), ExactFreshTransitionRestorationError> {
        self.conn.exclusive_tx(|tx| {
            let summary = if change_summary {
                Some("deterministically changed after exact removal".to_owned())
            } else {
                preimage.state.summary.clone()
            };
            diesel::insert_into(model::state::table)
                .values((
                    model::state::id.eq(i32::from(preimage.state.id)),
                    model::state::type_.eq(preimage.state.kind.to_string()),
                    model::state::created.eq(preimage.state.created.timestamp()),
                    model::state::summary.eq(summary),
                    model::state::description.eq(preimage.state.description.clone()),
                    model::state::transition_id.eq(Some(preimage.transition_id.as_str())),
                ))
                .execute(tx)?;

            let selections = preimage
                .state
                .selections
                .iter()
                .map(|selection| model::NewSelection {
                    state_id: i32::from(preimage.state.id),
                    package_id: selection.package.as_str(),
                    explicit: selection.explicit,
                    reason: selection.reason.as_deref(),
                })
                .collect::<Vec<_>>();
            if !selections.is_empty() {
                diesel::insert_into(model::state_selections::table)
                    .values(&selections)
                    .execute(tx)?;
            }
            if restore_provenance {
                metadata_provenance::insert_metadata_provenance_row(
                    tx,
                    preimage.state.id,
                    &preimage.metadata_provenance,
                )?;
            }
            Ok(())
        })
    }

    #[cfg(not(test))]
    fn restore_exact_fresh_transition_for_test(
        &self,
        _preimage: &ExactFreshTransitionPreimage,
        _restore_provenance: bool,
        _change_summary: bool,
    ) -> Result<(), ExactFreshTransitionRestorationError> {
        unreachable!("partial restoration is available only to deterministic tests")
    }
}

fn inspect_exact_fresh_transition_impl(
    tx: &mut SqliteConnection,
    database: &Database,
    state_id: Id,
    transition_id: &TransitionId,
) -> Result<ExactFreshTransitionObservation, ExactFreshTransitionInspectionError> {
    let in_flight = model::state::table
        .filter(model::state::transition_id.is_not_null())
        .select((model::state::id, model::state::transition_id))
        .order(model::state::id.asc())
        .limit(2)
        .load::<(i32, Option<String>)>(tx)
        .map_err(Error::from)?
        .into_iter()
        .map(|(actual_state_id, raw_transition_id)| {
            let actual_state_id = Id::from(actual_state_id);
            let raw_transition_id =
                raw_transition_id.ok_or(ExactFreshTransitionInspectionError::UnexpectedNullTransition {
                    state_id: i32::from(actual_state_id),
                })?;
            let actual_transition_id = parse_transition_evidence(actual_state_id, raw_transition_id)
                .map_err(ExactFreshTransitionInspectionError::TransitionEvidence)?;
            Ok((actual_state_id, actual_transition_id))
        })
        .collect::<Result<Vec<_>, ExactFreshTransitionInspectionError>>()?;

    match in_flight.as_slice() {
        [] => {}
        [(actual_state_id, actual_transition_id)]
            if *actual_state_id == state_id && *actual_transition_id == *transition_id => {}
        [(actual_state_id, actual_transition_id)] if *actual_state_id == state_id => {
            return Err(ExactFreshTransitionInspectionError::ForeignTransition {
                state_id: i32::from(state_id),
                expected: transition_id.clone(),
                actual: actual_transition_id.clone(),
            });
        }
        [(actual_state_id, actual_transition_id)] => {
            return Err(ExactFreshTransitionInspectionError::UnexpectedInFlightTransition {
                expected_state_id: i32::from(state_id),
                expected_transition: transition_id.clone(),
                actual_state_id: i32::from(*actual_state_id),
                actual_transition: actual_transition_id.clone(),
            });
        }
        [first, second] => {
            return Err(ExactFreshTransitionInspectionError::MultipleInFlightTransitions {
                first_state_id: i32::from(first.0),
                second_state_id: i32::from(second.0),
            });
        }
        _ => unreachable!("the bounded in-flight query loads at most two rows"),
    }

    let stored_state = model::state::table
        .find(i32::from(state_id))
        .select(model::State::as_select())
        .first::<model::State>(tx)
        .optional()
        .map_err(Error::from)?;
    let stored_provenance = metadata_provenance::metadata_provenance_impl(tx, state_id)?;
    let stored_selection_count = model::state_selections::table
        .filter(model::state_selections::state_id.eq(i32::from(state_id)))
        .count()
        .get_result::<i64>(tx)
        .map_err(Error::from)?;

    let (stored_state, stored_provenance) = match (stored_state, stored_provenance) {
        (None, None) if stored_selection_count == 0 => {
            return Ok(ExactFreshTransitionObservation::JointlyAbsent(
                ExactFreshTransitionAbsence {
                    database: database.clone(),
                    state_id,
                    transition_id: transition_id.clone(),
                },
            ));
        }
        (None, _) if stored_selection_count != 0 => {
            return Err(ExactFreshTransitionInspectionError::OrphanSelections {
                state_id: i32::from(state_id),
                count: stored_selection_count,
            });
        }
        (None, Some(_)) => {
            return Err(ExactFreshTransitionInspectionError::OrphanProvenance {
                state_id: i32::from(state_id),
            });
        }
        (Some(_), None) => {
            return Err(ExactFreshTransitionInspectionError::MissingProvenance {
                state_id: i32::from(state_id),
            });
        }
        (Some(state), Some(provenance)) => (state, provenance),
        (None, None) => unreachable!("zero selection count was handled as joint absence"),
    };

    let raw_transition =
        stored_state
            .transition_id
            .as_ref()
            .ok_or(ExactFreshTransitionInspectionError::ClearedTransition {
                state_id: i32::from(state_id),
            })?;
    let actual_transition = parse_transition_evidence(state_id, raw_transition.clone())
        .map_err(ExactFreshTransitionInspectionError::TransitionEvidence)?;
    if actual_transition != *transition_id {
        return Err(ExactFreshTransitionInspectionError::ForeignTransition {
            state_id: i32::from(state_id),
            expected: transition_id.clone(),
            actual: actual_transition,
        });
    }

    let state = load_selected_state(tx, stored_state)?;
    Ok(ExactFreshTransitionObservation::Present(ExactFreshTransitionPreimage {
        database: database.clone(),
        state,
        transition_id: actual_transition,
        metadata_provenance: stored_provenance,
    }))
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ExactFreshTransitionInspectionError {
    #[error("fresh state {state_id} has a cleared transition correlation")]
    ClearedTransition { state_id: i32 },
    #[error("fresh state {state_id} belongs to transition {actual} instead of requested transition {expected}")]
    ForeignTransition {
        state_id: i32,
        expected: TransitionId,
        actual: TransitionId,
    },
    #[error(
        "expected fresh state {expected_state_id} transition {expected_transition}, but state {actual_state_id} transition {actual_transition} is the sole in-flight row"
    )]
    UnexpectedInFlightTransition {
        expected_state_id: i32,
        expected_transition: TransitionId,
        actual_state_id: i32,
        actual_transition: TransitionId,
    },
    #[error("state database contains multiple in-flight transition rows ({first_state_id} and {second_state_id})")]
    MultipleInFlightTransitions { first_state_id: i32, second_state_id: i32 },
    #[error("state {state_id} unexpectedly has a null value in the bounded in-flight query")]
    UnexpectedNullTransition { state_id: i32 },
    #[error("fresh state {state_id} has no required generated-metadata provenance")]
    MissingProvenance { state_id: i32 },
    #[error("generated-metadata provenance for absent fresh state {state_id} is orphaned")]
    OrphanProvenance { state_id: i32 },
    #[error("absent fresh state {state_id} retains {count} orphan selection rows")]
    OrphanSelections { state_id: i32, count: i64 },
    #[error(transparent)]
    TransitionEvidence(#[from] super::TransitionEvidenceError),
    #[error(transparent)]
    MetadataProvenance(#[from] metadata_provenance::MetadataProvenanceError),
    #[error(transparent)]
    Database(#[from] Error),
}

impl From<diesel::result::Error> for ExactFreshTransitionInspectionError {
    fn from(source: diesel::result::Error) -> Self {
        Self::Database(Error::from(source))
    }
}

#[derive(Debug, thiserror::Error)]
enum ExactFreshTransitionAttemptError {
    #[error("exact fresh-transition preimage for state {state_id} belongs to another database capability")]
    DatabaseInstanceMismatch { state_id: i32 },
    #[error("exact fresh-transition preimage for state {state_id} changed before removal")]
    PreimageChanged { state_id: i32 },
    #[error("exact delete from {relation} for state {state_id} affected {actual} rows instead of {expected}")]
    AffectedRows {
        relation: &'static str,
        state_id: i32,
        expected: usize,
        actual: usize,
    },
    #[error("injected exact fresh-transition removal fault at {point:?}")]
    FaultInjected { point: ExactFreshTransitionRemovalFault },
    #[error("restore deterministic fresh-transition state after committed removal")]
    Restoration {
        #[source]
        source: Box<ExactFreshTransitionRestorationError>,
    },
    #[error(transparent)]
    Inspection(#[from] ExactFreshTransitionInspectionError),
    #[error(transparent)]
    Database(#[from] Error),
}

impl From<diesel::result::Error> for ExactFreshTransitionAttemptError {
    fn from(source: diesel::result::Error) -> Self {
        Self::Database(Error::from(source))
    }
}

impl ExactFreshTransitionAttemptError {
    /// Whether returning this error proves the removal transaction never
    /// started or returned through a closure error which Diesel rolled back.
    /// Raw SQLite and commit errors remain uncertain.
    fn rolled_back_or_not_started(&self) -> bool {
        match self {
            Self::DatabaseInstanceMismatch { .. }
            | Self::PreimageChanged { .. }
            | Self::AffectedRows { .. }
            | Self::Inspection(_) => true,
            Self::FaultInjected { point } => matches!(
                point,
                ExactFreshTransitionRemovalFault::BeforeTransaction
                    | ExactFreshTransitionRemovalFault::BetweenProvenanceAndStateDelete
                    | ExactFreshTransitionRemovalFault::BeforeCommit
            ),
            Self::Restoration { .. } | Self::Database(_) => false,
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum ExactFreshTransitionRestorationError {
    #[error(transparent)]
    MetadataProvenance(#[from] metadata_provenance::MetadataProvenanceError),
    #[error(transparent)]
    Database(#[from] Error),
}

impl From<diesel::result::Error> for ExactFreshTransitionRestorationError {
    fn from(source: diesel::result::Error) -> Self {
        Self::Database(Error::from(source))
    }
}

#[derive(Debug)]
enum ExactFreshTransitionReconciliation {
    ExactPreimageAfterUncertainAttempt,
    ChangedPreimage,
    Unobservable(Box<ExactFreshTransitionInspectionError>),
}

impl std::fmt::Display for ExactFreshTransitionReconciliation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ExactPreimageAfterUncertainAttempt => {
                formatter.write_str("the exact preimage remains after a successful or uncertain attempt")
            }
            Self::ChangedPreimage => formatter.write_str("the observed complete preimage changed"),
            Self::Unobservable(source) => write!(formatter, "the joint state is unobservable: {source}"),
        }
    }
}

/// Reconciled outcome of a reported exact fresh-transition removal error.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ExactFreshTransitionRemovalOutcome {
    DefinitelyNotApplied,
    Ambiguous,
}

/// A removal failure whose outcome is derived only from a fresh exclusive
/// reconciliation snapshot.
#[derive(Debug, thiserror::Error)]
#[error("exact fresh-transition removal for state {state_id} is {outcome:?}: {detail}")]
pub(crate) struct ExactFreshTransitionRemovalError {
    state_id: i32,
    outcome: ExactFreshTransitionRemovalOutcome,
    detail: ExactFreshTransitionRemovalFailure,
}

#[derive(Debug)]
enum ExactFreshTransitionRemovalFailure {
    DefinitelyNotApplied {
        attempt: Box<ExactFreshTransitionAttemptError>,
    },
    Ambiguous {
        attempt: Option<Box<ExactFreshTransitionAttemptError>>,
        reconciliation: ExactFreshTransitionReconciliation,
    },
}

impl std::fmt::Display for ExactFreshTransitionRemovalFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DefinitelyNotApplied { attempt } => {
                write!(
                    formatter,
                    "source binding or fresh reconciliation proves no removal effect: {attempt}"
                )
            }
            Self::Ambiguous {
                attempt: Some(attempt),
                reconciliation,
            } => write!(formatter, "reported error {attempt}; {reconciliation}"),
            Self::Ambiguous {
                attempt: None,
                reconciliation,
            } => write!(formatter, "the attempt reported success but {reconciliation}"),
        }
    }
}

impl ExactFreshTransitionRemovalError {
    fn not_applied_error(state_id: Id, attempt: ExactFreshTransitionAttemptError) -> Self {
        Self {
            state_id: i32::from(state_id),
            outcome: ExactFreshTransitionRemovalOutcome::DefinitelyNotApplied,
            detail: ExactFreshTransitionRemovalFailure::DefinitelyNotApplied {
                attempt: Box::new(attempt),
            },
        }
    }

    fn ambiguous(
        state_id: Id,
        attempt: Option<ExactFreshTransitionAttemptError>,
        reconciliation: ExactFreshTransitionReconciliation,
    ) -> Self {
        Self {
            state_id: i32::from(state_id),
            outcome: ExactFreshTransitionRemovalOutcome::Ambiguous,
            detail: ExactFreshTransitionRemovalFailure::Ambiguous {
                attempt: attempt.map(Box::new),
                reconciliation,
            },
        }
    }

    pub(crate) fn outcome(&self) -> ExactFreshTransitionRemovalOutcome {
        self.outcome
    }

    pub(crate) fn definitely_not_applied(&self) -> bool {
        self.outcome == ExactFreshTransitionRemovalOutcome::DefinitelyNotApplied
    }
}

/// Deterministic boundaries for proving the one-attempt reconciliation
/// contract. No production constructor exists for these faults.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ExactFreshTransitionRemovalFault {
    BeforeTransaction,
    BetweenProvenanceAndStateDelete,
    BeforeCommit,
    AfterCommit,
    AfterCommitWithPartialRestoration,
    AfterCommitWithChangedRestoration,
    AfterCommitWithExactRestoration,
}

#[cfg(test)]
std::thread_local! {
    static EXACT_FRESH_TRANSITION_REMOVAL_FAULT:
        std::cell::Cell<Option<ExactFreshTransitionRemovalFault>> = const { std::cell::Cell::new(None) };
    static EXACT_FRESH_TRANSITION_REMOVAL_TRANSACTION_ATTEMPTS:
        std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn arm_exact_fresh_transition_removal_fault(fault: ExactFreshTransitionRemovalFault) {
    EXACT_FRESH_TRANSITION_REMOVAL_FAULT.with(|slot| {
        assert!(
            slot.replace(Some(fault)).is_none(),
            "an exact fresh-transition removal fault is already armed"
        );
    });
    EXACT_FRESH_TRANSITION_REMOVAL_TRANSACTION_ATTEMPTS.with(|attempts| attempts.set(0));
}

#[cfg(test)]
pub(crate) fn assert_exact_fresh_transition_removal_fault_consumed() {
    EXACT_FRESH_TRANSITION_REMOVAL_FAULT.with(|slot| {
        assert!(
            slot.get().is_none(),
            "armed exact fresh-transition removal fault was not reached"
        );
    });
}

#[cfg(test)]
pub(crate) fn exact_fresh_transition_removal_transaction_attempts() -> usize {
    EXACT_FRESH_TRANSITION_REMOVAL_TRANSACTION_ATTEMPTS.with(std::cell::Cell::get)
}

#[cfg(test)]
fn exact_fresh_transition_removal_fault(point: ExactFreshTransitionRemovalFault) -> bool {
    EXACT_FRESH_TRANSITION_REMOVAL_FAULT.with(|slot| {
        if slot.get() == Some(point) {
            slot.set(None);
            true
        } else {
            false
        }
    })
}

#[cfg(not(test))]
fn exact_fresh_transition_removal_fault(_point: ExactFreshTransitionRemovalFault) -> bool {
    false
}

#[cfg(test)]
fn increment_exact_fresh_transition_removal_transaction_attempts() {
    EXACT_FRESH_TRANSITION_REMOVAL_TRANSACTION_ATTEMPTS.with(|attempts| {
        attempts.set(attempts.get() + 1);
    });
}

#[cfg(not(test))]
fn increment_exact_fresh_transition_removal_transaction_attempts() {}

#[cfg(test)]
fn reset_exact_fresh_transition_removal_transaction_attempts() {
    EXACT_FRESH_TRANSITION_REMOVAL_TRANSACTION_ATTEMPTS.with(|attempts| attempts.set(0));
}

#[cfg(not(test))]
fn reset_exact_fresh_transition_removal_transaction_attempts() {}

#[cfg(test)]
mod tests;
