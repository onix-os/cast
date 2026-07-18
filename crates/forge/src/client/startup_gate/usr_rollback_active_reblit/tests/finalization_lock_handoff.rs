//! Same-store lock handoff from terminal deletion into clean startup.

use crate::transition_journal::{RollbackActionOutcome, StorageError, TransitionJournalStore};

use super::{
    super::candidate_test_support::CandidateSource,
    support::{
        CandidateOrigin, Epoch, assert_canonical_absent, assert_no_candidate_effects, build_active,
        enter_clean_candidate, persist_rollback_complete, reset_candidate_effect_observers,
    },
};

#[test]
fn startup_active_reblit_finalization_hands_the_same_lock_into_clean_startup_until_proof_drop() {
    let fixture = build_active(
        Epoch::Historical,
        CandidateSource::Intent,
        RollbackActionOutcome::AlreadySatisfied,
        CandidateOrigin::AlreadySatisfied,
    );
    let _terminal = persist_rollback_complete(&fixture, CandidateOrigin::AlreadySatisfied);
    let database_before = fixture.fixture.database_snapshot();
    let namespace_before = fixture.fixture.namespace_snapshot();
    reset_candidate_effect_observers();

    let clean = enter_clean_candidate(&fixture);

    assert_canonical_absent(&fixture.fixture.installation.root);
    let cast = fixture.fixture.installation.retained_mutable_cast_directory().unwrap();
    let locked =
        TransitionJournalStore::try_open_in_retained_cast(cast, &fixture.fixture.installation.root).unwrap_err();
    assert!(matches!(locked, StorageError::AcquireLock { .. }), "{locked:?}");
    assert_eq!(fixture.fixture.database_snapshot(), database_before);
    assert_eq!(fixture.fixture.namespace_snapshot(), namespace_before);
    assert_no_candidate_effects();

    drop(clean);
    let reopened = TransitionJournalStore::try_open_in_retained_cast(cast, &fixture.fixture.installation.root).unwrap();
    assert_eq!(reopened.load().unwrap(), None);
    assert_no_candidate_effects();
}
