use std::{fs, os::unix::fs::MetadataExt as _};

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_gate::UsrRollbackDecisionSeal,
        startup_reconciliation::{RecoveryBlocker, UsrRollbackDecisionAdmission, UsrRollbackDecisionAuthority},
        startup_recovery::{
            UsrExchangeParentDurabilityError, UsrExchangeParentDurabilityEvent, UsrExchangeParentDurabilityFaultPoint,
            arm_before_usr_exchange_parent_durability_final_revalidation, arm_usr_exchange_parent_durability_fault,
            normalize_usr_exchange_parent_durability,
        },
    },
    transition_journal::{Phase, TransitionJournalStore},
};

use super::{
    assert_parent_durability_failure, assert_success_events,
    fixture::{Fixture, OperationKind, SourceCase, canonical_journal, create_private_directory, pending},
    reset_events, take_events,
};

#[test]
fn startup_usr_exchange_parent_durability_final_revalidation_races_never_advance() {
    let fixture = Fixture::new(OperationKind::Archived, SourceCase::IntentPost);
    let expected_prefix = successful_parent_sync_events(&fixture);
    reset_events();
    arm_usr_exchange_parent_durability_fault(UsrExchangeParentDurabilityFaultPoint::FinalEvidenceRevalidation);

    let error = fixture.enter();

    assert_parent_durability_failure(error);
    fixture.assert_source_unchanged();
    assert_eq!(take_events(), expected_prefix);

    let fixture = Fixture::new(OperationKind::NewState, SourceCase::IntentPost);
    let expected_prefix = successful_parent_sync_events(&fixture);
    let database = fixture.database.clone();
    let candidate = fixture.candidate_state;
    let transition = fixture.source.transition_id.clone();
    reset_events();
    arm_before_usr_exchange_parent_durability_final_revalidation(move || {
        database.clear_transition_if_matches(candidate, &transition).unwrap();
    });

    let error = fixture.enter();

    assert_parent_durability_failure(error);
    fixture.assert_source_unchanged();
    assert_eq!(take_events(), expected_prefix);

    let fixture = Fixture::new(OperationKind::Archived, SourceCase::IntentPost);
    let expected_prefix = successful_parent_sync_events(&fixture);
    let inserted = fixture
        .installation
        .state_quarantine_dir()
        .join("parent-durability-final-race");
    reset_events();
    arm_before_usr_exchange_parent_durability_final_revalidation(move || {
        create_private_directory(&inserted);
    });

    let error = fixture.enter();

    assert_parent_durability_failure(error);
    fixture.assert_source_unchanged();
    assert_eq!(take_events(), expected_prefix);
}

#[test]
fn startup_usr_exchange_parent_durability_binding_database_and_namespace_conflicts_never_advance() {
    let first = Fixture::new(OperationKind::Archived, SourceCase::IntentPost);
    let second = Fixture::new(OperationKind::Archived, SourceCase::IntentPost);
    fs::write(canonical_journal(&second.installation.root), first.canonical_bytes()).unwrap();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let first_journal =
        TransitionJournalStore::open_retained(first.installation.root_directory(), &first.installation.root).unwrap();
    let seal = UsrRollbackDecisionSeal::new_for_test();
    let admission = UsrRollbackDecisionAuthority::capture(
        &seal,
        &first.installation,
        &first_journal,
        &first.database,
        &reservation,
        &first.source,
        first.database.audit_in_flight_transition().unwrap(),
    )
    .unwrap();
    let UsrRollbackDecisionAdmission::ParentDurabilityRequired(authority) = admission else {
        panic!("exact first-root Intent+POST evidence did not require parent durability");
    };
    let second_journal =
        TransitionJournalStore::open_retained(second.installation.root_directory(), &second.installation.root).unwrap();
    reset_events();

    let error = match normalize_usr_exchange_parent_durability(&second_journal, authority) {
        Ok(_) => panic!("mixed-root journal binding unexpectedly normalized parent durability"),
        Err(error) => error,
    };

    assert!(matches!(error, UsrExchangeParentDurabilityError::Authority(_)));
    assert!(take_events().is_empty());
    first.assert_source_unchanged();
    assert_eq!(second.canonical_record(), first.source);
    drop(second_journal);
    drop(first_journal);
    drop(reservation);
    drop(second);
    drop(first);

    let fixture = Fixture::new(OperationKind::NewState, SourceCase::IntentPost);
    let before = fixture.canonical_bytes();
    fixture
        .database
        .clear_transition_if_matches(fixture.candidate_state, &fixture.source.transition_id)
        .unwrap();
    reset_events();

    let error = fixture.enter();

    assert_eq!(pending(&error).phase(), Phase::UsrExchangeIntent);
    assert!(pending(&error).blockers().contains(&RecoveryBlocker::DatabaseConflict));
    assert_eq!(fixture.canonical_bytes(), before);
    assert!(take_events().is_empty());

    let fixture = Fixture::new(OperationKind::NewState, SourceCase::IntentPost);
    let before = fixture.canonical_bytes();
    fs::remove_file(fixture.installation.isolation_path("bin")).unwrap();
    reset_events();

    let error = fixture.enter();

    assert_eq!(pending(&error).phase(), Phase::UsrExchangeIntent);
    assert!(
        pending(&error)
            .blockers()
            .contains(&RecoveryBlocker::PhaseNamespaceConflict)
    );
    assert_eq!(fixture.canonical_bytes(), before);
    assert!(take_events().is_empty());
}

#[test]
fn startup_usr_exchange_parent_durability_historical_epoch_and_active_reblit_evidence_are_exact() {
    for kind in OperationKind::ALL {
        let fixture = Fixture::historical(kind, SourceCase::IntentPost);
        let source_epoch = fixture.source.creation_epoch.clone();
        reset_events();

        let error = fixture.enter();

        assert_eq!(pending(&error).phase(), Phase::RollbackDecided, "{kind:?}");
        let decision = fixture.canonical_record();
        fixture.assert_exact_pending_reverse_decision(&decision);
        assert_eq!(decision.creation_epoch, source_epoch, "{kind:?}");
        assert_success_events(&fixture);
    }

    let fixture = Fixture::new(OperationKind::ActiveReblit, SourceCase::IntentPost);
    assert_eq!(fixture.candidate_state, fixture.previous_state);
    assert_eq!(fixture.database.all().unwrap().len(), 1);
    let database_before = fixture.database_snapshot();
    let namespace_before = fixture.namespace_snapshot();
    let reservation = fixture
        .active_reblit_reservation
        .as_ref()
        .expect("active-reblit fixture retains its reserved replacement wrapper");
    let reservation_before = fs::symlink_metadata(reservation).unwrap();
    reset_events();

    let error = fixture.enter();

    assert_eq!(pending(&error).phase(), Phase::RollbackDecided);
    fixture.assert_exact_pending_reverse_decision(&fixture.canonical_record());
    let reservation_after = fs::symlink_metadata(reservation).unwrap();
    assert_eq!(
        (
            reservation_after.dev(),
            reservation_after.ino(),
            reservation_after.mode()
        ),
        (
            reservation_before.dev(),
            reservation_before.ino(),
            reservation_before.mode()
        )
    );
    assert_eq!(fs::read_dir(reservation).unwrap().count(), 0);
    assert_eq!(fixture.database.all().unwrap().len(), 1);
    assert_eq!(fixture.database_snapshot(), database_before);
    assert_eq!(fixture.namespace_snapshot(), namespace_before);
    assert_success_events(&fixture);
}

fn successful_parent_sync_events(fixture: &Fixture) -> Vec<UsrExchangeParentDurabilityEvent> {
    let ((staging_device, staging_inode), (root_device, root_inode)) = fixture.durability_parent_identities();
    vec![
        UsrExchangeParentDurabilityEvent::StagingParentSynced {
            device: staging_device,
            inode: staging_inode,
        },
        UsrExchangeParentDurabilityEvent::InstallationRootSynced {
            device: root_device,
            inode: root_inode,
        },
    ]
}
