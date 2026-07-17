use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_reconciliation::{
            UsrRollbackFreshDbInvalidationApplyReconciliation, UsrRollbackFreshDbInvalidationEffectAuthority,
            fresh_db_invalidation_removal_call_count,
        },
        startup_recovery::UsrRollbackFreshDbInvalidationEffectSeal,
    },
    transition_journal::{Phase, RollbackActionOutcome, TransitionJournalStore, TransitionRecord},
};

use super::super::invalidation_test_support::{
    CandidateOutcome, CandidateSource, FreshDbInvalidationFixture, FreshRowLayout,
};

pub(super) use super::super::{
    invalidation_test_support::{
        CandidateOutcome as CandidateResult, CandidateSource as Source, FreshDbInvalidationFixture as Fixture,
        canonical_journal, capture_record, transition_quarantine_path,
    },
    test_fixture::{DatabaseSnapshot, NamespaceEntry},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum FreshDbInvalidationOrigin {
    Applied,
    AlreadySatisfied,
}

impl FreshDbInvalidationOrigin {
    pub(super) const ALL: [Self; 2] = [Self::Applied, Self::AlreadySatisfied];

    pub(super) fn outcome(self) -> RollbackActionOutcome {
        match self {
            Self::Applied => RollbackActionOutcome::Applied,
            Self::AlreadySatisfied => RollbackActionOutcome::AlreadySatisfied,
        }
    }
}

pub(super) fn fixture_for_origin(
    origin: FreshDbInvalidationOrigin,
    historical: bool,
    source: CandidateSource,
    usr_outcome: RollbackActionOutcome,
    candidate_outcome: CandidateOutcome,
) -> FreshDbInvalidationFixture {
    let row = match origin {
        FreshDbInvalidationOrigin::Applied => FreshRowLayout::Present,
        FreshDbInvalidationOrigin::AlreadySatisfied => FreshRowLayout::JointlyAbsent,
    };
    if historical {
        FreshDbInvalidationFixture::historical(source, usr_outcome, candidate_outcome, row)
    } else {
        FreshDbInvalidationFixture::new(source, usr_outcome, candidate_outcome, row)
    }
}

pub(super) fn effect_authority<'reservation>(
    fixture: &FreshDbInvalidationFixture,
    journal: &TransitionJournalStore,
    reservation: &'reservation ActiveStateReservation,
    origin: FreshDbInvalidationOrigin,
) -> UsrRollbackFreshDbInvalidationEffectAuthority<'reservation> {
    let seal = UsrRollbackFreshDbInvalidationEffectSeal::new_for_test();
    let authority = match origin {
        FreshDbInvalidationOrigin::Applied => {
            let authority = fixture.capture_apply(journal, reservation);
            let UsrRollbackFreshDbInvalidationApplyReconciliation::Applied(authority) =
                authority.reconcile(&seal, journal).unwrap()
            else {
                panic!("exact fresh row did not reconcile as Applied");
            };
            authority
        }
        FreshDbInvalidationOrigin::AlreadySatisfied => fixture
            .capture_finish(journal, reservation)
            .reconcile(&seal, journal)
            .unwrap(),
    };
    assert_eq!(
        fresh_db_invalidation_removal_call_count(),
        if origin == FreshDbInvalidationOrigin::Applied {
            1
        } else {
            0
        }
    );
    fixture.assert_exact_joint_absence();
    authority
}

pub(super) fn expected_fresh_db_invalidated(
    fixture: &FreshDbInvalidationFixture,
    origin: FreshDbInvalidationOrigin,
) -> TransitionRecord {
    let record = fixture
        .record
        .rollback_successor(Some(origin.outcome()))
        .expect("fresh invalidation fixture must admit its authority-owned successor");
    assert_eq!(record.phase, Phase::FreshDbInvalidated);
    record
}

pub(super) fn database_snapshot(fixture: &FreshDbInvalidationFixture) -> DatabaseSnapshot {
    fixture.fixture.fixture.database_snapshot()
}

pub(super) fn non_journal_namespace_snapshot(fixture: &FreshDbInvalidationFixture) -> Vec<NamespaceEntry> {
    fixture.namespace_snapshot()
}
