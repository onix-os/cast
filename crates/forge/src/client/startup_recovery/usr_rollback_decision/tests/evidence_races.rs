use std::{
    fs,
    os::unix::fs::{MetadataExt as _, PermissionsExt as _, symlink},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_gate,
        startup_reconciliation::{
            RecoveryBlocker, arm_before_usr_rollback_decision_fresh_namespace_capture,
            arm_between_usr_rollback_decision_database_captures,
        },
    },
    transition_journal::Phase,
};

use super::{
    super::{
        DurableUsrRollbackDecisionRecord, UsrRollbackDecisionPersistenceError,
        UsrRollbackDecisionSuccessorBindingError,
        arm_after_usr_rollback_decision_successor_binding_check_before_reopen,
        arm_before_usr_rollback_decision_final_revalidation,
        arm_before_usr_rollback_decision_successor_binding_revalidation,
    },
    fixture::{Fixture, OperationKind, SourceCase, canonical_journal, create_private_directory, pending},
};

#[test]
fn startup_root_links_complete_same_byte_journal_replacement_breaks_record_binding() {
    let fixture = Fixture::new(OperationKind::Archived, SourceCase::RootLinksCompletePost);
    let canonical = canonical_journal(&fixture.installation.root);
    let displaced = fixture.installation.root.join("root-links-complete-journal-displaced");
    let before = fixture.canonical_bytes();
    let hook_canonical = canonical.clone();
    let hook_displaced = displaced.clone();
    let hook_bytes = before.clone();
    arm_before_usr_rollback_decision_final_revalidation(move || {
        fs::rename(&hook_canonical, &hook_displaced).unwrap();
        fs::write(&hook_canonical, hook_bytes).unwrap();
        fs::set_permissions(&hook_canonical, fs::Permissions::from_mode(0o600)).unwrap();
    });

    assert_authority_failure(fixture.enter());

    assert_eq!(fixture.canonical_bytes(), before);
    assert_eq!(fs::read(&displaced).unwrap(), before);
    let retained = fs::symlink_metadata(displaced).unwrap();
    let replacement = fs::symlink_metadata(canonical).unwrap();
    assert_ne!((retained.dev(), retained.ino()), (replacement.dev(), replacement.ino()));
}

#[test]
fn startup_root_links_complete_successor_same_byte_replacement_reopens_but_never_succeeds() {
    let fixture = Fixture::new(OperationKind::Archived, SourceCase::RootLinksCompletePost);
    let canonical = canonical_journal(&fixture.installation.root);
    let displaced = fixture.installation.root.join("root-links-complete-successor-displaced");
    let hook_canonical = canonical.clone();
    let hook_displaced = displaced.clone();
    arm_before_usr_rollback_decision_successor_binding_revalidation(move || {
        let bytes = fs::read(&hook_canonical).unwrap();
        fs::rename(&hook_canonical, &hook_displaced).unwrap();
        fs::write(&hook_canonical, bytes).unwrap();
        fs::set_permissions(&hook_canonical, fs::Permissions::from_mode(0o600)).unwrap();
    });

    let error = fixture.enter();
    assert!(matches!(
        error,
        startup_gate::Error::UsrRollbackDecisionPersistence(
            UsrRollbackDecisionPersistenceError::SuccessorRecordBinding {
                durable: DurableUsrRollbackDecisionRecord::Decision,
                source: UsrRollbackDecisionSuccessorBindingError::Changed,
            }
        )
    ));
    let decision = fixture.canonical_record();
    fixture.assert_exact_decision(&decision);
    assert_eq!(fs::read(&displaced).unwrap(), fixture.canonical_bytes());
    let retained = fs::symlink_metadata(displaced).unwrap();
    let replacement = fs::symlink_metadata(canonical).unwrap();
    assert_ne!((retained.dev(), retained.ino()), (replacement.dev(), replacement.ino()));
}

#[test]
fn startup_root_links_complete_successor_same_byte_replacement_after_binding_before_reopen_never_succeeds() {
    for historical in [false, true] {
        for kind in OperationKind::ALL {
            let fixture = if historical {
                Fixture::historical(kind, SourceCase::RootLinksCompletePost)
            } else {
                Fixture::new(kind, SourceCase::RootLinksCompletePost)
            };
            let canonical = canonical_journal(&fixture.installation.root);
            let displaced = fixture
                .installation
                .root
                .join("root-links-complete-bound-successor-displaced");
            let hook_canonical = canonical.clone();
            let hook_displaced = displaced.clone();
            arm_after_usr_rollback_decision_successor_binding_check_before_reopen(move || {
                let bytes = fs::read(&hook_canonical).unwrap();
                fs::rename(&hook_canonical, &hook_displaced).unwrap();
                fs::write(&hook_canonical, bytes).unwrap();
                fs::set_permissions(&hook_canonical, fs::Permissions::from_mode(0o600)).unwrap();
            });

            let error = fixture.enter();

            assert!(
                matches!(
                    error,
                    startup_gate::Error::UsrRollbackDecisionPersistence(
                        UsrRollbackDecisionPersistenceError::SuccessorRecordBinding {
                            durable: DurableUsrRollbackDecisionRecord::Decision,
                            source: UsrRollbackDecisionSuccessorBindingError::Changed,
                        }
                    )
                ),
                "{kind:?} historical={historical}: {error:?}"
            );
            let decision = fixture.canonical_record();
            fixture.assert_exact_decision(&decision);
            assert_eq!(
                fs::read(&displaced).unwrap(),
                fixture.canonical_bytes(),
                "{kind:?} historical={historical}"
            );
            let retained = fs::symlink_metadata(displaced).unwrap();
            let replacement = fs::symlink_metadata(canonical).unwrap();
            assert_ne!(
                (retained.dev(), retained.ino()),
                (replacement.dev(), replacement.ino()),
                "{kind:?} historical={historical}"
            );
        }
    }
}

#[test]
fn startup_usr_rollback_decision_database_and_provenance_conflicts_never_advance() {
    for kind in OperationKind::ALL {
        for source in [SourceCase::IntentPre, SourceCase::RootLinksCompletePost] {
            let fixture = Fixture::new(kind, source);
            let before = fixture.canonical_bytes();
            if kind == OperationKind::NewState {
                fixture
                    .database
                    .clear_transition_if_matches(fixture.candidate_state, &fixture.source.transition_id)
                    .unwrap();
            } else {
                fixture.database.remove(&fixture.candidate_state).unwrap();
            }
            let error = fixture.enter();
            assert_eq!(pending(&error).phase(), source.phase(), "{kind:?} {source:?}");
            assert!(
                pending(&error).blockers().contains(&RecoveryBlocker::DatabaseConflict),
                "{kind:?} {source:?}: {:?}",
                pending(&error).blockers()
            );
            assert_eq!(fixture.canonical_bytes(), before, "{kind:?} {source:?}");
            fixture.assert_source_unchanged();

            let fixture = Fixture::new(kind, source);
            let before = fixture.canonical_bytes();
            fixture
                .database
                .delete_metadata_provenance_for_test(fixture.candidate_state)
                .unwrap();
            let error = fixture.enter();
            assert_eq!(pending(&error).phase(), source.phase(), "{kind:?} {source:?}");
            assert!(
                pending(&error)
                    .blockers()
                    .contains(&RecoveryBlocker::MetadataProvenanceConflict),
                "{kind:?} {source:?}: {:?}",
                pending(&error).blockers()
            );
            assert_eq!(fixture.canonical_bytes(), before, "{kind:?} {source:?}");
            fixture.assert_source_unchanged();
        }
    }
}

#[test]
fn startup_usr_rollback_decision_namespace_layout_and_abi_conflicts_never_advance() {
    for kind in OperationKind::ALL {
        let fixture = Fixture::new(kind, SourceCase::ExchangedPre);
        let before = fixture.canonical_bytes();
        let error = fixture.enter();
        assert_eq!(pending(&error).phase(), Phase::UsrExchanged, "{kind:?}");
        assert!(
            pending(&error)
                .blockers()
                .contains(&RecoveryBlocker::PhaseNamespaceConflict),
            "{kind:?}: {:?}",
            pending(&error).blockers()
        );
        assert_eq!(fixture.canonical_bytes(), before, "{kind:?}");
    }

    for kind in OperationKind::ALL {
        let fixture = Fixture::new(kind, SourceCase::RootLinksCompletePost);
        let before = fixture.canonical_bytes();
        fs::remove_file(fixture.installation.root.join("bin")).unwrap();
        let error = fixture.enter();
        assert_eq!(pending(&error).phase(), Phase::RootLinksComplete, "{kind:?}");
        assert!(
            pending(&error)
                .blockers()
                .contains(&RecoveryBlocker::PhaseNamespaceConflict),
            "{kind:?}: {:?}",
            pending(&error).blockers()
        );
        assert_eq!(fixture.canonical_bytes(), before, "{kind:?}");
    }

    for kind in [OperationKind::NewState, OperationKind::ActiveReblit] {
        let fixture = Fixture::new(kind, SourceCase::IntentPre);
        let before = fixture.canonical_bytes();
        fs::remove_file(fixture.installation.isolation_path("bin")).unwrap();
        let error = fixture.enter();
        assert_eq!(pending(&error).phase(), Phase::UsrExchangeIntent, "{kind:?}");
        assert!(
            pending(&error)
                .blockers()
                .contains(&RecoveryBlocker::PhaseNamespaceConflict),
            "{kind:?}: {:?}",
            pending(&error).blockers()
        );
        assert_eq!(fixture.canonical_bytes(), before, "{kind:?}");
    }

    let fixture = Fixture::new(OperationKind::Archived, SourceCase::IntentPre);
    let before = fixture.canonical_bytes();
    symlink("usr/not-bin", fixture.installation.root.join("bin")).unwrap();
    let error = fixture.enter();
    assert_eq!(pending(&error).phase(), Phase::UsrExchangeIntent);
    assert!(
        pending(&error)
            .blockers()
            .contains(&RecoveryBlocker::ActivationNamespaceRejected),
        "{:?}",
        pending(&error).blockers()
    );
    assert_eq!(fixture.canonical_bytes(), before);

    let fixture = Fixture::new(OperationKind::Archived, SourceCase::IntentPre);
    let before = fixture.canonical_bytes();
    create_private_directory(&fixture.installation.root_path("foreign-wrapper"));
    let error = fixture.enter();
    assert_eq!(pending(&error).phase(), Phase::UsrExchangeIntent);
    assert!(
        pending(&error)
            .blockers()
            .contains(&RecoveryBlocker::ActivationNamespaceRejected),
        "{:?}",
        pending(&error).blockers()
    );
    assert_eq!(fixture.canonical_bytes(), before);
}

#[test]
fn startup_usr_rollback_decision_evidence_races_fail_before_advance() {
    let fixture = Fixture::new(OperationKind::NewState, SourceCase::IntentPre);
    let before = fixture.canonical_bytes();
    let database = fixture.database.clone();
    let candidate = fixture.candidate_state;
    let transition = fixture.source.transition_id.clone();
    arm_between_usr_rollback_decision_database_captures(move || {
        database.clear_transition_if_matches(candidate, &transition).unwrap();
    });
    let error = fixture.enter();
    assert_eq!(pending(&error).phase(), Phase::UsrExchangeIntent);
    assert_eq!(fixture.canonical_bytes(), before);

    let fixture = Fixture::new(OperationKind::Archived, SourceCase::IntentPre);
    let before = fixture.canonical_bytes();
    let database = fixture.database.clone();
    let candidate = fixture.candidate_state;
    arm_between_usr_rollback_decision_database_captures(move || {
        database.delete_metadata_provenance_for_test(candidate).unwrap();
    });
    let error = fixture.enter();
    assert_eq!(pending(&error).phase(), Phase::UsrExchangeIntent);
    assert_eq!(fixture.canonical_bytes(), before);

    let fixture = Fixture::new(OperationKind::Archived, SourceCase::IntentPre);
    let before = fixture.canonical_bytes();
    let inserted = fixture
        .installation
        .state_quarantine_dir()
        .join("rollback-race-ambient");
    arm_before_usr_rollback_decision_fresh_namespace_capture(move || {
        create_private_directory(&inserted);
    });
    assert_authority_failure(fixture.enter());
    assert_eq!(fixture.canonical_bytes(), before);

    let fixture = Fixture::new(OperationKind::NewState, SourceCase::IntentPre);
    let before = fixture.canonical_bytes();
    let isolation_link = fixture.installation.isolation_path("bin");
    arm_before_usr_rollback_decision_fresh_namespace_capture(move || {
        fs::remove_file(isolation_link).unwrap();
    });
    assert_authority_failure(fixture.enter());
    assert_eq!(fixture.canonical_bytes(), before);

    let fixture = Fixture::new(OperationKind::NewState, SourceCase::ExchangedPost);
    let before = fixture.canonical_bytes();
    let database = fixture.database.clone();
    let candidate = fixture.candidate_state;
    let transition = fixture.source.transition_id.clone();
    arm_before_usr_rollback_decision_final_revalidation(move || {
        database.clear_transition_if_matches(candidate, &transition).unwrap();
    });
    assert_authority_failure(fixture.enter());
    assert_eq!(fixture.canonical_bytes(), before);

    let fixture = Fixture::new(OperationKind::Archived, SourceCase::RootLinksCompletePost);
    let before = fixture.canonical_bytes();
    let root_link = fixture.installation.root.join("bin");
    arm_before_usr_rollback_decision_final_revalidation(move || {
        fs::remove_file(root_link).unwrap();
    });
    assert_authority_failure(fixture.enter());
    assert_eq!(fixture.canonical_bytes(), before);
}

#[test]
fn startup_usr_rollback_decision_historical_epoch_uses_durable_identity() {
    for kind in OperationKind::ALL {
        for source in [SourceCase::ExchangedPost, SourceCase::RootLinksCompletePost] {
            let fixture = Fixture::historical(kind, source);
            let source_epoch = fixture.source.creation_epoch.clone();
            let error = fixture.enter();
            assert_eq!(pending(&error).phase(), Phase::RollbackDecided, "{kind:?} {source:?}");
            let decision = fixture.canonical_record();
            fixture.assert_exact_decision(&decision);
            assert_eq!(decision.creation_epoch, source_epoch, "{kind:?} {source:?}");
        }
    }
}

#[test]
fn startup_usr_rollback_decision_active_reblit_uses_one_state_row_and_retains_reservation() {
    for source in [
        SourceCase::IntentPre,
        SourceCase::ExchangedPost,
        SourceCase::RootLinksCompletePost,
    ] {
        let fixture = Fixture::new(OperationKind::ActiveReblit, source);
        assert_eq!(fixture.candidate_state, fixture.previous_state);
        assert_eq!(fixture.database.all().unwrap().len(), 1);
        let database_before = fixture.database_snapshot();
        let reservation = fixture
            .active_reblit_reservation
            .as_ref()
            .expect("active-reblit fixture has a reserved replacement wrapper");
        let before = fs::symlink_metadata(reservation).unwrap();
        assert_eq!(fs::read_dir(reservation).unwrap().count(), 0);

        let contender_acquired = Arc::new(AtomicBool::new(false));
        let contender_acquired_in_thread = Arc::clone(&contender_acquired);
        let contender = Arc::new(Mutex::new(None));
        let contender_in_hook = Arc::clone(&contender);
        arm_before_usr_rollback_decision_final_revalidation(move || {
            let (started_tx, started_rx) = mpsc::channel();
            let acquired_by_contender = Arc::clone(&contender_acquired_in_thread);
            let handle = thread::spawn(move || {
                started_tx.send(()).unwrap();
                let reservation = ActiveStateReservation::acquire().unwrap();
                acquired_by_contender.store(true, Ordering::SeqCst);
                drop(reservation);
            });
            *contender_in_hook.lock().unwrap() = Some(handle);
            started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
            thread::sleep(Duration::from_millis(50));
            assert!(
                !contender_acquired_in_thread.load(Ordering::SeqCst),
                "cooperating writer acquired during final rollback-decision revalidation"
            );
        });

        let error = fixture.enter();
        assert_eq!(pending(&error).phase(), Phase::RollbackDecided, "{source:?}");
        fixture.assert_exact_decision(&fixture.canonical_record());

        for _ in 0..100 {
            if contender_acquired.load(Ordering::SeqCst) {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert!(
            contender_acquired.load(Ordering::SeqCst),
            "cooperating writer did not acquire after startup returned"
        );
        contender
            .lock()
            .unwrap()
            .take()
            .expect("final-revalidation hook spawned a contender")
            .join()
            .unwrap();

        let after = fs::symlink_metadata(reservation).unwrap();
        assert_eq!(
            (after.dev(), after.ino(), after.mode()),
            (before.dev(), before.ino(), before.mode())
        );
        assert_eq!(fs::read_dir(reservation).unwrap().count(), 0);
        assert_eq!(fixture.database.all().unwrap().len(), 1);
        assert_eq!(fixture.database_snapshot(), database_before);
    }
}

fn assert_authority_failure(error: startup_gate::Error) {
    assert!(matches!(
        error,
        startup_gate::Error::UsrRollbackDecisionPersistence(UsrRollbackDecisionPersistenceError::Authority(_))
    ));
}
