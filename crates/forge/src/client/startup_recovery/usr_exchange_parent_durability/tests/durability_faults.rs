use crate::{
    client::startup_recovery::{
        UsrExchangeParentDurabilityEvent, UsrExchangeParentDurabilityFaultPoint,
        arm_usr_exchange_parent_durability_fault,
    },
    transition_identity::{reset_retained_exchange_syscall_count, retained_exchange_syscall_count},
    transition_journal::Phase,
};

use super::{
    assert_parent_durability_failure, assert_success_events,
    fixture::{Fixture, OperationKind, SourceCase, pending},
    reset_events, take_events,
};

#[test]
fn startup_usr_exchange_parent_durability_syncs_each_parent_once_in_exact_order() {
    let fixture = Fixture::new(OperationKind::Archived, SourceCase::IntentPost);
    reset_events();

    let error = fixture.enter();

    assert_eq!(pending(&error).phase(), Phase::RollbackDecided);
    fixture.assert_exact_pending_reverse_decision(&fixture.canonical_record());
    assert_success_events(&fixture);
}

#[test]
fn startup_usr_exchange_parent_durability_staging_sync_failure_retains_exact_intent_post() {
    let fixture = Fixture::new(OperationKind::NewState, SourceCase::IntentPost);
    let namespace_before = fixture.namespace_snapshot();
    let database_before = fixture.database_snapshot();
    reset_events();
    arm_usr_exchange_parent_durability_fault(UsrExchangeParentDurabilityFaultPoint::StagingParentSync);

    let error = fixture.enter();

    assert_parent_durability_failure(error);
    fixture.assert_source_unchanged();
    assert_eq!(fixture.namespace_snapshot(), namespace_before);
    assert_eq!(fixture.database_snapshot(), database_before);
    assert!(take_events().is_empty());
}

#[test]
fn startup_usr_exchange_parent_durability_root_sync_failure_retains_exact_intent_post() {
    let fixture = Fixture::new(OperationKind::Archived, SourceCase::IntentPost);
    let ((staging_device, staging_inode), _) = fixture.durability_parent_identities();
    let namespace_before = fixture.namespace_snapshot();
    let database_before = fixture.database_snapshot();
    reset_events();
    arm_usr_exchange_parent_durability_fault(UsrExchangeParentDurabilityFaultPoint::InstallationRootSync);

    let error = fixture.enter();

    assert_parent_durability_failure(error);
    fixture.assert_source_unchanged();
    assert_eq!(fixture.namespace_snapshot(), namespace_before);
    assert_eq!(fixture.database_snapshot(), database_before);
    assert_eq!(
        take_events(),
        vec![UsrExchangeParentDurabilityEvent::StagingParentSynced {
            device: staging_device,
            inode: staging_inode,
        }]
    );
}

#[test]
fn startup_usr_exchange_parent_durability_retry_is_idempotent_and_never_reexchanges() {
    let fixture = Fixture::new(OperationKind::ActiveReblit, SourceCase::IntentPost);
    reset_events();
    arm_usr_exchange_parent_durability_fault(UsrExchangeParentDurabilityFaultPoint::InstallationRootSync);
    reset_retained_exchange_syscall_count();

    let first = fixture.enter();

    assert_parent_durability_failure(first);
    fixture.assert_source_unchanged();
    assert_eq!(retained_exchange_syscall_count(), 0);
    assert_eq!(take_events().len(), 1);

    reset_events();
    reset_retained_exchange_syscall_count();
    let second = fixture.enter();
    assert_eq!(pending(&second).phase(), Phase::RollbackDecided);
    fixture.assert_exact_pending_reverse_decision(&fixture.canonical_record());
    assert_eq!(retained_exchange_syscall_count(), 0);
    assert_success_events(&fixture);
    let decision = fixture.canonical_record();

    reset_events();
    reset_retained_exchange_syscall_count();
    let third = fixture.enter();
    let expected_route = decision.rollback_successor(None).unwrap();
    assert_eq!(pending(&third).phase(), Phase::ReverseExchangeIntent);
    assert_eq!(fixture.canonical_record(), expected_route);
    assert_eq!(retained_exchange_syscall_count(), 0);
    assert!(take_events().is_empty());
}
