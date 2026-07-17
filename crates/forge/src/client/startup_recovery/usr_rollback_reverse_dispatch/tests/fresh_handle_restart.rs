use std::{fs, path::Path};

use crate::{
    Installation,
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_gate::{self, CleanSystemStartup},
        startup_reconciliation::{
            UsrRollbackReverseNamespaceDurabilityFaultPoint, arm_usr_rollback_reverse_namespace_durability_fault,
            reset_usr_rollback_reverse_namespace_durability_events,
        },
    },
    transition_identity::{reset_retained_exchange_syscall_count, retained_exchange_syscall_count},
    transition_journal::{
        RollbackActionOutcome, arm_next_temporary_sync_fault, assert_temporary_sync_fault_consumed, decode,
    },
};

use super::super::UsrRollbackReverseDispatchError;
use super::support::{
    Fixture, OperationKind, ReverseLayout, assert_layout_reversed, assert_layout_unchanged,
    assert_usr_restored_pending, expected_usr_restored, open_state_database, persistent_state_database,
    release_fixture_handles, usr_layout, usr_layout_at,
};

#[derive(Clone, Copy, Debug)]
enum RestartFault {
    FinalPreCapture,
    JournalTemporarySync,
}

impl RestartFault {
    const ALL: [Self; 2] = [Self::FinalPreCapture, Self::JournalTemporarySync];

    fn arm(self) {
        match self {
            Self::FinalPreCapture => arm_usr_rollback_reverse_namespace_durability_fault(
                UsrRollbackReverseNamespaceDurabilityFaultPoint::FinalPreCapture,
            ),
            Self::JournalTemporarySync => arm_next_temporary_sync_fault(),
        }
    }

    fn assert_error(self, error: &startup_gate::Error) {
        match self {
            Self::FinalPreCapture => assert!(
                matches!(
                    error,
                    startup_gate::Error::UsrRollbackReverseDispatch(UsrRollbackReverseDispatchError::Durability(_))
                ),
                "expected parent-durability failure, got {error:?}"
            ),
            Self::JournalTemporarySync => {
                assert_temporary_sync_fault_consumed();
                assert!(
                    matches!(
                        error,
                        startup_gate::Error::UsrRollbackReverseDispatch(UsrRollbackReverseDispatchError::Persistence(
                            _
                        ))
                    ),
                    "expected journal-persistence failure, got {error:?}"
                );
            }
        }
    }
}

#[test]
fn startup_usr_rollback_reverse_dispatch_fresh_handles_restart_pre_without_second_exchange() {
    for kind in OperationKind::ALL {
        for fault in RestartFault::ALL {
            let mut fixture = Fixture::for_effect(kind, ReverseLayout::Post);
            let root = fixture.fixture.installation.root.clone();
            let source = fixture.record.clone();
            let restored = expected_usr_restored(&fixture, RollbackActionOutcome::AlreadySatisfied);
            let layout_before = usr_layout(&fixture);
            let state_database = persistent_state_database(&fixture, kind);
            let states_before = state_database.all().unwrap();
            let in_flight_before = state_database.audit_in_flight_transition().unwrap();
            reset_retained_exchange_syscall_count();
            reset_usr_rollback_reverse_namespace_durability_events();
            fault.arm();

            let first = enter_with_handles(&fixture.fixture.installation, &state_database);

            fault.assert_error(&first);
            assert_eq!(retained_exchange_syscall_count(), 1, "{kind:?} {fault:?}");
            assert_eq!(canonical_record(&root), source, "{kind:?} {fault:?}");
            assert_layout_reversed(layout_before, usr_layout_at(&root));
            let pre_layout = usr_layout_at(&root);

            // No startup result, retained database connection, installation
            // authority, or reservation crosses the simulated restart.
            drop(first);
            drop(state_database);
            let replacement_root = release_fixture_handles(&mut fixture);

            let installation = Installation::open(&root, None).unwrap();
            let state_database = open_state_database(&installation);
            assert_eq!(state_database.all().unwrap(), states_before, "{kind:?} {fault:?}");
            assert_eq!(
                state_database.audit_in_flight_transition().unwrap(),
                in_flight_before,
                "{kind:?} {fault:?}"
            );

            let restart = enter_with_handles(&installation, &state_database);

            assert_usr_restored_pending(&restart);
            assert_eq!(canonical_record(&root), restored, "{kind:?} {fault:?}");
            assert_eq!(
                retained_exchange_syscall_count(),
                1,
                "fresh-handle PRE restart exchanged twice for {kind:?} {fault:?}"
            );
            assert_layout_unchanged(pre_layout, usr_layout_at(&root));
            assert_eq!(state_database.all().unwrap(), states_before, "{kind:?} {fault:?}");
            assert_eq!(
                state_database.audit_in_flight_transition().unwrap(),
                in_flight_before,
                "{kind:?} {fault:?}"
            );
            reset_usr_rollback_reverse_namespace_durability_events();

            drop(restart);
            drop(state_database);
            drop(installation);
            drop(fixture);
            drop(replacement_root);
        }
    }
}

fn enter_with_handles(installation: &Installation, state_database: &crate::db::state::Database) -> startup_gate::Error {
    let reservation = ActiveStateReservation::acquire().unwrap();
    match CleanSystemStartup::enter(installation, state_database, &reservation) {
        Ok(_) => panic!("startup unexpectedly admitted an unresolved rollback"),
        Err(error) => error,
    }
}

fn canonical_record(root: &Path) -> crate::transition_journal::TransitionRecord {
    decode(&fs::read(root.join(".cast/journal/state-transition")).unwrap()).unwrap()
}
