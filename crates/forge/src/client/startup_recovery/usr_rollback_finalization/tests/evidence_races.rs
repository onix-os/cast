use std::{fs, os::unix::fs::PermissionsExt as _};

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_recovery::{
            UsrRollbackFinalizationError, arm_before_usr_rollback_finalization_final_revalidation,
            finalize_usr_rollback,
        },
    },
    transition_journal::{RollbackActionOutcome, encode},
};

use super::support::{CandidateResult, FinalizationFixture, FreshDbOutcome, Source, canonical_journal};

#[test]
fn startup_usr_rollback_finalization_rejects_reopened_and_cross_root_journal_bindings() {
    for origin in FreshDbOutcome::ALL {
        let fixture = FinalizationFixture::new(
            origin,
            Source::Exchanged,
            RollbackActionOutcome::Applied,
            CandidateResult::Applied,
        );
        let first = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        let authority = fixture.capture_ready(&first, &reservation);
        drop(first);
        let reopened = fixture.open_journal();

        let error = finalize_usr_rollback(reopened, authority).unwrap_err();

        assert!(matches!(error, UsrRollbackFinalizationError::Authority(_)));
        assert_eq!(fixture.canonical_record(), fixture.source);
        fixture.assert_no_second_removal();
        drop(reservation);

        let foreign = FinalizationFixture::new(
            origin,
            Source::Intent,
            RollbackActionOutcome::AlreadySatisfied,
            CandidateResult::AlreadySatisfied,
        );
        let source = FinalizationFixture::historical(
            origin,
            Source::Intent,
            RollbackActionOutcome::AlreadySatisfied,
            CandidateResult::AlreadySatisfied,
        );
        let source_journal = source.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        let authority = source.capture_ready(&source_journal, &reservation);
        let foreign_journal = foreign.open_journal();

        let error = finalize_usr_rollback(foreign_journal, authority).unwrap_err();

        assert!(matches!(error, UsrRollbackFinalizationError::Authority(_)));
        assert_eq!(source_journal.load().unwrap(), Some(source.source.clone()));
        assert_eq!(foreign.canonical_record(), foreign.source);
        source.assert_no_second_removal();
    }
}

#[derive(Clone, Copy, Debug)]
enum FinalRace {
    Database,
    Journal,
    Namespace,
}

#[test]
fn startup_usr_rollback_finalization_final_evidence_races_never_delete() {
    for race in [FinalRace::Database, FinalRace::Journal, FinalRace::Namespace] {
        let fixture = FinalizationFixture::new(
            FreshDbOutcome::Applied,
            Source::Exchanged,
            RollbackActionOutcome::Applied,
            CandidateResult::Applied,
        );
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        let authority = fixture.capture_ready(&journal, &reservation);
        arm_final_race(&fixture, race);

        let error = finalize_usr_rollback(journal, authority).unwrap_err();

        assert!(
            matches!(error, UsrRollbackFinalizationError::Authority(_)),
            "race={race:?}: {error:?}"
        );
        if !matches!(race, FinalRace::Journal) {
            assert_eq!(fixture.canonical_record(), fixture.source, "{race:?}");
        }
        fixture.assert_no_second_removal();
    }
}

fn arm_final_race(fixture: &FinalizationFixture, race: FinalRace) {
    let hook: Box<dyn FnOnce()> = match race {
        FinalRace::Database => {
            let database = fixture.database().clone();
            let previous = fixture.previous_state();
            Box::new(move || database.remove(&previous).unwrap())
        }
        FinalRace::Journal => {
            let canonical = canonical_journal(&fixture.installation().root);
            let changed = encode(fixture.preterminal_record()).unwrap();
            Box::new(move || fs::write(canonical, changed).unwrap())
        }
        FinalRace::Namespace => {
            let target = fixture.transition_target();
            Box::new(move || fs::set_permissions(target, fs::Permissions::from_mode(0o500)).unwrap())
        }
    };
    arm_before_usr_rollback_finalization_final_revalidation(hook);
}
