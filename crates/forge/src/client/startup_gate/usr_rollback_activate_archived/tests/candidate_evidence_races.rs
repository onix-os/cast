//! Final evidence races exercised through the real production startup entry.

use std::{fs, os::unix::fs::PermissionsExt as _};

use crate::{
    client::{
        startup_reconciliation::{
            arm_before_archived_candidate_preserve_durable_post_revalidation_capture,
            arm_before_archived_candidate_preserve_persistence_durable_trailing_evidence,
        },
        startup_recovery::arm_before_usr_rollback_archived_candidate_preserve_persistence_final_revalidation,
    },
    transition_journal::RollbackActionOutcome,
};

use super::{
    super::{
        candidate_test_support::{CandidateLayout, CandidatePreserveFixture},
        test_fixture::{OperationKind, canonical_journal},
    },
    support::{
        CandidateOrigin, CandidateSource, Epoch, assert_candidate_persistence_authority_error, build_candidate,
        candidate_move_count, enter_candidate, reset_candidate_observers,
    },
};

#[derive(Clone, Copy, Debug)]
enum EvidenceRace {
    Database,
    Provenance,
    Journal,
    Installation,
    Namespace,
    Plan,
}

#[test]
fn startup_activate_archived_candidate_dispatch_rejects_every_final_evidence_race() {
    for epoch in Epoch::ALL {
        for origin in CandidateOrigin::ALL {
            for race in [
                EvidenceRace::Database,
                EvidenceRace::Provenance,
                EvidenceRace::Journal,
                EvidenceRace::Installation,
                EvidenceRace::Namespace,
                EvidenceRace::Plan,
            ] {
                let fixture = build_candidate(
                    epoch,
                    CandidateSource::Exchanged,
                    RollbackActionOutcome::Applied,
                    origin,
                );
                reset_candidate_observers();
                arm_final_race(&fixture, race);

                let error = enter_candidate(&fixture);

                assert_candidate_persistence_authority_error(&error);
                assert_eq!(
                    candidate_move_count(),
                    usize::from(origin == CandidateOrigin::Applied),
                    "{epoch:?} {origin:?} {race:?}",
                );
            }
        }
    }
}

fn arm_final_race(fixture: &CandidatePreserveFixture, race: EvidenceRace) {
    let hook: Box<dyn FnOnce()> = match race {
        EvidenceRace::Database => {
            let database = fixture.fixture.database.clone();
            let candidate = fixture.fixture.candidate_state;
            Box::new(move || {
                arm_before_archived_candidate_preserve_persistence_durable_trailing_evidence(move || {
                    database.remove(&candidate).unwrap();
                });
            })
        }
        EvidenceRace::Provenance => {
            let database = fixture.fixture.database.clone();
            let candidate = fixture.fixture.candidate_state;
            Box::new(move || {
                arm_before_archived_candidate_preserve_persistence_durable_trailing_evidence(move || {
                    database.delete_metadata_provenance_for_test(candidate).unwrap();
                });
            })
        }
        EvidenceRace::Journal => {
            let mutation = fixture.journal_change_hook();
            Box::new(move || {
                arm_before_archived_candidate_preserve_persistence_durable_trailing_evidence(mutation);
            })
        }
        EvidenceRace::Installation => {
            let cast = fixture.fixture.installation.root.join(".cast");
            let displaced = fixture.fixture.installation.root.join(".cast-archived-startup-rebound");
            Box::new(move || {
                fs::rename(&cast, displaced).unwrap();
                fs::create_dir(&cast).unwrap();
                fs::set_permissions(cast, fs::Permissions::from_mode(0o700)).unwrap();
            })
        }
        EvidenceRace::Namespace => {
            let mutation = fixture.namespace_change_hook("archived-startup-final-namespace".to_owned());
            Box::new(move || {
                arm_before_archived_candidate_preserve_durable_post_revalidation_capture(mutation);
            })
        }
        EvidenceRace::Plan => {
            let changed = CandidatePreserveFixture::new(
                OperationKind::ActiveReblit,
                CandidateSource::Exchanged,
                RollbackActionOutcome::Applied,
                CandidateLayout::Staged,
            );
            let bytes = changed.fixture.canonical_bytes();
            let canonical = canonical_journal(&fixture.fixture.installation.root);
            Box::new(move || {
                arm_before_archived_candidate_preserve_persistence_durable_trailing_evidence(move || {
                    fs::write(canonical, bytes).unwrap();
                });
            })
        }
    };
    arm_before_usr_rollback_archived_candidate_preserve_persistence_final_revalidation(hook);
}
