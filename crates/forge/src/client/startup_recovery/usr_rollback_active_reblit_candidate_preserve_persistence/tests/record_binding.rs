use std::{
    fs,
    os::unix::fs::{MetadataExt as _, PermissionsExt as _},
    path::Path,
};

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_reconciliation::{
            active_reblit_candidate_preserve_exchange_attempt_count,
            reset_active_reblit_candidate_preserve_exchange_attempt_count,
        },
    },
    transition_journal::{
        PublicBindingRevalidationBoundary, RollbackActionOutcome,
        arm_public_binding_revalidation_callback, assert_public_binding_revalidation_callback_consumed,
    },
};

use super::super::{
    DurableUsrRollbackActiveReblitCandidatePreserveRecord,
    UsrRollbackActiveReblitCandidatePreservePersistenceError,
    UsrRollbackActiveReblitCandidatePreserveSuccessorBindingError,
    arm_after_usr_rollback_active_reblit_candidate_preserve_successor_binding_check_before_reopen,
    arm_before_usr_rollback_active_reblit_candidate_preserve_successor_binding_revalidation,
    persist_usr_rollback_active_reblit_candidate_preserve_and_reopen,
};
use super::super::candidate_test_support::CandidatePreserveFixture;
use super::support::{
    CandidateOrigin, Epoch, Source, durable_authority, expected_candidate_preserved, fixture_for_origin,
    non_journal_namespace_snapshot,
};

fn canonical_journal(fixture: &CandidatePreserveFixture) -> std::path::PathBuf {
    fixture
        .fixture
        .installation
        .root
        .join(".cast/journal/state-transition")
}

fn inode_identity(path: &Path) -> (u64, u64) {
    let metadata = fs::symlink_metadata(path).unwrap();
    (metadata.dev(), metadata.ino())
}

fn same_byte_different_inode_hook(
    fixture: &CandidatePreserveFixture,
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

fn assert_unchanged_outside_journal(
    fixture: &CandidatePreserveFixture,
    database_before: &super::super::test_fixture::DatabaseSnapshot,
    namespace_before: &[super::support::NonJournalNamespaceEntry],
    effect_count_before: usize,
) {
    assert_eq!(fixture.fixture.database_snapshot(), *database_before);
    assert_eq!(non_journal_namespace_snapshot(fixture), namespace_before);
    assert_eq!(
        active_reblit_candidate_preserve_exchange_attempt_count(),
        effect_count_before
    );
    let names = fs::read_dir(fixture.fixture.installation.root.join(".cast/journal"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    assert_eq!(names.len(), 2, "bound update left journal residue: {names:?}");
}

#[test]
fn startup_active_reblit_candidate_preserve_bound_advance_same_byte_replacements_never_succeed() {
    let mut exercised = 0;
    for (boundary, expected_durable) in [
        (
            PublicBindingRevalidationBoundary::BeforeBoundAdvancePublish,
            DurableUsrRollbackActiveReblitCandidatePreserveRecord::Source,
        ),
        (
            PublicBindingRevalidationBoundary::BeforeBoundAdvanceFinalBinding,
            DurableUsrRollbackActiveReblitCandidatePreserveRecord::CandidatePreserved,
        ),
    ] {
        for epoch in Epoch::ALL {
            for source in Source::ALL {
                for origin in CandidateOrigin::ALL {
                    for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
                        let fixture = fixture_for_origin(epoch, origin, source, usr_outcome);
                        let journal = fixture.open_journal();
                        let reservation = ActiveStateReservation::acquire().unwrap();
                        reset_active_reblit_candidate_preserve_exchange_attempt_count();
                        let authority = durable_authority(&fixture, &journal, &reservation, origin);
                        let effect_count_before = active_reblit_candidate_preserve_exchange_attempt_count();
                        assert_eq!(effect_count_before, usize::from(origin == CandidateOrigin::Applied));
                        let database_before = fixture.fixture.database_snapshot();
                        let namespace_before = non_journal_namespace_snapshot(&fixture);
                        let successor = expected_candidate_preserved(&fixture, origin);
                        let hook = same_byte_different_inode_hook(
                            &fixture,
                            format!("bound-{boundary:?}-{epoch:?}-{source:?}-{origin:?}-{usr_outcome:?}"),
                        );
                        arm_public_binding_revalidation_callback(boundary, hook);

                        let result =
                            persist_usr_rollback_active_reblit_candidate_preserve_and_reopen(journal, authority);
                        drop(reservation);
                        let error = result.unwrap_err();

                        assert_public_binding_revalidation_callback_consumed();
                        assert!(matches!(
                            error,
                            UsrRollbackActiveReblitCandidatePreservePersistenceError::Advance { durable, .. }
                                if durable == expected_durable
                        ));
                        match expected_durable {
                            DurableUsrRollbackActiveReblitCandidatePreserveRecord::Source => {
                                assert_eq!(fixture.fixture.canonical_record(), fixture.candidate_intent)
                            }
                            DurableUsrRollbackActiveReblitCandidatePreserveRecord::CandidatePreserved => {
                                assert_eq!(fixture.fixture.canonical_record(), successor)
                            }
                        }
                        assert_unchanged_outside_journal(
                            &fixture,
                            &database_before,
                            &namespace_before,
                            effect_count_before,
                        );
                        exercised += 1;
                    }
                }
            }
        }
    }
    assert_eq!(exercised, 48);
}

#[test]
fn startup_active_reblit_candidate_preserve_same_byte_successor_replacement_after_publication_fails_exact_binding() {
    let mut exercised = 0;
    for epoch in Epoch::ALL {
        for source in Source::ALL {
            for origin in CandidateOrigin::ALL {
                for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
                    let fixture = fixture_for_origin(epoch, origin, source, usr_outcome);
                    let journal = fixture.open_journal();
                    let reservation = ActiveStateReservation::acquire().unwrap();
                    reset_active_reblit_candidate_preserve_exchange_attempt_count();
                    let authority = durable_authority(&fixture, &journal, &reservation, origin);
                    let effect_count_before = active_reblit_candidate_preserve_exchange_attempt_count();
                    assert_eq!(effect_count_before, usize::from(origin == CandidateOrigin::Applied));
                    let database_before = fixture.fixture.database_snapshot();
                    let namespace_before = non_journal_namespace_snapshot(&fixture);
                    let successor = expected_candidate_preserved(&fixture, origin);
                    let hook = same_byte_different_inode_hook(
                        &fixture,
                        format!("published-{epoch:?}-{source:?}-{origin:?}-{usr_outcome:?}"),
                    );
                    arm_before_usr_rollback_active_reblit_candidate_preserve_successor_binding_revalidation(hook);

                    let result = persist_usr_rollback_active_reblit_candidate_preserve_and_reopen(journal, authority);
                    drop(reservation);
                    let error = result.unwrap_err();

                    assert!(matches!(
                        error,
                        UsrRollbackActiveReblitCandidatePreservePersistenceError::SuccessorRecordBinding {
                            durable: DurableUsrRollbackActiveReblitCandidatePreserveRecord::CandidatePreserved,
                            source: UsrRollbackActiveReblitCandidatePreserveSuccessorBindingError::Changed,
                        }
                    ));
                    assert_eq!(fixture.fixture.canonical_record(), successor);
                    assert_unchanged_outside_journal(
                        &fixture,
                        &database_before,
                        &namespace_before,
                        effect_count_before,
                    );
                    exercised += 1;
                }
            }
        }
    }
    assert_eq!(exercised, 24);
}

#[test]
fn startup_active_reblit_candidate_preserve_same_byte_successor_replacement_after_same_store_binding_fails_reopened_binding()
{
    let mut exercised = 0;
    for epoch in Epoch::ALL {
        for source in Source::ALL {
            for origin in CandidateOrigin::ALL {
                for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
                    let fixture = fixture_for_origin(epoch, origin, source, usr_outcome);
                    let journal = fixture.open_journal();
                    let reservation = ActiveStateReservation::acquire().unwrap();
                    reset_active_reblit_candidate_preserve_exchange_attempt_count();
                    let authority = durable_authority(&fixture, &journal, &reservation, origin);
                    let effect_count_before = active_reblit_candidate_preserve_exchange_attempt_count();
                    assert_eq!(effect_count_before, usize::from(origin == CandidateOrigin::Applied));
                    let database_before = fixture.fixture.database_snapshot();
                    let namespace_before = non_journal_namespace_snapshot(&fixture);
                    let successor = expected_candidate_preserved(&fixture, origin);
                    let hook = same_byte_different_inode_hook(
                        &fixture,
                        format!("reopened-{epoch:?}-{source:?}-{origin:?}-{usr_outcome:?}"),
                    );
                    arm_after_usr_rollback_active_reblit_candidate_preserve_successor_binding_check_before_reopen(hook);

                    let result = persist_usr_rollback_active_reblit_candidate_preserve_and_reopen(journal, authority);
                    drop(reservation);
                    let error = result.unwrap_err();

                    assert!(matches!(
                        error,
                        UsrRollbackActiveReblitCandidatePreservePersistenceError::SuccessorRecordBinding {
                            durable: DurableUsrRollbackActiveReblitCandidatePreserveRecord::CandidatePreserved,
                            source: UsrRollbackActiveReblitCandidatePreserveSuccessorBindingError::Changed,
                        }
                    ));
                    assert_eq!(fixture.fixture.canonical_record(), successor);
                    assert_unchanged_outside_journal(
                        &fixture,
                        &database_before,
                        &namespace_before,
                        effect_count_before,
                    );
                    exercised += 1;
                }
            }
        }
    }
    assert_eq!(exercised, 24);
}
