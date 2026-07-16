use std::{
    fs,
    os::unix::fs::{MetadataExt as _, PermissionsExt as _},
    sync::mpsc,
    thread,
    time::Duration,
};

use crate::{
    Installation, db,
    state::{self, TransitionId},
    test_support::private_installation_tempdir,
    transition_journal::{
        AbortDisposition, BootId, BootRollback, CandidateRollback, ForwardPhase, MountNamespaceIdentity, Operation,
        Phase, Previous, PreviousOrigin, QuarantineName, RollbackAction, RollbackPlan, RuntimeEpoch,
        RuntimeEvidenceError, RuntimeTreeIdentity, TransitionJournalStore, TransitionRecord, TreeToken,
    },
    tree_marker::{TreeMarkerError, TreeMarkerStore},
};

use super::super::{Client, Error as ClientError, startup_gate};
use super::*;

const FORWARD_PHASES: [Phase; 19] = [
    Phase::Preparing,
    Phase::FreshStateAllocating,
    Phase::FreshStateAllocated,
    Phase::CandidatePrepareStarted,
    Phase::CandidatePrepared,
    Phase::TransactionTriggersStarted,
    Phase::TransactionTriggersComplete,
    Phase::UsrExchangeIntent,
    Phase::UsrExchanged,
    Phase::RootLinksComplete,
    Phase::SystemTriggersStarted,
    Phase::SystemTriggersComplete,
    Phase::PreviousArchiveIntent,
    Phase::PreviousArchived,
    Phase::BootSyncStarted,
    Phase::BootSyncComplete,
    Phase::CommitDecided,
    Phase::CommitCleanupComplete,
    Phase::Complete,
];

const ROLLBACK_CASES: [(Phase, RollbackAction, FreshDatabaseExpectation); 13] = [
    (
        Phase::RollbackDecided,
        RollbackAction::Pending,
        FreshDatabaseExpectation::Matching,
    ),
    (
        Phase::PreviousRestoreIntent,
        RollbackAction::Pending,
        FreshDatabaseExpectation::Matching,
    ),
    (
        Phase::PreviousRestoredToStaging,
        RollbackAction::Pending,
        FreshDatabaseExpectation::Matching,
    ),
    (
        Phase::ReverseExchangeIntent,
        RollbackAction::Pending,
        FreshDatabaseExpectation::Matching,
    ),
    (
        Phase::UsrRestored,
        RollbackAction::Pending,
        FreshDatabaseExpectation::Matching,
    ),
    (
        Phase::CandidatePreserveIntent,
        RollbackAction::Pending,
        FreshDatabaseExpectation::Matching,
    ),
    (
        Phase::CandidatePreserved,
        RollbackAction::Pending,
        FreshDatabaseExpectation::Matching,
    ),
    (
        Phase::FreshDbInvalidationIntent,
        RollbackAction::Pending,
        FreshDatabaseExpectation::MatchingOrMissing,
    ),
    (
        Phase::FreshDbInvalidated,
        RollbackAction::Applied,
        FreshDatabaseExpectation::Missing,
    ),
    (
        Phase::BootRepairRequired,
        RollbackAction::Applied,
        FreshDatabaseExpectation::Missing,
    ),
    (
        Phase::BootRepairStarted,
        RollbackAction::Applied,
        FreshDatabaseExpectation::Missing,
    ),
    (
        Phase::BootRepairUnverified,
        RollbackAction::Applied,
        FreshDatabaseExpectation::Missing,
    ),
    (
        Phase::RollbackComplete,
        RollbackAction::Applied,
        FreshDatabaseExpectation::Missing,
    ),
];

fn transition_id() -> TransitionId {
    TransitionId::parse("0123456789abcdef0123456789abcdef").unwrap()
}

fn epoch(number: u64) -> RuntimeEpoch {
    RuntimeEpoch {
        boot_id: BootId::parse(format!("01234567-89ab-4cde-8f01-{number:012x}")).unwrap(),
        mount_namespace: MountNamespaceIdentity {
            st_dev: 30 + number,
            inode: 31 + number,
        },
    }
}

fn tree_token(digit: char) -> TreeToken {
    TreeToken::parse(digit.to_string().repeat(TreeToken::TEXT_LENGTH)).unwrap()
}

fn runtime_tree(inode: u64) -> RuntimeTreeIdentity {
    RuntimeTreeIdentity {
        st_dev: 10,
        inode,
        mount_id: 12,
    }
}

fn creation_record() -> TransitionRecord {
    TransitionRecord::preparing(
        transition_id(),
        epoch(1),
        Operation::NewState,
        None,
        tree_token('a'),
        runtime_tree(10),
        Previous {
            id: None,
            tree_token: tree_token('b'),
            usr_runtime_identity: runtime_tree(20),
            origin: PreviousOrigin::SynthesizedEmpty,
        },
        true,
        true,
        QuarantineName::parse("failed-startup-reconciliation").unwrap(),
    )
    .unwrap()
}

fn record_at(phase: Phase) -> TransitionRecord {
    let mut record = creation_record();
    record.phase = phase;
    record.candidate.id = Some(42);
    record
}

fn rollback_record(phase: Phase, fresh_db: RollbackAction) -> TransitionRecord {
    let mut record = record_at(phase);
    record.rollback = Some(RollbackPlan {
        source: ForwardPhase::FreshStateAllocated,
        previous_archive: RollbackAction::NotRequired,
        usr_exchange: RollbackAction::NotRequired,
        candidate: CandidateRollback {
            action: RollbackAction::Pending,
            disposition: AbortDisposition::Quarantine,
        },
        fresh_db,
        boot: BootRollback::NotRequired,
        external_effects_may_remain: false,
    });
    record
}

fn candidate_evidence(ownership: db::state::TransitionOwnership) -> DatabaseEvidence {
    DatabaseEvidence::CandidateOwnership {
        state: state::Id::from(42),
        ownership,
        previous: None,
    }
}

fn expectation_accepts(expectation: FreshDatabaseExpectation, ownership: db::state::TransitionOwnership) -> bool {
    match expectation {
        FreshDatabaseExpectation::Matching => ownership == db::state::TransitionOwnership::Matching,
        FreshDatabaseExpectation::MatchingOrCleared => matches!(
            ownership,
            db::state::TransitionOwnership::Matching | db::state::TransitionOwnership::Cleared
        ),
        FreshDatabaseExpectation::Cleared => ownership == db::state::TransitionOwnership::Cleared,
        FreshDatabaseExpectation::MatchingOrMissing => matches!(
            ownership,
            db::state::TransitionOwnership::Matching | db::state::TransitionOwnership::Missing
        ),
        FreshDatabaseExpectation::Missing => ownership == db::state::TransitionOwnership::Missing,
    }
}

fn create_tree(path: &std::path::Path) {
    fs::create_dir(path).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn expect_recovery_pending(result: Result<Client, ClientError>) -> Box<startup_gate::Error> {
    let source = match result {
        Err(ClientError::SystemStartupGate { source }) => source,
        Err(other) => panic!("expected recovery-pending startup gate, got {other:?}"),
        Ok(_) => panic!("startup unexpectedly succeeded"),
    };
    match source.downcast::<startup_gate::Error>() {
        Ok(source) if matches!(source.as_ref(), startup_gate::Error::RecoveryPending(_)) => source,
        Ok(source) => panic!("expected RecoveryPending, got {source:?}"),
        Err(source) => panic!("unexpected startup-gate source: {source}"),
    }
}

#[test]
fn startup_reconciliation_database_phase_matrix_is_exact() {
    let ownerships = [
        db::state::TransitionOwnership::Matching,
        db::state::TransitionOwnership::Cleared,
        db::state::TransitionOwnership::Missing,
    ];

    for phase in FORWARD_PHASES {
        let record = record_at(phase);
        let expected = match phase {
            Phase::CommitDecided => FreshDatabaseExpectation::MatchingOrCleared,
            Phase::CommitCleanupComplete | Phase::Complete => FreshDatabaseExpectation::Cleared,
            _ => FreshDatabaseExpectation::Matching,
        };
        assert_eq!(fresh_database_expectation(&record), expected, "{phase:?}");
        for ownership in ownerships {
            assert_eq!(
                database_evidence_compatible(&record, &candidate_evidence(ownership)),
                expectation_accepts(expected, ownership),
                "{phase:?} {ownership:?}"
            );
        }

        let mut before_allocation = record.clone();
        before_allocation.candidate.id = None;
        assert_eq!(
            database_evidence_compatible(
                &before_allocation,
                &DatabaseEvidence::AllocationNotObserved { previous: None }
            ),
            matches!(phase, Phase::Preparing | Phase::FreshStateAllocating),
            "allocation-not-observed at {phase:?}"
        );
        assert_eq!(
            database_evidence_compatible(
                &before_allocation,
                &DatabaseEvidence::AllocationCommittedBehindJournal {
                    state: state::Id::from(42),
                    previous: None,
                }
            ),
            phase == Phase::FreshStateAllocating,
            "allocation-behind-journal at {phase:?}"
        );
    }

    for (phase, action, expected) in ROLLBACK_CASES {
        let record = rollback_record(phase, action);
        assert_eq!(fresh_database_expectation(&record), expected, "{phase:?}");
        for ownership in ownerships {
            assert_eq!(
                database_evidence_compatible(&record, &candidate_evidence(ownership)),
                expectation_accepts(expected, ownership),
                "{phase:?} {ownership:?}"
            );
        }
    }

    assert_eq!(
        fresh_database_expectation(&rollback_record(
            Phase::RollbackDecided,
            RollbackAction::AlreadySatisfied
        )),
        FreshDatabaseExpectation::Missing
    );
    assert_eq!(
        fresh_database_expectation(&rollback_record(Phase::RollbackDecided, RollbackAction::NotRequired)),
        FreshDatabaseExpectation::Matching
    );

    let mut preparing_rollback = rollback_record(Phase::RollbackDecided, RollbackAction::NotRequired);
    preparing_rollback.candidate.id = None;
    preparing_rollback.rollback.as_mut().unwrap().source = ForwardPhase::Preparing;
    assert!(database_evidence_compatible(
        &preparing_rollback,
        &DatabaseEvidence::AllocationNotObserved { previous: None }
    ));

    let record = record_at(Phase::CandidatePrepared);
    for ownership in [
        db::state::TransitionOwnership::Missing,
        db::state::TransitionOwnership::Foreign,
    ] {
        let evidence = DatabaseEvidence::CandidateOwnership {
            state: state::Id::from(42),
            ownership: db::state::TransitionOwnership::Matching,
            previous: Some(ExistingStateEvidence {
                state: state::Id::from(41),
                ownership,
            }),
        };
        assert!(
            !database_evidence_compatible(&record, &evidence),
            "recorded previous ownership {ownership:?} must block recovery"
        );
    }
}

#[test]
fn startup_reconciliation_matching_allocation_behind_journal_is_retained() {
    let database = db::state::Database::new(":memory:").unwrap();
    let previous = database.add(&[], Some("previous"), None).unwrap();
    let candidate = database
        .add_with_transition(&transition_id(), &[], Some("candidate"), None)
        .unwrap();
    let mut record = record_at(Phase::FreshStateAllocating);
    record.candidate.id = None;
    record.previous.id = Some(previous.id.into());
    record.previous.origin = PreviousOrigin::ActiveState;

    let evidence = inspect_database(&record, &database, database.audit_in_flight_transition().unwrap()).unwrap();

    assert_eq!(
        evidence,
        DatabaseEvidence::AllocationCommittedBehindJournal {
            state: candidate.id,
            previous: Some(ExistingStateEvidence {
                state: previous.id,
                ownership: db::state::TransitionOwnership::Cleared,
            }),
        }
    );
    assert!(database_evidence_compatible(&record, &evidence));
}

#[test]
fn startup_reconciliation_inconsistent_database_audit_is_blocked() {
    let database = db::state::Database::new(":memory:").unwrap();
    let candidate = database
        .add_with_transition(&transition_id(), &[], Some("candidate"), None)
        .unwrap();
    let mut record = record_at(Phase::CandidatePrepared);
    record.candidate.id = Some(candidate.id.into());
    record.creation_epoch = RuntimeEpoch::capture().unwrap();

    assert_eq!(
        inspect_database(&record, &database, None).unwrap(),
        DatabaseEvidence::Conflict(DatabaseConflict::InconsistentAuditOwnership {
            state: candidate.id,
            audit_present: false,
            ownership: db::state::TransitionOwnership::Matching,
        })
    );

    let temporary = private_installation_tempdir();
    let installation = Installation::open(temporary.path(), None).unwrap();
    let journal = TransitionJournalStore::open_retained(installation.root_directory(), &installation.root).unwrap();
    let initial_audit = database.audit_in_flight_transition().unwrap();
    let mutation_database = database.clone();
    let mutation_transition = transition_id();
    arm_between_database_inspections(move || {
        mutation_database
            .clear_transition_if_matches(candidate.id, &mutation_transition)
            .unwrap();
    });

    let pending = PendingSystemTransition::inspect(&installation, &database, journal, record, initial_audit).unwrap();

    assert!(matches!(
        pending.database_evidence(),
        DatabaseEvidence::CandidateOwnership {
            state,
            ownership: db::state::TransitionOwnership::Matching,
            ..
        } if *state == candidate.id
    ));
    assert!(matches!(
        pending.database_stability(),
        DatabaseInspectionStability::Changed {
            after: DatabaseEvidence::CandidateOwnership {
                state,
                ownership: db::state::TransitionOwnership::Cleared,
                ..
            }
        } if *state == candidate.id
    ));
    assert!(
        pending
            .blockers()
            .contains(&RecoveryBlocker::DatabaseChangedDuringInspection)
    );
}

#[test]
fn startup_reconciliation_current_and_historical_runtime_epochs_are_distinguished() {
    let mut record = creation_record();
    record.creation_epoch = epoch(1);

    let current = RuntimeEpochEvidence {
        before: Ok(epoch(1)),
        after: Ok(epoch(1)),
    };
    assert_eq!(current.comparability(&record), RuntimeEpochComparability::Current);

    let historical = RuntimeEpochEvidence {
        before: Ok(epoch(2)),
        after: Ok(epoch(2)),
    };
    assert_eq!(
        historical.comparability(&record),
        RuntimeEpochComparability::RecordedEpochChanged
    );

    let changed = RuntimeEpochEvidence {
        before: Ok(epoch(1)),
        after: Ok(epoch(2)),
    };
    assert_eq!(
        changed.comparability(&record),
        RuntimeEpochComparability::ChangedDuringInspection
    );

    let unavailable = RuntimeEpochEvidence {
        before: Err(RuntimeEvidenceError::TreeChanged),
        after: Ok(epoch(1)),
    };
    assert_eq!(
        unavailable.comparability(&record),
        RuntimeEpochComparability::Unavailable
    );
}

#[test]
fn startup_reconciliation_two_link_tree_marker_remains_unresolved() {
    let temporary = private_installation_tempdir();
    let tree = temporary.path().join("usr-tree");
    create_tree(&tree);
    let store = TreeMarkerStore::open_path(&tree).unwrap();
    let marker = store.adopt_or_create_before_journal().unwrap();
    let extra = temporary.path().join("state-slot-marker");
    fs::hard_link(tree.join(".cast-tree-id"), &extra).unwrap();
    drop(marker);
    drop(store);

    let evidence = inspect_known_tree(KnownTreeLocation {
        path: tree.clone(),
        roles: vec![KnownTreeRole::Live],
    });

    assert!(matches!(
        evidence,
        KnownTreeEvidence::Unresolved {
            retained: Some(_),
            reason: UnresolvedTreeReason::StateSlotLinkUnauthenticated,
            ..
        }
    ));
    let canonical = fs::metadata(tree.join(".cast-tree-id")).unwrap();
    let linked = fs::metadata(extra).unwrap();
    assert_eq!((canonical.dev(), canonical.ino()), (linked.dev(), linked.ino()));
    assert_eq!(canonical.nlink(), 2);
}

#[test]
fn startup_reconciliation_final_tree_name_substitution_is_not_retained() {
    let temporary = private_installation_tempdir();
    let tree = temporary.path().join("usr-tree");
    let parked = temporary.path().join("parked-tree");
    create_tree(&tree);
    let store = TreeMarkerStore::open_path(&tree).unwrap();
    drop(store.adopt_or_create_before_journal().unwrap());
    drop(store);

    let hook_tree = tree.clone();
    let hook_parked = parked.clone();
    arm_before_final_tree_reopen(move || {
        fs::rename(&hook_tree, &hook_parked).unwrap();
        create_tree(&hook_tree);
    });

    let evidence = inspect_known_tree(KnownTreeLocation {
        path: tree.clone(),
        roles: vec![KnownTreeRole::Live],
    });

    assert!(matches!(
        evidence,
        KnownTreeEvidence::Unresolved {
            retained: Some(_),
            reason: UnresolvedTreeReason::Rejected(TreeMarkerError::DirectoryChanged { path }),
            ..
        } if path == tree
    ));
    assert!(!tree.join(".cast-tree-id").exists());
    assert!(parked.join(".cast-tree-id").is_file());

    let marker_tree = temporary.path().join("marker-tree");
    create_tree(&marker_tree);
    let store = TreeMarkerStore::open_path(&marker_tree).unwrap();
    drop(store.adopt_or_create_before_journal().unwrap());
    drop(store);
    let canonical = marker_tree.join(".cast-tree-id");
    let displaced = marker_tree.join(".cast-tree-id.displaced");
    let replacement_canonical = canonical.clone();
    let replacement_displaced = displaced.clone();
    arm_before_final_tree_reopen(move || {
        let bytes = fs::read(&replacement_canonical).unwrap();
        fs::rename(&replacement_canonical, &replacement_displaced).unwrap();
        fs::write(&replacement_canonical, bytes).unwrap();
        fs::set_permissions(&replacement_canonical, fs::Permissions::from_mode(0o444)).unwrap();
    });

    let marker_evidence = inspect_known_tree(KnownTreeLocation {
        path: marker_tree,
        roles: vec![KnownTreeRole::Live],
    });

    assert!(matches!(
        marker_evidence,
        KnownTreeEvidence::Unresolved {
            retained: Some(_),
            reason: UnresolvedTreeReason::Rejected(TreeMarkerError::MarkerChanged { path }),
            ..
        } if path == canonical
    ));
    let replacement = fs::metadata(canonical).unwrap();
    let original = fs::metadata(displaced).unwrap();
    assert_ne!((replacement.dev(), replacement.ino()), (original.dev(), original.ino()));
}

#[test]
fn startup_reconciliation_retains_exact_database_instance() {
    let temporary = private_installation_tempdir();
    let installation = Installation::open(temporary.path(), None).unwrap();
    let database_path = installation.db_path("state");
    let database = db::state::Database::new(database_path.to_str().unwrap()).unwrap();
    let reopened = db::state::Database::new(database_path.to_str().unwrap()).unwrap();
    let journal = TransitionJournalStore::open_retained(installation.root_directory(), &installation.root).unwrap();
    let mut record = creation_record();
    record.creation_epoch = RuntimeEpoch::capture().unwrap();

    let pending = PendingSystemTransition::inspect(&installation, &database, journal, record, None).unwrap();

    assert!(pending.retains_database(&database));
    assert!(!pending.retains_database(&reopened));
}

#[test]
fn startup_reconciliation_pending_error_releases_journal_before_retry() {
    let temporary = private_installation_tempdir();
    let root = temporary.path().to_path_buf();
    let installation = Installation::open(&root, None).unwrap();
    let journal = TransitionJournalStore::open_retained(installation.root_directory(), &installation.root).unwrap();
    journal.create(&creation_record()).unwrap();
    drop(journal);

    // Consume the only Installation handle. The returned error must retain
    // neither its global lock nor the exclusive journal lock.
    let first = expect_recovery_pending(Client::builder("startup-reconciliation-first", installation).build());
    assert!(matches!(first.as_ref(), startup_gate::Error::RecoveryPending(_)));

    let (sender, receiver) = mpsc::sync_channel(1);
    let worker = thread::spawn(move || {
        let reopened = Installation::open(root, None).unwrap();
        let second = expect_recovery_pending(Client::builder("startup-reconciliation-second", reopened).build());
        sender
            .send(matches!(second.as_ref(), startup_gate::Error::RecoveryPending(_)))
            .unwrap();
    });

    assert_eq!(
        receiver.recv_timeout(Duration::from_secs(10)),
        Ok(true),
        "a live pending diagnostic retained startup mutation authority"
    );
    worker.join().unwrap();
    drop(first);
}
