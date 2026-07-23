//! Fault, retry, and namespace-race proofs for archived repair.

use std::{
    cell::Cell,
    ffi::OsStr,
    os::unix::fs::{FileTypeExt as _, symlink},
    path::{Path, PathBuf},
    rc::Rc,
};

use fs_err as fs;
use nix::{sys::stat::Mode, unistd::mkfifo};

use super::*;
use crate::{
    transition_identity::{
        ArchivedStateRepairError, ArchivedStateRepairFaultPoint as FaultPoint, ArchivedStateRepairPublication,
        arm_archived_state_repair_faults, arm_before_archived_state_repair_cleanup,
        arm_before_archived_state_repair_preservation, arm_before_archived_state_repair_publication,
        arm_before_archived_state_repair_suffix_retry, arm_between_archived_state_repair_layout_reads,
    },
    tree_marker::TreeMarkerStore,
};

#[test]
fn every_single_archived_repair_publication_fault_resumes_without_tree_loss() {
    for point in [
        FaultPoint::CandidatePreSync,
        FaultPoint::StagingPreSync,
        FaultPoint::CanonicalPreSync,
        FaultPoint::ReplacementPreSync,
        FaultPoint::BeforePublication,
        FaultPoint::AfterPublication,
        FaultPoint::BeforeCleanup,
        FaultPoint::AfterCleanup,
        FaultPoint::CandidatePostSync,
        FaultPoint::StagingPostSync,
        FaultPoint::RetainedPayloadPostSync,
        FaultPoint::RootsParentSync,
        FaultPoint::QuarantineParentSync,
        FaultPoint::FinalRevalidation,
    ] {
        let fixture = Fixture::new(true);
        let old_wrapper = directory_identity(&fixture.archived_root);
        arm_archived_state_repair_faults([point]);

        let publication = fixture
            .client
            .repair_archived_state(
                fixture.empty_candidate(),
                &fixture.repaired,
                fixture.snapshot("single-fault-package"),
            )
            .unwrap();
        let ArchivedStateRepairPublication::Replaced { displaced_wrapper } = publication else {
            panic!("existing archive must remain a replacement at {point:?}");
        };
        assert_eq!(directory_identity(&displaced_wrapper), old_wrapper, "fault {point:?}");
        assert_repaired_snapshot(&fixture.archived_root, fixture.repaired.id, "single-fault-package");
        assert_exact_empty_private_staging(&fixture.client.installation.staging_dir());
    }
}

#[test]
fn archived_repair_preparation_faults_leave_candidate_staged_and_archive_unchanged() {
    for point in [
        FaultPoint::ReplacementPostCreate,
        FaultPoint::ReplacementPreparationSync,
        FaultPoint::QuarantinePreparationSync,
        FaultPoint::FinalPreparationRevalidation,
    ] {
        let fixture = Fixture::new(true);
        let old_wrapper = directory_identity(&fixture.archived_root);
        arm_archived_state_repair_faults([point]);

        let error = fixture
            .client
            .repair_archived_state(
                fixture.empty_candidate(),
                &fixture.repaired,
                fixture.snapshot("preparation-fault"),
            )
            .unwrap_err();
        assert!(matches!(repair_error(error), RepairError::Preparation { .. }));
        assert_eq!(
            directory_identity(&fixture.archived_root),
            old_wrapper,
            "fault {point:?}"
        );
        assert_eq!(
            fs::read_to_string(fixture.client.installation.staging_path("usr/.stateID")).unwrap(),
            fixture.repaired.id.to_string()
        );
        assert_eq!(
            fixture.client.state_db.get(fixture.repaired.id).unwrap(),
            fixture.repaired
        );
        assert!(
            archived_repair_quarantine_paths(&fixture).is_empty(),
            "preparation fault {point:?} leaked its unused reservation"
        );
    }
}

#[test]
fn preparation_reports_primary_and_exact_reservation_cleanup_failures() {
    let fixture = Fixture::new(true);
    arm_archived_state_repair_faults([
        FaultPoint::ReplacementPostCreate,
        FaultPoint::BeforePreparationReservationRetirement,
    ]);

    let error = fixture
        .client
        .repair_archived_state(
            fixture.empty_candidate(),
            &fixture.repaired,
            fixture.snapshot("preparation-cleanup-fault"),
        )
        .unwrap_err();
    let RepairError::Preparation { source, .. } = repair_error(error) else {
        panic!("preparation and cleanup faults must remain a preparation failure");
    };
    let source = source
        .downcast::<ArchivedStateRepairError>()
        .expect("preparation must retain the structured archived-repair error");
    let ArchivedStateRepairError::PreparationReservationCleanupFailed { primary, cleanup } = *source else {
        panic!("preparation cleanup failure must preserve both errors");
    };
    assert!(matches!(
        *primary,
        ArchivedStateRepairError::InjectedFault {
            point: FaultPoint::ReplacementPostCreate
        }
    ));
    assert!(matches!(
        *cleanup,
        ArchivedStateRepairError::InjectedFault {
            point: FaultPoint::BeforePreparationReservationRetirement
        }
    ));
    let reservations = archived_repair_quarantine_paths(&fixture);
    assert_eq!(reservations.len(), 1);
    assert!(read_entry_names(&reservations[0]).is_empty());
}

#[test]
fn queued_not_applied_publication_faults_preserve_candidate_once() {
    let fixture = Fixture::new(true);
    let old_wrapper = directory_identity(&fixture.archived_root);
    arm_archived_state_repair_faults([FaultPoint::BeforePublication, FaultPoint::BeforePublication]);

    let error = fixture
        .client
        .repair_archived_state(
            fixture.empty_candidate(),
            &fixture.repaired,
            fixture.snapshot("not-applied-package"),
        )
        .unwrap_err();
    let RepairError::CandidatePreserved { quarantine, .. } = repair_error(error) else {
        panic!("exhausted exact NotApplied retries must preserve the candidate");
    };
    assert_eq!(directory_identity(&fixture.archived_root), old_wrapper);
    assert_repaired_snapshot(&quarantine, fixture.repaired.id, "not-applied-package");
    assert_exact_empty_private_staging(&fixture.client.installation.staging_dir());
}

#[test]
fn queued_applied_suffix_faults_never_reverse_committed_candidate() {
    let fixture = Fixture::new(true);
    arm_archived_state_repair_faults([FaultPoint::CandidatePostSync, FaultPoint::CandidatePostSync]);

    let error = fixture
        .client
        .repair_archived_state(
            fixture.empty_candidate(),
            &fixture.repaired,
            fixture.snapshot("committed-package"),
        )
        .unwrap_err();
    let RepairError::PublicationIncomplete { outcome, .. } = repair_error(error) else {
        panic!("exhausted committed suffix retries require a committed error");
    };
    assert_eq!(outcome, "applied");
    assert_repaired_snapshot(&fixture.archived_root, fixture.repaired.id, "committed-package");
    assert_exact_empty_private_staging(&fixture.client.installation.staging_dir());
    let parked = archived_repair_quarantine_paths(&fixture);
    assert_eq!(parked.len(), 1);
    assert_eq!(
        fs::read(parked[0].join("opaque-root-sentinel")).unwrap(),
        b"old-wrapper"
    );
}

#[test]
fn substituted_staging_before_publication_is_ambiguous_and_never_adopted() {
    let fixture = Fixture::new(true);
    let old_wrapper = directory_identity(&fixture.archived_root);
    let staging = fixture.client.installation.staging_dir();
    let detached = fixture.client.installation.root_path("detached-repair-candidate");
    let hook_staging = staging.clone();
    let hook_detached = detached.clone();
    arm_before_archived_state_repair_publication(move || {
        fs::rename(&hook_staging, &hook_detached).unwrap();
        fs::create_dir(&hook_staging).unwrap();
        fs::write(hook_staging.join("foreign-sentinel"), b"foreign").unwrap();
    });

    let error = fixture
        .client
        .repair_archived_state(
            fixture.empty_candidate(),
            &fixture.repaired,
            fixture.snapshot("ambiguous-package"),
        )
        .unwrap_err();
    let RepairError::PublicationIncomplete { outcome, .. } = repair_error(error) else {
        panic!("substitution must surface a publication ambiguity");
    };
    assert_eq!(outcome, "ambiguous");
    assert_eq!(directory_identity(&fixture.archived_root), old_wrapper);
    assert_eq!(fs::read(staging.join("foreign-sentinel")).unwrap(), b"foreign");
    assert_eq!(
        fs::read_to_string(detached.join("usr/.stateID")).unwrap(),
        fixture.repaired.id.to_string()
    );
}

#[test]
fn substituted_staging_before_preservation_is_ambiguous_and_never_overwritten() {
    let fixture = Fixture::new(true);
    let old_wrapper = directory_identity(&fixture.archived_root);
    let staging = fixture.client.installation.staging_dir();
    let detached = fixture.client.installation.root_path("detached-failed-repair");
    let hook_staging = staging.clone();
    let hook_detached = detached.clone();
    arm_before_archived_state_repair_preservation(move || {
        fs::rename(&hook_staging, &hook_detached).unwrap();
        fs::create_dir(&hook_staging).unwrap();
        fs::write(hook_staging.join("foreign-sentinel"), b"foreign").unwrap();
    });

    let error = fixture
        .client
        .repair_archived_state_with_checkpoint(
            fixture.empty_candidate(),
            &fixture.repaired,
            fixture.snapshot("preservation-race"),
            |point| {
                if point == ArchivedRepairCheckpoint::AfterTransactionTriggers {
                    return Err(Error::Io(std::io::Error::other("force preservation")));
                }
                Ok(())
            },
        )
        .unwrap_err();
    let RepairError::CandidatePreservationIncomplete { outcome, .. } = repair_error(error) else {
        panic!("preservation substitution must remain structured");
    };
    assert_eq!(outcome, "ambiguous");
    assert_eq!(directory_identity(&fixture.archived_root), old_wrapper);
    assert_eq!(fs::read(staging.join("foreign-sentinel")).unwrap(), b"foreign");
    assert_eq!(
        fs::read_to_string(detached.join("usr/.stateID")).unwrap(),
        fixture.repaired.id.to_string()
    );
}

#[test]
fn substituted_roots_parent_proof_is_ambiguous_without_a_rename_or_retry() {
    let fixture = Fixture::new(true);
    let fstree = fixture.empty_candidate();
    let roots = fixture.client.installation.root_path("");
    let detached = fixture
        .client
        .installation
        .root
        .join(".cast/detached-archived-repair-roots");
    let old_wrapper = directory_identity(&fixture.archived_root);
    let candidate_wrapper = Rc::new(Cell::new(None));
    let retries = observe_archived_repair_retries();
    let hook_roots = roots.clone();
    let hook_detached = detached.clone();
    let hook_candidate_wrapper = Rc::clone(&candidate_wrapper);
    arm_before_archived_state_repair_publication(move || {
        hook_candidate_wrapper.set(Some(directory_identity(&hook_roots.join("staging"))));
        fs::rename(&hook_roots, &hook_detached).unwrap();
        fs::create_dir(&hook_roots).unwrap();
        fs::write(hook_roots.join("foreign-roots-sentinel"), b"foreign roots").unwrap();
    });

    let error = fixture
        .client
        .repair_archived_state(fstree, &fixture.repaired, fixture.snapshot("roots-parent-substitution"))
        .unwrap_err();

    assert_ambiguous_publication(error);
    assert_eq!(retries.get(), 0, "namespace uncertainty must not authorize a retry");
    assert_eq!(
        fs::read(roots.join("foreign-roots-sentinel")).unwrap(),
        b"foreign roots"
    );
    assert_eq!(
        directory_identity(&detached.join(fixture.repaired.id.to_string())),
        old_wrapper,
        "the old archive must not be exchanged"
    );
    assert_eq!(
        directory_identity(&detached.join("staging")),
        candidate_wrapper.get().unwrap(),
        "the retained candidate must not be exchanged"
    );
    assert!(
        read_entry_names(&single_repair_path(
            &fixture.client.installation.state_quarantine_dir(),
            fixture.repaired.id,
        ))
        .is_empty()
    );
}

#[test]
fn substituted_quarantine_parent_proof_is_ambiguous_without_a_rename_or_retry() {
    let fixture = Fixture::new(true);
    let fstree = fixture.empty_candidate();
    let quarantine = fixture.client.installation.state_quarantine_dir();
    let detached = fixture
        .client
        .installation
        .root
        .join(".cast/detached-archived-repair-quarantine");
    let old_wrapper = directory_identity(&fixture.archived_root);
    let candidate_wrapper = Rc::new(Cell::new(None));
    let retries = observe_archived_repair_retries();
    let hook_quarantine = quarantine.clone();
    let hook_detached = detached.clone();
    let hook_staging = fixture.client.installation.staging_dir();
    let hook_candidate_wrapper = Rc::clone(&candidate_wrapper);
    arm_before_archived_state_repair_publication(move || {
        hook_candidate_wrapper.set(Some(directory_identity(&hook_staging)));
        fs::rename(&hook_quarantine, &hook_detached).unwrap();
        fs::create_dir(&hook_quarantine).unwrap();
        fs::write(
            hook_quarantine.join("foreign-quarantine-sentinel"),
            b"foreign quarantine",
        )
        .unwrap();
    });

    let error = fixture
        .client
        .repair_archived_state(
            fstree,
            &fixture.repaired,
            fixture.snapshot("quarantine-parent-substitution"),
        )
        .unwrap_err();

    assert_ambiguous_publication(error);
    assert_eq!(retries.get(), 0, "namespace uncertainty must not authorize a retry");
    assert_eq!(
        fs::read(quarantine.join("foreign-quarantine-sentinel")).unwrap(),
        b"foreign quarantine"
    );
    assert_eq!(directory_identity(&fixture.archived_root), old_wrapper);
    assert_eq!(
        directory_identity(&fixture.client.installation.staging_dir()),
        candidate_wrapper.get().unwrap()
    );
    assert!(read_entry_names(&single_repair_path(&detached, fixture.repaired.id)).is_empty());
}

#[test]
fn replacement_content_substitution_is_ambiguous_without_a_rename_or_retry() {
    let fixture = Fixture::new(true);
    let fstree = fixture.empty_candidate();
    let quarantine = fixture.client.installation.state_quarantine_dir();
    let old_wrapper = directory_identity(&fixture.archived_root);
    let candidate_wrapper = Rc::new(Cell::new(None));
    let retries = observe_archived_repair_retries();
    let hook_quarantine = quarantine.clone();
    let hook_staging = fixture.client.installation.staging_dir();
    let hook_candidate_wrapper = Rc::clone(&candidate_wrapper);
    let state = fixture.repaired.id;
    arm_before_archived_state_repair_publication(move || {
        hook_candidate_wrapper.set(Some(directory_identity(&hook_staging)));
        let replacement = single_repair_path(&hook_quarantine, state);
        fs::write(replacement.join("foreign-replacement-sentinel"), b"foreign replacement").unwrap();
    });

    let error = fixture
        .client
        .repair_archived_state(
            fstree,
            &fixture.repaired,
            fixture.snapshot("replacement-content-substitution"),
        )
        .unwrap_err();

    assert_ambiguous_publication(error);
    assert_eq!(retries.get(), 0, "namespace uncertainty must not authorize a retry");
    assert_eq!(directory_identity(&fixture.archived_root), old_wrapper);
    assert_eq!(
        directory_identity(&fixture.client.installation.staging_dir()),
        candidate_wrapper.get().unwrap()
    );
    let replacement = single_repair_path(&quarantine, fixture.repaired.id);
    assert_eq!(
        fs::read(replacement.join("foreign-replacement-sentinel")).unwrap(),
        b"foreign replacement"
    );
}

#[test]
fn replacement_inode_substitution_is_ambiguous_without_a_rename_or_retry() {
    let fixture = Fixture::new(true);
    let fstree = fixture.empty_candidate();
    let quarantine = fixture.client.installation.state_quarantine_dir();
    let detached = fixture
        .client
        .installation
        .root
        .join("detached-archived-repair-replacement");
    let old_wrapper = directory_identity(&fixture.archived_root);
    let candidate_wrapper = Rc::new(Cell::new(None));
    let retries = observe_archived_repair_retries();
    let hook_quarantine = quarantine.clone();
    let hook_detached = detached.clone();
    let hook_staging = fixture.client.installation.staging_dir();
    let hook_candidate_wrapper = Rc::clone(&candidate_wrapper);
    let state = fixture.repaired.id;
    arm_before_archived_state_repair_publication(move || {
        hook_candidate_wrapper.set(Some(directory_identity(&hook_staging)));
        let replacement = single_repair_path(&hook_quarantine, state);
        fs::rename(&replacement, &hook_detached).unwrap();
        fs::create_dir(&replacement).unwrap();
        fs::write(replacement.join("foreign-replacement-sentinel"), b"foreign replacement").unwrap();
    });

    let error = fixture
        .client
        .repair_archived_state(
            fstree,
            &fixture.repaired,
            fixture.snapshot("replacement-inode-substitution"),
        )
        .unwrap_err();

    assert_ambiguous_publication(error);
    assert_eq!(retries.get(), 0, "namespace uncertainty must not authorize a retry");
    assert_eq!(directory_identity(&fixture.archived_root), old_wrapper);
    assert_eq!(
        directory_identity(&fixture.client.installation.staging_dir()),
        candidate_wrapper.get().unwrap()
    );
    assert!(read_entry_names(&detached).is_empty());
    let replacement = single_repair_path(&quarantine, fixture.repaired.id);
    assert_eq!(
        fs::read(replacement.join("foreign-replacement-sentinel")).unwrap(),
        b"foreign replacement"
    );
}

#[test]
fn archived_repair_quarantine_scan_skips_foreign_file_types() {
    let fixture = Fixture::new(true);
    let fstree = fixture.empty_candidate();
    let token = prepare_candidate_marker(&fixture);
    let name = |index| repair_name(&fixture, &token, index);
    let quarantine = fixture.client.installation.state_quarantine_dir();
    fs::write(quarantine.join(name(0)), b"file").unwrap();
    symlink("foreign-target", quarantine.join(name(1))).unwrap();
    mkfifo(&quarantine.join(name(2)), Mode::from_bits_truncate(0o600)).unwrap();
    fs::create_dir(quarantine.join(name(3))).unwrap();

    let publication = fixture
        .client
        .repair_archived_state(fstree, &fixture.repaired, fixture.snapshot("scan-package"))
        .unwrap();
    let ArchivedStateRepairPublication::Replaced { displaced_wrapper } = publication else {
        panic!("existing archive must be replaced");
    };
    let expected_name = name(4);
    assert_eq!(displaced_wrapper.file_name().unwrap(), OsStr::new(&expected_name));
    assert_eq!(fs::read(quarantine.join(name(0))).unwrap(), b"file");
    assert!(quarantine.join(name(1)).is_symlink());
    assert!(
        fs::symlink_metadata(quarantine.join(name(2)))
            .unwrap()
            .file_type()
            .is_fifo()
    );
    assert!(quarantine.join(name(3)).is_dir());
}

#[test]
fn archived_repair_quarantine_exhaustion_preserves_every_namespace() {
    let fixture = Fixture::new(true);
    let fstree = fixture.empty_candidate();
    let token = prepare_candidate_marker(&fixture);
    let quarantine = fixture.client.installation.state_quarantine_dir();
    for index in 0..256 {
        fs::write(quarantine.join(repair_name(&fixture, &token, index)), index.to_string()).unwrap();
    }
    let old_wrapper = directory_identity(&fixture.archived_root);

    let error = fixture
        .client
        .repair_archived_state(fstree, &fixture.repaired, fixture.snapshot("exhausted-package"))
        .unwrap_err();
    assert!(matches!(repair_error(error), RepairError::Preparation { .. }));
    assert_eq!(directory_identity(&fixture.archived_root), old_wrapper);
    assert_eq!(
        fs::read_to_string(fixture.client.installation.staging_path("usr/.stateID")).unwrap(),
        fixture.repaired.id.to_string()
    );
    for index in 0..256 {
        assert_eq!(
            fs::read_to_string(quarantine.join(repair_name(&fixture, &token, index))).unwrap(),
            index.to_string()
        );
    }
    assert_eq!(
        fixture.client.state_db.get(fixture.repaired.id).unwrap(),
        fixture.repaired
    );
}

#[test]
fn externally_completed_cleanup_is_adopted_without_a_second_exchange() {
    let fixture = Fixture::new(true);
    let staging = fixture.client.installation.staging_dir();
    let quarantine = fixture.client.installation.state_quarantine_dir();
    let roots = fixture.client.installation.root_path("");
    let state = fixture.repaired.id;
    arm_before_archived_state_repair_cleanup(move || {
        let replacement = single_repair_path(&quarantine, state);
        exchange_names(&staging, &replacement, &roots.join("external-cleanup-exchange"));
    });

    let publication = fixture
        .client
        .repair_archived_state(
            fixture.empty_candidate(),
            &fixture.repaired,
            fixture.snapshot("external-cleanup"),
        )
        .unwrap();
    assert!(matches!(publication, ArchivedStateRepairPublication::Replaced { .. }));
    assert_repaired_snapshot(&fixture.archived_root, fixture.repaired.id, "external-cleanup");
    assert_exact_empty_private_staging(&fixture.client.installation.staging_dir());
}

#[test]
fn a_layout_change_between_sandwich_reads_is_ambiguous_and_never_reversed() {
    let fixture = Fixture::new(true);
    let staging = fixture.client.installation.staging_dir();
    let archive = fixture.archived_root.clone();
    let old_wrapper = directory_identity(&archive);
    let temporary = fixture.client.installation.root_path("between-layout-read-exchange");
    let hook_staging = staging.clone();
    let hook_archive = archive.clone();
    let hook_temporary = temporary.clone();

    let error = fixture
        .client
        .repair_archived_state_with_checkpoint(
            fixture.empty_candidate(),
            &fixture.repaired,
            fixture.snapshot("layout-sandwich-race"),
            |point| {
                if point == ArchivedRepairCheckpoint::BeforePublication {
                    let staging = hook_staging.clone();
                    let archive = hook_archive.clone();
                    let temporary = hook_temporary.clone();
                    arm_between_archived_state_repair_layout_reads(move || {
                        exchange_names(&staging, &archive, &temporary);
                    });
                }
                Ok(())
            },
        )
        .unwrap_err();

    let RepairError::CandidatePreservationIncomplete { outcome, .. } = repair_error(error) else {
        panic!("a between-read publication race must remain a structured ambiguity");
    };
    assert_eq!(outcome, "applied");
    assert_repaired_snapshot(&archive, fixture.repaired.id, "layout-sandwich-race");
    assert_eq!(directory_identity(&staging), old_wrapper);
    assert_eq!(fs::read(staging.join("opaque-root-sentinel")).unwrap(), b"old-wrapper");
}

#[test]
fn publication_suffix_retry_reconciles_staging_substitution_as_ambiguous() {
    let fixture = Fixture::new(true);
    let staging = fixture.client.installation.staging_dir();
    let detached = fixture.client.installation.root_path("detached-complete-replacement");
    let hook_staging = staging.clone();
    let hook_detached = detached.clone();
    arm_archived_state_repair_faults([FaultPoint::CandidatePostSync]);
    arm_before_archived_state_repair_suffix_retry(move || {
        fs::rename(&hook_staging, &hook_detached).unwrap();
        fs::create_dir(&hook_staging).unwrap();
        fs::write(hook_staging.join("foreign-sentinel"), b"foreign suffix occupant").unwrap();
    });

    let error = fixture
        .client
        .repair_archived_state(
            fixture.empty_candidate(),
            &fixture.repaired,
            fixture.snapshot("publication-suffix-substitution"),
        )
        .unwrap_err();

    let RepairError::PublicationIncomplete { outcome, .. } = repair_error(error) else {
        panic!("a substituted applied suffix must remain a publication ambiguity");
    };
    assert_eq!(outcome, "ambiguous");
    assert_repaired_snapshot(
        &fixture.archived_root,
        fixture.repaired.id,
        "publication-suffix-substitution",
    );
    assert_eq!(
        fs::read(staging.join("foreign-sentinel")).unwrap(),
        b"foreign suffix occupant"
    );
    assert!(read_entry_names(&detached).is_empty());
    let parked = archived_repair_quarantine_paths(&fixture);
    assert_eq!(parked.len(), 1);
    assert_eq!(
        fs::read(parked[0].join("opaque-root-sentinel")).unwrap(),
        b"old-wrapper"
    );
}

#[test]
fn preservation_suffix_retry_reconciles_staging_substitution_as_ambiguous() {
    let fixture = Fixture::new(true);
    let staging = fixture.client.installation.staging_dir();
    let detached = fixture.client.installation.root_path("detached-preserved-replacement");
    let hook_staging = staging.clone();
    let hook_detached = detached.clone();
    arm_archived_state_repair_faults([FaultPoint::PreservedWrapperPostSync]);
    arm_before_archived_state_repair_suffix_retry(move || {
        fs::rename(&hook_staging, &hook_detached).unwrap();
        fs::create_dir(&hook_staging).unwrap();
        fs::write(hook_staging.join("foreign-sentinel"), b"foreign preservation suffix").unwrap();
    });

    let error = fixture
        .client
        .repair_archived_state_with_checkpoint(
            fixture.empty_candidate(),
            &fixture.repaired,
            fixture.snapshot("preservation-suffix-substitution"),
            |point| {
                if point == ArchivedRepairCheckpoint::AfterTransactionTriggers {
                    return Err(Error::Io(std::io::Error::other("force preservation suffix")));
                }
                Ok(())
            },
        )
        .unwrap_err();

    let RepairError::CandidatePreservationIncomplete { outcome, .. } = repair_error(error) else {
        panic!("a substituted preservation suffix must remain a structured ambiguity");
    };
    assert_eq!(outcome, "ambiguous");
    assert_eq!(
        fs::read(staging.join("foreign-sentinel")).unwrap(),
        b"foreign preservation suffix"
    );
    assert!(read_entry_names(&detached).is_empty());
    let parked = archived_repair_quarantine_paths(&fixture);
    assert_eq!(parked.len(), 1);
    assert_repaired_snapshot(&parked[0], fixture.repaired.id, "preservation-suffix-substitution");
    assert_eq!(
        fs::read(fixture.archived_root.join("opaque-root-sentinel")).unwrap(),
        b"old-wrapper"
    );
}

fn prepare_candidate_marker(fixture: &Fixture) -> String {
    record_state_id(&fixture.client.installation.staging_dir(), fixture.repaired.id).unwrap();
    let store = TreeMarkerStore::open_path(&fixture.client.installation.staging_path("usr")).unwrap();
    store
        .adopt_or_create_before_journal()
        .unwrap()
        .token()
        .as_str()
        .to_owned()
}

fn repair_name(fixture: &Fixture, token: &str, index: usize) -> String {
    format!("archived-repair-{}-{token}-{index}", i32::from(fixture.repaired.id))
}

fn single_repair_path(quarantine: &Path, state: crate::state::Id) -> PathBuf {
    let prefix = format!("archived-repair-{}-", i32::from(state));
    let paths = fs::read_dir(quarantine)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with(&prefix))
        })
        .collect::<Vec<_>>();
    assert_eq!(paths.len(), 1);
    paths.into_iter().next().unwrap()
}

fn exchange_names(first: &Path, second: &Path, temporary: &Path) {
    fs::rename(first, temporary).unwrap();
    fs::rename(second, first).unwrap();
    fs::rename(temporary, second).unwrap();
}

fn observe_archived_repair_retries() -> Rc<Cell<usize>> {
    let retries = Rc::new(Cell::new(0));
    let observed = Rc::clone(&retries);
    arm_before_archived_state_repair_suffix_retry(move || observed.set(observed.get() + 1));
    retries
}

fn assert_ambiguous_publication(error: Error) {
    let RepairError::PublicationIncomplete { outcome, .. } = repair_error(error) else {
        panic!("namespace-proof uncertainty must stop publication without preservation");
    };
    assert_eq!(outcome, "ambiguous");
}
