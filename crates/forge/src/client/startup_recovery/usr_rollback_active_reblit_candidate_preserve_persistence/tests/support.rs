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
            ActiveReblitCandidatePreservePostExchangeDurabilityEvent,
            UsrRollbackActiveReblitCandidatePreserveApplyReconciliation,
            UsrRollbackActiveReblitCandidatePreserveDurableEffectAuthority, UsrRollbackCandidatePreserveAdmission,
            UsrRollbackCandidatePreserveApplyEffectSelection, UsrRollbackCandidatePreserveFinishDurabilitySelection,
        },
        startup_recovery::{
            UsrRollbackActiveReblitCandidatePreserveDurabilitySeal, UsrRollbackCandidatePreserveEffectSeal,
        },
    },
    transition_journal::{Phase, RollbackActionOutcome, TransitionJournalStore, TransitionRecord},
};

use super::super::candidate_test_support::{
    CandidateLayout, CandidatePreserveFixture, CandidateSource, active_reblit_wrapper_path,
};

pub(super) use super::super::candidate_test_support::{
    CandidatePreserveFixture as Fixture, capture_record,
};
pub(super) use super::super::test_fixture::OperationKind;

pub(super) const WRAPPER_INDEX: usize = 13;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Epoch {
    Current,
    Historical,
}

impl Epoch {
    pub(super) const ALL: [Self; 2] = [Self::Current, Self::Historical];
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Source {
    Intent,
    Exchanged,
    BootSyncStarted,
}

impl Source {
    pub(super) const ALL: [Self; 3] = [Self::Intent, Self::Exchanged, Self::BootSyncStarted];
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
    source: Source,
    usr_outcome: RollbackActionOutcome,
) -> CandidatePreserveFixture {
    let fixture = match (epoch, source) {
        (Epoch::Current, Source::Intent) => CandidatePreserveFixture::new(
            OperationKind::ActiveReblit,
            CandidateSource::Intent,
            usr_outcome,
            origin.layout(),
        ),
        (Epoch::Current, Source::Exchanged) => CandidatePreserveFixture::new(
            OperationKind::ActiveReblit,
            CandidateSource::Exchanged,
            usr_outcome,
            origin.layout(),
        ),
        (Epoch::Historical, Source::Intent) => CandidatePreserveFixture::historical(
            OperationKind::ActiveReblit,
            CandidateSource::Intent,
            usr_outcome,
            origin.layout(),
        ),
        (Epoch::Historical, Source::Exchanged) => CandidatePreserveFixture::historical(
            OperationKind::ActiveReblit,
            CandidateSource::Exchanged,
            usr_outcome,
            origin.layout(),
        ),
        (epoch, Source::BootSyncStarted) => CandidatePreserveFixture::active_reblit_boot_sync_started(
            epoch == Epoch::Historical,
            usr_outcome,
            origin.layout(),
        ),
    };
    fixture.with_active_reblit_wrapper_index(WRAPPER_INDEX)
}

pub(super) fn durable_authority<'reservation>(
    fixture: &CandidatePreserveFixture,
    journal: &TransitionJournalStore,
    reservation: &'reservation ActiveStateReservation,
    origin: CandidateOrigin,
) -> UsrRollbackActiveReblitCandidatePreserveDurableEffectAuthority<'reservation> {
    let effect_seal = UsrRollbackCandidatePreserveEffectSeal::new_for_test();
    let durability_seal = UsrRollbackActiveReblitCandidatePreserveDurabilitySeal::new_for_test();
    match origin {
        CandidateOrigin::Applied => {
            let UsrRollbackCandidatePreserveAdmission::Apply(authority) = fixture.capture(journal, reservation) else {
                panic!("exact staged ActiveReblit evidence did not admit Apply authority");
            };
            let UsrRollbackCandidatePreserveApplyEffectSelection::ExchangeActiveReblit(lease) =
                authority.into_effect_selection(&effect_seal, journal).unwrap()
            else {
                panic!("exact staged ActiveReblit evidence did not select exchange");
            };
            let UsrRollbackActiveReblitCandidatePreserveApplyReconciliation::Applied(authority) =
                lease.reconcile(&effect_seal, journal).unwrap()
            else {
                panic!("normal ActiveReblit wrapper exchange did not reconcile as Applied");
            };
            authority
                .complete_post_exchange_durability(&durability_seal, journal)
                .unwrap()
        }
        CandidateOrigin::AlreadySatisfied => {
            let UsrRollbackCandidatePreserveAdmission::Finish(authority) = fixture.capture(journal, reservation) else {
                panic!("exact preserved ActiveReblit POST did not admit Finish authority");
            };
            let UsrRollbackCandidatePreserveFinishDurabilitySelection::ActiveReblit(authority) = authority
                .into_post_move_durability_selection(&effect_seal, journal)
                .unwrap()
            else {
                panic!("exact preserved ActiveReblit evidence did not select ActiveReblit durability");
            };
            authority
                .complete_post_exchange_durability(&durability_seal, journal)
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
        .expect("ActiveReblit fixture must admit its authority-fixed successor");
    assert_eq!(record.phase, Phase::CandidatePreserved);
    record
}

pub(super) fn expected_post_events(
    fixture: &CandidatePreserveFixture,
) -> Vec<ActiveReblitCandidatePreservePostExchangeDurabilityEvent> {
    let target = active_reblit_wrapper_path(&fixture.fixture, &fixture.candidate_intent, WRAPPER_INDEX);
    let candidate = identity(&target.join("usr"));
    let candidate_wrapper = identity(&target);
    let reservation_wrapper = identity(&fixture.fixture.installation.staging_dir());
    let roots = identity(&fixture.fixture.installation.root.join(".cast/root"));
    let quarantine = identity(&fixture.fixture.installation.state_quarantine_dir());
    vec![
        ActiveReblitCandidatePreservePostExchangeDurabilityEvent::CandidateSynced {
            device: candidate.0,
            inode: candidate.1,
        },
        ActiveReblitCandidatePreservePostExchangeDurabilityEvent::CandidateWrapperSynced {
            device: candidate_wrapper.0,
            inode: candidate_wrapper.1,
        },
        ActiveReblitCandidatePreservePostExchangeDurabilityEvent::ReservationWrapperSynced {
            device: reservation_wrapper.0,
            inode: reservation_wrapper.1,
        },
        ActiveReblitCandidatePreservePostExchangeDurabilityEvent::RootsParentSynced {
            device: roots.0,
            inode: roots.1,
        },
        ActiveReblitCandidatePreservePostExchangeDurabilityEvent::QuarantineParentSynced {
            device: quarantine.0,
            inode: quarantine.1,
        },
        ActiveReblitCandidatePreservePostExchangeDurabilityEvent::FinalPostProven,
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
