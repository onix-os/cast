//! Same-store lock handoff from terminal deletion into clean startup.

use crate::transition_journal::{RollbackActionOutcome, StorageError, TransitionJournalStore};

use super::{
    super::candidate_test_support::CandidateSource,
    support::{
        CandidateOutcome, Epoch, RouteFixture, assert_canonical_absent, candidate_move_count, enter_clean_route,
        persist_rollback_complete, reset_candidate_observers,
    },
};

#[test]
fn startup_activate_archived_finalization_hands_the_same_lock_into_clean_startup_until_proof_drop() {
    let fixture = RouteFixture::new(
        Epoch::Historical,
        CandidateSource::Intent,
        RollbackActionOutcome::AlreadySatisfied,
        CandidateOutcome::AlreadySatisfied,
    );
    let _terminal = persist_rollback_complete(&fixture);
    let database_before = fixture.database_snapshot();
    let namespace_before = fixture.namespace_snapshot();
    reset_candidate_observers();

    let clean = enter_clean_route(&fixture);

    assert_canonical_absent(&fixture.fixture.fixture.installation.root);
    let installation = &fixture.fixture.fixture.installation;
    let cast = installation.retained_mutable_cast_directory().unwrap();
    let locked = TransitionJournalStore::try_open_in_retained_cast(cast, &installation.root).unwrap_err();
    assert!(matches!(locked, StorageError::AcquireLock { .. }), "{locked:?}");
    assert_eq!(fixture.database_snapshot(), database_before);
    assert_eq!(fixture.namespace_snapshot(), namespace_before);
    assert_eq!(candidate_move_count(), 0);

    drop(clean);
    let reopened = TransitionJournalStore::try_open_in_retained_cast(cast, &installation.root).unwrap();
    assert_eq!(reopened.load().unwrap(), None);
    assert_eq!(candidate_move_count(), 0);
}
