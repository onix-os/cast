use std::{
    fs,
    io::Write as _,
    os::unix::{
        ffi::OsStrExt as _,
        fs::{MetadataExt as _, PermissionsExt as _, symlink},
    },
    process::Command,
    sync::{Arc, mpsc},
    thread,
    time::{Duration, Instant},
};

use super::*;

fn id() -> TransitionId {
    TransitionId::parse("0123456789abcdef0123456789abcdef").unwrap()
}

fn other_id() -> TransitionId {
    TransitionId::parse("11111111111111111111111111111111").unwrap()
}

fn boot_id() -> BootId {
    BootId::parse("01234567-89ab-4cde-8f01-23456789abcd").unwrap()
}

fn runtime_epoch() -> RuntimeEpoch {
    RuntimeEpoch {
        boot_id: boot_id(),
        mount_namespace: MountNamespaceIdentity { st_dev: 30, inode: 31 },
    }
}

fn tree_token(digit: char) -> TreeToken {
    TreeToken::parse(digit.to_string().repeat(TreeToken::TEXT_LENGTH)).unwrap()
}

fn identity(inode: u64) -> RuntimeTreeIdentity {
    RuntimeTreeIdentity {
        st_dev: 10,
        inode,
        mount_id: 12,
    }
}

fn new_state_record(phase: Phase) -> TransitionRecord {
    let forward = phase.forward().expect("new_state_record requires a forward phase");
    TransitionRecord {
        format: PAYLOAD_FORMAT.to_owned(),
        version: PAYLOAD_VERSION,
        generation: 7,
        transition_id: id(),
        creation_epoch: runtime_epoch(),
        operation: Operation::NewState,
        phase,
        rollback: None,
        candidate: Candidate {
            id: (!matches!(forward, ForwardPhase::Preparing | ForwardPhase::FreshStateAllocating)).then_some(42),
            origin: CandidateOrigin::Fresh,
            tree_token: tree_token('a'),
            usr_runtime_identity: identity(10),
        },
        previous: Previous {
            id: Some(41),
            tree_token: tree_token('b'),
            usr_runtime_identity: identity(20),
            origin: PreviousOrigin::ActiveState,
        },
        options: TransitionOptions {
            archive_previous: true,
            run_system_triggers: true,
            run_boot_sync: true,
        },
        quarantine_name: QuarantineName::parse("failed-0123456789abcdef").unwrap(),
    }
}

fn record(phase: Phase) -> TransitionRecord {
    if phase.forward().is_some() {
        new_state_record(phase)
    } else {
        valid_rollback_record(phase)
    }
}

pub(super) fn creation_record() -> TransitionRecord {
    TransitionRecord::preparing(
        id(),
        runtime_epoch(),
        Operation::NewState,
        None,
        tree_token('a'),
        identity(10),
        Previous {
            id: Some(41),
            tree_token: tree_token('b'),
            usr_runtime_identity: identity(20),
            origin: PreviousOrigin::ActiveState,
        },
        true,
        true,
        QuarantineName::parse("failed-0123456789abcdef").unwrap(),
    )
    .unwrap()
}

fn archived_record(phase: Phase) -> TransitionRecord {
    assert!(phase.forward().is_some());
    let mut record = new_state_record(phase);
    record.operation = Operation::ActivateArchived;
    record.candidate.origin = CandidateOrigin::Archived;
    record.candidate.id = Some(42);
    record
}

fn reblit_record(phase: Phase) -> TransitionRecord {
    assert!(phase.forward().is_some());
    let mut record = new_state_record(phase);
    record.operation = Operation::ActiveReblit;
    record.candidate.origin = CandidateOrigin::ActiveReblit;
    record.candidate.id = Some(42);
    record.previous.id = Some(42);
    record.previous.origin = PreviousOrigin::ActiveReblitCorrupt;
    record.options.archive_previous = false;
    record
}

fn without_previous_archive(mut record: TransitionRecord, origin: PreviousOrigin) -> TransitionRecord {
    assert!(matches!(
        origin,
        PreviousOrigin::SynthesizedEmpty | PreviousOrigin::Unmanaged
    ));
    record.previous.id = None;
    record.previous.origin = origin;
    record.options.archive_previous = false;
    record
}

fn rollback_decided(current: &TransitionRecord) -> TransitionRecord {
    let source = current.phase.forward().expect("rollback starts from a forward phase");
    let previous_possible =
        current.options.archive_previous && source.ordinal() >= ForwardPhase::PreviousArchiveIntent.ordinal();
    let usr_possible = source.ordinal() >= ForwardPhase::UsrExchangeIntent.ordinal();
    let fresh_possible = matches!(current.operation, Operation::NewState)
        && source.ordinal() >= ForwardPhase::FreshStateAllocating.ordinal();
    let boot_possible = source == ForwardPhase::BootSyncStarted;
    let external_effects_may_remain = (current.runs_transaction_triggers()
        && source.ordinal() >= ForwardPhase::TransactionTriggersStarted.ordinal())
        || (current.options.run_system_triggers
            && source.ordinal() >= ForwardPhase::SystemTriggersStarted.ordinal())
        || boot_possible;

    let mut next = current.clone();
    next.generation += 1;
    next.phase = Phase::RollbackDecided;
    if fresh_possible && next.candidate.id.is_none() {
        next.candidate.id = Some(42);
    }
    next.rollback = Some(RollbackPlan {
        source,
        previous_archive: if previous_possible {
            RollbackAction::Pending
        } else {
            RollbackAction::NotRequired
        },
        usr_exchange: if usr_possible {
            RollbackAction::Pending
        } else {
            RollbackAction::NotRequired
        },
        candidate: CandidateRollback {
            action: RollbackAction::Pending,
            disposition: current.candidate_disposition_for(source),
        },
        fresh_db: if fresh_possible {
            RollbackAction::Pending
        } else {
            RollbackAction::NotRequired
        },
        boot: if boot_possible {
            BootRollback::PendingUnverifiable
        } else {
            BootRollback::NotRequired
        },
        external_effects_may_remain,
    });
    next
}

fn advance_record(current: &TransitionRecord, phase: Phase) -> TransitionRecord {
    if phase == Phase::RollbackDecided {
        return rollback_decided(current);
    }

    let mut next = current.clone();
    next.generation += 1;
    next.phase = phase;
    if (current.phase, phase) == (Phase::FreshStateAllocating, Phase::FreshStateAllocated) {
        next.candidate.id = Some(42);
    }
    if let Some(plan) = next.rollback.as_mut() {
        match (current.phase, phase) {
            (Phase::PreviousRestoreIntent, Phase::PreviousRestoredToStaging) => {
                plan.previous_archive = RollbackAction::Applied;
            }
            (Phase::ReverseExchangeIntent, Phase::UsrRestored) => {
                plan.usr_exchange = RollbackAction::Applied;
            }
            (Phase::CandidatePreserveIntent, Phase::CandidatePreserved) => {
                plan.candidate.action = RollbackAction::Applied;
            }
            (Phase::FreshDbInvalidationIntent, Phase::FreshDbInvalidated) => {
                plan.fresh_db = RollbackAction::Applied;
            }
            (Phase::BootRepairStarted, Phase::BootRepairUnverified) => {
                plan.boot = BootRollback::Unverified;
            }
            _ => {}
        }
    }
    next
}

fn legal_forward_advance(current: &TransitionRecord) -> TransitionRecord {
    let current_phase = current.phase.forward().unwrap();
    let phase = next_forward_phase(current, current_phase).unwrap().into();
    advance_record(current, phase)
}

fn legal_rollback_advance(current: &TransitionRecord) -> TransitionRecord {
    let plan = current.rollback.as_ref().expect("rollback plan");
    let phase = next_rollback_phase(plan, current.phase).expect("nonterminal rollback successor");
    advance_record(current, phase)
}

fn rollback_sequence(source: &TransitionRecord) -> Vec<TransitionRecord> {
    let mut current = rollback_decided(source);
    let mut records = vec![current.clone()];
    while !current.phase.blocks_advance() {
        let next = legal_rollback_advance(&current);
        validate_advance(&current, &next).unwrap();
        records.push(next.clone());
        current = next;
    }
    records
}

fn valid_rollback_record(phase: Phase) -> TransitionRecord {
    let source = if matches!(
        phase,
        Phase::BootRepairRequired | Phase::BootRepairStarted | Phase::BootRepairUnverified
    ) {
        new_state_record(Phase::BootSyncStarted)
    } else {
        let mut source = new_state_record(Phase::PreviousArchiveIntent);
        source.options.run_boot_sync = false;
        source
    };
    rollback_sequence(&source)
        .into_iter()
        .find(|record| record.phase == phase)
        .expect("requested rollback phase is reachable")
}

fn satisfied_preparing_rollback(current: &TransitionRecord) -> TransitionRecord {
    let mut rollback = rollback_decided(current);
    rollback.rollback.as_mut().unwrap().candidate.action = RollbackAction::AlreadySatisfied;
    rollback
}

fn advance_to_complete(store: &TransitionJournalStore, mut current: TransitionRecord) -> TransitionRecord {
    while current.phase != Phase::Complete {
        let next = legal_forward_advance(&current);
        store.advance(&current, &next).unwrap();
        current = next;
    }
    current
}

fn frame_payload(payload: &[u8]) -> Vec<u8> {
    let length = u32::try_from(payload.len()).unwrap().to_be_bytes();
    let version = FRAME_VERSION.to_be_bytes();
    let checksum = checksum(&version, &length, payload);
    let mut framed = Vec::new();
    framed.extend_from_slice(MAGIC);
    framed.extend_from_slice(&version);
    framed.extend_from_slice(&length);
    framed.extend_from_slice(&checksum);
    framed.extend_from_slice(payload);
    framed
}

fn replace_payload(framed: &[u8], mutate: impl FnOnce(&str) -> String) -> Vec<u8> {
    let payload = std::str::from_utf8(&framed[HEADER_SIZE..]).unwrap();
    frame_payload(mutate(payload).as_bytes())
}

fn fixture() -> (tempfile::TempDir, TransitionJournalStore) {
    let temporary = tempfile::tempdir().unwrap();
    let cast = temporary.path().join(".cast");
    fs::create_dir(&cast).unwrap();
    fs::set_permissions(&cast, fs::Permissions::from_mode(0o700)).unwrap();
    let store = TransitionJournalStore::open(temporary.path()).unwrap();
    (temporary, store)
}

fn canonical(root: &Path) -> PathBuf {
    root.join(".cast/journal/state-transition")
}

fn stale_temporary_path(root: &Path, sequence: usize) -> PathBuf {
    root.join(".cast/journal").join(format!(
        ".state-transition.tmp-{:08x}-{sequence:016x}",
        std::process::id()
    ))
}

fn create_stale_temporaries(root: &Path, count: usize) {
    for sequence in 0..count {
        let path = stale_temporary_path(root, sequence);
        fs::write(&path, b"stale").unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }
}

fn assert_no_journal_temporaries(root: &Path) {
    assert!(
        fs::read_dir(root.join(".cast/journal"))
            .unwrap()
            .all(|entry| !valid_temporary_name(entry.unwrap().file_name().as_bytes()))
    );
}


include!("record_contract.rs");
include!("transition_semantics.rs");
include!("storage_transactions.rs");
include!("storage_resilience.rs");
