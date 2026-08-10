//! Startup reconciliation for a repair the marker says was interrupted.
//!
//! Archived repair carries no journal record (`plans/future_impl.md` §1.3), so
//! a restart has nothing to resume from except the marker and the namespace.
//! Detecting the interruption was already handled; this is the half that acts
//! on it. Without it the startup baseline refuses forever and the system can
//! never run another operation — measured on a guest 2026-08-10 by the
//! `repair:BeforePublication` crash cell, which stalled at
//! `driver=stalled-at-unknown`.
//!
//! The repair performs no database writes at all: it rebuilds one state's tree
//! from selections that do not change. So an interrupted repair leaves no torn
//! commit, only an unpublished candidate. `publish` already resumes from the
//! on-disk `RepairLayout`, which is exactly what a restart needs — so the
//! reconciliation finishes the repair rather than discarding it.

use crate::{
    Installation, db,
    state::{self},
    transition_identity::{ArchivedStateRepairIdentity, ArchivedStateRepairOutcome},
};

use super::Error;

/// Reconcile an interrupted repair when the marker names one.
///
/// Called before the startup gate rather than inside it: rebinding the repair
/// reopens the journal with the blocking open, which inside the gate waits on a
/// lock that same call already holds.
pub(in crate::client) fn reconcile_pending(installation: &Installation, state_db: &db::state::Database) -> Result<(), Error> {
    let pending = super::archived_repair_marker::pending(installation).map_err(|source| Error::ArchivedRepairMarker {
        source: Box::new(source),
    })?;
    let Some(state) = pending else {
        return Ok(());
    };
    reconcile_interrupted(installation, state_db, state)
}

/// Drive one interrupted repair to a decided end and clear its marker.
///
/// Returns without acting when no marker is armed. An ambiguous publication
/// deliberately leaves the marker armed: that is the one case where the
/// namespace cannot be described and a human has to look.
fn reconcile_interrupted(
    installation: &Installation,
    state_db: &db::state::Database,
    state: state::Id,
) -> Result<(), Error> {
    let expected = state_db.get(state)?;
    let identity = ArchivedStateRepairIdentity::prepare_interrupted_candidate(installation, state_db, &expected)
        .map_err(|source| Error::ArchivedStateRepair {
            source: Box::new(super::archived_repair::RepairError::Preparation {
                state,
                staging: installation.staging_dir(),
                source: Box::new(source),
            }),
        })?;

    match identity.publish(installation, state_db) {
        Ok(_publication) => disarm(installation),
        Err(failure) if failure.outcome() == ArchivedStateRepairOutcome::NotApplied => {
            // Nothing was published, so the candidate is still whole. Preserve
            // it rather than delete it: the same choice the in-process failure
            // path makes, and it keeps the evidence a later diagnosis needs.
            identity
                .preserve_failed_candidate(installation, state_db)
                .map_err(|preservation| Error::ArchivedStateRepair {
                    source: Box::new(super::archived_repair::RepairError::PublicationIncomplete {
                        state,
                        outcome: "NotApplied",
                        source: Box::new(preservation),
                    }),
                })?;
            disarm(installation)
        }
        Err(failure) => Err(Error::ArchivedStateRepair {
            source: Box::new(super::archived_repair::RepairError::PublicationIncomplete {
                state,
                outcome: "Ambiguous",
                source: Box::new(failure),
            }),
        }),
    }
}

fn disarm(installation: &Installation) -> Result<(), Error> {
    super::archived_repair_marker::disarm(installation).map_err(|source| Error::ArchivedRepairMarker {
        source: Box::new(source),
    })
}
