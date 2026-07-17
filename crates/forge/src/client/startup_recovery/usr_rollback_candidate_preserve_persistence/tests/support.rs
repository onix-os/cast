use std::{
    ffi::OsString,
    fs,
    os::unix::fs::{MetadataExt as _, PermissionsExt as _},
    path::{Path, PathBuf},
};

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_reconciliation::{
            NewStateCandidatePreservePostMoveDurabilityEvent, UsrRollbackCandidatePreserveAdmission,
            UsrRollbackCandidatePreserveApplyEffectSelection, UsrRollbackCandidatePreserveFinishDurabilitySelection,
            UsrRollbackNewStateCandidatePreserveApplyReconciliation,
            UsrRollbackNewStateCandidatePreserveDurableEffectAuthority,
        },
        startup_recovery::{UsrRollbackCandidatePreserveDurabilitySeal, UsrRollbackCandidatePreserveEffectSeal},
    },
    transition_journal::{Phase, RollbackActionOutcome, TransitionJournalStore, TransitionRecord},
};

use super::super::candidate_test_support::{
    CandidateLayout, CandidatePreserveFixture, CandidateSource, transition_quarantine_path,
};

pub(super) use super::super::candidate_test_support::{
    CandidatePreserveFixture as Fixture, CandidateSource as Source, capture_record,
};
pub(super) use super::super::test_fixture::OperationKind;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CandidateOrigin {
    Applied,
    AlreadySatisfied,
}

impl CandidateOrigin {
    pub(super) const ALL: [Self; 2] = [Self::Applied, Self::AlreadySatisfied];

    pub(super) fn outcome(self) -> RollbackActionOutcome {
        match self {
            Self::Applied => RollbackActionOutcome::Applied,
            Self::AlreadySatisfied => RollbackActionOutcome::AlreadySatisfied,
        }
    }
}

pub(super) fn fixture_for_origin(
    origin: CandidateOrigin,
    source: CandidateSource,
    usr_outcome: RollbackActionOutcome,
) -> CandidatePreserveFixture {
    match origin {
        CandidateOrigin::Applied => CandidatePreserveFixture::new_state_empty_quarantine_prefix(source, usr_outcome),
        CandidateOrigin::AlreadySatisfied => {
            CandidatePreserveFixture::new(OperationKind::NewState, source, usr_outcome, CandidateLayout::Preserved)
        }
    }
}

pub(super) fn durable_authority<'reservation>(
    fixture: &CandidatePreserveFixture,
    journal: &TransitionJournalStore,
    reservation: &'reservation ActiveStateReservation,
    origin: CandidateOrigin,
) -> UsrRollbackNewStateCandidatePreserveDurableEffectAuthority<'reservation> {
    let effect_seal = UsrRollbackCandidatePreserveEffectSeal::new_for_test();
    let durability_seal = UsrRollbackCandidatePreserveDurabilitySeal::new_for_test();
    match origin {
        CandidateOrigin::Applied => {
            let UsrRollbackCandidatePreserveAdmission::Apply(authority) = fixture.capture(journal, reservation) else {
                panic!("exact NewState move prefix did not admit Apply authority");
            };
            let UsrRollbackCandidatePreserveApplyEffectSelection::MoveNewState(lease) =
                authority.into_effect_selection(&effect_seal, journal).unwrap()
            else {
                panic!("exact NewState move prefix did not select the move lease");
            };
            let UsrRollbackNewStateCandidatePreserveApplyReconciliation::Applied(authority) =
                lease.reconcile(&effect_seal, journal).unwrap()
            else {
                panic!("normal NewState candidate move did not reconcile as Applied");
            };
            authority
                .complete_post_move_durability(&durability_seal, journal)
                .unwrap()
        }
        CandidateOrigin::AlreadySatisfied => {
            let UsrRollbackCandidatePreserveAdmission::Finish(authority) = fixture.capture(journal, reservation) else {
                panic!("exact preserved NewState POST did not admit Finish authority");
            };
            let UsrRollbackCandidatePreserveFinishDurabilitySelection::NewState(authority) = authority
                .into_post_move_durability_selection(&effect_seal, journal)
                .unwrap()
            else {
                panic!("exact preserved NewState POST did not select durability");
            };
            authority
                .complete_post_move_durability(&durability_seal, journal)
                .unwrap()
        }
    }
}

pub(super) fn expected_candidate_preserved(
    fixture: &CandidatePreserveFixture,
    origin: CandidateOrigin,
) -> TransitionRecord {
    let record = fixture
        .candidate_intent
        .rollback_successor(Some(origin.outcome()))
        .expect("candidate fixture must admit its authority-fixed successor");
    assert_eq!(record.phase, Phase::CandidatePreserved);
    record
}

pub(super) fn expected_post_events(
    fixture: &CandidatePreserveFixture,
) -> Vec<NewStateCandidatePreservePostMoveDurabilityEvent> {
    let target = transition_quarantine_path(&fixture.fixture, &fixture.candidate_intent);
    let candidate = identity(&target.join("usr"));
    let staging = identity(&fixture.fixture.installation.staging_dir());
    let target = identity(&target);
    let quarantine = identity(&fixture.fixture.installation.state_quarantine_dir());
    vec![
        NewStateCandidatePreservePostMoveDurabilityEvent::CandidateSynced {
            device: candidate.0,
            inode: candidate.1,
        },
        NewStateCandidatePreservePostMoveDurabilityEvent::StagingParentSynced {
            device: staging.0,
            inode: staging.1,
        },
        NewStateCandidatePreservePostMoveDurabilityEvent::TargetParentSynced {
            device: target.0,
            inode: target.1,
        },
        NewStateCandidatePreservePostMoveDurabilityEvent::QuarantineParentSynced {
            device: quarantine.0,
            inode: quarantine.1,
        },
        NewStateCandidatePreservePostMoveDurabilityEvent::FinalPostProven,
    ]
}

#[derive(Debug, Eq, PartialEq)]
pub(super) struct NonJournalNamespaceEntry {
    relative: PathBuf,
    kind: &'static str,
    device: u64,
    inode: u64,
    mode: u32,
    links: u64,
    length: u64,
    payload: Vec<u8>,
}

pub(super) fn non_journal_namespace_snapshot(fixture: &CandidatePreserveFixture) -> Vec<NonJournalNamespaceEntry> {
    let root = &fixture.fixture.installation.root;
    let mut entries = Vec::new();
    snapshot_non_journal(root, root, &mut entries);
    entries
}

fn snapshot_non_journal(root: &Path, path: &Path, entries: &mut Vec<NonJournalNamespaceEntry>) {
    let relative = path.strip_prefix(root).unwrap();
    if relative == Path::new(".cast/journal") || relative.starts_with(".cast/journal/") {
        return;
    }
    let metadata = fs::symlink_metadata(path).unwrap();
    let file_type = metadata.file_type();
    let (kind, payload) = if file_type.is_dir() {
        ("directory", Vec::new())
    } else if file_type.is_symlink() {
        (
            "symlink",
            fs::read_link(path).unwrap().as_os_str().as_encoded_bytes().to_vec(),
        )
    } else {
        ("file", fs::read(path).unwrap())
    };
    entries.push(NonJournalNamespaceEntry {
        relative: relative.to_owned(),
        kind,
        device: metadata.dev(),
        inode: metadata.ino(),
        mode: metadata.permissions().mode(),
        links: metadata.nlink(),
        length: metadata.len(),
        payload,
    });
    if file_type.is_dir() {
        let mut children = fs::read_dir(path)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<OsString>>();
        children.sort();
        for child in children {
            snapshot_non_journal(root, &path.join(child), entries);
        }
    }
}

fn identity(path: &Path) -> (u64, u64) {
    let metadata = fs::metadata(path).unwrap();
    (metadata.dev(), metadata.ino())
}
