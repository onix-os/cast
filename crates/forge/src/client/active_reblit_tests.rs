//! Focused whole-wrapper and strict-state-ID tests for active-state reblits.

use std::{
    collections::BTreeSet,
    fs::{self, Permissions},
    io,
    os::unix::fs::{FileTypeExt as _, MetadataExt as _, PermissionsExt as _, symlink},
    path::PathBuf,
};

use super::*;
use crate::{
    test_support::prepare_private_installation_root,
    transition_identity::{
        RetainedActivePreviousSlotParkingFaultPoint as SlotFaultPoint,
        RetainedStagingWrapperRotationFaultPoint as FaultPoint, arm_active_previous_slot_parking_faults,
        arm_before_active_previous_slot_parking_rename, arm_before_staging_wrapper_exchange,
        arm_staging_wrapper_rotation_faults,
    },
    tree_marker::TreeMarkerStore,
};

struct ActiveFixture {
    _temporary: tempfile::TempDir,
    client: Client,
    state: State,
    old_usr_inode: u64,
}

fn fixture() -> ActiveFixture {
    let temporary = tempfile::tempdir().unwrap();
    prepare_private_installation_root(temporary.path());
    let installation = Installation::open(temporary.path(), None).unwrap();
    let client = Client::builder("active-reblit-wrapper-test", installation)
        .repositories(repository::Map::default())
        .build()
        .unwrap();
    let state = client.state_db.add(&[], Some("active"), None).unwrap();
    record_state_id(&client.installation.root, state.id).unwrap();
    record_system_snapshot(&client.installation.root, snapshot("old-active")).unwrap();
    fs::write(client.installation.root.join("usr/old-payload"), b"old tree").unwrap();
    drop(client);

    let installation = Installation::open(temporary.path(), None).unwrap();
    let client = Client::builder("active-reblit-wrapper-test", installation)
        .repositories(repository::Map::default())
        .build()
        .unwrap();
    let old_usr_inode = fs::symlink_metadata(client.installation.root.join("usr"))
        .unwrap()
        .ino();
    ActiveFixture {
        _temporary: temporary,
        client,
        state,
        old_usr_inode,
    }
}

fn snapshot(package: &str) -> SystemModel {
    system_model::create(
        repository::Map::default(),
        BTreeSet::from([Provider::package_name(package)]),
    )
}

fn run(
    fixture: &ActiveFixture,
    checkpoint: impl FnMut(StatefulTransitionCheckpoint) -> Result<(), Error>,
) -> Result<(), Error> {
    fixture.client.apply_stateful_blit_with_checkpoint(
        vfs(Vec::new()).unwrap(),
        &fixture.state,
        None,
        snapshot("repaired-active"),
        checkpoint,
    )
}

fn wrapper_quarantines(fixture: &ActiveFixture) -> Vec<PathBuf> {
    let mut paths = fs::read_dir(fixture.client.installation.state_quarantine_dir())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("replaced-active-reblit-wrapper-")
        })
        .collect::<Vec<_>>();
    paths.sort();
    paths
}

fn failed_usr_quarantines(fixture: &ActiveFixture) -> Vec<PathBuf> {
    fs::read_dir(fixture.client.installation.state_quarantine_dir())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("failed-active-reblit-")
        })
        .collect()
}

fn assert_empty_fixed_staging(fixture: &ActiveFixture) {
    let staging = fixture.client.installation.staging_dir();
    assert!(staging.is_dir());
    assert_eq!(fs::read_dir(&staging).unwrap().count(), 0);
    assert_eq!(
        fs::symlink_metadata(staging).unwrap().permissions().mode() & 0o7777,
        0o700
    );
}

fn assert_old_tree_live(fixture: &ActiveFixture) {
    let live = fixture.client.installation.root.join("usr");
    assert_eq!(fs::symlink_metadata(&live).unwrap().ino(), fixture.old_usr_inode);
    assert_eq!(fs::read(live.join("old-payload")).unwrap(), b"old tree");
    assert_eq!(
        fs::read_to_string(live.join(".stateID")).unwrap(),
        fixture.state.id.to_string()
    );
}

fn assert_repaired_tree_live(fixture: &ActiveFixture) {
    let live = fixture.client.installation.root.join("usr");
    assert_ne!(fs::symlink_metadata(&live).unwrap().ino(), fixture.old_usr_inode);
    assert!(!live.join("old-payload").exists());
    assert_eq!(
        fs::read_to_string(live.join(".stateID")).unwrap(),
        fixture.state.id.to_string()
    );
    assert_eq!(fs::symlink_metadata(live.join(".cast-tree-id")).unwrap().nlink(), 1);
}

#[test]
fn active_reblit_rotates_the_whole_old_wrapper_and_leaves_exact_empty_staging() {
    let fixture = fixture();
    run(&fixture, |checkpoint| {
        if checkpoint == StatefulTransitionCheckpoint::AfterTransactionTriggers {
            fs::write(
                fixture.client.installation.staging_dir().join("wrapper-sentinel"),
                b"whole wrapper evidence",
            )?;
        }
        Ok(())
    })
    .unwrap();

    assert_repaired_tree_live(&fixture);
    assert_empty_fixed_staging(&fixture);
    let quarantines = wrapper_quarantines(&fixture);
    assert_eq!(quarantines.len(), 1);
    assert_eq!(
        fs::symlink_metadata(quarantines[0].join("usr")).unwrap().ino(),
        fixture.old_usr_inode
    );
    assert_eq!(fs::read(quarantines[0].join("usr/old-payload")).unwrap(), b"old tree");
    assert_eq!(
        fs::read(quarantines[0].join("wrapper-sentinel")).unwrap(),
        b"whole wrapper evidence"
    );
}

#[test]
fn active_reblit_refuses_missing_or_malformed_live_state_id_without_staging_mutation() {
    for old_state_id in [None, Some(b"corrupt".as_slice())] {
        let fixture = fixture();
        let path = fixture.client.installation.root.join("usr/.stateID");
        match old_state_id {
            None => fs::remove_file(&path).unwrap(),
            Some(contents) => fs::write(&path, contents).unwrap(),
        }

        let error = run(&fixture, |_| Ok(())).unwrap_err();

        assert!(matches!(error, Error::LiveActiveStateProof { .. }), "{error:#?}");
        let live = fixture.client.installation.root.join("usr");
        assert_eq!(fs::symlink_metadata(&live).unwrap().ino(), fixture.old_usr_inode);
        assert_eq!(fs::read(live.join("old-payload")).unwrap(), b"old tree");
        match old_state_id {
            None => assert!(!path.exists()),
            Some(contents) => assert_eq!(fs::read(path).unwrap(), contents),
        }
        assert_empty_fixed_staging(&fixture);
        assert!(wrapper_quarantines(&fixture).is_empty());
    }
}

#[test]
fn active_reblit_rejects_same_inode_state_id_rewrite_before_exchange() {
    let fixture = fixture();
    let candidate = fixture.client.installation.staging_path("usr/.stateID");
    let error = run(&fixture, |checkpoint| {
        if checkpoint == StatefulTransitionCheckpoint::AfterTransactionTriggers {
            fs::write(&candidate, b"9")?;
        }
        Ok(())
    })
    .unwrap_err();

    assert!(matches!(error, Error::StatefulCandidatePreserved { .. }), "{error:#?}");
    assert_old_tree_live(&fixture);
    assert_empty_fixed_staging(&fixture);
    let failed = wrapper_quarantines(&fixture);
    assert_eq!(failed.len(), 1);
    assert_eq!(fs::read(failed[0].join("usr/.stateID")).unwrap(), b"9");
}

#[test]
fn active_reblit_rejects_same_content_new_state_id_inode() {
    let fixture = fixture();
    let candidate = fixture.client.installation.staging_path("usr/.stateID");
    let displaced = fixture.client.installation.staging_path("usr/.stateID.retained");
    let error = run(&fixture, |checkpoint| {
        if checkpoint == StatefulTransitionCheckpoint::AfterTransactionTriggers {
            let contents = fs::read(&candidate)?;
            fs::rename(&candidate, &displaced)?;
            fs::write(&candidate, contents)?;
            fs::set_permissions(&candidate, Permissions::from_mode(0o644))?;
        }
        Ok(())
    })
    .unwrap_err();

    assert!(matches!(error, Error::StatefulCandidatePreserved { .. }), "{error:#?}");
    assert_old_tree_live(&fixture);
    let failed = wrapper_quarantines(&fixture).pop().unwrap();
    assert!(failed.join("usr/.stateID").is_file());
    assert!(failed.join("usr/.stateID.retained").is_file());
    assert_ne!(
        fs::symlink_metadata(failed.join("usr/.stateID")).unwrap().ino(),
        fs::symlink_metadata(failed.join("usr/.stateID.retained"))
            .unwrap()
            .ino()
    );
}

#[test]
fn active_reblit_exchange_preflight_rejects_last_moment_state_id_replacement() {
    let fixture = fixture();
    let candidate = fixture.client.installation.staging_path("usr/.stateID");
    let displaced = fixture.client.installation.staging_path("usr/.stateID.original");
    arm_before_staging_state_id_exchange(candidate.clone(), displaced.clone());

    let error = run(&fixture, |_| Ok(())).unwrap_err();

    assert!(matches!(error, Error::StatefulCandidatePreserved { .. }), "{error:#?}");
    assert_old_tree_live(&fixture);
    let failed = wrapper_quarantines(&fixture).pop().unwrap();
    assert!(failed.join("usr/.stateID.original").is_file());
}

fn arm_before_staging_state_id_exchange(candidate: PathBuf, displaced: PathBuf) {
    crate::transition_identity::arm_before_retained_exchange_rename(move || {
        let contents = fs::read(&candidate).unwrap();
        fs::rename(&candidate, &displaced).unwrap();
        fs::write(&candidate, contents).unwrap();
        fs::set_permissions(&candidate, Permissions::from_mode(0o644)).unwrap();
    });
}

#[test]
fn active_reblit_system_boundary_corruption_reverses_and_preserves_bad_candidate() {
    for mutation in ["rewrite", "remove", "replace"] {
        let fixture = fixture();
        let live = fixture.client.installation.root.join("usr/.stateID");
        let retained = fixture.client.installation.root.join("usr/.stateID.retained");
        let error = run(&fixture, |checkpoint| {
            if checkpoint == StatefulTransitionCheckpoint::AfterSystemTriggersStarted {
                match mutation {
                    "rewrite" => fs::write(&live, b"9")?,
                    "remove" => fs::remove_file(&live)?,
                    "replace" => {
                        let contents = fs::read(&live)?;
                        fs::rename(&live, &retained)?;
                        fs::write(&live, contents)?;
                        fs::set_permissions(&live, Permissions::from_mode(0o644))?;
                    }
                    _ => unreachable!(),
                }
            }
            Ok(())
        })
        .unwrap_err();

        assert!(
            matches!(error, Error::StatefulTransitionUsrRestored { .. }),
            "{mutation}: {error:#?}"
        );
        assert_old_tree_live(&fixture);
        assert_empty_fixed_staging(&fixture);
        assert_eq!(wrapper_quarantines(&fixture).len(), 1);
    }
}

#[test]
fn active_reblit_pre_boot_checkpoint_state_id_mutation_is_rejected_before_boot() {
    let fixture = fixture();
    let live = fixture.client.installation.root.join("usr/.stateID");
    let error = run(&fixture, |checkpoint| {
        if checkpoint == StatefulTransitionCheckpoint::BeforeCandidateBootSynchronization {
            fs::write(&live, b"9")?;
        }
        Ok(())
    })
    .unwrap_err();

    assert!(
        matches!(error, Error::StatefulTransitionUsrRestored { .. }),
        "{error:#?}"
    );
    assert_old_tree_live(&fixture);
    assert_empty_fixed_staging(&fixture);
    let failed = wrapper_quarantines(&fixture);
    assert_eq!(failed.len(), 1);
    assert_eq!(fs::read(failed[0].join("usr/.stateID")).unwrap(), b"9");
}

#[test]
fn every_single_staging_wrapper_fault_is_resumed_without_tree_loss() {
    let points = [
        FaultPoint::ReplacementPostCreate,
        FaultPoint::ReplacementPreparationSync,
        FaultPoint::QuarantinePreparationSync,
        FaultPoint::FinalPreparationRevalidation,
        FaultPoint::OriginalPreSync,
        FaultPoint::ReplacementPreSync,
        FaultPoint::QuarantinePreSync,
        FaultPoint::BeforeExchange,
        FaultPoint::AfterExchange,
        FaultPoint::OriginalPostSync,
        FaultPoint::ReplacementPostSync,
        FaultPoint::RootsParentSync,
        FaultPoint::QuarantineParentSync,
        FaultPoint::FinalRevalidation,
    ];
    for point in points {
        let fixture = fixture();
        arm_staging_wrapper_rotation_faults([point]);
        let result = run(&fixture, |_| Ok(()));
        arm_staging_wrapper_rotation_faults([]);
        result.unwrap_or_else(|error| panic!("single {point:?} fault was not resumed: {error:#?}"));
        assert_repaired_tree_live(&fixture);
        assert_empty_fixed_staging(&fixture);
        assert_eq!(wrapper_quarantines(&fixture).len(), 1);
    }
}

#[test]
fn queued_not_applied_rotation_faults_reverse_then_preserve_one_whole_wrapper() {
    let fixture = fixture();
    arm_staging_wrapper_rotation_faults([FaultPoint::BeforeExchange, FaultPoint::BeforeExchange]);
    let error = run(&fixture, |_| Ok(())).unwrap_err();
    arm_staging_wrapper_rotation_faults([]);

    assert!(
        matches!(
            error,
            Error::StatefulTransitionRecoveryFailed {
                reverse_exchange: None,
                preserve_candidate: None,
                repair_boot: Some(_),
                ..
            }
        ),
        "{error:#?}"
    );
    assert_old_tree_live(&fixture);
    assert_empty_fixed_staging(&fixture);
    let failed = wrapper_quarantines(&fixture);
    assert_eq!(failed.len(), 1);
    assert!(!failed[0].join("usr/old-payload").exists());
}

#[test]
fn queued_applied_suffix_faults_never_exchange_the_wrapper_twice() {
    for point in [FaultPoint::OriginalPostSync, FaultPoint::FinalRevalidation] {
        let fixture = fixture();
        arm_staging_wrapper_rotation_faults([point, point]);
        let error = run(&fixture, |_| Ok(())).unwrap_err();
        arm_staging_wrapper_rotation_faults([]);

        assert!(matches!(
            error,
            Error::ActiveReblitCommittedCleanupIncomplete { outcome: "applied", .. }
        ));
        assert_repaired_tree_live(&fixture);
        assert_empty_fixed_staging(&fixture);
        let old = wrapper_quarantines(&fixture);
        assert_eq!(old.len(), 1);
        assert_eq!(
            fs::symlink_metadata(old[0].join("usr")).unwrap().ino(),
            fixture.old_usr_inode
        );
    }
}

#[test]
fn staging_wrapper_substitution_is_ambiguous_and_never_retried() {
    let fixture = fixture();
    let staging = fixture.client.installation.staging_dir();
    let displaced = fixture.client.installation.root_path("substituted-staging-wrapper");
    arm_before_staging_wrapper_exchange(move || {
        fs::rename(&staging, &displaced).unwrap();
        fs::create_dir(&staging).unwrap();
        fs::set_permissions(&staging, Permissions::from_mode(0o700)).unwrap();
    });

    let error = run(&fixture, |_| Ok(())).unwrap_err();

    assert!(matches!(
        error,
        Error::ActiveReblitCommittedCleanupIncomplete {
            outcome: "ambiguous",
            ..
        }
    ));
    assert_repaired_tree_live(&fixture);
    assert!(
        fixture
            .client
            .installation
            .root_path("substituted-staging-wrapper/usr")
            .is_dir()
    );
}

#[test]
fn staging_wrapper_scan_skips_foreign_types_and_uses_next_index() {
    let fixture = fixture();
    let token = prepare_live_marker_token(&fixture);
    let quarantine = fixture.client.installation.state_quarantine_dir();
    let name = |index| wrapper_name(fixture.state.id, &token, index);
    fs::write(quarantine.join(name(0)), b"file").unwrap();
    symlink("missing-target", quarantine.join(name(1))).unwrap();
    fs::create_dir(quarantine.join(name(2))).unwrap();
    fs::set_permissions(quarantine.join(name(2)), Permissions::from_mode(0o755)).unwrap();
    nix::unistd::mkfifo(&quarantine.join(name(3)), Mode::from_bits_truncate(0o600)).unwrap();

    run(&fixture, |_| Ok(())).unwrap();

    assert!(quarantine.join(name(0)).is_file());
    assert!(
        fs::symlink_metadata(quarantine.join(name(1)))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(quarantine.join(name(2)).is_dir());
    assert!(quarantine.join(name(3)).exists());
    assert!(quarantine.join(name(4)).is_dir());
}

#[test]
fn staging_wrapper_name_exhaustion_falls_back_without_touching_live_or_occupants() {
    let fixture = fixture();
    let token = prepare_live_marker_token(&fixture);
    let quarantine = fixture.client.installation.state_quarantine_dir();
    for index in 0..256 {
        fs::write(
            quarantine.join(wrapper_name(fixture.state.id, &token, index)),
            index.to_string(),
        )
        .unwrap();
    }

    let error = run(&fixture, |_| Ok(())).unwrap_err();

    assert!(matches!(error, Error::StatefulCandidatePreserved { .. }), "{error:#?}");
    assert_old_tree_live(&fixture);
    for index in 0..256 {
        assert_eq!(
            fs::read_to_string(quarantine.join(wrapper_name(fixture.state.id, &token, index))).unwrap(),
            index.to_string()
        );
    }
    assert!(wrapper_quarantines(&fixture).iter().all(|path| path.is_file()));
    assert_eq!(failed_usr_quarantines(&fixture).len(), 1);
}

#[test]
fn staging_wrapper_pre_retention_substitution_uses_marker_authenticated_fallback() {
    let fixture = fixture();
    let quarantine = fixture.client.installation.state_quarantine_dir();
    let observed = std::rc::Rc::new(std::cell::RefCell::new(None));
    let hook_observed = observed.clone();
    crate::transition_identity::arm_before_quarantine_slot_reopen(move || {
        let created = fs::read_dir(&quarantine)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("replaced-active-reblit-wrapper-")
            })
            .unwrap();
        let displaced = created.with_extension("created");
        fs::rename(&created, &displaced).unwrap();
        fs::write(&created, b"racing foreign occupant").unwrap();
        hook_observed.replace(Some((created, displaced)));
    });

    let error = run(&fixture, |_| Ok(())).unwrap_err();

    assert!(matches!(error, Error::StatefulCandidatePreserved { .. }), "{error:#?}");
    assert_old_tree_live(&fixture);
    let (foreign, displaced) = observed.as_ref().borrow().clone().unwrap();
    assert_eq!(fs::read(foreign).unwrap(), b"racing foreign occupant");
    assert_eq!(fs::read_dir(displaced).unwrap().count(), 0);
    assert_eq!(failed_usr_quarantines(&fixture).len(), 1);
}

#[test]
fn two_successful_active_reblits_on_one_client_use_distinct_wrapper_slots() {
    let fixture = fixture();
    run(&fixture, |_| Ok(())).unwrap();
    run(&fixture, |_| Ok(())).unwrap();

    assert_repaired_tree_live(&fixture);
    assert_empty_fixed_staging(&fixture);
    let wrappers = wrapper_quarantines(&fixture);
    assert_eq!(wrappers.len(), 2);
    assert_ne!(wrappers[0], wrappers[1]);
    for wrapper in wrappers {
        assert!(wrapper.join("usr/.cast-tree-id").is_file());
        assert!(wrapper.join("usr/.stateID").exists());
    }
}

fn prepare_live_marker_token(fixture: &ActiveFixture) -> String {
    let store = TreeMarkerStore::open_path(fixture.client.installation.root.join("usr")).unwrap();
    let marker = store.adopt_or_create_before_journal().unwrap();
    marker.token().as_str().to_owned()
}

fn wrapper_name(state: state::Id, token: &str, index: usize) -> String {
    format!("replaced-active-reblit-wrapper-{}-{token}-{index}", i32::from(state))
}

fn archived_candidate_slot_name(state: state::Id, token: &str, index: usize) -> String {
    format!(".archived-candidate-slot-{}-{token}-{index}", i32::from(state))
}

fn prepare_canonical_previous_slot(fixture: &ActiveFixture) -> (String, PathBuf, PathBuf, u64) {
    let token = prepare_live_marker_token(fixture);
    let wrapper = fixture.client.installation.root_path(fixture.state.id.to_string());
    fs::create_dir(&wrapper).unwrap();
    fs::set_permissions(&wrapper, Permissions::from_mode(0o700)).unwrap();
    let slot = wrapper.join(format!(".cast-state-slot-{}-{token}", fixture.state.id));
    fs::hard_link(fixture.client.installation.root.join("usr/.cast-tree-id"), &slot).unwrap();
    let marker_inode = fs::symlink_metadata(&slot).unwrap().ino();
    (token, wrapper, slot, marker_inode)
}

#[test]
fn active_reblit_preserves_authorized_two_link_previous_marker_pair() {
    let fixture = fixture();
    let (token, wrapper, _slot, marker_inode) = prepare_canonical_previous_slot(&fixture);

    run(&fixture, |_| Ok(())).unwrap();

    let old_marker = wrapper_quarantines(&fixture).pop().unwrap().join("usr/.cast-tree-id");
    let parked_wrapper =
        fixture
            .client
            .installation
            .root_path(archived_candidate_slot_name(fixture.state.id, &token, 0));
    let parked_slot = parked_wrapper.join(format!(".cast-state-slot-{}-{token}", fixture.state.id));
    assert!(!wrapper.exists());
    assert_eq!(fs::read_dir(&parked_wrapper).unwrap().count(), 1);
    assert_eq!(fs::symlink_metadata(&old_marker).unwrap().ino(), marker_inode);
    assert_eq!(fs::symlink_metadata(&parked_slot).unwrap().ino(), marker_inode);
    assert_eq!(fs::symlink_metadata(old_marker).unwrap().nlink(), 2);

    let next = fixture.client.state_db.add(&[], Some("next"), None).unwrap();
    fixture
        .client
        .apply_stateful_blit_with_checkpoint(
            vfs(Vec::new()).unwrap(),
            &next,
            Some(fixture.state.id),
            snapshot("next"),
            |_| Ok(()),
        )
        .unwrap();

    let archived_repaired = fixture.client.installation.root_path(fixture.state.id.to_string());
    assert!(archived_repaired.join("usr").is_dir());
    assert_eq!(
        fs::read_to_string(archived_repaired.join("usr/.stateID")).unwrap(),
        fixture.state.id.to_string()
    );
    assert_eq!(fs::symlink_metadata(&parked_slot).unwrap().ino(), marker_inode);
    assert_eq!(fs::symlink_metadata(&parked_slot).unwrap().nlink(), 2);
}

#[test]
fn every_single_active_previous_slot_parking_fault_resumes_without_a_second_move() {
    let points = [
        SlotFaultPoint::MarkerPreSync,
        SlotFaultPoint::WrapperPreSync,
        SlotFaultPoint::RootsPreSync,
        SlotFaultPoint::BeforeRename,
        SlotFaultPoint::AfterRename,
        SlotFaultPoint::MarkerPostSync,
        SlotFaultPoint::WrapperPostSync,
        SlotFaultPoint::RootsPostSync,
        SlotFaultPoint::FinalRevalidation,
    ];
    for point in points {
        let fixture = fixture();
        let (token, canonical, _slot, marker_inode) = prepare_canonical_previous_slot(&fixture);
        arm_active_previous_slot_parking_faults([point]);
        let result = run(&fixture, |_| Ok(()));
        arm_active_previous_slot_parking_faults([]);
        result.unwrap_or_else(|error| panic!("single {point:?} parking fault was not resumed: {error:#?}"));

        let parked = fixture
            .client
            .installation
            .root_path(archived_candidate_slot_name(fixture.state.id, &token, 0));
        assert!(!canonical.exists());
        assert_eq!(fs::read_dir(&parked).unwrap().count(), 1);
        assert_eq!(
            fs::symlink_metadata(parked.join(format!(".cast-state-slot-{}-{token}", fixture.state.id)))
                .unwrap()
                .ino(),
            marker_inode
        );
        assert_repaired_tree_live(&fixture);
    }
}

#[test]
fn active_previous_slot_parking_exhaustion_preserves_every_name_and_old_live_tree() {
    let fixture = fixture();
    let (token, canonical, slot, marker_inode) = prepare_canonical_previous_slot(&fixture);
    let roots = fixture.client.installation.root_path("");
    for index in 0..256 {
        fs::write(
            roots.join(archived_candidate_slot_name(fixture.state.id, &token, index)),
            index.to_string(),
        )
        .unwrap();
    }

    let error = run(&fixture, |_| Ok(())).unwrap_err();

    assert!(
        matches!(
            error,
            Error::StatefulTransitionRecoveryFailed {
                reverse_exchange: None,
                preserve_candidate: Some(_),
                ..
            }
        ),
        "{error:#?}"
    );
    assert_old_tree_live(&fixture);
    assert!(canonical.is_dir());
    assert_eq!(fs::symlink_metadata(slot).unwrap().ino(), marker_inode);
    for index in 0..256 {
        assert_eq!(
            fs::read_to_string(roots.join(archived_candidate_slot_name(fixture.state.id, &token, index))).unwrap(),
            index.to_string()
        );
    }
    assert_empty_fixed_staging(&fixture);
    assert_eq!(wrapper_quarantines(&fixture).len(), 1);
}

#[test]
fn active_previous_slot_scan_skips_every_foreign_occupant_kind() {
    let fixture = fixture();
    let (token, canonical, _slot, marker_inode) = prepare_canonical_previous_slot(&fixture);
    let roots = fixture.client.installation.root_path("");
    let parking = |index| roots.join(archived_candidate_slot_name(fixture.state.id, &token, index));
    fs::write(parking(0), b"regular occupant").unwrap();
    symlink("missing-target", parking(1)).unwrap();
    nix::unistd::mkfifo(&parking(2), Mode::from_bits_truncate(0o600)).unwrap();
    fs::create_dir(parking(3)).unwrap();
    fs::set_permissions(parking(3), Permissions::from_mode(0o755)).unwrap();

    run(&fixture, |_| Ok(())).unwrap();

    assert!(!canonical.exists());
    assert_eq!(fs::read(parking(0)).unwrap(), b"regular occupant");
    assert!(fs::symlink_metadata(parking(1)).unwrap().file_type().is_symlink());
    assert!(fs::symlink_metadata(parking(2)).unwrap().file_type().is_fifo());
    assert_eq!(
        fs::symlink_metadata(parking(3)).unwrap().permissions().mode() & 0o7777,
        0o755
    );
    assert_eq!(
        fs::symlink_metadata(parking(4).join(format!(".cast-state-slot-{}-{token}", fixture.state.id)))
            .unwrap()
            .ino(),
        marker_inode
    );
}

#[test]
fn queued_active_previous_slot_suffix_faults_keep_the_move_applied() {
    let fixture = fixture();
    let (token, canonical, _slot, marker_inode) = prepare_canonical_previous_slot(&fixture);
    arm_active_previous_slot_parking_faults([SlotFaultPoint::RootsPostSync, SlotFaultPoint::RootsPostSync]);

    let error = run(&fixture, |_| Ok(())).unwrap_err();
    arm_active_previous_slot_parking_faults([]);

    assert!(matches!(error, Error::StatefulCandidatePreserved { .. }), "{error:#?}");
    assert_old_tree_live(&fixture);
    assert!(!canonical.exists());
    let parked = fixture
        .client
        .installation
        .root_path(archived_candidate_slot_name(fixture.state.id, &token, 0));
    assert_eq!(
        fs::symlink_metadata(parked.join(format!(".cast-state-slot-{}-{token}", fixture.state.id)))
            .unwrap()
            .ino(),
        marker_inode
    );
    assert_empty_fixed_staging(&fixture);
    assert_eq!(wrapper_quarantines(&fixture).len(), 1);
}

#[test]
fn active_previous_slot_substitution_never_moves_or_adopts_the_foreign_wrapper() {
    let fixture = fixture();
    let (token, canonical, _slot, marker_inode) = prepare_canonical_previous_slot(&fixture);
    let displaced = fixture.client.installation.root_path("retained-active-slot-race");
    let hook_canonical = canonical.clone();
    let hook_displaced = displaced.clone();
    arm_before_active_previous_slot_parking_rename(move || {
        fs::rename(&hook_canonical, &hook_displaced).unwrap();
        fs::create_dir(&hook_canonical).unwrap();
        fs::set_permissions(&hook_canonical, Permissions::from_mode(0o700)).unwrap();
        fs::write(hook_canonical.join("foreign"), b"racing wrapper").unwrap();
    });

    let error = run(&fixture, |_| Ok(())).unwrap_err();

    assert!(
        matches!(
            error,
            Error::StatefulTransitionRecoveryFailed {
                reverse_exchange: None,
                preserve_candidate: Some(_),
                ..
            }
        ),
        "{error:#?}"
    );
    assert_old_tree_live(&fixture);
    assert_eq!(fs::read(canonical.join("foreign")).unwrap(), b"racing wrapper");
    let displaced_slot = displaced.join(format!(".cast-state-slot-{}-{token}", fixture.state.id));
    assert_eq!(fs::symlink_metadata(displaced_slot).unwrap().ino(), marker_inode);
    assert!(
        !fixture
            .client
            .installation
            .root_path(archived_candidate_slot_name(fixture.state.id, &token, 0))
            .exists()
    );
}

#[test]
fn active_previous_slot_parking_adopts_an_exact_externally_applied_move() {
    let fixture = fixture();
    let (token, canonical, _slot, marker_inode) = prepare_canonical_previous_slot(&fixture);
    let parked = fixture
        .client
        .installation
        .root_path(archived_candidate_slot_name(fixture.state.id, &token, 0));
    let hook_canonical = canonical.clone();
    let hook_parked = parked.clone();
    arm_before_active_previous_slot_parking_rename(move || {
        fs::rename(hook_canonical, hook_parked).unwrap();
    });

    run(&fixture, |_| Ok(())).unwrap();

    assert!(!canonical.exists());
    assert_eq!(
        fs::symlink_metadata(parked.join(format!(".cast-state-slot-{}-{token}", fixture.state.id)))
            .unwrap()
            .ino(),
        marker_inode
    );
    assert_repaired_tree_live(&fixture);
}

#[test]
fn already_parked_previous_slot_with_foreign_canonical_name_fails_closed() {
    let fixture = fixture();
    let (token, canonical, _slot, marker_inode) = prepare_canonical_previous_slot(&fixture);
    let parked = fixture
        .client
        .installation
        .root_path(archived_candidate_slot_name(fixture.state.id, &token, 0));
    fs::rename(&canonical, &parked).unwrap();
    fs::create_dir(&canonical).unwrap();
    fs::set_permissions(&canonical, Permissions::from_mode(0o700)).unwrap();
    fs::write(canonical.join("foreign"), b"canonical occupant").unwrap();

    let error = run(&fixture, |_| Ok(())).unwrap_err();

    assert!(
        matches!(
            error,
            Error::StatefulTransitionRecoveryFailed {
                reverse_exchange: None,
                preserve_candidate: Some(_),
                ..
            }
        ),
        "{error:#?}"
    );
    assert_old_tree_live(&fixture);
    assert_eq!(fs::read(canonical.join("foreign")).unwrap(), b"canonical occupant");
    assert_eq!(
        fs::symlink_metadata(parked.join(format!(".cast-state-slot-{}-{token}", fixture.state.id)))
            .unwrap()
            .ino(),
        marker_inode
    );
}

#[test]
fn active_reblit_rejects_a_slot_moved_back_to_canonical_after_triggers() {
    let fixture = fixture();
    let (token, canonical, _slot, marker_inode) = prepare_canonical_previous_slot(&fixture);
    let parked = fixture
        .client
        .installation
        .root_path(archived_candidate_slot_name(fixture.state.id, &token, 0));

    let error = run(&fixture, |checkpoint| {
        if checkpoint == StatefulTransitionCheckpoint::AfterTransactionTriggers {
            fs::rename(&parked, &canonical)?;
        }
        Ok(())
    })
    .unwrap_err();

    assert!(
        matches!(
            error,
            Error::StatefulTransitionRecoveryFailed {
                reverse_exchange: None,
                preserve_candidate: Some(_),
                ..
            }
        ),
        "{error:#?}"
    );
    assert_old_tree_live(&fixture);
    assert!(!parked.exists());
    assert_eq!(
        fs::symlink_metadata(canonical.join(format!(".cast-state-slot-{}-{token}", fixture.state.id)))
            .unwrap()
            .ino(),
        marker_inode
    );
}

#[test]
fn active_reblit_reversal_cannot_report_success_after_parked_slot_is_moved_back() {
    let fixture = fixture();
    let (token, canonical, _slot, marker_inode) = prepare_canonical_previous_slot(&fixture);
    let parked = fixture
        .client
        .installation
        .root_path(archived_candidate_slot_name(fixture.state.id, &token, 0));

    let error = run(&fixture, |checkpoint| {
        if checkpoint == StatefulTransitionCheckpoint::AfterSystemTriggers {
            fs::rename(&parked, &canonical)?;
            return Err(Error::Io(io::Error::other("force recovery after parked-slot mutation")));
        }
        Ok(())
    })
    .unwrap_err();

    assert!(
        matches!(
            error,
            Error::StatefulTransitionRecoveryFailed {
                reverse_exchange: None,
                preserve_candidate: Some(_),
                ..
            }
        ),
        "{error:#?}"
    );
    assert_old_tree_live(&fixture);
    assert_empty_fixed_staging(&fixture);
    assert_eq!(wrapper_quarantines(&fixture).len(), 1);
    assert_eq!(
        fs::symlink_metadata(canonical.join(format!(".cast-state-slot-{}-{token}", fixture.state.id)))
            .unwrap()
            .ino(),
        marker_inode
    );
}
