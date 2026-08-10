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

/// Preserve one interrupted repair's candidate and clear its marker.
///
/// It deliberately does **not** publish. Publishing looks tempting because
/// `publish` resumes from the on-disk `RepairLayout` — but that describes where
/// the tree *moved*, not how much of it was *built*. A repair decorates
/// metadata and runs transaction triggers before publication, and neither
/// leaves a mark a later startup can read, precisely because this operation
/// carries no journal (`plans/future_impl.md` §1.3). So a crash before either
/// step is indistinguishable here from a crash after both, and publishing would
/// present a half-built tree as a completed repair.
///
/// Measured 2026-08-10: cuts at `IdentityPrepared` — before decoration and
/// triggers — and at `AfterTransactionTriggers` produced byte-identical
/// "success" when this published. That uniformity is the proof that the
/// namespace cannot answer the question.
///
/// The candidate is preserved rather than deleted, matching the in-process
/// failure path and keeping the evidence a later diagnosis needs. State N stays
/// damaged, which is true and already actionable: `cast state verify` finds it
/// and rebuilds it from scratch, decoration and triggers included.
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

    match identity.preserve_failed_candidate(installation, state_db) {
        Ok(_quarantine) => disarm(installation),
        // A sticky canonical candidate means publication already applied, so
        // the repair completed and only the marker outlived it.
        Err(preservation) if preservation.outcome() == ArchivedStateRepairOutcome::Applied => disarm(installation),
        Err(preservation) => Err(Error::ArchivedStateRepair {
            source: Box::new(super::archived_repair::RepairError::PublicationIncomplete {
                state,
                outcome: outcome_name(preservation.outcome()),
                source: Box::new(preservation),
            }),
        }),
    }
}

fn outcome_name(outcome: ArchivedStateRepairOutcome) -> &'static str {
    match outcome {
        ArchivedStateRepairOutcome::Applied => "Applied",
        ArchivedStateRepairOutcome::NotApplied => "NotApplied",
        ArchivedStateRepairOutcome::Ambiguous => "Ambiguous",
    }
}

fn disarm(installation: &Installation) -> Result<(), Error> {
    super::archived_repair_marker::disarm(installation).map_err(|source| Error::ArchivedRepairMarker {
        source: Box::new(source),
    })
}
