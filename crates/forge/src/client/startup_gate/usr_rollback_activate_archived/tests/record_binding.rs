//! Exact predecessor and successor journal-record binding races.

use std::{
    fs,
    os::unix::fs::{MetadataExt as _, PermissionsExt as _},
    path::Path,
};

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_recovery::{
            DurableUsrRollbackActivateArchivedCompleteRouteRecord,
            UsrRollbackActivateArchivedCompleteRoutePersistenceError,
            UsrRollbackActivateArchivedCompleteRouteSuccessorBindingError,
            arm_after_usr_rollback_activate_archived_complete_route_successor_binding_check_before_reopen,
            arm_before_usr_rollback_activate_archived_complete_route_successor_binding_revalidation,
            persist_usr_rollback_activate_archived_complete_route_and_reopen,
        },
    },
    transition_journal::{
        PublicBindingRevalidationBoundary, RollbackActionOutcome, arm_public_binding_revalidation_callback,
        assert_public_binding_revalidation_callback_consumed,
    },
};

use super::support::{
    CandidateOutcome, CandidateSource, Epoch, RouteFixture, candidate_move_count, reset_candidate_observers,
};

fn canonical_journal(fixture: &RouteFixture) -> std::path::PathBuf {
    fixture
        .fixture
        .fixture
        .installation
        .root
        .join(".cast/journal/state-transition")
}

fn inode_identity(path: &Path) -> (u64, u64) {
    let metadata = fs::symlink_metadata(path).unwrap();
    (metadata.dev(), metadata.ino())
}

fn same_byte_different_inode_hook(fixture: &RouteFixture, label: String) -> impl FnOnce() + 'static {
    let canonical = canonical_journal(fixture);
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
    fixture.assert_exact_database_pair();
    fixture.assert_exact_archived_topology();
    assert_eq!(candidate_move_count(), 0);
    let names = fs::read_dir(
        fixture
            .fixture
            .fixture
            .installation
            .root
            .join(".cast/journal"),
    )
    .unwrap()
    .map(|entry| entry.unwrap().file_name())
    .collect::<Vec<_>>();
    assert_eq!(names.len(), 2, "bound route left journal residue: {names:?}");
}

#[test]
fn startup_activate_archived_complete_route_bound_advance_same_byte_replacements_never_succeed() {
    let mut cases = 0;
    for (boundary, expected_durable) in [
        (
            PublicBindingRevalidationBoundary::BeforeBoundAdvancePublish,
            DurableUsrRollbackActivateArchivedCompleteRouteRecord::CandidatePreserved,
        ),
        (
            PublicBindingRevalidationBoundary::BeforeBoundAdvanceFinalBinding,
            DurableUsrRollbackActivateArchivedCompleteRouteRecord::RollbackComplete,
        ),
    ] {
        for epoch in Epoch::ALL {
            for source in CandidateSource::THROUGH_CANDIDATE_PRESERVED {
                for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
                    for candidate_outcome in CandidateOutcome::ALL {
                        let fixture = RouteFixture::new(epoch, source, usr_outcome, candidate_outcome);
                        let journal = fixture.open_journal();
                        let reservation = ActiveStateReservation::acquire().unwrap();
                        reset_candidate_observers();
                        let authority = fixture.capture_ready(&journal, &reservation);
                        let database_before = fixture.database_snapshot();
                        let namespace_before = fixture.namespace_snapshot();
                        let successor = fixture.expected_successor();
                        let hook = same_byte_different_inode_hook(
                            &fixture,
                            format!("bound-{boundary:?}-{epoch:?}-{source:?}-{usr_outcome:?}-{candidate_outcome:?}"),
                        );
                        arm_public_binding_revalidation_callback(boundary, hook);

                        let error = persist_usr_rollback_activate_archived_complete_route_and_reopen(
                            journal, authority,
                        )
                        .unwrap_err();

                        assert_public_binding_revalidation_callback_consumed();
                        assert!(matches!(
                            error,
                            UsrRollbackActivateArchivedCompleteRoutePersistenceError::Advance { durable, .. }
                                if durable == expected_durable
                        ));
                        match expected_durable {
                            DurableUsrRollbackActivateArchivedCompleteRouteRecord::CandidatePreserved => {
                                assert_eq!(fixture.canonical_record(), fixture.source)
                            }
                            DurableUsrRollbackActivateArchivedCompleteRouteRecord::RollbackComplete => {
                                assert_eq!(fixture.canonical_record(), successor)
                            }
                        }
                        assert_route_only(&fixture, &database_before, &namespace_before);
                        cases += 1;
                    }
                }
            }
        }
    }
    assert_eq!(cases, 48);
}

#[test]
fn startup_activate_archived_complete_route_same_byte_successor_replacement_fails_same_store_binding() {
    let mut cases = 0;
    for epoch in Epoch::ALL {
        for source in CandidateSource::THROUGH_CANDIDATE_PRESERVED {
            for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
                for candidate_outcome in CandidateOutcome::ALL {
                    let fixture = RouteFixture::new(epoch, source, usr_outcome, candidate_outcome);
                    let journal = fixture.open_journal();
                    let reservation = ActiveStateReservation::acquire().unwrap();
                    reset_candidate_observers();
                    let authority = fixture.capture_ready(&journal, &reservation);
                    let database_before = fixture.database_snapshot();
                    let namespace_before = fixture.namespace_snapshot();
                    let successor = fixture.expected_successor();
                    let hook = same_byte_different_inode_hook(
                        &fixture,
                        format!("published-{epoch:?}-{source:?}-{usr_outcome:?}-{candidate_outcome:?}"),
                    );
                    arm_before_usr_rollback_activate_archived_complete_route_successor_binding_revalidation(hook);

                    let error =
                        persist_usr_rollback_activate_archived_complete_route_and_reopen(journal, authority)
                            .unwrap_err();

                    assert!(matches!(
                        error,
                        UsrRollbackActivateArchivedCompleteRoutePersistenceError::SuccessorRecordBinding {
                            durable: DurableUsrRollbackActivateArchivedCompleteRouteRecord::RollbackComplete,
                            source: UsrRollbackActivateArchivedCompleteRouteSuccessorBindingError::Changed,
                        }
                    ));
                    assert_eq!(fixture.canonical_record(), successor);
                    assert_route_only(&fixture, &database_before, &namespace_before);
                    cases += 1;
                }
            }
        }
    }
    assert_eq!(cases, 24);
}

#[test]
fn startup_activate_archived_complete_route_same_byte_successor_replacement_fails_reopened_binding() {
    let mut cases = 0;
    for epoch in Epoch::ALL {
        for source in CandidateSource::THROUGH_CANDIDATE_PRESERVED {
            for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
                for candidate_outcome in CandidateOutcome::ALL {
                    let fixture = RouteFixture::new(epoch, source, usr_outcome, candidate_outcome);
                    let journal = fixture.open_journal();
                    let reservation = ActiveStateReservation::acquire().unwrap();
                    reset_candidate_observers();
                    let authority = fixture.capture_ready(&journal, &reservation);
                    let database_before = fixture.database_snapshot();
                    let namespace_before = fixture.namespace_snapshot();
                    let successor = fixture.expected_successor();
                    let hook = same_byte_different_inode_hook(
                        &fixture,
                        format!("reopened-{epoch:?}-{source:?}-{usr_outcome:?}-{candidate_outcome:?}"),
                    );
                    arm_after_usr_rollback_activate_archived_complete_route_successor_binding_check_before_reopen(
                        hook,
                    );

                    let error =
                        persist_usr_rollback_activate_archived_complete_route_and_reopen(journal, authority)
                            .unwrap_err();

                    assert!(matches!(
                        error,
                        UsrRollbackActivateArchivedCompleteRoutePersistenceError::SuccessorRecordBinding {
                            durable: DurableUsrRollbackActivateArchivedCompleteRouteRecord::RollbackComplete,
                            source: UsrRollbackActivateArchivedCompleteRouteSuccessorBindingError::Changed,
                        }
                    ));
                    assert_eq!(fixture.canonical_record(), successor);
                    assert_route_only(&fixture, &database_before, &namespace_before);
                    cases += 1;
                }
            }
        }
    }
    assert_eq!(cases, 24);
}
