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
        AbortDisposition, BootId, BootRepairOutcome, BootRollback, CandidateRollback, ForwardPhase,
        InitialRollbackAction, MountNamespaceIdentity, Operation, Phase, Previous, PreviousOrigin, QuarantineName,
        RollbackAction, RollbackActionOutcome, RollbackObservations, RollbackPlan, RuntimeEpoch, RuntimeEvidenceError,
        RuntimeTreeIdentity, TransitionJournalStore, TransitionRecord, TreeToken,
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

const ROLLBACK_CASES: [(Phase, RollbackAction, FreshDatabaseExpectation); 14] = [
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
        Phase::BootRepairComplete,
        RollbackAction::AlreadySatisfied,
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

const PROVENANCE_PHASE_CASES: [(Phase, ForwardPhase, bool, bool); 19] = [
    (Phase::Preparing, ForwardPhase::Preparing, true, false),
    (
        Phase::FreshStateAllocating,
        ForwardPhase::FreshStateAllocating,
        true,
        false,
    ),
    (
        Phase::FreshStateAllocated,
        ForwardPhase::FreshStateAllocated,
        true,
        false,
    ),
    (
        Phase::CandidatePrepareStarted,
        ForwardPhase::CandidatePrepareStarted,
        true,
        true,
    ),
    (Phase::CandidatePrepared, ForwardPhase::CandidatePrepared, false, true),
    (
        Phase::TransactionTriggersStarted,
        ForwardPhase::TransactionTriggersStarted,
        false,
        true,
    ),
    (
        Phase::TransactionTriggersComplete,
        ForwardPhase::TransactionTriggersComplete,
        false,
        true,
    ),
    (Phase::UsrExchangeIntent, ForwardPhase::UsrExchangeIntent, false, true),
    (Phase::UsrExchanged, ForwardPhase::UsrExchanged, false, true),
    (Phase::RootLinksComplete, ForwardPhase::RootLinksComplete, false, true),
    (
        Phase::SystemTriggersStarted,
        ForwardPhase::SystemTriggersStarted,
        false,
        true,
    ),
    (
        Phase::SystemTriggersComplete,
        ForwardPhase::SystemTriggersComplete,
        false,
        true,
    ),
    (
        Phase::PreviousArchiveIntent,
        ForwardPhase::PreviousArchiveIntent,
        false,
        true,
    ),
    (Phase::PreviousArchived, ForwardPhase::PreviousArchived, false, true),
    (Phase::BootSyncStarted, ForwardPhase::BootSyncStarted, false, true),
    (Phase::BootSyncComplete, ForwardPhase::BootSyncComplete, false, true),
    (Phase::CommitDecided, ForwardPhase::CommitDecided, false, true),
    (
        Phase::CommitCleanupComplete,
        ForwardPhase::CommitCleanupComplete,
        false,
        true,
    ),
    (Phase::Complete, ForwardPhase::Complete, false, true),
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

/// A minimal receipt pair for test records that reach `BootSyncStarted` or
/// later, where validation now requires `boot_publication_receipts` to be
/// present (the live path stages these via `boot_sync_started_successor`).
fn test_boot_publication_receipt_pair() -> crate::boot_publication::BootPublicationReceiptPair {
    crate::boot_publication::BootPublicationReceiptPair {
        committed: None,
        pending: crate::boot_publication::BootPublicationReceiptFingerprint::from_bytes([0x11; 32]),
    }
}

fn boot_repair_complete_database_record() -> TransitionRecord {
    let mut source = record_at(Phase::BootSyncStarted);
    // The record carries `run_boot_sync` and sits at `BootSyncStarted`, so the
    // receipt-presence invariant requires a committed/pending pair.
    source.boot_publication_receipts = Some(test_boot_publication_receipt_pair());
    let decided = source
        .rollback_decision(RollbackObservations {
            allocated_candidate_id: None,
            previous_archive: None,
            usr_exchange: Some(InitialRollbackAction::AlreadySatisfied),
            candidate: InitialRollbackAction::AlreadySatisfied,
            fresh_db: Some(InitialRollbackAction::AlreadySatisfied),
        })
        .unwrap();
    let required = decided.rollback_successor(None).unwrap();
    let started = required.boot_repair_started_successor().unwrap();
    started
        .boot_repair_complete_successor(BootRepairOutcome::Applied)
        .unwrap()
}

fn startup_metadata_provenance() -> db::state::MetadataProvenance {
    db::state::MetadataProvenance::from_outputs(b"NAME=startup\n", b"let startup = true\n")
}

fn candidate_evidence(ownership: db::state::TransitionOwnership) -> DatabaseEvidence {
    DatabaseEvidence::CandidateOwnership {
        state: state::Id::from(42),
        ownership,
        provenance: None,
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
                database_ownership_evidence_compatible(&record, &candidate_evidence(ownership)),
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
                    provenance: None,
                    previous: None,
                }
            ),
            phase == Phase::FreshStateAllocating,
            "allocation-behind-journal at {phase:?}"
        );
    }

    for (phase, action, expected) in ROLLBACK_CASES {
        let record = if phase == Phase::BootRepairComplete {
            let record = boot_repair_complete_database_record();
            assert_eq!(record.rollback.as_ref().unwrap().fresh_db, action);
            assert_eq!(record.rollback.as_ref().unwrap().boot, BootRollback::Applied);
            record
        } else {
            rollback_record(phase, action)
        };
        assert_eq!(fresh_database_expectation(&record), expected, "{phase:?}");
        for ownership in ownerships {
            assert_eq!(
                database_ownership_evidence_compatible(&record, &candidate_evidence(ownership)),
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
            provenance: Some(startup_metadata_provenance()),
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
            provenance: None,
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
fn startup_reconciliation_metadata_provenance_phase_matrix_is_fail_closed_and_sandwiched() {
    let exact = Some(startup_metadata_provenance());
    let evidence = |provenance| DatabaseEvidence::CandidateOwnership {
        state: state::Id::from(42),
        ownership: db::state::TransitionOwnership::Matching,
        provenance,
        previous: None,
    };
    let assert_admission = |record: &TransitionRecord, absent_allowed, present_allowed| {
        assert_eq!(
            metadata_provenance_evidence_compatible(record, &evidence(None)),
            absent_allowed,
            "absent provenance at {:?}",
            record.phase
        );
        assert_eq!(
            metadata_provenance_evidence_compatible(record, &evidence(exact)),
            present_allowed,
            "present provenance at {:?}",
            record.phase
        );
    };

    for (phase, source, absent_allowed, present_allowed) in PROVENANCE_PHASE_CASES {
        let record = record_at(phase);
        assert_admission(&record, absent_allowed, present_allowed);

        for action in [RollbackAction::Pending, RollbackAction::NotRequired] {
            let mut rollback = rollback_record(Phase::RollbackDecided, action);
            rollback.rollback.as_mut().unwrap().source = source;
            assert_admission(&rollback, absent_allowed, present_allowed);
        }
        for action in [RollbackAction::Applied, RollbackAction::AlreadySatisfied] {
            let mut rollback = rollback_record(Phase::RollbackDecided, action);
            rollback.rollback.as_mut().unwrap().source = source;
            assert_admission(&rollback, true, false);
        }
    }

    let mut active_reblit = record_at(Phase::Preparing);
    active_reblit.operation = Operation::ActiveReblit;
    let candidate = ExistingStateEvidence {
        state: state::Id::from(42),
        ownership: db::state::TransitionOwnership::Cleared,
    };
    assert!(!metadata_provenance_evidence_compatible(
        &active_reblit,
        &DatabaseEvidence::ExistingCandidate {
            candidate,
            provenance: None,
            previous: None,
        }
    ));
    assert!(metadata_provenance_evidence_compatible(
        &active_reblit,
        &DatabaseEvidence::ExistingCandidate {
            candidate,
            provenance: exact,
            previous: None,
        }
    ));

    let mut allocation_behind = record_at(Phase::FreshStateAllocating);
    allocation_behind.candidate.id = None;
    assert!(!metadata_provenance_evidence_compatible(
        &allocation_behind,
        &DatabaseEvidence::AllocationCommittedBehindJournal {
            state: state::Id::from(42),
            provenance: exact,
            previous: None,
        }
    ));
    assert!(!metadata_provenance_evidence_compatible(
        &record_at(Phase::CandidatePrepareStarted),
        &DatabaseEvidence::CandidateOwnership {
            state: state::Id::from(42),
            ownership: db::state::TransitionOwnership::Missing,
            provenance: exact,
            previous: None,
        }
    ));

    let legacy_temporary = private_installation_tempdir();
    let legacy_installation = Installation::open(legacy_temporary.path(), None).unwrap();
    let legacy_database =
        db::state::Database::new(legacy_installation.db_path("legacy-provenance").to_str().unwrap()).unwrap();
    let legacy_candidate = legacy_database
        .add_with_transition(&transition_id(), &[], Some("legacy candidate"), None)
        .unwrap();
    let mut legacy_record = record_at(Phase::CandidatePrepared);
    legacy_record.candidate.id = Some(legacy_candidate.id.into());
    legacy_record.creation_epoch = RuntimeEpoch::capture().unwrap();
    let legacy_journal =
        TransitionJournalStore::open_retained(legacy_installation.root_directory(), &legacy_installation.root).unwrap();
    let legacy_audit = legacy_database.audit_in_flight_transition().unwrap();
    let legacy_pending = PendingSystemTransition::inspect(
        &legacy_installation,
        &legacy_database,
        legacy_journal,
        legacy_record,
        legacy_audit,
    )
    .unwrap();
    assert!(matches!(
        legacy_pending.database_evidence(),
        DatabaseEvidence::CandidateOwnership { provenance: None, .. }
    ));
    assert!(
        legacy_pending
            .blockers()
            .contains(&RecoveryBlocker::MetadataProvenanceConflict)
    );
    assert!(
        !legacy_pending
            .blockers()
            .contains(&RecoveryBlocker::DatabaseChangedDuringInspection)
    );

    let temporary = private_installation_tempdir();
    let installation = Installation::open(temporary.path(), None).unwrap();
    let database = db::state::Database::new(installation.db_path("startup-provenance").to_str().unwrap()).unwrap();
    let candidate = database
        .add_with_transition(&transition_id(), &[], Some("startup provenance candidate"), None)
        .unwrap();
    database
        .insert_fresh_metadata_provenance_if_transition_matches(
            candidate.id,
            &transition_id(),
            &startup_metadata_provenance(),
        )
        .unwrap();
    let mut record = record_at(Phase::CandidatePrepared);
    record.candidate.id = Some(candidate.id.into());
    record.creation_epoch = RuntimeEpoch::capture().unwrap();
    let journal = TransitionJournalStore::open_retained(installation.root_directory(), &installation.root).unwrap();
    let initial_audit = database.audit_in_flight_transition().unwrap();
    let mutation_database = database.clone();
    arm_between_database_inspections(move || {
        mutation_database
            .delete_metadata_provenance_for_test(candidate.id)
            .unwrap();
    });

    let pending = PendingSystemTransition::inspect(&installation, &database, journal, record, initial_audit).unwrap();
    assert!(matches!(
        pending.database_evidence(),
        DatabaseEvidence::CandidateOwnership {
            provenance: Some(_),
            ..
        }
    ));
    assert!(matches!(
        pending.database_stability(),
        DatabaseInspectionStability::Changed {
            after: DatabaseEvidence::CandidateOwnership { provenance: None, .. }
        }
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
        receiver.recv_timeout(Duration::from_secs(120)),
        Ok(true),
        "a live pending diagnostic retained startup mutation authority"
    );
    worker.join().unwrap();
    drop(first);
}

/// Every forward phase a rollback can be entered from before `/usr` is touched.
///
/// This deliberately covers the whole pre-exchange span, not just the three
/// phases the admission lists used to name. Restricting it to those three is
/// what let a crash while preparing a candidate or allocating the fresh state
/// go unnoticed: the earlier phases were never exercised, so nothing observed
/// that they stranded.
const PRE_EXCHANGE_ROLLBACK_SOURCES: [Phase; 7] = [
    Phase::Preparing,
    Phase::FreshStateAllocating,
    Phase::FreshStateAllocated,
    Phase::CandidatePrepareStarted,
    Phase::CandidatePrepared,
    Phase::TransactionTriggersStarted,
    Phase::TransactionTriggersComplete,
];

fn forward_record_for(operation: Operation, phase: Phase) -> TransitionRecord {
    // Each operation has its own legal candidate/previous shape. Only
    // `NewState` allocates its candidate during the transition; the other two
    // adopt an existing state and must name it up front, and `ActiveReblit`
    // reblits the active state onto itself.
    let (candidate_id, previous_id, previous_origin) = match operation {
        Operation::NewState => (None, None, PreviousOrigin::SynthesizedEmpty),
        Operation::ActivateArchived => (Some(42), Some(7), PreviousOrigin::ActiveState),
        Operation::ActiveReblit => (Some(42), Some(42), PreviousOrigin::ActiveReblitCorrupt),
    };
    let mut record = TransitionRecord::preparing(
        transition_id(),
        epoch(1),
        operation,
        candidate_id,
        tree_token('a'),
        runtime_tree(10),
        Previous {
            id: previous_id,
            tree_token: tree_token('b'),
            usr_runtime_identity: runtime_tree(20),
            origin: previous_origin,
        },
        true,
        true,
        QuarantineName::parse("failed-startup-reconciliation").unwrap(),
    )
    .unwrap();
    record.phase = phase;
    // NewState allocates its candidate mid-transition, so the ID is present
    // exactly from `FreshStateAllocated` on. Forcing it everywhere made records
    // the model rejects, which silently skipped the early phases.
    record.candidate.id = match operation {
        Operation::NewState => phase
            .forward()
            .filter(|forward| forward.ordinal() >= ForwardPhase::FreshStateAllocated.ordinal())
            .map(|_| 42),
        Operation::ActivateArchived | Operation::ActiveReblit => candidate_id,
    };
    record
}

/// The rollback chain has four ordinary actions plus a terminal phase, so no
/// legal walk is longer than this. The bound makes a non-advancing chain fail
/// the test instead of hanging it.
const MAX_ROLLBACK_CHAIN_STEPS: usize = 12;

/// Completing a persisted intent requires an explicit outcome; the routing
/// phases between them accept none.
fn rollback_outcome_for(phase: Phase) -> Option<RollbackActionOutcome> {
    matches!(
        phase,
        Phase::PreviousRestoreIntent
            | Phase::ReverseExchangeIntent
            | Phase::CandidatePreserveIntent
            | Phase::FreshDbInvalidationIntent
    )
    .then_some(RollbackActionOutcome::Applied)
}

/// A record sitting at `phase`, or `None` when that phase is not on this
/// operation's forward chain.
///
/// The generation is taken from `expected_forward_generation` rather than left
/// at its constructed value, because several admission gates still match on
/// `(operation, phase, generation)` tuples. A fixture with the wrong generation
/// is refused for a reason that has nothing to do with what is being tested.
fn chain_record_for(operation: Operation, phase: Phase) -> Option<TransitionRecord> {
    let forward = phase.forward()?;
    let mut record = forward_record_for(operation, phase);
    record.generation = crate::transition_journal::expected_forward_generation(&record, forward)?;
    Some(record)
}

/// Every phase startup would begin a rollback from must have an authority that
/// accepts that decision.
///
/// This is the contract `admitted_rollback_resume_routes_always_have_a_consuming_successor`
/// cannot see, because that test starts from the decision gate's own list and
/// so can never notice a phase the gate omits. This one starts from
/// `recovery_disposition`, which is what startup actually consults. If it says
/// `BeginRollback` and no authority admits the source, the boot stalls with the
/// journal pinned — the §1.4 failure, which has now recurred twice.
#[test]
fn every_begin_rollback_phase_has_an_admitting_decision_authority() {
    let mut stranded = Vec::new();
    let mut checked = 0_usize;
    for operation in [
        Operation::NewState,
        Operation::ActivateArchived,
        Operation::ActiveReblit,
    ] {
        for phase in FORWARD_PHASES {
            let Some(record) = chain_record_for(operation, phase) else {
                continue;
            };
            if !matches!(
                record.recovery_disposition(),
                crate::transition_journal::RecoveryDisposition::BeginRollback { .. }
            ) {
                continue;
            }
            checked += 1;
            if !usr_rollback_decision_source_is_supported_for_test(&record) {
                stranded.push((operation, phase));
            }
        }
    }
    assert!(checked > 0, "no BeginRollback phase was exercised");
    // These are known, unfixed stalls, pinned deliberately rather than asserted
    // empty. Every pair here is a phase where startup decides to roll back and
    // no authority admits the decision, so the boot stalls with the journal
    // pinned — the same failure that was just fixed for the pre-exchange
    // sources, measured 2026-07-30.
    //
    // ActivateArchived is the worst of them: it has no post-exchange rollback
    // admission at all beyond `RootLinksComplete`, because the decision gate's
    // `(operation, phase, generation)` table contains only NewState and
    // ActiveReblit rows.
    //
    // Pinned instead of emptied because closing fourteen admission gates at
    // once, with no crash-matrix cell behind any of them, is precisely the
    // over-widening that produced the original defect. Fix them deliberately,
    // per operation, and shorten this list as each one is measured — the test
    // fails if a new stall appears *or* if one is fixed without updating here.
    //
    // Not covered: `ArchivedCandidateStagingIntent` / `ArchivedCandidateStaged`
    // are absent from `FORWARD_PHASES`, so this list is a lower bound.
    // Every remaining entry is post-exchange, where recovery has real work to
    // undo — reverse the exchange, restore the previous state, repair boot.
    // The eight pre-exchange stalls that were here on 2026-07-30 are fixed:
    // nothing in `/usr` had been touched at those phases, so admitting them
    // only required the head and tail to agree.
    //
    // These six are a different problem and must not be closed the same way.
    // ActivateArchived has no post-exchange rollback admission at all past
    // `RootLinksComplete`, so a power cut while activation runs its system
    // triggers, archives the previous state, or syncs boot is unrecoverable.
    // Fix them per operation behind a crash-matrix cell that proves the effect
    // actually runs, not by widening a predicate.
    let known_unadmitted = vec![
        (Operation::NewState, Phase::BootSyncStarted),
        (Operation::ActivateArchived, Phase::SystemTriggersStarted),
        (Operation::ActivateArchived, Phase::SystemTriggersComplete),
        (Operation::ActivateArchived, Phase::PreviousArchiveIntent),
        (Operation::ActivateArchived, Phase::PreviousArchived),
        (Operation::ActivateArchived, Phase::BootSyncStarted),
    ];
    assert_eq!(
        stranded, known_unadmitted,
        "the set of phases that strand a rollback decision changed; \
         shorten this list when one is fixed, and investigate any addition"
    );
}

fn pre_exchange_rollback_decision(operation: Operation, source: Phase) -> Option<TransitionRecord> {
    chain_record_for(operation, source)?
        .rollback_decision(RollbackObservations {
            allocated_candidate_id: None,
            previous_archive: None,
            usr_exchange: None,
            candidate: InitialRollbackAction::Pending,
            // Mirrors the journal's own `fresh_possible` rule. Supplying an
            // observation the source phase makes impossible is rejected, which
            // would skip the case instead of testing it.
            fresh_db: (operation == Operation::NewState
                && source
                    .forward()
                    .is_some_and(|f| f.ordinal() >= ForwardPhase::FreshStateAllocating.ordinal()))
            .then_some(InitialRollbackAction::Pending),
        })
        .ok()
}

/// The rollback-resume route must never advance a record into a phase whose own
/// authority refuses it.
///
/// These are two independent copies of the same admission condition, and they
/// disagreed. The resume route accepted the pre-exchange sources for *every*
/// operation, while candidate preservation accepted them for `NewState` only.
/// An ActivateArchived pre-exchange rollback therefore advanced exactly once,
/// `RollbackDecided -> CandidatePreserveIntent`, and was then refused forever:
/// startup demanded `ResumeRollback { CandidatePreserveIntent }` on every boot
/// and the machine never recovered. Measured in the crash matrix on 2026-07-30
/// as 26 consecutive attempts with the phase never moving and `state=absent`.
#[test]
fn admitted_rollback_resume_routes_always_have_a_consuming_successor() {
    let mut stranded = Vec::new();
    let mut checked = Vec::new();
    for operation in [
        Operation::NewState,
        Operation::ActivateArchived,
        Operation::ActiveReblit,
    ] {
        for source in PRE_EXCHANGE_ROLLBACK_SOURCES {
            // Not every source is on every operation's chain: only NewState
            // and ActiveReblit run transaction triggers. A source the journal
            // refuses to build is not a gap.
            let Some(decided) = pre_exchange_rollback_decision(operation, source) else {
                continue;
            };
            // Pre-exchange by construction, so `/usr` is still in its original
            // layout. This is the fact the crashed machine presents at boot.
            if !usr_rollback_resume_route_plan_is_exact_for_test(&decided, false) {
                stranded.push((operation, source, Phase::RollbackDecided));
                continue;
            }
            checked.push((operation, source));
            // Walk the whole chain, not just the first step. Checking only the
            // first successor hid a stall two phases deeper: a rollback that
            // began before the transaction triggers reached
            // `FreshDbInvalidationIntent` and had no route out, because that
            // gate asserted `external_effects_may_remain` rather than deriving
            // it. A chain is only recoverable if *every* phase on it is.
            let mut current = decided;
            for _ in 0..MAX_ROLLBACK_CHAIN_STEPS {
                let Ok(successor) = current.rollback_successor(rollback_outcome_for(current.phase)) else {
                    break;
                };
                let consumed = match successor.phase {
                    Phase::CandidatePreserveIntent => {
                        usr_rollback_candidate_preserve_plan_is_exact_for_test(&successor)
                    }
                    Phase::ReverseExchangeIntent => usr_rollback_reverse_plan_is_exact_for_test(&successor),
                    Phase::FreshDbInvalidationIntent => {
                        usr_rollback_fresh_db_invalidation_plan_is_exact_for_test(&successor)
                    }
                    // The terminal phases, gated for real. Waving these through
                    // as `_ => true` is what let the chain reach
                    // `RollbackComplete` and stall on `FinalizeRollback` — the
                    // rollback started, advanced three phases, and still never
                    // finished, so the machine never recovered. Measured in the
                    // VM 2026-07-31, invisible to a green suite.
                    // `rollback_complete_route_plan_is_exact` gates
                    // `FreshDbInvalidated`, not this phase — using it here was
                    // simply the wrong predicate and produced a false stall.
                    // Which authority carries NewState from `CandidatePreserved`
                    // straight to `RollbackComplete` when no fresh row was ever
                    // allocated is still unidentified; see the plan.
                    Phase::FreshDbInvalidated if operation == Operation::NewState => {
                        usr_rollback_complete_route_plan_is_exact_for_test(&successor)
                    }
                    Phase::RollbackComplete if operation == Operation::NewState => {
                        usr_rollback_finalization_plan_is_exact_for_test(&successor)
                    }
                    // The ActivateArchived and ActiveReblit terminal gates are
                    // not exported yet; their chains are covered by the
                    // operation-specific startup-gate suites.
                    _ => true,
                };
                if !consumed {
                    stranded.push((operation, source, successor.phase));
                    break;
                }
                if successor.phase == Phase::RollbackComplete {
                    break;
                }
                current = successor;
            }
        }
    }
    // Guard against the failure mode this test already had once: both gates
    // above `continue`, so a predicate that refuses everything makes the
    // assertion below vacuously true. Naming the expected coverage means a
    // silent skip fails instead of passing.
    assert_eq!(
        checked,
        vec![
            (Operation::NewState, Phase::Preparing),
            (Operation::NewState, Phase::FreshStateAllocated),
            (Operation::NewState, Phase::CandidatePrepareStarted),
            (Operation::NewState, Phase::CandidatePrepared),
            (Operation::NewState, Phase::TransactionTriggersStarted),
            (Operation::NewState, Phase::TransactionTriggersComplete),
            (Operation::ActivateArchived, Phase::Preparing),
            (Operation::ActivateArchived, Phase::CandidatePrepareStarted),
            (Operation::ActivateArchived, Phase::CandidatePrepared),
            (Operation::ActiveReblit, Phase::Preparing),
            (Operation::ActiveReblit, Phase::CandidatePrepareStarted),
            (Operation::ActiveReblit, Phase::CandidatePrepared),
            (Operation::ActiveReblit, Phase::TransactionTriggersStarted),
            (Operation::ActiveReblit, Phase::TransactionTriggersComplete),
        ],
        "the pre-exchange rollback cases this test claims to cover were skipped"
    );
    // `(NewState, FreshStateAllocating)` is absent on purpose and is NOT proven
    // here: mid-allocation, the fixed `candidate: Pending` / `fresh_db: Pending`
    // observations this helper supplies are a combination the journal rejects,
    // because the row may or may not exist yet. Production observes the real
    // state. The decision gate admits it (see the characterization test), but
    // whether its chain is consumable needs an observation-aware fixture.
    // Known terminal stalls, pinned for the same reason as the decision-gate
    // list: these are real, they brick the machine, and closing them blind is
    // how the last one got made. The chain *starts* and advances, then dies at
    // the end — `RollbackComplete` cannot finalize, so recovery never completes
    // and every later boot fails the startup baseline. Measured in the VM
    // 2026-07-31 and reproduced here.
    //
    // Deriving `external_effects_may_remain` in the five terminal gates fixed
    // the transaction-trigger sources, which now walk the whole chain. What
    // remains is a further pre-exchange assumption inside
    // `rollback_finalization_plan_is_exact` / `rollback_complete_route_plan_is_exact`
    // — find it the same way, by asking which field the plan legitimately
    // carries at an early source that the gate insists on seeing differently.
    // One left. `rollback_finalization_plan_is_exact` requires
    // `record.candidate.id.is_some()`, but a NewState rollback begun at
    // `Preparing` never allocated a candidate, so the field is legitimately
    // `None` and finalization refuses forever. The chain does everything
    // asked of it and then cannot finish.
    //
    // Decide deliberately which is true before changing it: either the
    // decision authority always observes an allocated ID by the time it
    // persists (in which case this fixture is unrepresentative and should
    // supply one), or the finalization gate must accept an absent candidate
    // for sources below `FreshStateAllocating`. Do not just drop the check.
    let known_terminal_stalls = vec![(Operation::NewState, Phase::Preparing, Phase::RollbackComplete)];
    assert_eq!(
        stranded, known_terminal_stalls,
        "the set of rollback chains that cannot reach a terminal phase changed; \
         shorten this list when one is fixed, and investigate any addition"
    );
}
