use crate::{
    transition_identity::{reset_retained_exchange_syscall_count, retained_exchange_syscall_count},
    transition_journal::{Phase, RollbackActionOutcome},
};

use super::support::{
    Fixture, OperationKind, ReverseLayout, assert_candidate_preserve_intent_pending, assert_layout_reversed,
    assert_layout_unchanged, assert_usr_restored_pending, enter, expected_candidate_preserve_intent,
    expected_usr_restored, namespace_snapshot, persist_usr_restored_fixture, usr_layout,
};

#[test]
fn startup_usr_rollback_reverse_dispatch_post_and_pre_matrix_reaches_exact_usr_restored() {
    for kind in OperationKind::ALL {
        for layout in [ReverseLayout::Post, ReverseLayout::Pre] {
            let fixture = Fixture::for_effect(kind, layout);
            let outcome = match layout {
                ReverseLayout::Post => RollbackActionOutcome::Applied,
                ReverseLayout::Pre => RollbackActionOutcome::AlreadySatisfied,
            };
            let expected = expected_usr_restored(&fixture, outcome);
            let database_before = fixture.fixture.database_snapshot();
            let namespace_before = namespace_snapshot(&fixture);
            let layout_before = usr_layout(&fixture);
            let root_abi_before = fixture.root_abi_snapshot();
            reset_retained_exchange_syscall_count();

            let error = enter(&fixture);

            assert_usr_restored_pending(&error);
            assert_eq!(fixture.fixture.canonical_record(), expected, "{kind:?} {layout:?}");
            assert_eq!(
                fixture.fixture.canonical_record().generation,
                fixture.record.generation + 1,
                "{kind:?} {layout:?}"
            );
            assert_eq!(
                retained_exchange_syscall_count(),
                usize::from(layout == ReverseLayout::Post),
                "{kind:?} {layout:?}"
            );
            assert_eq!(
                fixture.fixture.database_snapshot(),
                database_before,
                "{kind:?} {layout:?}"
            );
            let layout_after = usr_layout(&fixture);
            match layout {
                ReverseLayout::Post => {
                    assert_layout_reversed(layout_before, layout_after);
                    assert_ne!(namespace_snapshot(&fixture), namespace_before, "{kind:?}");
                }
                ReverseLayout::Pre => {
                    assert_layout_unchanged(layout_before, layout_after);
                    assert_eq!(namespace_snapshot(&fixture), namespace_before, "{kind:?}");
                }
            }
            assert_eq!(fixture.fixture.canonical_record().phase, Phase::UsrRestored);
            fixture.assert_root_abi_unchanged(&root_abi_before);
        }
    }
}

#[test]
fn startup_usr_rollback_reverse_dispatch_usr_restored_routes_without_reverse_redispatch() {
    for kind in OperationKind::ALL {
        for outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
            let fixture = Fixture::for_effect(kind, ReverseLayout::Pre);
            let restored = persist_usr_restored_fixture(&fixture, outcome);
            let preserve_intent = expected_candidate_preserve_intent(&restored);
            let database_before = fixture.fixture.database_snapshot();
            let namespace_before = namespace_snapshot(&fixture);
            let layout_before = usr_layout(&fixture);
            let root_abi_before = fixture.root_abi_snapshot();
            reset_retained_exchange_syscall_count();

            let error = enter(&fixture);

            assert_candidate_preserve_intent_pending(&error);
            assert_eq!(
                fixture.fixture.canonical_record(),
                preserve_intent,
                "{kind:?} {outcome:?}"
            );
            assert_eq!(
                fixture.fixture.database_snapshot(),
                database_before,
                "{kind:?} {outcome:?}"
            );
            assert_eq!(namespace_snapshot(&fixture), namespace_before, "{kind:?} {outcome:?}");
            assert_layout_unchanged(layout_before, usr_layout(&fixture));
            assert_eq!(retained_exchange_syscall_count(), 0, "{kind:?} {outcome:?}");
            fixture.assert_root_abi_unchanged(&root_abi_before);
        }
    }
}
