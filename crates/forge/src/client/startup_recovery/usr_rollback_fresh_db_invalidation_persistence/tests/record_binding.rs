use std::{
    fs,
    os::unix::fs::{MetadataExt as _, PermissionsExt as _},
    path::Path,
};

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_reconciliation::fresh_db_invalidation_removal_call_count,
        startup_recovery::{
            DurableUsrRollbackFreshDbInvalidationRecord, UsrRollbackFreshDbInvalidationPersistenceError,
            UsrRollbackFreshDbInvalidationSuccessorBindingError,
            arm_after_usr_rollback_fresh_db_invalidation_successor_binding_check_before_reopen,
            arm_before_usr_rollback_fresh_db_invalidation_successor_binding_revalidation,
            persist_usr_rollback_fresh_db_invalidation_and_reopen,
        },
    },
    transition_journal::{
        PublicBindingRevalidationBoundary, RollbackActionOutcome, arm_public_binding_revalidation_callback,
        assert_public_binding_revalidation_callback_consumed,
    },
};

use super::support::{
    CandidateResult, Fixture, FreshDbInvalidationOrigin, Source, canonical_journal, database_snapshot,
    effect_authority, expected_fresh_db_invalidated, fixture_for_origin, non_journal_namespace_snapshot,
};

fn inode_identity(path: &Path) -> (u64, u64) {
    let metadata = fs::symlink_metadata(path).unwrap();
    (metadata.dev(), metadata.ino())
}

fn same_byte_different_inode_hook(fixture: &Fixture, label: String) -> impl FnOnce() + 'static {
    let canonical = canonical_journal(&fixture.fixture.fixture.installation.root);
    let displaced = fixture
        .fixture
        .fixture
        .installation
        .root
        .join(".cast/journal")
        .join(format!(".{label}-displaced"));
    move || {
        let bytes = fs::read(&canonical).unwrap();
        fs::rename(&canonical, &displaced).unwrap();
        let retained_identity = inode_identity(&displaced);
        fs::write(&canonical, &bytes).unwrap();
        fs::set_permissions(&canonical, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(fs::read(&canonical).unwrap(), bytes);
        assert_ne!(retained_identity, inode_identity(&canonical));
        fs::remove_file(displaced).unwrap();
    }
}

fn assert_invalidation_only(
    fixture: &Fixture,
    database_before: &super::super::test_fixture::DatabaseSnapshot,
    namespace_before: &[super::super::test_fixture::NamespaceEntry],
    expected_removals: usize,
) {
    assert_eq!(database_snapshot(fixture), *database_before);
    assert_eq!(non_journal_namespace_snapshot(fixture), namespace_before);
    fixture.assert_exact_joint_absence();
    assert_eq!(fresh_db_invalidation_removal_call_count(), expected_removals);
    let names = fs::read_dir(fixture.fixture.fixture.installation.root.join(".cast/journal"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    assert_eq!(names.len(), 2, "bound invalidation left journal residue: {names:?}");
}

#[test]
fn startup_fresh_db_invalidation_bound_advance_same_byte_replacements_never_succeed() {
    let mut executions = 0;
    for (boundary, expected_durable) in [
        (
            PublicBindingRevalidationBoundary::BeforeBoundAdvancePublish,
            DurableUsrRollbackFreshDbInvalidationRecord::FreshDbInvalidationIntent,
        ),
        (
            PublicBindingRevalidationBoundary::BeforeBoundAdvanceFinalBinding,
            DurableUsrRollbackFreshDbInvalidationRecord::FreshDbInvalidated,
        ),
    ] {
        for origin in FreshDbInvalidationOrigin::ALL {
            for historical in [false, true] {
                for source in Source::THROUGH_FRESH_DB_INVALIDATED {
                    for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
                        for candidate_outcome in CandidateResult::ALL {
                            executions += 1;
                            let fixture =
                                fixture_for_origin(origin, historical, source, usr_outcome, candidate_outcome);
                            let journal = fixture.open_journal();
                            let reservation = ActiveStateReservation::acquire().unwrap();
                            let authority = effect_authority(&fixture, &journal, &reservation, origin);
                            let database_before = database_snapshot(&fixture);
                            let namespace_before = non_journal_namespace_snapshot(&fixture);
                            let successor = expected_fresh_db_invalidated(&fixture, origin);
                            let expected_removals = usize::from(origin == FreshDbInvalidationOrigin::Applied);
                            let hook = same_byte_different_inode_hook(
                                &fixture,
                                format!(
                                    "bound-{boundary:?}-{origin:?}-{historical}-{source:?}-{usr_outcome:?}-{candidate_outcome:?}"
                                ),
                            );
                            arm_public_binding_revalidation_callback(boundary, hook);

                            let error = persist_usr_rollback_fresh_db_invalidation_and_reopen(journal, authority)
                                .unwrap_err();

                            assert_public_binding_revalidation_callback_consumed();
                            assert!(matches!(
                                error,
                                UsrRollbackFreshDbInvalidationPersistenceError::Advance { durable, .. }
                                    if durable == expected_durable
                            ));
                            match expected_durable {
                                DurableUsrRollbackFreshDbInvalidationRecord::FreshDbInvalidationIntent => {
                                    assert_eq!(fixture.canonical_record(), fixture.record)
                                }
                                DurableUsrRollbackFreshDbInvalidationRecord::FreshDbInvalidated => {
                                    assert_eq!(fixture.canonical_record(), successor)
                                }
                            }
                            assert_invalidation_only(
                                &fixture,
                                &database_before,
                                &namespace_before,
                                expected_removals,
                            );
                        }
                    }
                }
            }
        }
    }
    assert_eq!(executions, 96);
}

#[test]
fn startup_fresh_db_invalidation_same_byte_successor_replacement_fails_same_store_binding() {
    let mut executions = 0;
    for origin in FreshDbInvalidationOrigin::ALL {
        for historical in [false, true] {
            for source in Source::THROUGH_FRESH_DB_INVALIDATED {
                for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
                    for candidate_outcome in CandidateResult::ALL {
                        executions += 1;
                        let fixture = fixture_for_origin(origin, historical, source, usr_outcome, candidate_outcome);
                        let journal = fixture.open_journal();
                        let reservation = ActiveStateReservation::acquire().unwrap();
                        let authority = effect_authority(&fixture, &journal, &reservation, origin);
                        let database_before = database_snapshot(&fixture);
                        let namespace_before = non_journal_namespace_snapshot(&fixture);
                        let successor = expected_fresh_db_invalidated(&fixture, origin);
                        let expected_removals = usize::from(origin == FreshDbInvalidationOrigin::Applied);
                        let hook = same_byte_different_inode_hook(
                            &fixture,
                            format!("published-{origin:?}-{historical}-{source:?}-{usr_outcome:?}-{candidate_outcome:?}"),
                        );
                        arm_before_usr_rollback_fresh_db_invalidation_successor_binding_revalidation(hook);

                        let error =
                            persist_usr_rollback_fresh_db_invalidation_and_reopen(journal, authority).unwrap_err();

                        assert!(matches!(
                            error,
                            UsrRollbackFreshDbInvalidationPersistenceError::SuccessorRecordBinding {
                                durable: DurableUsrRollbackFreshDbInvalidationRecord::FreshDbInvalidated,
                                source: UsrRollbackFreshDbInvalidationSuccessorBindingError::Changed,
                            }
                        ));
                        assert_eq!(fixture.canonical_record(), successor);
                        assert_invalidation_only(
                            &fixture,
                            &database_before,
                            &namespace_before,
                            expected_removals,
                        );
                    }
                }
            }
        }
    }
    assert_eq!(executions, 48);
}

#[test]
fn startup_fresh_db_invalidation_same_byte_successor_replacement_fails_reopened_binding() {
    let mut executions = 0;
    for origin in FreshDbInvalidationOrigin::ALL {
        for historical in [false, true] {
            for source in Source::THROUGH_FRESH_DB_INVALIDATED {
                for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
                    for candidate_outcome in CandidateResult::ALL {
                        executions += 1;
                        let fixture = fixture_for_origin(origin, historical, source, usr_outcome, candidate_outcome);
                        let journal = fixture.open_journal();
                        let reservation = ActiveStateReservation::acquire().unwrap();
                        let authority = effect_authority(&fixture, &journal, &reservation, origin);
                        let database_before = database_snapshot(&fixture);
                        let namespace_before = non_journal_namespace_snapshot(&fixture);
                        let successor = expected_fresh_db_invalidated(&fixture, origin);
                        let expected_removals = usize::from(origin == FreshDbInvalidationOrigin::Applied);
                        let hook = same_byte_different_inode_hook(
                            &fixture,
                            format!("reopened-{origin:?}-{historical}-{source:?}-{usr_outcome:?}-{candidate_outcome:?}"),
                        );
                        arm_after_usr_rollback_fresh_db_invalidation_successor_binding_check_before_reopen(hook);

                        let error =
                            persist_usr_rollback_fresh_db_invalidation_and_reopen(journal, authority).unwrap_err();

                        assert!(matches!(
                            error,
                            UsrRollbackFreshDbInvalidationPersistenceError::SuccessorRecordBinding {
                                durable: DurableUsrRollbackFreshDbInvalidationRecord::FreshDbInvalidated,
                                source: UsrRollbackFreshDbInvalidationSuccessorBindingError::Changed,
                            }
                        ));
                        assert_eq!(fixture.canonical_record(), successor);
                        assert_invalidation_only(
                            &fixture,
                            &database_before,
                            &namespace_before,
                            expected_removals,
                        );
                    }
                }
            }
        }
    }
    assert_eq!(executions, 48);
}
