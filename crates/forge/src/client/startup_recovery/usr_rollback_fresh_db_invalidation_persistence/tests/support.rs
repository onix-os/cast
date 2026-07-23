use std::path::Path;

use crate::{
    Installation, db,
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_gate::UsrRollbackFreshDbInvalidationSeal,
        startup_reconciliation::{
            UsrRollbackFreshDbInvalidationAdmission, UsrRollbackFreshDbInvalidationApplyReconciliation,
            UsrRollbackFreshDbInvalidationAuthority, UsrRollbackFreshDbInvalidationAuthorityError,
            UsrRollbackFreshDbInvalidationEffectAuthority, fresh_db_invalidation_removal_call_count,
        },
        startup_recovery::UsrRollbackFreshDbInvalidationEffectSeal,
    },
    installation::DatabaseKind,
    test_support::private_installation_tempdir,
    transition_journal::{Phase, RollbackActionOutcome, TransitionJournalStore, TransitionRecord},
};

use super::super::invalidation_test_support::{
    CandidateOutcome, CandidateSource, FreshDbInvalidationFixture, FreshRowLayout,
};

pub(super) use super::super::{
    invalidation_test_support::{
        CandidateOutcome as CandidateResult, CandidateSource as Source, FreshDbInvalidationFixture as Fixture,
        canonical_journal, transition_quarantine_path,
    },
    test_fixture::{DatabaseSnapshot, NamespaceEntry},
};

const OS_RELEASE: &[u8] = b"NAME=Rollback Decision Test\nID=rollback-decision-test\n";
const SYSTEM_MODEL: &[u8] = b"let system = { hostname = \"rollback-decision-test\" } in system\n";

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

pub(super) fn install_persistent_database(
    fixture: &mut FreshDbInvalidationFixture,
    origin: FreshDbInvalidationOrigin,
) {
    let database = open_state_database(&fixture.fixture.fixture.installation);
    let previous = database.add(&[], Some("rollback previous"), None).unwrap().id;
    let candidate = database
        .add_with_transition(
            &fixture.record.transition_id,
            &[],
            Some("rollback fresh candidate"),
            None,
        )
        .unwrap()
        .id;
    assert_eq!(previous, fixture.fixture.fixture.previous_state);
    assert_eq!(candidate, fixture.fixture.fixture.candidate_state);
    let provenance = db::state::MetadataProvenance::from_outputs(OS_RELEASE, SYSTEM_MODEL);
    database
        .insert_fresh_metadata_provenance_if_transition_matches(
            candidate,
            &fixture.record.transition_id,
            &provenance,
        )
        .unwrap();
    if origin == FreshDbInvalidationOrigin::AlreadySatisfied {
        let observation = database
            .inspect_exact_fresh_transition(candidate, &fixture.record.transition_id)
            .unwrap();
        let db::state::ExactFreshTransitionObservation::Present(preimage) = observation else {
            panic!("persistent fresh-invalidation fixture expected one exact preimage");
        };
        database.remove_exact_fresh_transition(preimage).unwrap();
    }
    let old = std::mem::replace(&mut fixture.fixture.fixture.database, database);
    drop(old);
    match origin {
        FreshDbInvalidationOrigin::Applied => fixture.assert_exact_present(),
        FreshDbInvalidationOrigin::AlreadySatisfied => fixture.assert_exact_joint_absence(),
    }
}

pub(super) fn release_handles(mut fixture: FreshDbInvalidationFixture) -> tempfile::TempDir {
    let retained = std::mem::replace(
        &mut fixture.fixture.fixture._temporary,
        private_installation_tempdir(),
    );
    drop(fixture);
    retained
}

pub(super) struct FreshInvalidationHandles {
    pub(super) installation: Installation,
    pub(super) database: db::state::Database,
    pub(super) journal: TransitionJournalStore,
    pub(super) record: TransitionRecord,
}

impl FreshInvalidationHandles {
    pub(super) fn open(root: &Path) -> Self {
        let installation = Installation::open(root, None).unwrap();
        let database = open_state_database(&installation);
        let journal = TransitionJournalStore::open_retained(installation.root_directory(), root).unwrap();
        let record = journal
            .load()
            .unwrap()
            .expect("fresh-handle reopen requires one durable invalidation record");
        Self {
            installation,
            database,
            journal,
            record,
        }
    }

    pub(super) fn capture<'reservation>(
        &self,
        reservation: &'reservation ActiveStateReservation,
    ) -> Result<UsrRollbackFreshDbInvalidationAdmission<'reservation>, UsrRollbackFreshDbInvalidationAuthorityError>
    {
        let seal = UsrRollbackFreshDbInvalidationSeal::new_for_test();
        UsrRollbackFreshDbInvalidationAuthority::capture(
            &seal,
            &self.installation,
            &self.journal,
            &self.database,
            reservation,
            &self.record,
        )
    }
}

fn open_state_database(installation: &Installation) -> db::state::Database {
    let location = installation.mutable_database_location(DatabaseKind::State).unwrap();
    let (url, anchor) = location.parts();
    let database = db::state::Database::new_anchored(url, anchor).unwrap();
    location.revalidate().unwrap();
    installation.revalidate_mutable_namespace().unwrap();
    database
}
