use crate::{
    client::startup_reconciliation::arm_before_reverse_exchange_reconciliation_capture,
    transition_identity::{
        RetainedExchangeSyscallFault, arm_retained_exchange_syscall_fault, reset_retained_exchange_syscall_count,
        retained_exchange_syscall_count,
    },
    transition_journal::RollbackActionOutcome,
};

use super::support::{
    Fixture, OperationKind, ReverseLayout, assert_ambiguous, assert_layout_reversed, assert_layout_unchanged,
    assert_not_applied, assert_root_links_absent, assert_usr_restored_pending, enter, expected_usr_restored,
    namespace_snapshot, usr_layout,
};

#[derive(Clone, Copy, Debug)]
enum RawSyscallCase {
    SuccessAfterApply,
    ErrorAfterApply,
    ErrorWithoutApply,
    SuccessWithoutApply,
}

impl RawSyscallCase {
    const ALL: [Self; 4] = [
        Self::SuccessAfterApply,
        Self::ErrorAfterApply,
        Self::ErrorWithoutApply,
        Self::SuccessWithoutApply,
    ];

    fn arm(self) {
        match self {
            Self::SuccessAfterApply => reset_retained_exchange_syscall_count(),
            Self::ErrorAfterApply => arm_retained_exchange_syscall_fault(RetainedExchangeSyscallFault::ErrorAfterApply),
            Self::ErrorWithoutApply => {
                arm_retained_exchange_syscall_fault(RetainedExchangeSyscallFault::ErrorWithoutApply)
            }
            Self::SuccessWithoutApply => {
                arm_retained_exchange_syscall_fault(RetainedExchangeSyscallFault::SuccessWithoutApply)
            }
        }
    }

    fn applied(self) -> bool {
        matches!(self, Self::SuccessAfterApply | Self::ErrorAfterApply)
    }
}

#[test]
fn startup_usr_rollback_reverse_dispatch_classifies_all_raw_syscall_reports_by_fresh_layout() {
    for kind in OperationKind::ALL {
        for case in RawSyscallCase::ALL {
            let fixture = Fixture::for_effect(kind, ReverseLayout::Post);
            let source = fixture.record.clone();
            let database_before = fixture.fixture.database_snapshot();
            let namespace_before = namespace_snapshot(&fixture);
            let layout_before = usr_layout(&fixture);
            case.arm();

            let error = enter(&fixture);

            assert_eq!(retained_exchange_syscall_count(), 1, "{kind:?} {case:?}");
            assert_eq!(
                fixture.fixture.database_snapshot(),
                database_before,
                "{kind:?} {case:?}"
            );
            if case.applied() {
                assert_usr_restored_pending(&error);
                assert_eq!(
                    fixture.fixture.canonical_record(),
                    expected_usr_restored(&fixture, RollbackActionOutcome::Applied),
                    "{kind:?} {case:?}"
                );
                assert_layout_reversed(layout_before, usr_layout(&fixture));
                assert_ne!(namespace_snapshot(&fixture), namespace_before, "{kind:?} {case:?}");
            } else {
                assert_not_applied(error);
                assert_eq!(fixture.fixture.canonical_record(), source, "{kind:?} {case:?}");
                assert_layout_unchanged(layout_before, usr_layout(&fixture));
                assert_eq!(namespace_snapshot(&fixture), namespace_before, "{kind:?} {case:?}");
            }
            assert_root_links_absent(&fixture);
        }
    }
}

#[test]
fn startup_usr_rollback_reverse_dispatch_ambiguous_post_attempt_evidence_consumes_retry_capability() {
    for kind in OperationKind::ALL {
        for raw_error in [false, true] {
            let fixture = Fixture::for_effect(kind, ReverseLayout::Post);
            let source = fixture.record.clone();
            let database_before = fixture.fixture.database_snapshot();
            let layout_before = usr_layout(&fixture);
            arm_before_reverse_exchange_reconciliation_capture(
                fixture.namespace_change_hook(format!("real-reverse-dispatch-ambiguous-{kind:?}-{raw_error}")),
            );
            if raw_error {
                arm_retained_exchange_syscall_fault(RetainedExchangeSyscallFault::ErrorAfterApply);
            } else {
                reset_retained_exchange_syscall_count();
            }

            let error = enter(&fixture);

            assert_ambiguous(error);
            assert_eq!(
                fixture.fixture.canonical_record(),
                source,
                "{kind:?} raw_error={raw_error}"
            );
            assert_eq!(
                fixture.fixture.database_snapshot(),
                database_before,
                "{kind:?} raw_error={raw_error}"
            );
            assert_layout_reversed(layout_before, usr_layout(&fixture));
            assert_eq!(retained_exchange_syscall_count(), 1, "{kind:?} raw_error={raw_error}");
            assert_root_links_absent(&fixture);
        }
    }
}
