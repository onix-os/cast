use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_gate::UsrRollbackCompleteRouteSeal,
        startup_reconciliation::{
            UsrRollbackCompleteRouteAdmission, UsrRollbackCompleteRouteAuthority,
            UsrRollbackCompleteRouteAuthorityError, UsrRollbackFreshDbInvalidationApplyReconciliation,
            fresh_db_invalidation_removal_call_count,
        },
        startup_recovery::{
            UsrRollbackFreshDbInvalidationEffectSeal, persist_usr_rollback_fresh_db_invalidation_and_reopen,
        },
    },
    transition_journal::{Phase, RollbackAction, RollbackActionOutcome, TransitionJournalStore, TransitionRecord},
};

use super::super::invalidation_test_support::{
    CandidateOutcome, CandidateSource, FreshDbInvalidationFixture, FreshRowLayout,
};

pub(super) use super::super::{
    invalidation_test_support::{
        canonical_journal, capture_record as capture_invalidation_record, create_private_directory,
        transition_quarantine_path,
    },
    test_fixture::{DatabaseSnapshot, NamespaceEntry},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum FreshDbOutcome {
    Applied,
    AlreadySatisfied,
}

impl FreshDbOutcome {
    pub(super) const ALL: [Self; 2] = [Self::Applied, Self::AlreadySatisfied];

    pub(super) fn outcome(self) -> RollbackActionOutcome {
        match self {
            Self::Applied => RollbackActionOutcome::Applied,
            Self::AlreadySatisfied => RollbackActionOutcome::AlreadySatisfied,
        }
    }

    pub(super) fn action(self) -> RollbackAction {
        match self {
            Self::Applied => RollbackAction::Applied,
            Self::AlreadySatisfied => RollbackAction::AlreadySatisfied,
        }
    }

    pub(super) fn expected_removals(self) -> usize {
        usize::from(self == Self::Applied)
    }
}

pub(super) struct RouteFixture {
    pub(super) fixture: FreshDbInvalidationFixture,
    pub(super) source: TransitionRecord,
    pub(super) origin: FreshDbOutcome,
}

impl RouteFixture {
    pub(super) fn new(
        origin: FreshDbOutcome,
        source: CandidateSource,
        usr_outcome: RollbackActionOutcome,
        candidate_outcome: CandidateOutcome,
    ) -> Self {
        Self::build(origin, false, source, usr_outcome, candidate_outcome)
    }

    pub(super) fn historical(
        origin: FreshDbOutcome,
        source: CandidateSource,
        usr_outcome: RollbackActionOutcome,
        candidate_outcome: CandidateOutcome,
    ) -> Self {
        Self::build(origin, true, source, usr_outcome, candidate_outcome)
    }

    fn build(
        origin: FreshDbOutcome,
        historical: bool,
        source: CandidateSource,
        usr_outcome: RollbackActionOutcome,
        candidate_outcome: CandidateOutcome,
    ) -> Self {
        let row = match origin {
            FreshDbOutcome::Applied => FreshRowLayout::Present,
            FreshDbOutcome::AlreadySatisfied => FreshRowLayout::JointlyAbsent,
        };
        let fixture = if historical {
            FreshDbInvalidationFixture::historical(source, usr_outcome, candidate_outcome, row)
        } else {
            FreshDbInvalidationFixture::new(source, usr_outcome, candidate_outcome, row)
        };
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        let seal = UsrRollbackFreshDbInvalidationEffectSeal::new_for_test();
        let authority = match origin {
            FreshDbOutcome::Applied => {
                let authority = fixture.capture_apply(&journal, &reservation);
                let UsrRollbackFreshDbInvalidationApplyReconciliation::Applied(authority) =
                    authority.reconcile(&seal, &journal).unwrap()
                else {
                    panic!("exact fresh row did not reconcile as Applied");
                };
                authority
            }
            FreshDbOutcome::AlreadySatisfied => fixture
                .capture_finish(&journal, &reservation)
                .reconcile(&seal, &journal)
                .unwrap(),
        };
        let expected = fixture.record.rollback_successor(Some(origin.outcome())).unwrap();
        let (reopened, actual) = persist_usr_rollback_fresh_db_invalidation_and_reopen(journal, authority).unwrap();
        drop(reopened);
        drop(reservation);
        assert_eq!(actual, expected);
        assert_eq!(actual.phase, Phase::FreshDbInvalidated);
        assert_eq!(fresh_db_invalidation_removal_call_count(), origin.expected_removals());
        fixture.assert_exact_joint_absence();
        Self {
            fixture,
            source: actual,
            origin,
        }
    }

    /// Build an intentionally inexact source: its journal claims invalidation,
    /// but the exact fresh row remains present.
    pub(super) fn with_present_fresh_row(
        source: CandidateSource,
        usr_outcome: RollbackActionOutcome,
        candidate_outcome: CandidateOutcome,
    ) -> Self {
        let fixture = FreshDbInvalidationFixture::new(source, usr_outcome, candidate_outcome, FreshRowLayout::Present);
        let source = fixture
            .record
            .rollback_successor(Some(RollbackActionOutcome::Applied))
            .unwrap();
        let journal = fixture.open_journal();
        journal.advance(&fixture.record, &source).unwrap();
        drop(journal);
        fixture.assert_exact_present();
        assert_eq!(fixture.canonical_record(), source);
        Self {
            fixture,
            source,
            origin: FreshDbOutcome::Applied,
        }
    }

    pub(super) fn open_journal(&self) -> TransitionJournalStore {
        self.fixture.open_journal()
    }

    pub(super) fn canonical_record(&self) -> TransitionRecord {
        self.fixture.canonical_record()
    }

    pub(super) fn canonical_bytes(&self) -> Vec<u8> {
        self.fixture.canonical_bytes()
    }

    pub(super) fn database_snapshot(&self) -> DatabaseSnapshot {
        self.fixture.fixture.fixture.database_snapshot()
    }

    pub(super) fn namespace_snapshot(&self) -> Vec<NamespaceEntry> {
        self.fixture.namespace_snapshot()
    }

    pub(super) fn expected_successor(&self) -> TransitionRecord {
        let successor = self.source.rollback_successor(None).unwrap();
        assert_eq!(successor.phase, Phase::RollbackComplete);
        successor
    }

    pub(super) fn capture<'reservation>(
        &self,
        journal: &TransitionJournalStore,
        reservation: &'reservation ActiveStateReservation,
    ) -> Result<UsrRollbackCompleteRouteAdmission<'reservation>, UsrRollbackCompleteRouteAuthorityError> {
        capture_record(&self.fixture, journal, reservation, &self.source)
    }

    pub(super) fn capture_ready<'reservation>(
        &self,
        journal: &TransitionJournalStore,
        reservation: &'reservation ActiveStateReservation,
    ) -> UsrRollbackCompleteRouteAuthority<'reservation> {
        match self.capture(journal, reservation).unwrap() {
            UsrRollbackCompleteRouteAdmission::Ready(authority) => authority,
            _ => panic!("exact FreshDbInvalidated joint-absence evidence did not admit completion"),
        }
    }

    pub(super) fn assert_no_second_removal(&self) {
        self.fixture.assert_exact_joint_absence();
        assert_eq!(
            fresh_db_invalidation_removal_call_count(),
            self.origin.expected_removals()
        );
    }
}

pub(super) fn capture_record<'reservation>(
    fixture: &FreshDbInvalidationFixture,
    journal: &TransitionJournalStore,
    reservation: &'reservation ActiveStateReservation,
    record: &TransitionRecord,
) -> Result<UsrRollbackCompleteRouteAdmission<'reservation>, UsrRollbackCompleteRouteAuthorityError> {
    let seal = UsrRollbackCompleteRouteSeal::new_for_test();
    UsrRollbackCompleteRouteAuthority::capture(
        &seal,
        &fixture.fixture.fixture.installation,
        journal,
        &fixture.fixture.fixture.database,
        reservation,
        record,
    )
}

pub(super) use super::super::invalidation_test_support::{
    CandidateOutcome as CandidateResult, CandidateSource as Source,
};
