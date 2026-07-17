use std::{fs, os::unix::fs::PermissionsExt as _, path::Path};

use crate::{
    client::{
        startup_gate,
        startup_reconciliation::{
            arm_before_reverse_exchange_reconciliation_capture,
            arm_before_usr_rollback_reverse_durable_namespace_capture,
            arm_before_usr_rollback_reverse_fresh_namespace_capture,
            arm_between_usr_rollback_reverse_database_captures,
        },
        startup_recovery::{
            UsrRollbackReverseDispatchError, UsrRollbackReversePersistenceError,
            arm_before_usr_rollback_reverse_persistence_final_revalidation,
        },
    },
    transition_identity::{reset_retained_exchange_syscall_count, retained_exchange_syscall_count},
    transition_journal::{Phase, RollbackActionOutcome},
};

use super::support::{
    Fixture, OperationKind, ReverseLayout, assert_layout_reversed, assert_layout_unchanged, assert_root_links_absent,
    assert_usr_restored_pending, enter, expected_usr_restored, namespace_snapshot, pending, usr_layout,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EvidenceRace {
    Database,
    Journal,
    Namespace,
}

impl EvidenceRace {
    const ALL: [Self; 3] = [Self::Database, Self::Journal, Self::Namespace];

    fn supports_post_effect_restore(self, operation: OperationKind) -> bool {
        self != Self::Database || operation == OperationKind::NewState
    }
}

fn evidence_hooks(
    fixture: &Fixture,
    race: EvidenceRace,
    operation: OperationKind,
    label: &str,
) -> (Box<dyn FnOnce()>, Option<Box<dyn FnOnce()>>) {
    match race {
        EvidenceRace::Database => {
            let candidate = fixture.fixture.candidate_state;
            let transition = fixture.record.transition_id.clone();
            let provenance = fixture
                .fixture
                .database
                .metadata_provenance(candidate)
                .unwrap()
                .expect("NewState reverse evidence must retain candidate provenance");
            let injecting_database = fixture.fixture.database.clone();
            let restoring_database = fixture.fixture.database.clone();
            let inject = Box::new(move || {
                injecting_database
                    .delete_metadata_provenance_for_test(candidate)
                    .unwrap();
            });
            let restore = (operation == OperationKind::NewState).then(|| {
                Box::new(move || {
                    restoring_database
                        .insert_fresh_metadata_provenance_if_transition_matches(candidate, &transition, &provenance)
                        .unwrap();
                }) as Box<dyn FnOnce()>
            });
            (inject, restore)
        }
        EvidenceRace::Journal => {
            let inject = fixture.journal_change_hook();
            let canonical = fixture.fixture.installation.root.join(".cast/journal/state-transition");
            let source = fixture.fixture.canonical_bytes();
            (
                Box::new(inject),
                Some(Box::new(move || {
                    fs::write(canonical, source).unwrap();
                })),
            )
        }
        EvidenceRace::Namespace => {
            let path = fixture.fixture.installation.state_quarantine_dir().join(label);
            let inserted = path.clone();
            (
                Box::new(move || create_private_directory(&inserted)),
                Some(Box::new(move || fs::remove_dir(path).unwrap())),
            )
        }
    }
}

fn create_private_directory(path: &Path) {
    fs::create_dir(path).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

fn assert_dispatch_authority(error: &startup_gate::Error) {
    assert!(
        matches!(
            error,
            startup_gate::Error::UsrRollbackReverseDispatch(UsrRollbackReverseDispatchError::Authority(_))
        ),
        "expected typed dispatcher authority error, got {error:?}"
    );
}

fn assert_final_persistence_authority(error: &startup_gate::Error) {
    assert!(
        matches!(
            error,
            startup_gate::Error::UsrRollbackReverseDispatch(UsrRollbackReverseDispatchError::Persistence(
                UsrRollbackReversePersistenceError::Authority(_)
            ))
        ),
        "expected typed final persistence-authority error, got {error:?}"
    );
}

fn assert_injected_evidence<Database, Namespace>(
    fixture: &Fixture,
    race: EvidenceRace,
    source: &crate::transition_journal::TransitionRecord,
    database_before: &Database,
    database_after: &Database,
    namespace_before: &Namespace,
    namespace_after: &Namespace,
) where
    Database: std::fmt::Debug + Eq,
    Namespace: std::fmt::Debug + Eq,
{
    match race {
        EvidenceRace::Database => {
            assert_ne!(database_after, database_before);
            assert_eq!(fixture.fixture.canonical_record(), *source);
        }
        EvidenceRace::Journal => {
            assert_eq!(database_after, database_before);
            assert_ne!(fixture.fixture.canonical_record(), *source);
        }
        EvidenceRace::Namespace => {
            assert_eq!(database_after, database_before);
            assert_ne!(namespace_after, namespace_before);
            assert_eq!(fixture.fixture.canonical_record(), *source);
        }
    }
}

fn restore_source_and_finish(
    fixture: &Fixture,
    restore: Box<dyn FnOnce()>,
    expected_outcome: RollbackActionOutcome,
    expected_exchange_count: usize,
) {
    restore();
    assert_eq!(fixture.fixture.canonical_record(), fixture.record);

    let restarted = enter(fixture);

    assert_usr_restored_pending(&restarted);
    assert_eq!(
        fixture.fixture.canonical_record(),
        expected_usr_restored(fixture, expected_outcome)
    );
    assert_eq!(retained_exchange_syscall_count(), expected_exchange_count);
    assert_root_links_absent(fixture);
}

#[test]
fn startup_usr_rollback_reverse_dispatch_admission_races_are_zero_effect_zero_advance() {
    for race in EvidenceRace::ALL {
        for kind in OperationKind::ALL {
            for layout in [ReverseLayout::Post, ReverseLayout::Pre] {
                let fixture = Fixture::for_effect(kind, layout);
                let source = fixture.record.clone();
                let database_before = fixture.fixture.database_snapshot();
                let namespace_before = namespace_snapshot(&fixture);
                let layout_before = usr_layout(&fixture);
                let (inject, restore) = evidence_hooks(
                    &fixture,
                    race,
                    kind,
                    &format!("reverse-dispatch-admission-{race:?}-{kind:?}-{layout:?}"),
                );
                arm_between_usr_rollback_reverse_database_captures(inject);
                reset_retained_exchange_syscall_count();

                let error = enter(&fixture);

                assert_eq!(pending(&error).phase(), Phase::ReverseExchangeIntent);
                assert!(!pending(&error).blockers().is_empty());
                assert_eq!(retained_exchange_syscall_count(), 0, "{race:?} {kind:?} {layout:?}");
                assert_layout_unchanged(layout_before, usr_layout(&fixture));
                let database_after = fixture.fixture.database_snapshot();
                let namespace_after = namespace_snapshot(&fixture);
                assert_injected_evidence(
                    &fixture,
                    race,
                    &source,
                    &database_before,
                    &database_after,
                    &namespace_before,
                    &namespace_after,
                );
                assert_root_links_absent(&fixture);
                drop(error);

                if let Some(restore) = restore {
                    let expected_outcome = match layout {
                        ReverseLayout::Post => RollbackActionOutcome::Applied,
                        ReverseLayout::Pre => RollbackActionOutcome::AlreadySatisfied,
                    };
                    restore_source_and_finish(
                        &fixture,
                        restore,
                        expected_outcome,
                        usize::from(layout == ReverseLayout::Post),
                    );
                }
            }
        }
    }
}

#[test]
fn startup_usr_rollback_reverse_dispatch_effect_boundary_races_never_advance_or_retry() {
    for race in EvidenceRace::ALL {
        for kind in OperationKind::ALL {
            for layout in [ReverseLayout::Post, ReverseLayout::Pre] {
                let fixture = Fixture::for_effect(kind, layout);
                let source = fixture.record.clone();
                let database_before = fixture.fixture.database_snapshot();
                let namespace_before = namespace_snapshot(&fixture);
                let layout_before = usr_layout(&fixture);
                let (inject, restore) = evidence_hooks(
                    &fixture,
                    race,
                    kind,
                    &format!("reverse-dispatch-pre-effect-{race:?}-{kind:?}-{layout:?}"),
                );
                arm_before_usr_rollback_reverse_fresh_namespace_capture(inject);
                reset_retained_exchange_syscall_count();

                let error = enter(&fixture);

                assert_dispatch_authority(&error);
                assert_eq!(retained_exchange_syscall_count(), 0, "{race:?} {kind:?} {layout:?}");
                assert_layout_unchanged(layout_before, usr_layout(&fixture));
                let database_after = fixture.fixture.database_snapshot();
                let namespace_after = namespace_snapshot(&fixture);
                assert_injected_evidence(
                    &fixture,
                    race,
                    &source,
                    &database_before,
                    &database_after,
                    &namespace_before,
                    &namespace_after,
                );
                assert_root_links_absent(&fixture);
                drop(error);

                if let Some(restore) = restore {
                    let expected_outcome = match layout {
                        ReverseLayout::Post => RollbackActionOutcome::Applied,
                        ReverseLayout::Pre => RollbackActionOutcome::AlreadySatisfied,
                    };
                    restore_source_and_finish(
                        &fixture,
                        restore,
                        expected_outcome,
                        usize::from(layout == ReverseLayout::Post),
                    );
                }
            }
        }
    }

    for race in EvidenceRace::ALL {
        for kind in OperationKind::ALL {
            if !race.supports_post_effect_restore(kind) {
                continue;
            }
            let fixture = Fixture::for_effect(kind, ReverseLayout::Post);
            let source = fixture.record.clone();
            let database_before = fixture.fixture.database_snapshot();
            let namespace_before = namespace_snapshot(&fixture);
            let layout_before = usr_layout(&fixture);
            let (inject, restore) = evidence_hooks(
                &fixture,
                race,
                kind,
                &format!("reverse-dispatch-post-effect-{race:?}-{kind:?}"),
            );
            arm_before_reverse_exchange_reconciliation_capture(inject);
            reset_retained_exchange_syscall_count();

            let error = enter(&fixture);

            match race {
                EvidenceRace::Namespace => assert!(
                    matches!(
                        error,
                        startup_gate::Error::UsrRollbackReverseDispatch(UsrRollbackReverseDispatchError::Ambiguous)
                    ),
                    "expected typed ambiguity, got {error:?}"
                ),
                EvidenceRace::Database | EvidenceRace::Journal => assert_dispatch_authority(&error),
            }
            assert_eq!(retained_exchange_syscall_count(), 1, "{race:?} {kind:?}");
            assert_layout_reversed(layout_before, usr_layout(&fixture));
            let database_after = fixture.fixture.database_snapshot();
            let namespace_after = namespace_snapshot(&fixture);
            assert_injected_evidence(
                &fixture,
                race,
                &source,
                &database_before,
                &database_after,
                &namespace_before,
                &namespace_after,
            );
            assert_root_links_absent(&fixture);
            drop(error);

            restore_source_and_finish(
                &fixture,
                restore.expect("post-effect matrix includes only reversibly restored evidence"),
                RollbackActionOutcome::AlreadySatisfied,
                1,
            );
        }
    }
}

#[test]
fn startup_usr_rollback_reverse_dispatch_final_durable_revalidation_races_leave_source_for_fresh_startup() {
    for race in EvidenceRace::ALL {
        for kind in OperationKind::ALL {
            if !race.supports_post_effect_restore(kind) {
                continue;
            }
            for layout in [ReverseLayout::Post, ReverseLayout::Pre] {
                let fixture = Fixture::for_effect(kind, layout);
                let source = fixture.record.clone();
                let database_before = fixture.fixture.database_snapshot();
                let namespace_before = namespace_snapshot(&fixture);
                let layout_before = usr_layout(&fixture);
                let (inject, restore) = evidence_hooks(
                    &fixture,
                    race,
                    kind,
                    &format!("reverse-dispatch-final-durable-{race:?}-{kind:?}-{layout:?}"),
                );
                let final_race: Box<dyn FnOnce()> = match race {
                    EvidenceRace::Namespace => Box::new(move || {
                        arm_before_usr_rollback_reverse_durable_namespace_capture(inject);
                    }),
                    EvidenceRace::Database | EvidenceRace::Journal => inject,
                };
                arm_before_usr_rollback_reverse_persistence_final_revalidation(final_race);
                reset_retained_exchange_syscall_count();

                let error = enter(&fixture);

                assert_final_persistence_authority(&error);
                let expected_exchange_count = usize::from(layout == ReverseLayout::Post);
                assert_eq!(
                    retained_exchange_syscall_count(),
                    expected_exchange_count,
                    "{race:?} {kind:?} {layout:?}"
                );
                match layout {
                    ReverseLayout::Post => assert_layout_reversed(layout_before, usr_layout(&fixture)),
                    ReverseLayout::Pre => assert_layout_unchanged(layout_before, usr_layout(&fixture)),
                }
                let database_after = fixture.fixture.database_snapshot();
                let namespace_after = namespace_snapshot(&fixture);
                assert_injected_evidence(
                    &fixture,
                    race,
                    &source,
                    &database_before,
                    &database_after,
                    &namespace_before,
                    &namespace_after,
                );
                assert_root_links_absent(&fixture);
                drop(error);

                restore_source_and_finish(
                    &fixture,
                    restore.expect("final-persistence matrix includes only reversibly restored evidence"),
                    RollbackActionOutcome::AlreadySatisfied,
                    expected_exchange_count,
                );
            }
        }
    }
}
