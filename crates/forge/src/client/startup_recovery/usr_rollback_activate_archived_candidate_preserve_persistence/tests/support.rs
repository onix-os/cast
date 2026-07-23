//! Shared fixtures for ActivateArchived candidate-preservation persistence.

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
            ArchivedCandidatePreservePostMoveDurabilityEvent, UsrRollbackArchivedCandidatePreserveApplyReconciliation,
            UsrRollbackArchivedCandidatePreserveDurableEffectAuthority, UsrRollbackCandidatePreserveAdmission,
            UsrRollbackCandidatePreserveApplyEffectSelection, UsrRollbackCandidatePreserveFinishDurabilitySelection,
        },
        startup_recovery::{
            UsrRollbackArchivedCandidatePreserveDurabilitySeal, UsrRollbackCandidatePreserveEffectSeal,
        },
    },
    transition_journal::{Phase, RollbackActionOutcome, TransitionJournalStore, TransitionRecord},
};

use super::super::candidate_test_support::{
    CandidateLayout, CandidatePreserveFixture, CandidateSource, archived_slot_path,
};
use super::super::test_fixture::OperationKind;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Epoch {
    Current,
    Historical,
}

impl Epoch {
    pub(super) const ALL: [Self; 2] = [Self::Current, Self::Historical];
}

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

    fn layout(self) -> CandidateLayout {
        match self {
            Self::Applied => CandidateLayout::Staged,
            Self::AlreadySatisfied => CandidateLayout::Preserved,
        }
    }
}

pub(super) fn fixture_for_origin(
    epoch: Epoch,
    origin: CandidateOrigin,
    source: CandidateSource,
    usr_outcome: RollbackActionOutcome,
) -> CandidatePreserveFixture {
    match epoch {
        Epoch::Current => CandidatePreserveFixture::new(OperationKind::Archived, source, usr_outcome, origin.layout()),
        Epoch::Historical => {
            CandidatePreserveFixture::historical(OperationKind::Archived, source, usr_outcome, origin.layout())
        }
    }
}

pub(super) fn durable_authority<'reservation>(
    fixture: &CandidatePreserveFixture,
    journal: &TransitionJournalStore,
    reservation: &'reservation ActiveStateReservation,
    origin: CandidateOrigin,
) -> UsrRollbackArchivedCandidatePreserveDurableEffectAuthority<'reservation> {
    let effect_seal = UsrRollbackCandidatePreserveEffectSeal::new_for_test();
    let durability_seal = UsrRollbackArchivedCandidatePreserveDurabilitySeal::new_for_test();
    match origin {
        CandidateOrigin::Applied => {
            let UsrRollbackCandidatePreserveAdmission::Apply(authority) = fixture.capture(journal, reservation) else {
                panic!("exact staged ActivateArchived evidence did not admit Apply")
            };
            let UsrRollbackCandidatePreserveApplyEffectSelection::MoveArchived(lease) =
                authority.into_effect_selection(&effect_seal, journal).unwrap()
            else {
                panic!("exact staged ActivateArchived evidence did not select child movement")
            };
            let UsrRollbackArchivedCandidatePreserveApplyReconciliation::Applied(authority) =
                lease.reconcile(&effect_seal, journal).unwrap()
            else {
                panic!("normal archived child movement did not reconcile Applied")
            };
            authority
                .complete_post_move_durability(&durability_seal, journal)
                .unwrap()
        }
        CandidateOrigin::AlreadySatisfied => {
            let UsrRollbackCandidatePreserveAdmission::Finish(authority) = fixture.capture(journal, reservation) else {
                panic!("exact preserved ActivateArchived evidence did not admit Finish")
            };
            let UsrRollbackCandidatePreserveFinishDurabilitySelection::Archived(authority) = authority
                .into_post_move_durability_selection(&effect_seal, journal)
                .unwrap()
            else {
                panic!("exact preserved ActivateArchived evidence did not select durability")
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
        .unwrap();
    assert_eq!(record.phase, Phase::CandidatePreserved);
    record
}

pub(super) fn target_path(fixture: &CandidatePreserveFixture) -> PathBuf {
    fixture
        .fixture
        .installation
        .root
        .join(".cast/root")
        .join(fixture.fixture.candidate_state.to_string())
}

pub(super) fn assert_preserved(fixture: &CandidatePreserveFixture) {
    let target = target_path(fixture);
    assert!(!fixture.fixture.installation.staging_dir().join("usr").exists());
    assert!(target.join("usr").is_dir());
    assert_eq!(fs::read_dir(&target).unwrap().count(), 2);
    assert_eq!(
        identity(target.join("usr/.cast-tree-id")),
        identity(archived_slot_path(&fixture.fixture, &fixture.candidate_intent)),
    );
}

pub(super) fn expected_post_events(
    fixture: &CandidatePreserveFixture,
) -> Vec<ArchivedCandidatePreservePostMoveDurabilityEvent> {
    let target = target_path(fixture);
    let candidate = identity(target.join("usr"));
    let staging = identity(fixture.fixture.installation.staging_dir());
    let target_parent = identity(&target);
    let roots = identity(fixture.fixture.installation.root.join(".cast/root"));
    vec![
        ArchivedCandidatePreservePostMoveDurabilityEvent::CandidateSynced {
            device: candidate.0,
            inode: candidate.1,
        },
        ArchivedCandidatePreservePostMoveDurabilityEvent::StagingParentSynced {
            device: staging.0,
            inode: staging.1,
        },
        ArchivedCandidatePreservePostMoveDurabilityEvent::TargetParentSynced {
            device: target_parent.0,
            inode: target_parent.1,
        },
        ArchivedCandidatePreservePostMoveDurabilityEvent::RootsParentSynced {
            device: roots.0,
            inode: roots.1,
        },
        ArchivedCandidatePreservePostMoveDurabilityEvent::FinalPostProven,
    ]
}

fn identity(path: impl AsRef<Path>) -> (u64, u64) {
    let metadata = fs::metadata(path).unwrap();
    (metadata.dev(), metadata.ino())
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

pub(super) fn non_journal_namespace_snapshot(
    fixture: &CandidatePreserveFixture,
) -> Vec<NonJournalNamespaceEntry> {
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
