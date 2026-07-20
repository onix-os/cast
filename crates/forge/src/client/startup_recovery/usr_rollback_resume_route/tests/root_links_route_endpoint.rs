use std::{fs, os::unix::fs::MetadataExt as _, path::PathBuf};

use crate::{
    client::startup_reconciliation::{
        active_reblit_candidate_preserve_exchange_attempt_count,
        fresh_db_invalidation_removal_call_count, reset_active_reblit_candidate_preserve_exchange_attempt_count,
    },
    transition_identity::{reset_retained_exchange_syscall_count, retained_exchange_syscall_count},
    transition_journal::{Phase, RecoveryDisposition, RollbackActionOutcome, TransitionRecord},
};

use super::fixture::{Fixture, OperationKind, SourceCase, create_private_directory, pending};

#[test]
fn startup_root_links_complete_fresh_entries_reach_operation_specific_closed_suffix_without_second_reverse_exchange() {
    for historical in [false, true] {
        for kind in OperationKind::ALL {
            let fixture = if historical {
                Fixture::historical(kind, SourceCase::RootLinksCompletePost)
            } else {
                Fixture::new(kind, SourceCase::RootLinksCompletePost)
            };
            let case = format!("{kind:?} historical={historical}");
            fixture.assert_source_unchanged();
            let namespace_before = fixture.namespace_snapshot();
            let database_before = fixture.database_snapshot();
            let root_links_before = root_link_snapshot(&fixture);
            assert_eq!(root_links_before.len(), 5, "{case}");
            let usr_before = usr_layout(&fixture);
            reset_retained_exchange_syscall_count();
            reset_active_reblit_candidate_preserve_exchange_attempt_count();

            let decision_entry = fixture.enter();
            assert_eq!(pending(&decision_entry).phase(), Phase::RollbackDecided, "{case}");
            assert_eq!(retained_exchange_syscall_count(), 0, "{case}");
            drop(decision_entry);
            let decision = fixture.canonical_record();
            fixture.assert_exact_decision(&decision);
            assert_eq!(fixture.namespace_snapshot(), namespace_before, "{case}");
            assert_eq!(fixture.database_snapshot(), database_before, "{case}");

            let route_entry = fixture.enter();
            let reverse_intent = decision.rollback_successor(None).unwrap();
            assert_eq!(reverse_intent.phase, Phase::ReverseExchangeIntent, "{case}");
            assert_eq!(pending(&route_entry).phase(), Phase::ReverseExchangeIntent, "{case}");
            assert_eq!(
                pending(&route_entry).disposition(),
                RecoveryDisposition::ResumeRollback {
                    phase: Phase::ReverseExchangeIntent,
                },
                "{case}"
            );
            assert!(pending(&route_entry).blockers().is_empty(), "{case}");
            assert_eq!(fixture.canonical_record(), reverse_intent, "{case}");
            assert_eq!(retained_exchange_syscall_count(), 0, "{case}");
            assert_eq!(fixture.namespace_snapshot(), namespace_before, "{case}");
            assert_eq!(fixture.database_snapshot(), database_before, "{case}");

            let routed_bytes = fixture.canonical_bytes();
            drop(route_entry);
            let reverse_entry = fixture.enter();
            let restored = reverse_intent
                .rollback_successor(Some(RollbackActionOutcome::Applied))
                .unwrap();
            assert_eq!(restored.phase, Phase::UsrRestored, "{case}");
            assert_eq!(pending(&reverse_entry).phase(), Phase::UsrRestored, "{case}");
            assert_eq!(
                pending(&reverse_entry).disposition(),
                RecoveryDisposition::ResumeRollback {
                    phase: Phase::UsrRestored,
                },
                "{case}"
            );
            assert!(pending(&reverse_entry).blockers().is_empty(), "{case}");
            assert_eq!(fixture.canonical_record(), restored, "{case}");
            assert_ne!(fixture.canonical_bytes(), routed_bytes, "{case}");
            assert_eq!(retained_exchange_syscall_count(), 1, "{case}");
            assert_eq!(fixture.database_snapshot(), database_before, "{case}");
            assert_layout_reversed(usr_before, usr_layout(&fixture), &case);
            assert_eq!(root_link_snapshot(&fixture), root_links_before, "{case}");

            let restored_bytes = fixture.canonical_bytes();
            let usr_restored = usr_layout(&fixture);
            drop(reverse_entry);

            let candidate_route_entry = fixture.enter();
            let candidate_intent = restored.rollback_successor(None).unwrap();
            assert_eq!(candidate_intent.phase, Phase::CandidatePreserveIntent, "{case}");
            assert_eq!(pending(&candidate_route_entry).phase(), Phase::CandidatePreserveIntent, "{case}");
            assert!(pending(&candidate_route_entry).blockers().is_empty(), "{case}");
            assert_eq!(fixture.canonical_record(), candidate_intent, "{case}");
            assert_ne!(fixture.canonical_bytes(), restored_bytes, "{case}");
            assert_eq!(retained_exchange_syscall_count(), 1, "{case}");
            assert_eq!(fixture.database_snapshot(), database_before, "{case}");
            assert_eq!(usr_layout(&fixture), usr_restored, "{case}");
            assert_eq!(root_link_snapshot(&fixture), root_links_before, "{case}");
            drop(candidate_route_entry);

            prepare_archived_candidate_prefix(&fixture, &candidate_intent);
            let mut candidate_entry = fixture.enter();
            if kind == OperationKind::NewState {
                assert_eq!(pending(&candidate_entry).phase(), Phase::CandidatePreserveIntent, "{case}");
                assert_eq!(fixture.canonical_record(), candidate_intent, "{case}");
                assert_eq!(retained_exchange_syscall_count(), 1, "{case}");
                assert_eq!(fixture.database_snapshot(), database_before, "{case}");
                assert_eq!(usr_layout(&fixture), usr_restored, "{case}");
                assert_eq!(root_link_snapshot(&fixture), root_links_before, "{case}");
                drop(candidate_entry);
                candidate_entry = fixture.enter();
            }
            let candidate_preserved = candidate_intent
                .rollback_successor(Some(RollbackActionOutcome::Applied))
                .unwrap();
            assert_eq!(candidate_preserved.phase, Phase::CandidatePreserved, "{case}");
            assert_eq!(pending(&candidate_entry).phase(), Phase::CandidatePreserved, "{case}");
            assert!(pending(&candidate_entry).blockers().is_empty(), "{case}");
            assert_eq!(fixture.canonical_record(), candidate_preserved, "{case}");
            assert_eq!(retained_exchange_syscall_count(), 1, "{case}");
            assert_eq!(fixture.database_snapshot(), database_before, "{case}");
            assert_eq!(root_link_snapshot(&fixture), root_links_before, "{case}");
            assert_eq!(
                active_reblit_candidate_preserve_exchange_attempt_count(),
                usize::from(kind == OperationKind::ActiveReblit),
                "{case}"
            );

            let preserved_bytes = fixture.canonical_bytes();
            let preserved_namespace = fixture.namespace_snapshot();
            drop(candidate_entry);
            match kind {
                OperationKind::NewState => {
                    assert_eq!(candidate_preserved.generation, 15, "{case}");
                    let invalidation_entry = fixture.enter();
                    let invalidation_intent = candidate_preserved.rollback_successor(None).unwrap();
                    assert_eq!(invalidation_intent.phase, Phase::FreshDbInvalidationIntent, "{case}");
                    assert_eq!(invalidation_intent.generation, 16, "{case}");
                    assert_eq!(
                        pending(&invalidation_entry).phase(),
                        Phase::FreshDbInvalidationIntent,
                        "{case}"
                    );
                    assert!(pending(&invalidation_entry).blockers().is_empty(), "{case}");
                    assert_eq!(fixture.canonical_record(), invalidation_intent, "{case}");
                    assert_ne!(fixture.canonical_bytes(), preserved_bytes, "{case}");
                    assert_eq!(retained_exchange_syscall_count(), 1, "{case}");
                    assert_eq!(fixture.database_snapshot(), database_before, "{case}");
                    assert_eq!(fixture.namespace_snapshot(), preserved_namespace, "{case}");
                    assert_eq!(root_link_snapshot(&fixture), root_links_before, "{case}");

                    let invalidation_intent_bytes = fixture.canonical_bytes();
                    drop(invalidation_entry);

                    let invalidated_entry = fixture.enter();
                    let invalidated = invalidation_intent
                        .rollback_successor(Some(RollbackActionOutcome::Applied))
                        .unwrap();
                    assert_eq!(invalidated.phase, Phase::FreshDbInvalidated, "{case}");
                    assert_eq!(invalidated.generation, 17, "{case}");
                    assert_eq!(pending(&invalidated_entry).phase(), Phase::FreshDbInvalidated, "{case}");
                    assert!(pending(&invalidated_entry).blockers().is_empty(), "{case}");
                    assert_eq!(fixture.canonical_record(), invalidated, "{case}");
                    assert_ne!(fixture.canonical_bytes(), invalidation_intent_bytes, "{case}");
                    assert_eq!(retained_exchange_syscall_count(), 1, "{case}");
                    assert_eq!(fresh_db_invalidation_removal_call_count(), 1, "{case}");
                    assert!(matches!(
                        fixture.database.inspect_exact_fresh_transition(
                            fixture.candidate_state,
                            &invalidated.transition_id,
                        ),
                        Ok(crate::db::state::ExactFreshTransitionObservation::JointlyAbsent(_))
                    ));
                    assert_eq!(fixture.namespace_snapshot(), preserved_namespace, "{case}");
                    assert_eq!(root_link_snapshot(&fixture), root_links_before, "{case}");

                    let invalidated_bytes = fixture.canonical_bytes();
                    drop(invalidated_entry);
                    let complete_entry = fixture.enter();
                    let rollback_complete = invalidated.rollback_successor(None).unwrap();
                    assert_eq!(rollback_complete.phase, Phase::RollbackComplete, "{case}");
                    assert_eq!(rollback_complete.generation, 18, "{case}");
                    assert_eq!(pending(&complete_entry).phase(), Phase::RollbackComplete, "{case}");
                    assert!(pending(&complete_entry).blockers().is_empty(), "{case}");
                    assert_eq!(fixture.canonical_record(), rollback_complete, "{case}");
                    assert_ne!(fixture.canonical_bytes(), invalidated_bytes, "{case}");
                    assert_eq!(retained_exchange_syscall_count(), 1, "{case}");
                    assert_eq!(fresh_db_invalidation_removal_call_count(), 1, "{case}");
                    assert!(matches!(
                        fixture.database.inspect_exact_fresh_transition(
                            fixture.candidate_state,
                            &invalidated.transition_id,
                        ),
                        Ok(crate::db::state::ExactFreshTransitionObservation::JointlyAbsent(_))
                    ));
                    assert_eq!(fixture.namespace_snapshot(), preserved_namespace, "{case}");
                    assert_eq!(root_link_snapshot(&fixture), root_links_before, "{case}");

                    let complete_bytes = fixture.canonical_bytes();
                    drop(complete_entry);
                    let stable_entry = fixture.enter();
                    assert_eq!(pending(&stable_entry).phase(), Phase::RollbackComplete, "{case}");
                    assert_eq!(fixture.canonical_record(), rollback_complete, "{case}");
                    assert_eq!(fixture.canonical_bytes(), complete_bytes, "{case}");
                    assert_eq!(retained_exchange_syscall_count(), 1, "{case}");
                    assert_eq!(fresh_db_invalidation_removal_call_count(), 1, "{case}");
                    assert!(matches!(
                        fixture.database.inspect_exact_fresh_transition(
                            fixture.candidate_state,
                            &rollback_complete.transition_id,
                        ),
                        Ok(crate::db::state::ExactFreshTransitionObservation::JointlyAbsent(_))
                    ));
                    assert_eq!(fixture.namespace_snapshot(), preserved_namespace, "{case}");
                    assert_eq!(root_link_snapshot(&fixture), root_links_before, "{case}");
                }
                OperationKind::Archived => {
                    assert_eq!(candidate_preserved.generation, 11, "{case}");
                    let complete_entry = fixture.enter();
                    let rollback_complete = candidate_preserved.rollback_successor(None).unwrap();
                    assert_eq!(rollback_complete.phase, Phase::RollbackComplete, "{case}");
                    assert_eq!(rollback_complete.generation, 12, "{case}");
                    assert_eq!(pending(&complete_entry).phase(), Phase::RollbackComplete, "{case}");
                    assert!(pending(&complete_entry).blockers().is_empty(), "{case}");
                    assert_eq!(fixture.canonical_record(), rollback_complete, "{case}");
                    assert_ne!(fixture.canonical_bytes(), preserved_bytes, "{case}");
                    assert_eq!(retained_exchange_syscall_count(), 1, "{case}");
                    assert_eq!(fixture.database_snapshot(), database_before, "{case}");
                    assert_eq!(fixture.namespace_snapshot(), preserved_namespace, "{case}");
                    assert_eq!(root_link_snapshot(&fixture), root_links_before, "{case}");

                    let complete_bytes = fixture.canonical_bytes();
                    drop(complete_entry);
                    let stable_entry = fixture.enter();
                    assert_eq!(pending(&stable_entry).phase(), Phase::RollbackComplete, "{case}");
                    assert_eq!(fixture.canonical_record(), rollback_complete, "{case}");
                    assert_eq!(fixture.canonical_bytes(), complete_bytes, "{case}");
                    assert_eq!(retained_exchange_syscall_count(), 1, "{case}");
                    assert_eq!(fixture.database_snapshot(), database_before, "{case}");
                    assert_eq!(fixture.namespace_snapshot(), preserved_namespace, "{case}");
                    assert_eq!(root_link_snapshot(&fixture), root_links_before, "{case}");
                }
                OperationKind::ActiveReblit => {
                    assert_eq!(candidate_preserved.generation, 13, "{case}");
                    let complete_entry = fixture.enter();
                    let rollback_complete = candidate_preserved.rollback_successor(None).unwrap();
                    assert_eq!(rollback_complete.phase, Phase::RollbackComplete, "{case}");
                    assert_eq!(rollback_complete.generation, 14, "{case}");
                    assert_eq!(pending(&complete_entry).phase(), Phase::RollbackComplete, "{case}");
                    assert!(pending(&complete_entry).blockers().is_empty(), "{case}");
                    assert_eq!(fixture.canonical_record(), rollback_complete, "{case}");
                    assert_ne!(fixture.canonical_bytes(), preserved_bytes, "{case}");
                    assert_eq!(retained_exchange_syscall_count(), 1, "{case}");
                    assert_eq!(active_reblit_candidate_preserve_exchange_attempt_count(), 1, "{case}");
                    assert_eq!(fixture.database_snapshot(), database_before, "{case}");
                    assert_eq!(fixture.namespace_snapshot(), preserved_namespace, "{case}");
                    assert_eq!(root_link_snapshot(&fixture), root_links_before, "{case}");

                    let complete_bytes = fixture.canonical_bytes();
                    drop(complete_entry);
                    let stable_entry = fixture.enter();
                    assert_eq!(pending(&stable_entry).phase(), Phase::RollbackComplete, "{case}");
                    assert_eq!(fixture.canonical_record(), rollback_complete, "{case}");
                    assert_eq!(fixture.canonical_bytes(), complete_bytes, "{case}");
                    assert_eq!(retained_exchange_syscall_count(), 1, "{case}");
                    assert_eq!(active_reblit_candidate_preserve_exchange_attempt_count(), 1, "{case}");
                    assert_eq!(fixture.database_snapshot(), database_before, "{case}");
                    assert_eq!(fixture.namespace_snapshot(), preserved_namespace, "{case}");
                    assert_eq!(root_link_snapshot(&fixture), root_links_before, "{case}");
                }
            }
        }
    }
}

fn prepare_archived_candidate_prefix(fixture: &Fixture, record: &TransitionRecord) {
    if fixture.kind != OperationKind::Archived {
        return;
    }
    let wrapper = fixture
        .installation
        .root
        .join(".cast/root")
        .join(fixture.candidate_state.to_string());
    create_private_directory(&wrapper);
    fs::hard_link(
        fixture.installation.staging_dir().join("usr/.cast-tree-id"),
        wrapper.join(format!(
            ".cast-state-slot-{}-{}",
            fixture.candidate_state,
            record.candidate.tree_token.as_str()
        )),
    )
    .unwrap();
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct UsrLayout {
    live: (u64, u64),
    staged: (u64, u64),
}

fn usr_layout(fixture: &Fixture) -> UsrLayout {
    UsrLayout {
        live: directory_identity(&fixture.installation.root.join("usr")),
        staged: directory_identity(&fixture.installation.root.join(".cast/root/staging/usr")),
    }
}

fn directory_identity(path: &std::path::Path) -> (u64, u64) {
    let metadata = fs::symlink_metadata(path).unwrap();
    assert!(metadata.is_dir());
    (metadata.dev(), metadata.ino())
}

fn assert_layout_reversed(before: UsrLayout, after: UsrLayout, case: &str) {
    assert_eq!(after.live, before.staged, "{case}");
    assert_eq!(after.staged, before.live, "{case}");
}
