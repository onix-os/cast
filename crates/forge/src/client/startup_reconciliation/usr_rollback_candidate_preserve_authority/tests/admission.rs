//! Focused staged/preserved candidate-admission contracts.

use std::{
    fs,
    os::unix::fs::{MetadataExt as _, symlink},
    path::PathBuf,
};

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_reconciliation::{UsrRollbackCandidatePreserveAdmission, UsrRollbackCandidatePreserveTopology},
    },
    transition_journal::{AbortDisposition, BootRollback, ForwardPhase, Phase, RollbackAction, RollbackActionOutcome},
};

use super::{
    fixture::{OperationKind, ROOT_ABI},
    support::{CandidateLayout, CandidatePreserveFixture, CandidateSource, capture_record},
};

#[test]
fn startup_candidate_preserve_admission_splits_every_exact_staged_and_preserved_matrix_case() {
    for kind in OperationKind::ALL {
        for source in CandidateSource::THROUGH_CANDIDATE_PRESERVED {
            for usr_reverse_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
                for layout in [CandidateLayout::Staged, CandidateLayout::Preserved] {
                    let fixture = CandidatePreserveFixture::new(kind, source, usr_reverse_outcome, layout);
                    let before = fixture.evidence_snapshots();
                    let journal = fixture.open_journal();
                    let reservation = ActiveStateReservation::acquire().unwrap();
                    match (layout, fixture.capture(&journal, &reservation)) {
                        (CandidateLayout::Staged, UsrRollbackCandidatePreserveAdmission::Apply(authority)) => {
                            require_expected_topology(kind, layout, authority.topology());
                            authority.revalidate(&journal).unwrap();
                        }
                        (CandidateLayout::Preserved, UsrRollbackCandidatePreserveAdmission::Finish(authority)) => {
                            require_expected_topology(kind, layout, authority.topology());
                            authority.revalidate(&journal).unwrap();
                        }
                        _ => panic!(
                            "exact {kind:?} {source:?} {usr_reverse_outcome:?} {layout:?} evidence selected the wrong typestate"
                        ),
                    }
                    fixture.assert_evidence_unchanged(&before);
                }
            }
        }
    }
    exercise_root_abi_binding_races();
}

#[derive(Clone, Copy, Debug)]
enum RootAbiRace {
    Missing,
    WrongTarget,
    SameTargetDifferentInode,
}

impl RootAbiRace {
    const ALL: [Self; 3] = [Self::Missing, Self::WrongTarget, Self::SameTargetDifferentInode];
}

#[derive(Debug, Eq, PartialEq)]
struct RootAbiLinkIdentity {
    target: PathBuf,
    device: u64,
    inode: u64,
    mode: u32,
    links: u64,
}

fn exercise_root_abi_binding_races() {
    let mut cases = 0;
    for historical in [false, true] {
        for kind in OperationKind::ALL {
            for outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {
                for layout in [CandidateLayout::Staged, CandidateLayout::Preserved] {
                    for race in RootAbiRace::ALL {
                        for (link_index, (link_name, link_target)) in ROOT_ABI.into_iter().enumerate() {
                            let fixture = if historical {
                                CandidatePreserveFixture::historical(
                                    kind,
                                    CandidateSource::RootLinksComplete,
                                    outcome,
                                    layout,
                                )
                            } else {
                                CandidatePreserveFixture::new(
                                    kind,
                                    CandidateSource::RootLinksComplete,
                                    outcome,
                                    layout,
                                )
                            };
                            let journal = fixture.open_journal();
                            let reservation = ActiveStateReservation::acquire().unwrap();
                            let admission = fixture.capture(&journal, &reservation);
                            let journal_before = fixture.fixture.canonical_bytes();
                            let database_before = fixture.fixture.database_snapshot();
                            let root_abi_before = root_abi_snapshot(&fixture.fixture.installation.root);
                            apply_root_abi_race(
                                &fixture.fixture.installation.root,
                                link_name,
                                link_target,
                                race,
                            );

                            let rejected = match admission {
                                UsrRollbackCandidatePreserveAdmission::Apply(authority) => {
                                    authority.revalidate(&journal).is_err()
                                }
                                UsrRollbackCandidatePreserveAdmission::Finish(authority) => {
                                    authority.revalidate(&journal).is_err()
                                }
                                _ => false,
                            };

                            assert!(
                                rejected,
                                "{kind:?} {outcome:?} {layout:?} {race:?} root link {link_index}:{link_name}"
                            );
                            assert_eq!(fixture.fixture.canonical_bytes(), journal_before);
                            assert_eq!(fixture.fixture.database_snapshot(), database_before);
                            assert_root_abi_race(
                                &root_abi_before,
                                &root_abi_snapshot(&fixture.fixture.installation.root),
                                link_index,
                                link_name,
                                link_target,
                                race,
                            );
                            cases += 1;
                        }
                    }
                }
            }
        }
    }
    assert_eq!(cases, 360, "root ABI admission binding-race matrix drifted");
}

fn root_abi_snapshot(root: &std::path::Path) -> Vec<Option<RootAbiLinkIdentity>> {
    ROOT_ABI
        .into_iter()
        .map(|(name, _)| {
            let path = root.join(name);
            match fs::symlink_metadata(&path) {
                Ok(metadata) => Some(RootAbiLinkIdentity {
                    target: fs::read_link(path).unwrap(),
                    device: metadata.dev(),
                    inode: metadata.ino(),
                    mode: metadata.mode(),
                    links: metadata.nlink(),
                }),
                Err(source) if source.kind() == std::io::ErrorKind::NotFound => None,
                Err(source) => panic!("inspect root ABI race fixture: {source}"),
            }
        })
        .collect()
}

fn apply_root_abi_race(root: &std::path::Path, link_name: &str, link_target: &str, race: RootAbiRace) {
    let link = root.join(link_name);
    match race {
        RootAbiRace::Missing => fs::remove_file(link).unwrap(),
        RootAbiRace::WrongTarget => {
            fs::remove_file(&link).unwrap();
            symlink(wrong_root_abi_target(link_name), link).unwrap();
        }
        RootAbiRace::SameTargetDifferentInode => {
            let displaced = root.parent().unwrap().join(format!("root-abi-race-{link_name}"));
            fs::rename(&link, displaced).unwrap();
            symlink(link_target, link).unwrap();
        }
    }
}

fn wrong_root_abi_target(link_name: &str) -> PathBuf {
    PathBuf::from(format!("usr/not-{link_name}"))
}

fn assert_root_abi_race(
    before: &[Option<RootAbiLinkIdentity>],
    after: &[Option<RootAbiLinkIdentity>],
    link_index: usize,
    link_name: &str,
    link_target: &str,
    race: RootAbiRace,
) {
    assert_eq!(before.len(), ROOT_ABI.len());
    assert_eq!(after.len(), ROOT_ABI.len());
    for (other_index, (before_entry, after_entry)) in before.iter().zip(after).enumerate() {
        if other_index != link_index {
            assert_eq!(
                after_entry, before_entry,
                "root link {link_index}:{link_name} race changed peer link {other_index}"
            );
        }
    }
    let selected_before = before[link_index].as_ref().unwrap();
    assert_eq!(
        selected_before.target,
        PathBuf::from(link_target),
        "root link {link_index}:{link_name} fixture target drifted"
    );
    match race {
        RootAbiRace::Missing => assert!(
            after[link_index].is_none(),
            "root link {link_index}:{link_name} was not removed"
        ),
        RootAbiRace::WrongTarget => {
            assert_eq!(
                after[link_index].as_ref().unwrap().target,
                wrong_root_abi_target(link_name),
                "root link {link_index}:{link_name} wrong-target race drifted"
            );
        }
        RootAbiRace::SameTargetDifferentInode => {
            let selected_after = after[link_index].as_ref().unwrap();
            assert_eq!(
                selected_after.target, selected_before.target,
                "root link {link_index}:{link_name} same-target race changed target"
            );
            assert_ne!(
                (selected_after.device, selected_after.inode),
                (selected_before.device, selected_before.inode),
                "root link {link_index}:{link_name} same-target race retained its inode"
            );
        }
    }
}

#[test]
fn startup_candidate_preserve_admission_accepts_new_state_empty_quarantine_prefix() {
    let fixture = CandidatePreserveFixture::with_new_state_empty_quarantine_prefix();
    let before = fixture.evidence_snapshots();
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    let UsrRollbackCandidatePreserveAdmission::Apply(authority) = fixture.capture(&journal, &reservation) else {
        panic!("exact empty-quarantine crash prefix did not admit staged authority");
    };
    assert_eq!(
        authority.topology(),
        UsrRollbackCandidatePreserveTopology::NewStateStagedWithEmptyQuarantine
    );
    authority.revalidate(&journal).unwrap();
    fixture.assert_evidence_unchanged(&before);
}

#[test]
fn startup_candidate_preserve_admission_accepts_historical_runtime_evidence() {
    for kind in OperationKind::ALL {
        for layout in [CandidateLayout::Staged, CandidateLayout::Preserved] {
            let fixture = CandidatePreserveFixture::historical(
                kind,
                CandidateSource::Intent,
                RollbackActionOutcome::AlreadySatisfied,
                layout,
            );
            let journal = fixture.open_journal();
            let reservation = ActiveStateReservation::acquire().unwrap();
            match (layout, fixture.capture(&journal, &reservation)) {
                (CandidateLayout::Staged, UsrRollbackCandidatePreserveAdmission::Apply(authority)) => {
                    authority.revalidate(&journal).unwrap();
                }
                (CandidateLayout::Preserved, UsrRollbackCandidatePreserveAdmission::Finish(authority)) => {
                    authority.revalidate(&journal).unwrap();
                }
                _ => panic!("historical {kind:?} {layout:?} evidence did not admit"),
            }
        }
    }
}

#[test]
fn startup_candidate_preserve_admission_bypasses_other_phases_and_sources() {
    let fixture = CandidatePreserveFixture::new(
        OperationKind::Archived,
        CandidateSource::Exchanged,
        RollbackActionOutcome::Applied,
        CandidateLayout::Staged,
    );
    let journal = fixture.open_journal();
    let reservation = ActiveStateReservation::acquire().unwrap();
    assert!(matches!(
        capture_record(&fixture.fixture, &journal, &reservation, &fixture.fixture.source),
        UsrRollbackCandidatePreserveAdmission::NotApplicable
    ));

    let mut unsupported = fixture.candidate_intent.clone();
    unsupported.rollback.as_mut().unwrap().source = ForwardPhase::TransactionTriggersComplete;
    assert!(matches!(
        capture_record(&fixture.fixture, &journal, &reservation, &unsupported),
        UsrRollbackCandidatePreserveAdmission::NotApplicable
    ));
}

#[test]
fn startup_candidate_preserve_plan_requires_the_exact_operation_matrix() {
    for kind in OperationKind::ALL {
        let fixture = CandidatePreserveFixture::new(
            kind,
            CandidateSource::Exchanged,
            RollbackActionOutcome::Applied,
            CandidateLayout::Staged,
        );
        assert!(super::super::candidate_preserve_plan_is_exact(
            &fixture.candidate_intent
        ));

        let mut changed = fixture.candidate_intent.clone();
        changed.phase = Phase::CandidatePreserved;
        assert!(!super::super::candidate_preserve_plan_is_exact(&changed));

        let mut changed = fixture.candidate_intent.clone();
        changed.rollback.as_mut().unwrap().previous_archive = RollbackAction::Applied;
        assert!(!super::super::candidate_preserve_plan_is_exact(&changed));

        for action in [RollbackAction::NotRequired, RollbackAction::Pending] {
            let mut changed = fixture.candidate_intent.clone();
            changed.rollback.as_mut().unwrap().usr_exchange = action;
            assert!(!super::super::candidate_preserve_plan_is_exact(&changed));
        }

        let mut changed = fixture.candidate_intent.clone();
        changed.rollback.as_mut().unwrap().candidate.action = RollbackAction::AlreadySatisfied;
        assert!(!super::super::candidate_preserve_plan_is_exact(&changed));

        let mut changed = fixture.candidate_intent.clone();
        changed.rollback.as_mut().unwrap().boot = BootRollback::Unverified;
        assert!(!super::super::candidate_preserve_plan_is_exact(&changed));

        let mut changed = fixture.candidate_intent.clone();
        changed.rollback.as_mut().unwrap().fresh_db = match kind {
            OperationKind::NewState => RollbackAction::NotRequired,
            OperationKind::Archived | OperationKind::ActiveReblit => RollbackAction::Pending,
        };
        assert!(!super::super::candidate_preserve_plan_is_exact(&changed));

        let mut changed = fixture.candidate_intent.clone();
        changed.rollback.as_mut().unwrap().candidate.disposition = match kind {
            OperationKind::Archived => AbortDisposition::Quarantine,
            OperationKind::NewState | OperationKind::ActiveReblit => AbortDisposition::Rearchive,
        };
        assert!(!super::super::candidate_preserve_plan_is_exact(&changed));

        let mut changed = fixture.candidate_intent.clone();
        let rollback = changed.rollback.as_mut().unwrap();
        rollback.external_effects_may_remain = !rollback.external_effects_may_remain;
        assert!(!super::super::candidate_preserve_plan_is_exact(&changed));
    }
}

fn require_expected_topology(
    kind: OperationKind,
    layout: CandidateLayout,
    topology: UsrRollbackCandidatePreserveTopology,
) {
    match (kind, layout, topology) {
        (OperationKind::NewState, CandidateLayout::Staged, UsrRollbackCandidatePreserveTopology::NewStateStaged)
        | (
            OperationKind::NewState,
            CandidateLayout::Preserved,
            UsrRollbackCandidatePreserveTopology::NewStatePreserved,
        )
        | (
            OperationKind::Archived,
            CandidateLayout::Staged,
            UsrRollbackCandidatePreserveTopology::ArchivedStagedWithCanonicalSlot,
        )
        | (
            OperationKind::Archived,
            CandidateLayout::Preserved,
            UsrRollbackCandidatePreserveTopology::ArchivedPreserved,
        )
        | (
            OperationKind::ActiveReblit,
            CandidateLayout::Staged,
            UsrRollbackCandidatePreserveTopology::ActiveReblitStaged { wrapper_index: 0 },
        )
        | (
            OperationKind::ActiveReblit,
            CandidateLayout::Preserved,
            UsrRollbackCandidatePreserveTopology::ActiveReblitPreserved { wrapper_index: 0 },
        ) => {}
        _ => panic!("unexpected topology {topology:?} for {kind:?} {layout:?}"),
    }
}
