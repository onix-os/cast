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
            UsrRollbackReverseAdmission, UsrRollbackReverseApplyReconciliation,
            UsrRollbackReverseDurableEffectAuthority,
        },
        startup_recovery::{
            UsrRollbackReverseEffectSeal, complete_already_satisfied_usr_rollback_reverse_durability,
            complete_applied_usr_rollback_reverse_durability,
        },
    },
    transition_journal::{RollbackActionOutcome, TransitionJournalStore, TransitionRecord},
};

use super::super::reverse_test_support::{EffectOperationKind, ReverseFixture, ReverseLayout};

pub(super) use super::super::reverse_test_support::{
    EffectOperationKind as OperationKind, ReverseFixture as Fixture, capture_record,
};

pub(super) fn fixture_for_outcome(kind: EffectOperationKind, outcome: RollbackActionOutcome) -> ReverseFixture {
    ReverseFixture::for_effect(
        kind,
        match outcome {
            RollbackActionOutcome::Applied => ReverseLayout::Post,
            RollbackActionOutcome::AlreadySatisfied => ReverseLayout::Pre,
        },
    )
}

pub(super) fn durable_authority<'reservation>(
    fixture: &ReverseFixture,
    journal: &TransitionJournalStore,
    reservation: &'reservation ActiveStateReservation,
    outcome: RollbackActionOutcome,
) -> UsrRollbackReverseDurableEffectAuthority<'reservation> {
    let effect_seal = UsrRollbackReverseEffectSeal::new_for_test();
    match outcome {
        RollbackActionOutcome::Applied => {
            let UsrRollbackReverseAdmission::Apply(authority) = fixture.capture(journal, reservation) else {
                panic!("POST evidence did not admit reverse apply authority");
            };
            let lease = authority.into_effect_lease(&effect_seal, journal).unwrap();
            let UsrRollbackReverseApplyReconciliation::Applied(authority) =
                lease.reconcile(&effect_seal, journal).unwrap()
            else {
                panic!("reverse exchange did not reconcile as applied");
            };
            complete_applied_usr_rollback_reverse_durability(journal, authority).unwrap()
        }
        RollbackActionOutcome::AlreadySatisfied => {
            let UsrRollbackReverseAdmission::Finish(authority) = fixture.capture(journal, reservation) else {
                panic!("PRE evidence did not admit reverse finish authority");
            };
            let authority = authority
                .into_effect_lease(&effect_seal, journal)
                .unwrap()
                .reconcile(&effect_seal, journal)
                .unwrap();
            complete_already_satisfied_usr_rollback_reverse_durability(journal, authority).unwrap()
        }
    }
}

pub(super) fn expected_usr_restored(fixture: &ReverseFixture, outcome: RollbackActionOutcome) -> TransitionRecord {
    fixture
        .record
        .rollback_successor(Some(outcome))
        .expect("reverse fixture must admit its fixed UsrRestored outcome")
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

pub(super) fn non_journal_namespace_snapshot(fixture: &ReverseFixture) -> Vec<NonJournalNamespaceEntry> {
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
