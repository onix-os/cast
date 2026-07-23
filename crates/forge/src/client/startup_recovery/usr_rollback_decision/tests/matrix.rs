use std::fs;

use crate::client::startup_reconciliation::{
    RecoveryBlocker, reset_usr_exchanged_root_abi_effect_counts, usr_exchanged_root_abi_complete_sync_attempts,
    usr_exchanged_root_abi_publication_attempts,
};
use crate::client::{
    active_state_snapshot::ActiveStateReservation,
    startup_gate::{self, CleanSystemStartup, UsrRollbackDecisionSeal},
    startup_reconciliation::{
        UsrRollbackDecisionAdmission, UsrRollbackDecisionAuthority, usr_rollback_decision_source_is_supported_for_test,
    },
};
use crate::transition_journal::{BootRollback, ForwardPhase, Phase, RecoveryDisposition, TransitionJournalStore};

use super::{
    super::{UsrRollbackDecisionPersistenceError, persist_usr_rollback_decision_and_reopen},
    fixture::{Fixture, OperationKind, SourceCase, canonical_journal, pending},
};

#[test]
fn startup_usr_rollback_decision_admitted_matrix_persists_exact_plan() {
    for kind in OperationKind::ALL {
        for source in [
            SourceCase::IntentPre,
            SourceCase::ExchangedPost,
            SourceCase::RootLinksCompletePost,
        ] {
            let fixture = Fixture::new(kind, source);
            let error = fixture.enter();
            let pending = pending(&error);
            assert_eq!(pending.phase(), Phase::RollbackDecided, "{kind:?} {source:?}");
            assert_eq!(
                pending.disposition(),
                RecoveryDisposition::ResumeRollback {
                    phase: Phase::RollbackDecided,
                },
                "{kind:?} {source:?}"
            );
            assert!(
                pending.blockers().is_empty(),
                "{kind:?} {source:?}: {:?}",
                pending.blockers()
            );
            fixture.assert_exact_decision(&fixture.canonical_record());
        }
    }
}

#[test]
fn startup_usr_rollback_decision_exchanged_pre_remains_incompatible() {
    for kind in OperationKind::ALL {
        for source in [SourceCase::ExchangedPre, SourceCase::RootLinksCompletePre] {
            let fixture = Fixture::new(kind, source);
            let before = fixture.canonical_bytes();
            let error = fixture.enter();
            let pending = pending(&error);
            assert_eq!(pending.phase(), source.phase(), "{kind:?} {source:?}");
            assert!(
                pending.blockers().contains(&RecoveryBlocker::PhaseNamespaceConflict),
                "{kind:?} {source:?}: {:?}",
                pending.blockers()
            );
            assert_eq!(fixture.canonical_bytes(), before, "{kind:?} {source:?}");
            fixture.assert_source_unchanged();
        }
    }
}

#[test]
fn startup_root_links_complete_requires_exact_complete_abi_and_never_republishes() {
    for kind in OperationKind::ALL {
        for mask in 0_u8..32 {
            let fixture = Fixture::new(kind, SourceCase::RootLinksCompletePost);
            fixture.set_root_abi_subset(mask);
            let namespace_before = fixture.namespace_snapshot();
            let database_before = fixture.database_snapshot();
            let journal_before = fixture.canonical_bytes();
            reset_usr_exchanged_root_abi_effect_counts();

            let error = fixture.enter();
            let pending = pending(&error);
            if mask == 31 {
                assert_eq!(pending.phase(), Phase::RollbackDecided, "{kind:?}");
                assert!(pending.blockers().is_empty(), "{kind:?}: {:?}", pending.blockers());
                assert_ne!(fixture.canonical_bytes(), journal_before, "{kind:?}");
                fixture.assert_exact_decision(&fixture.canonical_record());
            } else {
                assert_eq!(pending.phase(), Phase::RootLinksComplete, "{kind:?} mask={mask}");
                assert!(
                    pending.blockers().contains(&RecoveryBlocker::PhaseNamespaceConflict),
                    "{kind:?} mask={mask}: {:?}",
                    pending.blockers()
                );
                assert_eq!(fixture.canonical_bytes(), journal_before, "{kind:?} mask={mask}");
            }
            assert_eq!(usr_exchanged_root_abi_publication_attempts(), 0, "{kind:?} mask={mask}");
            assert_eq!(usr_exchanged_root_abi_complete_sync_attempts(), 0, "{kind:?} mask={mask}");
            assert_eq!(fixture.namespace_snapshot(), namespace_before, "{kind:?} mask={mask}");
            assert_eq!(fixture.database_snapshot(), database_before, "{kind:?} mask={mask}");
        }
    }
}

#[test]
fn startup_usr_rollback_decision_changes_only_the_canonical_journal() {
    for kind in OperationKind::ALL {
        let fixture = Fixture::new(kind, SourceCase::ExchangedPost);
        let namespace_before = fixture.namespace_snapshot();
        let database_before = fixture.database_snapshot();
        let canonical_before = fixture.canonical_bytes();

        let error = fixture.enter();
        assert_eq!(pending(&error).phase(), Phase::RollbackDecided, "{kind:?}");

        assert_ne!(fixture.canonical_bytes(), canonical_before, "{kind:?}");
        fixture.assert_exact_decision(&fixture.canonical_record());
        assert_eq!(fixture.namespace_snapshot(), namespace_before, "{kind:?}");
        assert_eq!(fixture.database_snapshot(), database_before, "{kind:?}");
    }

    let first = Fixture::new(OperationKind::Archived, SourceCase::IntentPre);
    let second = Fixture::new(OperationKind::Archived, SourceCase::IntentPre);
    fs::write(canonical_journal(&second.installation.root), first.canonical_bytes()).unwrap();
    assert_eq!(second.canonical_record(), first.source);
    let reservation = ActiveStateReservation::acquire().unwrap();
    let first_journal =
        TransitionJournalStore::open_retained(first.installation.root_directory(), &first.installation.root).unwrap();
    let seal = UsrRollbackDecisionSeal::new_for_test();
    let authority = UsrRollbackDecisionAuthority::capture(
        &seal,
        &first.installation,
        &first_journal,
        &first.database,
        &reservation,
        &first.source,
        first.database.audit_in_flight_transition().unwrap(),
    )
    .unwrap();
    let UsrRollbackDecisionAdmission::Ready(authority) = authority else {
        panic!("exact first-root evidence did not admit rollback-decision authority");
    };
    let second_journal =
        TransitionJournalStore::open_retained(second.installation.root_directory(), &second.installation.root).unwrap();
    let error = persist_usr_rollback_decision_and_reopen(second_journal, authority).unwrap_err();
    assert!(matches!(error, UsrRollbackDecisionPersistenceError::Authority(_)));
    assert_eq!(first_journal.load().unwrap(), Some(first.source.clone()));
    assert_eq!(first.canonical_record(), first.source);
    assert_eq!(second.canonical_record(), first.source);
}

#[test]
fn startup_system_trigger_post_sources_reach_the_exact_terminal_outcome() {
    for historical in [false, true] {
        for kind in [OperationKind::NewState, OperationKind::ActiveReblit] {
            for source in [Phase::SystemTriggersStarted, Phase::SystemTriggersComplete] {
                let fixture = Fixture::system_trigger(kind, source, true, historical);
                assert!(usr_rollback_decision_source_is_supported_for_test(&fixture.source));
                let expected_source = system_trigger_forward_phase(fixture.source.phase);
                let expected_terminal_generation = fixture.source.generation
                    + match kind {
                        OperationKind::NewState => 8,
                        OperationKind::ActiveReblit => 6,
                        OperationKind::Archived => unreachable!("archived system triggers are excluded"),
                    };
                let mut observed = Vec::new();
                let mut last_record = None;
                let mut clean = false;

                for _ in 0..20 {
                    match enter_once(&fixture) {
                        Ok(()) => {
                            clean = true;
                            break;
                        }
                        Err(error) => {
                            let pending = pending(&error);
                            let record = fixture.canonical_record();
                            assert_eq!(record.phase, pending.phase(), "{kind:?} {source:?} historical={historical}");
                            assert_eq!(record.operation, fixture.source.operation);
                            assert_eq!(record.transition_id, fixture.source.transition_id);
                            assert_eq!(record.creation_epoch, fixture.source.creation_epoch);
                            assert_eq!(&record.candidate, &fixture.source.candidate);
                            assert_eq!(&record.previous, &fixture.source.previous);
                            assert_eq!(&record.options, &fixture.source.options);
                            assert_eq!(&record.quarantine_name, &fixture.source.quarantine_name);
                            let rollback = record.rollback.as_ref().expect("rollback successor retains its plan");
                            assert_eq!(rollback.source, expected_source);
                            assert_eq!(rollback.boot, BootRollback::NotRequired);
                            assert!(rollback.external_effects_may_remain);
                            assert!(record.generation > fixture.source.generation);
                            assert!(record.generation <= expected_terminal_generation);
                            if observed.is_empty() {
                                fixture.assert_exact_decision(&record);
                            }
                            observed.push((record.phase, record.generation));
                            last_record = Some(record);
                        }
                    }
                }

                assert!(clean, "system-trigger rollback did not converge: {kind:?} {source:?} historical={historical}");
                assert_eq!(observed.first().map(|entry| entry.0), Some(Phase::RollbackDecided));
                let terminal = last_record.expect("terminal rollback record was observed before deletion");
                assert_eq!(terminal.phase, Phase::RollbackComplete);
                assert_eq!(terminal.generation, expected_terminal_generation);
                assert_eq!(terminal.rollback.as_ref().unwrap().source, expected_source);
                assert_eq!(
                    fs::symlink_metadata(canonical_journal(&fixture.installation.root))
                        .unwrap_err()
                        .kind(),
                    std::io::ErrorKind::NotFound,
                );
            }
        }
    }
}

#[test]
fn startup_system_trigger_sources_require_post_and_exclude_activate_archived() {
    for historical in [false, true] {
        for kind in [OperationKind::NewState, OperationKind::ActiveReblit] {
            for source in [Phase::SystemTriggersStarted, Phase::SystemTriggersComplete] {
                let fixture = Fixture::system_trigger(kind, source, false, historical);
                assert!(usr_rollback_decision_source_is_supported_for_test(&fixture.source));
                let before = fixture.canonical_bytes();
                let error = fixture.enter();
                let pending = pending(&error);
                assert_eq!(pending.phase(), source);
                assert!(pending.blockers().contains(&RecoveryBlocker::PhaseNamespaceConflict));
                assert_eq!(fixture.canonical_bytes(), before);
                fixture.assert_source_unchanged();
            }
        }

        for source in [Phase::SystemTriggersStarted, Phase::SystemTriggersComplete] {
            let fixture = Fixture::system_trigger(OperationKind::Archived, source, true, historical);
            assert!(!usr_rollback_decision_source_is_supported_for_test(&fixture.source));
            let before = fixture.canonical_bytes();
            let error = fixture.enter();
            assert_eq!(pending(&error).phase(), source);
            assert_eq!(fixture.canonical_bytes(), before);
            fixture.assert_source_unchanged();
        }
    }
}

fn enter_once(fixture: &Fixture) -> Result<(), startup_gate::Error> {
    let reservation = ActiveStateReservation::acquire().unwrap();
    CleanSystemStartup::enter(&fixture.system, &reservation).map(drop)
}

fn system_trigger_forward_phase(phase: Phase) -> ForwardPhase {
    match phase {
        Phase::SystemTriggersStarted => ForwardPhase::SystemTriggersStarted,
        Phase::SystemTriggersComplete => ForwardPhase::SystemTriggersComplete,
        other => panic!("expected system-trigger source, got {other:?}"),
    }
}
