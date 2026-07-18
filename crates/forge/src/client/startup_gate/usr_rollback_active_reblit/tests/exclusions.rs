//! Exact phase and operation exclusions with zero Active exchange authority.

use crate::{
    client::active_state_snapshot::ActiveStateReservation,
    client::startup_reconciliation::{
        active_reblit_candidate_preserve_exchange_attempt_count,
        reset_active_reblit_candidate_preserve_exchange_attempt_count,
    },
};

use super::{
    super::{
        Dispatch,
        candidate_test_support::{CandidateLayout, CandidateSource},
        dispatch,
        test_fixture::OperationKind,
    },
    support::build_other,
};

#[test]
fn startup_active_reblit_candidate_dispatch_excludes_activate_archived_with_zero_effects() {
    let archived = build_other(
        OperationKind::Archived,
        CandidateSource::Exchanged,
        CandidateLayout::Staged,
    );
    let source = archived.candidate_intent.clone();
    let database_before = archived.fixture.database_snapshot();
    let namespace_before = archived.fixture.namespace_snapshot();
    reset_active_reblit_candidate_preserve_exchange_attempt_count();
    let journal = archived.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let in_flight = archived.fixture.database.audit_in_flight_transition().unwrap();

    let result = dispatch(
        &archived.fixture.installation,
        &archived.fixture.database,
        &reservation,
        journal,
        source.clone(),
        in_flight,
    )
    .unwrap();

    let Dispatch::Unhandled { journal, record } = result else {
        panic!("ActiveReblit child claimed an ActivateArchived candidate checkpoint")
    };
    assert_eq!(record, source);
    drop(journal);
    assert_eq!(archived.fixture.canonical_record(), source);
    assert_eq!(archived.fixture.database_snapshot(), database_before);
    assert_eq!(archived.fixture.namespace_snapshot(), namespace_before);
    assert_eq!(active_reblit_candidate_preserve_exchange_attempt_count(), 0);
}
