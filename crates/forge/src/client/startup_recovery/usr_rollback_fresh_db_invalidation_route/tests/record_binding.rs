use std::{
    fs,
    os::unix::fs::{MetadataExt as _, PermissionsExt as _},
    path::Path,
};

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_reconciliation::{
            new_state_candidate_preserve_move_attempt_count,
            reset_new_state_candidate_preserve_move_attempt_count,
        },
        startup_recovery::{
            DurableUsrRollbackFreshDbInvalidationRouteRecord,
            UsrRollbackFreshDbInvalidationRoutePersistenceError,
            UsrRollbackFreshDbInvalidationRouteSuccessorBindingError,
            arm_after_usr_rollback_fresh_db_invalidation_route_successor_binding_check_before_reopen,
            arm_before_usr_rollback_fresh_db_invalidation_route_successor_binding_revalidation,
            persist_usr_rollback_fresh_db_invalidation_route_and_reopen,
        },
    },
    transition_journal::{
        PublicBindingRevalidationBoundary, RollbackActionOutcome, arm_public_binding_revalidation_callback,
        assert_public_binding_revalidation_callback_consumed,
    },
};

use super::support::{CandidateOutcome, CandidateSource, RouteFixture, canonical_journal};

fn inode_identity(path: &Path) -> (u64, u64) {
    let metadata = fs::symlink_metadata(path).unwrap();
    (metadata.dev(), metadata.ino())
}

fn same_byte_different_inode_hook(fixture: &RouteFixture, label: String) -> impl FnOnce() + 'static {
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

fn assert_route_only(
    fixture: &RouteFixture,
    database_before: &super::super::test_fixture::DatabaseSnapshot,
    namespace_before: &[super::super::test_fixture::NamespaceEntry],
) {
    assert_eq!(fixture.database_snapshot(), *database_before);
    assert_eq!(fixture.namespace_snapshot(), namespace_before);
    assert_eq!(new_state_candidate_preserve_move_attempt_count(), 0);
    let names = fs::read_dir(fixture.fixture.fixture.installation.root.join(".cast/journal"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    assert_eq!(names.len(), 2, "bound route left journal residue: {names:?}");
}

#[test]
fn startup_usr_rollback_fresh_db_invalidation_route_bound_advance_same_byte_replacements_never_succeed() {
    for (boundary, expected_durable) in [
        (
            PublicBindingRevalidationBoundary::BeforeBoundAdvancePublish,
            DurableUsrRollbackFreshDbInvalidationRouteRecord::CandidatePreserved,
        ),
        (
            PublicBindingRevalidationBoundary::BeforeBoundAdvanceFinalBinding,
            DurableUsrRollbackFreshDbInvalidationRouteRecord::FreshDbInvalidationIntent,
        ),
    ] {
        for historical in [false, true] {
            for source in CandidateSource::THROUGH_CANDIDATE_PRESERVED {
                for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
                    for candidate_outcome in CandidateOutcome::ALL {
                        let fixture =
                            RouteFixture::at_epoch(historical, source, usr_outcome, candidate_outcome);
                        let journal = fixture.open_journal();
                        let reservation = ActiveStateReservation::acquire().unwrap();
                        reset_new_state_candidate_preserve_move_attempt_count();
                        let authority = fixture.capture_ready(&journal, &reservation);
                        let database_before = fixture.database_snapshot();
                        let namespace_before = fixture.namespace_snapshot();
                        let successor = fixture.expected_successor();
                        let hook = same_byte_different_inode_hook(
                            &fixture,
                            format!("bound-{boundary:?}-{historical}-{source:?}-{usr_outcome:?}-{candidate_outcome:?}"),
                        );
                        arm_public_binding_revalidation_callback(boundary, hook);

                        let error = persist_usr_rollback_fresh_db_invalidation_route_and_reopen(journal, authority)
                            .unwrap_err();

                        assert_public_binding_revalidation_callback_consumed();
                        assert!(matches!(
                            error,
                            UsrRollbackFreshDbInvalidationRoutePersistenceError::Advance { durable, .. }
                                if durable == expected_durable
                        ));
                        match expected_durable {
                            DurableUsrRollbackFreshDbInvalidationRouteRecord::CandidatePreserved => {
                                assert_eq!(fixture.canonical_record(), fixture.source)
                            }
                            DurableUsrRollbackFreshDbInvalidationRouteRecord::FreshDbInvalidationIntent => {
                                assert_eq!(fixture.canonical_record(), successor)
                            }
                        }
                        assert_route_only(&fixture, &database_before, &namespace_before);
                    }
                }
            }
        }
    }
}

#[test]
fn startup_usr_rollback_fresh_db_invalidation_route_same_byte_successor_replacement_fails_same_store_binding() {
    for historical in [false, true] {
        for source in CandidateSource::THROUGH_CANDIDATE_PRESERVED {
            for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
                for candidate_outcome in CandidateOutcome::ALL {
                    let fixture = RouteFixture::at_epoch(historical, source, usr_outcome, candidate_outcome);
                    let journal = fixture.open_journal();
                    let reservation = ActiveStateReservation::acquire().unwrap();
                    reset_new_state_candidate_preserve_move_attempt_count();
                    let authority = fixture.capture_ready(&journal, &reservation);
                    let database_before = fixture.database_snapshot();
                    let namespace_before = fixture.namespace_snapshot();
                    let successor = fixture.expected_successor();
                    let hook = same_byte_different_inode_hook(
                        &fixture,
                        format!("published-{historical}-{source:?}-{usr_outcome:?}-{candidate_outcome:?}"),
                    );
                    arm_before_usr_rollback_fresh_db_invalidation_route_successor_binding_revalidation(hook);

                    let error =
                        persist_usr_rollback_fresh_db_invalidation_route_and_reopen(journal, authority).unwrap_err();

                    assert!(matches!(
                        error,
                        UsrRollbackFreshDbInvalidationRoutePersistenceError::SuccessorRecordBinding {
                            durable: DurableUsrRollbackFreshDbInvalidationRouteRecord::FreshDbInvalidationIntent,
                            source: UsrRollbackFreshDbInvalidationRouteSuccessorBindingError::Changed,
                        }
                    ));
                    assert_eq!(fixture.canonical_record(), successor);
                    assert_route_only(&fixture, &database_before, &namespace_before);
                }
            }
        }
    }
}

#[test]
fn startup_usr_rollback_fresh_db_invalidation_route_same_byte_successor_replacement_fails_reopened_binding() {
    for historical in [false, true] {
        for source in CandidateSource::THROUGH_CANDIDATE_PRESERVED {
            for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
                for candidate_outcome in CandidateOutcome::ALL {
                    let fixture = RouteFixture::at_epoch(historical, source, usr_outcome, candidate_outcome);
                    let journal = fixture.open_journal();
                    let reservation = ActiveStateReservation::acquire().unwrap();
                    reset_new_state_candidate_preserve_move_attempt_count();
                    let authority = fixture.capture_ready(&journal, &reservation);
                    let database_before = fixture.database_snapshot();
                    let namespace_before = fixture.namespace_snapshot();
                    let successor = fixture.expected_successor();
                    let hook = same_byte_different_inode_hook(
                        &fixture,
                        format!("reopened-{historical}-{source:?}-{usr_outcome:?}-{candidate_outcome:?}"),
                    );
                    arm_after_usr_rollback_fresh_db_invalidation_route_successor_binding_check_before_reopen(hook);

                    let error =
                        persist_usr_rollback_fresh_db_invalidation_route_and_reopen(journal, authority).unwrap_err();

                    assert!(matches!(
                        error,
                        UsrRollbackFreshDbInvalidationRoutePersistenceError::SuccessorRecordBinding {
                            durable: DurableUsrRollbackFreshDbInvalidationRouteRecord::FreshDbInvalidationIntent,
                            source: UsrRollbackFreshDbInvalidationRouteSuccessorBindingError::Changed,
                        }
                    ));
                    assert_eq!(fixture.canonical_record(), successor);
                    assert_route_only(&fixture, &database_before, &namespace_before);
                }
            }
        }
    }
}
