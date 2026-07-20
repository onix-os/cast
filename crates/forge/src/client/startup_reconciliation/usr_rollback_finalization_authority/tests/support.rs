use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_gate::{UsrRollbackCompleteRouteSeal, UsrRollbackFinalizationSeal},
        startup_reconciliation::{
            UsrRollbackCompleteRouteAdmission, UsrRollbackCompleteRouteAuthority, UsrRollbackFinalizationAdmission,
            UsrRollbackFinalizationAuthority, UsrRollbackFinalizationAuthorityError,
            UsrRollbackFreshDbInvalidationApplyReconciliation, fresh_db_invalidation_removal_call_count,
        },
        startup_recovery::{
            UsrRollbackFreshDbInvalidationEffectSeal, persist_usr_rollback_complete_route_and_reopen,
            persist_usr_rollback_fresh_db_invalidation_and_reopen,
        },
    },
    transition_journal::{Phase, RollbackActionOutcome, TransitionJournalStore, TransitionRecord},
};

use super::super::invalidation_test_support::{
    CandidateOutcome, CandidateSource, FreshDbInvalidationFixture, FreshRowLayout,
};

pub(super) use super::super::{
    invalidation_test_support::{canonical_journal, create_private_directory, transition_quarantine_path},
    test_fixture::NamespaceEntry,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum FreshDbOutcome {
    Applied,
    AlreadySatisfied,
}

impl FreshDbOutcome {
    pub(super) const ALL: [Self; 2] = [Self::Applied, Self::AlreadySatisfied];

    fn row(self) -> FreshRowLayout {
        match self {
            Self::Applied => FreshRowLayout::Present,
            Self::AlreadySatisfied => FreshRowLayout::JointlyAbsent,
        }
    }

    pub(super) fn expected_removals(self) -> usize {
        usize::from(self == Self::Applied)
    }
}

pub(super) struct FinalizationFixture {
    pub(super) fixture: FreshDbInvalidationFixture,
    pub(super) record: TransitionRecord,
    origin: FreshDbOutcome,
}

impl FinalizationFixture {
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
        let fixture = if historical {
            FreshDbInvalidationFixture::historical(source, usr_outcome, candidate_outcome, origin.row())
        } else {
            FreshDbInvalidationFixture::new(source, usr_outcome, candidate_outcome, origin.row())
        };
        let journal = fixture.open_journal();
        let reservation = ActiveStateReservation::acquire().unwrap();
        let effect_seal = UsrRollbackFreshDbInvalidationEffectSeal::new_for_test();
        let invalidation_authority = match origin {
            FreshDbOutcome::Applied => {
                let authority = fixture.capture_apply(&journal, &reservation);
                let UsrRollbackFreshDbInvalidationApplyReconciliation::Applied(authority) =
                    authority.reconcile(&effect_seal, &journal).unwrap()
                else {
                    panic!("exact fresh row did not reconcile as Applied");
                };
                authority
            }
            FreshDbOutcome::AlreadySatisfied => fixture
                .capture_finish(&journal, &reservation)
                .reconcile(&effect_seal, &journal)
                .unwrap(),
        };
        let (journal, invalidated) =
            persist_usr_rollback_fresh_db_invalidation_and_reopen(journal, invalidation_authority).unwrap();
        assert_eq!(invalidated.phase, Phase::FreshDbInvalidated);

        let route_seal = UsrRollbackCompleteRouteSeal::new_for_test();
        let route = UsrRollbackCompleteRouteAuthority::capture(
            &route_seal,
            &fixture.fixture.fixture.installation,
            &journal,
            &fixture.fixture.fixture.database,
            &reservation,
            &invalidated,
        )
        .unwrap();
        let UsrRollbackCompleteRouteAdmission::Ready(route) = route else {
            panic!("exact FreshDbInvalidated evidence did not admit completion routing");
        };
        let (journal, record) = persist_usr_rollback_complete_route_and_reopen(journal, route).unwrap();
        drop(journal);
        drop(reservation);
        assert_eq!(record.phase, Phase::RollbackComplete);
        fixture.assert_exact_joint_absence();
        assert_eq!(fresh_db_invalidation_removal_call_count(), origin.expected_removals());
        Self {
            fixture,
            record,
            origin,
        }
    }

    /// Build a deliberately inexact terminal record while retaining the exact
    /// fresh row and provenance in the source database.
    pub(super) fn with_present_fresh_row(
        source: CandidateSource,
        usr_outcome: RollbackActionOutcome,
        candidate_outcome: CandidateOutcome,
    ) -> Self {
        let fixture = FreshDbInvalidationFixture::new(source, usr_outcome, candidate_outcome, FreshRowLayout::Present);
        let invalidated = fixture
            .record
            .rollback_successor(Some(RollbackActionOutcome::Applied))
            .unwrap();
        let record = invalidated.rollback_successor(None).unwrap();
        let journal = fixture.open_journal();
        journal.advance(&fixture.record, &invalidated).unwrap();
        journal.advance(&invalidated, &record).unwrap();
        drop(journal);
        fixture.assert_exact_present();
        assert_eq!(fixture.canonical_record(), record);
        Self {
            fixture,
            record,
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

    pub(super) fn namespace_snapshot(&self) -> Vec<NamespaceEntry> {
        self.fixture.namespace_snapshot()
    }

    pub(super) fn capture<'reservation>(
        &self,
        journal: &TransitionJournalStore,
        reservation: &'reservation ActiveStateReservation,
    ) -> Result<UsrRollbackFinalizationAdmission<'reservation>, UsrRollbackFinalizationAuthorityError> {
        capture_record(&self.fixture, journal, reservation, &self.record)
    }

    pub(super) fn capture_ready<'reservation>(
        &self,
        journal: &TransitionJournalStore,
        reservation: &'reservation ActiveStateReservation,
    ) -> UsrRollbackFinalizationAuthority<'reservation> {
        match self.capture(journal, reservation).unwrap() {
            UsrRollbackFinalizationAdmission::Ready(authority) => authority,
            UsrRollbackFinalizationAdmission::NotApplicable => {
                panic!("exact RollbackComplete evidence was unexpectedly not applicable")
            }
            UsrRollbackFinalizationAdmission::Deferred => {
                panic!("exact RollbackComplete evidence unexpectedly deferred finalization")
            }
        }
    }

    pub(super) fn assert_terminal_unchanged(&self, canonical: &[u8], namespace: &[NamespaceEntry]) {
        assert_eq!(self.canonical_bytes(), canonical);
        assert_eq!(self.namespace_snapshot(), namespace);
        assert_eq!(self.canonical_record(), self.record);
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
) -> Result<UsrRollbackFinalizationAdmission<'reservation>, UsrRollbackFinalizationAuthorityError> {
    let seal = UsrRollbackFinalizationSeal::new_for_test();
    UsrRollbackFinalizationAuthority::capture(
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
