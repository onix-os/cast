use std::{fs, os::unix::fs::MetadataExt as _, path::PathBuf};

use crate::{
    client::{
        boot::{boot_synchronize_attempt_count, reset_boot_synchronize_attempt_count},
        startup_reconciliation::{
            fresh_db_invalidation_removal_call_count, new_state_candidate_preserve_move_attempt_count,
            reset_new_state_candidate_preserve_move_attempt_count,
        },
    },
    transition_identity::{reset_retained_exchange_syscall_count, retained_exchange_syscall_count},
    transition_journal::{Phase, RollbackAction, RollbackActionOutcome},
};

use super::{
    super::test_fixture::{Fixture, OperationKind, SourceCase, pending},
    support::CandidateOutcome,
};

#[test]
fn startup_root_links_complete_new_state_reaches_generation_18_then_terminal_finalization_stays_closed() {
    for historical in [false, true] {
        for candidate_outcome in CandidateOutcome::ALL {
            for fresh_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
            let fixture = if historical {
                Fixture::historical(OperationKind::NewState, SourceCase::RootLinksCompletePost)
            } else {
                Fixture::new(OperationKind::NewState, SourceCase::RootLinksCompletePost)
            };
            let case = format!(
                "historical={historical} candidate={candidate_outcome:?} fresh={fresh_outcome:?}"
            );
            let database_before = fixture.database_snapshot();
            let root_links_before = root_link_snapshot(&fixture);
            reset_retained_exchange_syscall_count();

            let decision_entry = fixture.enter();
            assert_eq!(pending(&decision_entry).phase(), Phase::RollbackDecided, "{case}");
            drop(decision_entry);

            let reverse_route_entry = fixture.enter();
            assert_eq!(
                pending(&reverse_route_entry).phase(),
                Phase::ReverseExchangeIntent,
                "{case}"
            );
            drop(reverse_route_entry);

            let reverse_entry = fixture.enter();
            assert_eq!(pending(&reverse_entry).phase(), Phase::UsrRestored, "{case}");
            assert_eq!(retained_exchange_syscall_count(), 1, "{case}");
            drop(reverse_entry);

            let candidate_route_entry = fixture.enter();
            assert_eq!(
                pending(&candidate_route_entry).phase(),
                Phase::CandidatePreserveIntent,
                "{case}"
            );
            drop(candidate_route_entry);

            let target_creation_entry = fixture.enter();
            assert_eq!(
                pending(&target_creation_entry).phase(),
                Phase::CandidatePreserveIntent,
                "{case}"
            );
            drop(target_creation_entry);

            if candidate_outcome == CandidateOutcome::AlreadySatisfied {
                let intent = fixture.canonical_record();
                let destination = fixture
                    .installation
                    .state_quarantine_dir()
                    .join(intent.quarantine_name.as_str());
                fs::rename(
                    fixture.installation.staging_dir().join("usr"),
                    destination.join("usr"),
                )
                .unwrap();
            }

            reset_new_state_candidate_preserve_move_attempt_count();
            let candidate_entry = fixture.enter();
            let candidate_preserved = fixture.canonical_record();
            assert_eq!(pending(&candidate_entry).phase(), Phase::CandidatePreserved, "{case}");
            assert_eq!(candidate_preserved.phase, Phase::CandidatePreserved, "{case}");
            assert_eq!(candidate_preserved.generation, 15, "{case}");
            assert_eq!(
                candidate_preserved.rollback.as_ref().unwrap().candidate.action,
                match candidate_outcome {
                    CandidateOutcome::Applied => RollbackAction::Applied,
                    CandidateOutcome::AlreadySatisfied => {
                        RollbackAction::AlreadySatisfied
                    }
                },
                "{case}"
            );
            assert_eq!(
                new_state_candidate_preserve_move_attempt_count(),
                usize::from(candidate_outcome == CandidateOutcome::Applied),
                "{case}"
            );
            assert_eq!(retained_exchange_syscall_count(), 1, "{case}");
            assert_eq!(fixture.database_snapshot(), database_before, "{case}");
            assert_eq!(root_link_snapshot(&fixture), root_links_before, "{case}");
            let route_namespace_before = fixture.namespace_snapshot();
            drop(candidate_entry);

            reset_boot_synchronize_attempt_count();
            let route_entry = fixture.enter();
            let invalidation_intent = fixture.canonical_record();
            assert_eq!(pending(&route_entry).phase(), Phase::FreshDbInvalidationIntent, "{case}");
            assert_eq!(invalidation_intent.phase, Phase::FreshDbInvalidationIntent, "{case}");
            assert_eq!(invalidation_intent.generation, 16, "{case}");
            assert_eq!(
                invalidation_intent,
                candidate_preserved.rollback_successor(None).unwrap(),
                "{case}"
            );
            assert_eq!(retained_exchange_syscall_count(), 1, "{case}");
            assert_eq!(boot_synchronize_attempt_count(), 0, "{case}");
            assert_eq!(fixture.database_snapshot(), database_before, "{case}");
            assert_eq!(fixture.namespace_snapshot(), route_namespace_before, "{case}");
            assert_eq!(root_link_snapshot(&fixture), root_links_before, "{case}");
            drop(route_entry);

            let observation = fixture
                .database
                .inspect_exact_fresh_transition(fixture.candidate_state, &invalidation_intent.transition_id)
                .unwrap();
            match fresh_outcome {
                RollbackActionOutcome::Applied => assert!(matches!(
                    observation,
                    crate::db::state::ExactFreshTransitionObservation::Present(_)
                )),
                RollbackActionOutcome::AlreadySatisfied => {
                    let crate::db::state::ExactFreshTransitionObservation::Present(preimage) = observation else {
                        panic!("fresh endpoint fixture must start with one exact row: {case}");
                    };
                    fixture.database.remove_exact_fresh_transition(preimage).unwrap();
                }
            }

            let invalidation_entry = fixture.enter();
            let invalidated = fixture.canonical_record();
            assert_eq!(pending(&invalidation_entry).phase(), Phase::FreshDbInvalidated, "{case}");
            assert_eq!(invalidated.phase, Phase::FreshDbInvalidated, "{case}");
            assert_eq!(invalidated.generation, 17, "{case}");
            assert_eq!(
                invalidated,
                invalidation_intent.rollback_successor(Some(fresh_outcome)).unwrap(),
                "{case}"
            );
            assert_eq!(
                invalidated.rollback.as_ref().unwrap().fresh_db,
                match fresh_outcome {
                    RollbackActionOutcome::Applied => RollbackAction::Applied,
                    RollbackActionOutcome::AlreadySatisfied => RollbackAction::AlreadySatisfied,
                },
                "{case}"
            );
            assert_eq!(retained_exchange_syscall_count(), 1, "{case}");
            assert_eq!(
                fresh_db_invalidation_removal_call_count(),
                usize::from(fresh_outcome == RollbackActionOutcome::Applied),
                "{case}"
            );
            assert_eq!(boot_synchronize_attempt_count(), 0, "{case}");
            assert!(matches!(
                fixture.database.inspect_exact_fresh_transition(
                    fixture.candidate_state,
                    &invalidated.transition_id,
                ),
                Ok(crate::db::state::ExactFreshTransitionObservation::JointlyAbsent(_))
            ));
            assert_eq!(fixture.namespace_snapshot(), route_namespace_before, "{case}");
            assert_eq!(root_link_snapshot(&fixture), root_links_before, "{case}");
            let invalidated_bytes = fixture.canonical_bytes();
            drop(invalidation_entry);

            let completion_entry = fixture.enter();
            let complete = fixture.canonical_record();
            assert_eq!(pending(&completion_entry).phase(), Phase::RollbackComplete, "{case}");
            assert_eq!(complete.phase, Phase::RollbackComplete, "{case}");
            assert_eq!(complete.generation, 18, "{case}");
            assert_eq!(complete, invalidated.rollback_successor(None).unwrap(), "{case}");
            assert_ne!(fixture.canonical_bytes(), invalidated_bytes, "{case}");
            assert_eq!(retained_exchange_syscall_count(), 1, "{case}");
            assert_eq!(
                fresh_db_invalidation_removal_call_count(),
                usize::from(fresh_outcome == RollbackActionOutcome::Applied),
                "{case}"
            );
            assert_eq!(boot_synchronize_attempt_count(), 0, "{case}");
            assert!(matches!(
                fixture.database.inspect_exact_fresh_transition(
                    fixture.candidate_state,
                    &invalidated.transition_id,
                ),
                Ok(crate::db::state::ExactFreshTransitionObservation::JointlyAbsent(_))
            ));
            assert_eq!(fixture.namespace_snapshot(), route_namespace_before, "{case}");
            assert_eq!(root_link_snapshot(&fixture), root_links_before, "{case}");
            let complete_bytes = fixture.canonical_bytes();
            drop(completion_entry);

            let stable_entry = fixture.enter();
            assert_eq!(pending(&stable_entry).phase(), Phase::RollbackComplete, "{case}");
            assert_eq!(fixture.canonical_record(), complete, "{case}");
            assert_eq!(fixture.canonical_bytes(), complete_bytes, "{case}");
            assert_eq!(retained_exchange_syscall_count(), 1, "{case}");
            assert_eq!(
                fresh_db_invalidation_removal_call_count(),
                usize::from(fresh_outcome == RollbackActionOutcome::Applied),
                "{case}"
            );
            assert_eq!(boot_synchronize_attempt_count(), 0, "{case}");
            assert!(matches!(
                fixture.database.inspect_exact_fresh_transition(
                    fixture.candidate_state,
                    &complete.transition_id,
                ),
                Ok(crate::db::state::ExactFreshTransitionObservation::JointlyAbsent(_))
            ));
            assert_eq!(fixture.namespace_snapshot(), route_namespace_before, "{case}");
            assert_eq!(root_link_snapshot(&fixture), root_links_before, "{case}");
            }
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
struct RootLinkIdentity {
    name: &'static str,
    target: PathBuf,
    device: u64,
    inode: u64,
    mode: u32,
    links: u64,
}

fn root_link_snapshot(fixture: &Fixture) -> Vec<RootLinkIdentity> {
    fixture.assert_complete_root_abi();
    ["bin", "sbin", "lib", "lib32", "lib64"]
        .into_iter()
        .map(|name| {
            let path = fixture.installation.root.join(name);
            let metadata = fs::symlink_metadata(&path).unwrap();
            assert!(metadata.file_type().is_symlink());
            RootLinkIdentity {
                name,
                target: fs::read_link(path).unwrap(),
                device: metadata.dev(),
                inode: metadata.ino(),
                mode: metadata.mode(),
                links: metadata.nlink(),
            }
        })
        .collect()
}
