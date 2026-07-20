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
            DurableUsrRollbackActiveReblitCompleteRouteRecord,
            UsrRollbackActiveReblitCompleteRoutePersistenceError,
            UsrRollbackActiveReblitCompleteRouteSuccessorBindingError,
            arm_after_usr_rollback_active_reblit_complete_route_successor_binding_check_before_reopen,
            arm_before_usr_rollback_active_reblit_complete_route_successor_binding_revalidation,
            persist_usr_rollback_active_reblit_complete_route_and_reopen,
        },
    },
    transition_journal::{
        PublicBindingRevalidationBoundary, RollbackActionOutcome, arm_public_binding_revalidation_callback,
        assert_public_binding_revalidation_callback_consumed,
    },
};

use super::{
    super::candidate_test_support::CandidateSource,
    support::{
        CandidateOrigin, Epoch, active_wrapper_path, assert_complete_route_journal_only,
        assert_exact_no_boot_completion_plan, build_active, capture_complete_route_ready,
        expected_rollback_complete, persist_candidate_preserved, reset_complete_route_effect_observers,
    },
};

const USR_OUTCOMES: [RollbackActionOutcome; 2] =
    [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied];

fn canonical_journal(fixture: &super::super::candidate_test_support::CandidatePreserveFixture) -> std::path::PathBuf {
    fixture.fixture.installation.root.join(".cast/journal/state-transition")
}

fn inode_identity(path: &Path) -> (u64, u64) {
    let metadata = fs::symlink_metadata(path).unwrap();
    (metadata.dev(), metadata.ino())
}

fn same_byte_different_inode_hook(
    fixture: &super::super::candidate_test_support::CandidatePreserveFixture,
    label: String,
) -> impl FnOnce() + 'static {
    let canonical = canonical_journal(fixture);
    let displaced = fixture
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
    fixture: &super::super::candidate_test_support::CandidatePreserveFixture,
    database_before: &super::super::test_fixture::DatabaseSnapshot,
    namespace_before: &[super::super::test_fixture::NamespaceEntry],
) {
    assert_eq!(fixture.fixture.database_snapshot(), *database_before);
    assert_eq!(fixture.fixture.namespace_snapshot(), namespace_before);
    assert!(active_wrapper_path(fixture).join("usr").is_dir());
    assert_complete_route_journal_only();
    let names = fs::read_dir(fixture.fixture.installation.root.join(".cast/journal"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    assert_eq!(names.len(), 2, "bound route left journal residue: {names:?}");
}

#[test]
fn startup_active_reblit_complete_route_bound_advance_same_byte_replacements_never_succeed() {
    let mut cases = 0;
    for (boundary, expected_durable) in [
        (
            PublicBindingRevalidationBoundary::BeforeBoundAdvancePublish,
            DurableUsrRollbackActiveReblitCompleteRouteRecord::CandidatePreserved,
        ),
        (
            PublicBindingRevalidationBoundary::BeforeBoundAdvanceFinalBinding,
            DurableUsrRollbackActiveReblitCompleteRouteRecord::RollbackComplete,
        ),
    ] {
        for epoch in Epoch::ALL {
            for candidate_source in CandidateSource::THROUGH_CANDIDATE_PRESERVED {
                for usr_outcome in USR_OUTCOMES {
                    for candidate_outcome in CandidateOrigin::ALL {
                        let fixture = build_active(
                            epoch,
                            candidate_source,
                            usr_outcome,
                            CandidateOrigin::AlreadySatisfied,
                        );
                        let source = persist_candidate_preserved(&fixture, candidate_outcome);
                        let successor = expected_rollback_complete(&source);
                        let journal = fixture.open_journal();
                        let reservation = ActiveStateReservation::acquire().unwrap();
                        assert_exact_no_boot_completion_plan(&source, candidate_source);
                        reset_complete_route_effect_observers();
                        let authority = capture_complete_route_ready(&fixture, &journal, &reservation, &source);
                        let database_before = fixture.fixture.database_snapshot();
                        let namespace_before = fixture.fixture.namespace_snapshot();
                        let hook = same_byte_different_inode_hook(
                            &fixture,
                            format!(
                                "active-bound-{boundary:?}-{epoch:?}-{candidate_source:?}-{usr_outcome:?}-{candidate_outcome:?}"
                            ),
                        );
                        arm_public_binding_revalidation_callback(boundary, hook);

                        let error = persist_usr_rollback_active_reblit_complete_route_and_reopen(
                            journal, authority,
                        )
                        .unwrap_err();

                        assert_public_binding_revalidation_callback_consumed();
                        assert!(matches!(
                            error,
                            UsrRollbackActiveReblitCompleteRoutePersistenceError::Advance { durable, .. }
                                if durable == expected_durable
                        ));
                        match expected_durable {
                            DurableUsrRollbackActiveReblitCompleteRouteRecord::CandidatePreserved => {
                                assert_eq!(fixture.fixture.canonical_record(), source)
                            }
                            DurableUsrRollbackActiveReblitCompleteRouteRecord::RollbackComplete => {
                                assert_eq!(fixture.fixture.canonical_record(), successor)
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
fn startup_active_reblit_complete_route_same_byte_successor_replacement_fails_same_store_binding() {
    let mut cases = 0;
    for epoch in Epoch::ALL {
        for candidate_source in CandidateSource::THROUGH_CANDIDATE_PRESERVED {
            for usr_outcome in USR_OUTCOMES {
                for candidate_outcome in CandidateOrigin::ALL {
                    let fixture = build_active(
                        epoch,
                        candidate_source,
                        usr_outcome,
                        CandidateOrigin::AlreadySatisfied,
                    );
                    let source = persist_candidate_preserved(&fixture, candidate_outcome);
                    let successor = expected_rollback_complete(&source);
                    let journal = fixture.open_journal();
                    let reservation = ActiveStateReservation::acquire().unwrap();
                    assert_exact_no_boot_completion_plan(&source, candidate_source);
                    reset_complete_route_effect_observers();
                    let authority = capture_complete_route_ready(&fixture, &journal, &reservation, &source);
                    let database_before = fixture.fixture.database_snapshot();
                    let namespace_before = fixture.fixture.namespace_snapshot();
                    let hook = same_byte_different_inode_hook(
                        &fixture,
                        format!(
                            "active-published-{epoch:?}-{candidate_source:?}-{usr_outcome:?}-{candidate_outcome:?}"
                        ),
                    );
                    arm_before_usr_rollback_active_reblit_complete_route_successor_binding_revalidation(hook);

                    let error =
                        persist_usr_rollback_active_reblit_complete_route_and_reopen(journal, authority)
                            .unwrap_err();

                    assert!(matches!(
                        error,
                        UsrRollbackActiveReblitCompleteRoutePersistenceError::SuccessorRecordBinding {
                            durable: DurableUsrRollbackActiveReblitCompleteRouteRecord::RollbackComplete,
                            source: UsrRollbackActiveReblitCompleteRouteSuccessorBindingError::Changed,
                        }
                    ));
                    assert_eq!(fixture.fixture.canonical_record(), successor);
                    assert_route_only(&fixture, &database_before, &namespace_before);
                    cases += 1;
                }
            }
        }
    }
    assert_eq!(cases, 24);
}

#[test]
fn startup_active_reblit_complete_route_same_byte_successor_replacement_fails_reopened_binding() {
    let mut cases = 0;
    for epoch in Epoch::ALL {
        for candidate_source in CandidateSource::THROUGH_CANDIDATE_PRESERVED {
            for usr_outcome in USR_OUTCOMES {
                for candidate_outcome in CandidateOrigin::ALL {
                    let fixture = build_active(
                        epoch,
                        candidate_source,
                        usr_outcome,
                        CandidateOrigin::AlreadySatisfied,
                    );
                    let source = persist_candidate_preserved(&fixture, candidate_outcome);
                    let successor = expected_rollback_complete(&source);
                    let journal = fixture.open_journal();
                    let reservation = ActiveStateReservation::acquire().unwrap();
                    assert_exact_no_boot_completion_plan(&source, candidate_source);
                    reset_complete_route_effect_observers();
                    let authority = capture_complete_route_ready(&fixture, &journal, &reservation, &source);
                    let database_before = fixture.fixture.database_snapshot();
                    let namespace_before = fixture.fixture.namespace_snapshot();
                    let hook = same_byte_different_inode_hook(
                        &fixture,
                        format!(
                            "active-reopened-{epoch:?}-{candidate_source:?}-{usr_outcome:?}-{candidate_outcome:?}"
                        ),
                    );
                    arm_after_usr_rollback_active_reblit_complete_route_successor_binding_check_before_reopen(
                        hook,
                    );

                    let error =
                        persist_usr_rollback_active_reblit_complete_route_and_reopen(journal, authority)
                            .unwrap_err();

                    assert!(matches!(
                        error,
                        UsrRollbackActiveReblitCompleteRoutePersistenceError::SuccessorRecordBinding {
                            durable: DurableUsrRollbackActiveReblitCompleteRouteRecord::RollbackComplete,
                            source: UsrRollbackActiveReblitCompleteRouteSuccessorBindingError::Changed,
                        }
                    ));
                    assert_eq!(fixture.fixture.canonical_record(), successor);
                    assert_route_only(&fixture, &database_before, &namespace_before);
                    cases += 1;
                }
            }
        }
    }
    assert_eq!(cases, 24);
}
