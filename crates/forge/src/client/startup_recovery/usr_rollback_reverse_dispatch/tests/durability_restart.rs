use crate::{
    client::{
        startup_gate,
        startup_reconciliation::{
            UsrRollbackReverseNamespaceDurabilityEvent, UsrRollbackReverseNamespaceDurabilityFaultPoint,
            arm_usr_rollback_reverse_namespace_durability_fault,
            reset_usr_rollback_reverse_namespace_durability_events,
            take_usr_rollback_reverse_namespace_durability_events,
        },
    },
    transition_identity::{
        RetainedExchangeSyscallFault, arm_retained_exchange_syscall_fault, reset_retained_exchange_syscall_count,
        retained_exchange_syscall_count,
    },
    transition_journal::{Phase, RollbackActionOutcome},
};

use super::super::UsrRollbackReverseDispatchError;
use super::support::{
    Fixture, OperationKind, ReverseLayout, assert_layout_reversed, assert_layout_unchanged, assert_root_links_absent,
    assert_usr_restored_pending, enter, expected_usr_restored, usr_layout,
};

#[derive(Clone, Copy, Debug)]
enum ParentDurabilityFault {
    StagingParentSync,
    InstallationRootSync,
}

impl ParentDurabilityFault {
    const ALL: [Self; 2] = [Self::StagingParentSync, Self::InstallationRootSync];

    fn point(self) -> UsrRollbackReverseNamespaceDurabilityFaultPoint {
        match self {
            Self::StagingParentSync => UsrRollbackReverseNamespaceDurabilityFaultPoint::StagingParentSync,
            Self::InstallationRootSync => UsrRollbackReverseNamespaceDurabilityFaultPoint::InstallationRootSync,
        }
    }
}

#[test]
fn startup_usr_rollback_reverse_dispatch_parent_durability_faults_restart_as_pre_without_second_exchange() {
    for kind in OperationKind::ALL {
        for fault in ParentDurabilityFault::ALL {
            for raw_error in [false, true] {
                let fixture = Fixture::for_effect(kind, ReverseLayout::Post);
                let source = fixture.record.clone();
                let database_before = fixture.fixture.database_snapshot();
                let post_layout = usr_layout(&fixture);
                reset_retained_exchange_syscall_count();
                reset_usr_rollback_reverse_namespace_durability_events();
                arm_usr_rollback_reverse_namespace_durability_fault(fault.point());
                if raw_error {
                    arm_retained_exchange_syscall_fault(RetainedExchangeSyscallFault::ErrorAfterApply);
                }

                {
                    let first = enter(&fixture);

                    assert!(
                        matches!(
                            &first,
                            startup_gate::Error::UsrRollbackReverseDispatch(
                                UsrRollbackReverseDispatchError::Durability(_)
                            )
                        ),
                        "expected nested durability error for {kind:?} {fault:?} raw_error={raw_error}, got {first:?}"
                    );
                    assert_eq!(
                        fixture.fixture.canonical_record(),
                        source,
                        "{kind:?} {fault:?} raw_error={raw_error}"
                    );
                    assert_eq!(
                        retained_exchange_syscall_count(),
                        1,
                        "{kind:?} {fault:?} raw_error={raw_error}"
                    );
                    assert_eq!(
                        fixture.fixture.database_snapshot(),
                        database_before,
                        "{kind:?} {fault:?} raw_error={raw_error}"
                    );
                    assert_layout_reversed(post_layout, usr_layout(&fixture));
                    assert_eq!(
                        take_usr_rollback_reverse_namespace_durability_events(),
                        failure_events(&fixture, fault),
                        "{kind:?} {fault:?} raw_error={raw_error}"
                    );
                    assert_root_links_absent(&fixture);
                    drop(first);
                }

                let pre_layout = usr_layout(&fixture);
                reset_usr_rollback_reverse_namespace_durability_events();
                let restored = expected_usr_restored(&fixture, RollbackActionOutcome::AlreadySatisfied);
                let second = enter(&fixture);

                assert_usr_restored_pending(&second);
                assert_eq!(
                    fixture.fixture.canonical_record(),
                    restored,
                    "{kind:?} {fault:?} raw_error={raw_error}"
                );
                assert_eq!(fixture.fixture.canonical_record().phase, Phase::UsrRestored);
                assert_eq!(
                    retained_exchange_syscall_count(),
                    1,
                    "{kind:?} {fault:?} raw_error={raw_error}"
                );
                assert_eq!(
                    fixture.fixture.database_snapshot(),
                    database_before,
                    "{kind:?} {fault:?} raw_error={raw_error}"
                );
                assert_layout_unchanged(pre_layout, usr_layout(&fixture));
                assert_eq!(
                    take_usr_rollback_reverse_namespace_durability_events(),
                    success_events(&fixture),
                    "{kind:?} {fault:?} raw_error={raw_error}"
                );
                assert_root_links_absent(&fixture);
                drop(second);

                let third = enter(&fixture);
                assert_usr_restored_pending(&third);
                assert_eq!(
                    fixture.fixture.canonical_record(),
                    restored,
                    "{kind:?} {fault:?} raw_error={raw_error}"
                );
                assert_eq!(
                    retained_exchange_syscall_count(),
                    1,
                    "{kind:?} {fault:?} raw_error={raw_error}"
                );
                assert_eq!(fixture.fixture.database_snapshot(), database_before);
                assert_layout_unchanged(pre_layout, usr_layout(&fixture));
                assert!(take_usr_rollback_reverse_namespace_durability_events().is_empty());
                assert_root_links_absent(&fixture);
            }
        }
    }
}

fn failure_events(fixture: &Fixture, fault: ParentDurabilityFault) -> Vec<UsrRollbackReverseNamespaceDurabilityEvent> {
    match fault {
        ParentDurabilityFault::StagingParentSync => Vec::new(),
        ParentDurabilityFault::InstallationRootSync => {
            let ((device, inode), _) = fixture.durability_parent_identities();
            vec![UsrRollbackReverseNamespaceDurabilityEvent::StagingParentSynced { device, inode }]
        }
    }
}

fn success_events(fixture: &Fixture) -> Vec<UsrRollbackReverseNamespaceDurabilityEvent> {
    let ((staging_device, staging_inode), (root_device, root_inode)) = fixture.durability_parent_identities();
    vec![
        UsrRollbackReverseNamespaceDurabilityEvent::StagingParentSynced {
            device: staging_device,
            inode: staging_inode,
        },
        UsrRollbackReverseNamespaceDurabilityEvent::InstallationRootSynced {
            device: root_device,
            inode: root_inode,
        },
        UsrRollbackReverseNamespaceDurabilityEvent::FinalPreProven,
    ]
}
