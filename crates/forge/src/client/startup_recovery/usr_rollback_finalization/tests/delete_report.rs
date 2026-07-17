//! Defensive reconciliation when the one-shot delete reports no application.

use std::fs;

use crate::{
    client::active_state_snapshot::ActiveStateReservation,
    transition_journal::{RollbackActionOutcome, encode},
};

use super::{
    super::{
        DurableUsrRollbackFinalizationRecord, UsrRollbackFinalizationError, UsrRollbackFinalizationVerificationError,
        reconcile_delete,
    },
    support::{CandidateResult, FinalizationFixture, FreshDbOutcome, Source, canonical_journal},
};

#[derive(Clone, Copy, Debug)]
enum Observation {
    ExactSource,
    Absent,
    Unexpected,
}

#[test]
fn startup_usr_rollback_finalization_false_delete_classifies_only_exact_source_or_absence() {
    for observation in [Observation::ExactSource, Observation::Absent, Observation::Unexpected] {
        let fixture = FinalizationFixture::new(
            FreshDbOutcome::AlreadySatisfied,
            Source::Intent,
            RollbackActionOutcome::Applied,
            CandidateResult::AlreadySatisfied,
        );
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        let authority = fixture.capture_ready(&journal, &reservation);
        let installation = fixture.installation().clone();
        let source = fixture.source.clone();
        let canonical = canonical_journal(&installation.root);
        match observation {
            Observation::ExactSource => {}
            Observation::Absent => {
                fs::remove_file(&canonical).unwrap();
                fs::File::open(canonical.parent().unwrap()).unwrap().sync_all().unwrap();
            }
            Observation::Unexpected => {
                fs::write(&canonical, encode(fixture.preterminal_record()).unwrap()).unwrap();
            }
        }

        let error = reconcile_delete(Ok(false), journal, authority, &installation, source).unwrap_err();

        match observation {
            Observation::ExactSource => assert!(matches!(
                error,
                UsrRollbackFinalizationError::DeleteReportedFalse {
                    durable: DurableUsrRollbackFinalizationRecord::RollbackComplete,
                }
            )),
            Observation::Absent => assert!(matches!(
                error,
                UsrRollbackFinalizationError::DeleteReportedFalse {
                    durable: DurableUsrRollbackFinalizationRecord::Absent,
                }
            )),
            Observation::Unexpected => assert!(matches!(
                error,
                UsrRollbackFinalizationError::DeleteReportedFalseAndVerification {
                    source: UsrRollbackFinalizationVerificationError::UnexpectedRecord { actual: Some(_), .. },
                }
            )),
        }
        fixture.assert_no_second_removal();
    }
}
