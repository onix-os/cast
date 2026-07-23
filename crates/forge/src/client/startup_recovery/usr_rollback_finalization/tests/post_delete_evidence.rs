//! Races against the independent evidence consumed after terminal deletion.

use std::{fs, os::unix::fs::PermissionsExt as _};

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_reconciliation::{
            arm_before_usr_rollback_finalization_fresh_namespace_capture,
            arm_between_usr_rollback_finalization_database_captures,
        },
        startup_recovery::{
            UsrRollbackFinalizationError, arm_after_usr_rollback_finalization_delete,
            finalize_usr_rollback,
        },
    },
    transition_journal::RollbackActionOutcome,
};

use super::support::{CandidateResult, FinalizationFixture, FreshDbOutcome, Source};

#[derive(Clone, Copy, Debug)]
enum PostDeleteRace {
    Database,
    Namespace,
}

#[derive(Clone, Copy, Debug)]
enum RacePoint {
    ImmediatelyAfterDelete,
    InsideConsumedProof,
}

#[test]
fn startup_usr_rollback_finalization_post_delete_evidence_races_never_report_success() {
    for race in [PostDeleteRace::Database, PostDeleteRace::Namespace] {
        for point in [RacePoint::ImmediatelyAfterDelete, RacePoint::InsideConsumedProof] {
            let fixture = FinalizationFixture::new(
                FreshDbOutcome::Applied,
                Source::Exchanged,
                RollbackActionOutcome::Applied,
                CandidateResult::Applied,
            );
            let journal = fixture.open_journal();
            let reservation = ActiveStateReservation::acquire().unwrap();
            let authority = fixture.capture_ready(&journal, &reservation);
            let mutation: Box<dyn FnOnce()> = match race {
                PostDeleteRace::Database => {
                    let database = fixture.database().clone();
                    let previous = fixture.previous_state();
                    Box::new(move || database.remove(&previous).unwrap())
                }
                PostDeleteRace::Namespace => {
                    let target = fixture.transition_target();
                    Box::new(move || fs::set_permissions(target, fs::Permissions::from_mode(0o500)).unwrap())
                }
            };
            let hook: Box<dyn FnOnce()> = match (race, point) {
                (_, RacePoint::ImmediatelyAfterDelete) => mutation,
                (PostDeleteRace::Database, RacePoint::InsideConsumedProof) => {
                    Box::new(move || arm_between_usr_rollback_finalization_database_captures(mutation))
                }
                (PostDeleteRace::Namespace, RacePoint::InsideConsumedProof) => {
                    Box::new(move || arm_before_usr_rollback_finalization_fresh_namespace_capture(mutation))
                }
            };
            arm_after_usr_rollback_finalization_delete(hook);

            let error = finalize_usr_rollback(journal, authority).unwrap_err();

            assert!(
                matches!(
                    error,
                    UsrRollbackFinalizationError::PostDeleteAuthority(_)
                ),
                "race={race:?}, point={point:?}: {error:?}"
            );
            let observed = fixture.open_journal();
            assert_eq!(observed.load().unwrap(), None, "race={race:?}, point={point:?}");
            fixture.assert_no_second_removal();
        }
    }
}
