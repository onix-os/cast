//! One-shot namespace mutation proofs at the final proof-to-syscall boundary.

use std::path::{Path, PathBuf};

use fs_err as fs;

use super::*;
use crate::transition_identity::{
    ArchivedStateRepairNamespaceMove as NamespaceMove, archived_state_repair_namespace_syscall_count,
    arm_before_archived_state_repair_namespace_syscall,
};

#[test]
fn successful_existing_publication_reversal_is_ambiguous_without_a_second_exchange() {
    let fixture = Fixture::new(true);
    let staging = fixture.client.installation.staging_dir();
    let archive = fixture.archived_root.clone();
    let old_wrapper = directory_identity(&archive);
    let temporary = fixture.client.installation.root_path("external-publication-exchange");
    let hook_staging = staging.clone();
    let hook_archive = archive.clone();
    arm_before_archived_state_repair_namespace_syscall(NamespaceMove::PublishExisting, move || {
        exchange_names(&hook_staging, &hook_archive, &temporary);
    });

    let error = fixture
        .client
        .repair_archived_state(
            fixture.empty_candidate(),
            &fixture.repaired,
            fixture.snapshot("existing-publication-reversal"),
        )
        .unwrap_err();

    let RepairError::PublicationIncomplete { outcome, .. } = repair_error(error) else {
        panic!("a successful exchange reconciled to its original layout must be ambiguous");
    };
    assert_eq!(outcome, "ambiguous");
    assert_eq!(
        archived_state_repair_namespace_syscall_count(NamespaceMove::PublishExisting),
        1
    );
    assert_eq!(directory_identity(&archive), old_wrapper);
    assert_repaired_snapshot(&staging, fixture.repaired.id, "existing-publication-reversal");
}

#[test]
fn externally_published_missing_candidate_is_adopted_without_a_second_publish() {
    let fixture = Fixture::new(false);
    let staging = fixture.client.installation.staging_dir();
    let archive = fixture.archived_root.clone();
    let hook_staging = staging.clone();
    let hook_archive = archive.clone();
    arm_before_archived_state_repair_namespace_syscall(NamespaceMove::PublishMissing, move || {
        fs::rename(&hook_staging, &hook_archive).unwrap();
    });

    let publication = fixture
        .client
        .repair_archived_state(
            fixture.empty_candidate(),
            &fixture.repaired,
            fixture.snapshot("missing-publication-race"),
        )
        .unwrap();

    assert_eq!(publication, ArchivedStateRepairPublication::Published);
    assert_eq!(
        archived_state_repair_namespace_syscall_count(NamespaceMove::PublishMissing),
        1
    );
    assert_eq!(
        archived_state_repair_namespace_syscall_count(NamespaceMove::CleanupMissing),
        1
    );
    assert_repaired_snapshot(&archive, fixture.repaired.id, "missing-publication-race");
    assert_exact_empty_private_staging(&staging);
}

#[test]
fn successful_existing_cleanup_reversal_is_ambiguous_without_a_second_exchange() {
    let fixture = Fixture::new(true);
    let staging = fixture.client.installation.staging_dir();
    let archive = fixture.archived_root.clone();
    let quarantine = fixture.client.installation.state_quarantine_dir();
    let state = fixture.repaired.id;
    let temporary = fixture
        .client
        .installation
        .root_path("external-existing-cleanup-exchange");
    let hook_staging = staging.clone();
    arm_before_archived_state_repair_namespace_syscall(NamespaceMove::CleanupExisting, move || {
        let replacement = single_repair_path(&quarantine, state);
        exchange_names(&hook_staging, &replacement, &temporary);
    });

    let error = fixture
        .client
        .repair_archived_state(
            fixture.empty_candidate(),
            &fixture.repaired,
            fixture.snapshot("existing-cleanup-reversal"),
        )
        .unwrap_err();

    let RepairError::PublicationIncomplete { outcome, .. } = repair_error(error) else {
        panic!("a successful cleanup exchange reconciled to its original layout must be ambiguous");
    };
    assert_eq!(outcome, "ambiguous");
    assert_eq!(
        archived_state_repair_namespace_syscall_count(NamespaceMove::CleanupExisting),
        1
    );
    assert_repaired_snapshot(&archive, fixture.repaired.id, "existing-cleanup-reversal");
    assert_eq!(fs::read(staging.join("opaque-root-sentinel")).unwrap(), b"old-wrapper");
}

#[test]
fn externally_completed_missing_cleanup_is_adopted_without_a_second_restore() {
    let fixture = Fixture::new(false);
    let staging = fixture.client.installation.staging_dir();
    let archive = fixture.archived_root.clone();
    let quarantine = fixture.client.installation.state_quarantine_dir();
    let state = fixture.repaired.id;
    let hook_staging = staging.clone();
    arm_before_archived_state_repair_namespace_syscall(NamespaceMove::CleanupMissing, move || {
        let replacement = single_repair_path(&quarantine, state);
        fs::rename(replacement, &hook_staging).unwrap();
    });

    let publication = fixture
        .client
        .repair_archived_state(
            fixture.empty_candidate(),
            &fixture.repaired,
            fixture.snapshot("missing-cleanup-race"),
        )
        .unwrap();

    assert_eq!(publication, ArchivedStateRepairPublication::Published);
    assert_eq!(
        archived_state_repair_namespace_syscall_count(NamespaceMove::CleanupMissing),
        1
    );
    assert_repaired_snapshot(&archive, fixture.repaired.id, "missing-cleanup-race");
    assert_exact_empty_private_staging(&staging);
}

#[test]
fn successful_preservation_reversal_is_ambiguous_without_a_second_exchange() {
    let fixture = Fixture::new(true);
    let staging = fixture.client.installation.staging_dir();
    let archive = fixture.archived_root.clone();
    let old_wrapper = directory_identity(&archive);
    let quarantine = fixture.client.installation.state_quarantine_dir();
    let state = fixture.repaired.id;
    let temporary = fixture.client.installation.root_path("external-preservation-exchange");
    let hook_staging = staging.clone();
    arm_before_archived_state_repair_namespace_syscall(NamespaceMove::PreserveFailedCandidate, move || {
        let replacement = single_repair_path(&quarantine, state);
        exchange_names(&hook_staging, &replacement, &temporary);
    });

    let error = fixture
        .client
        .repair_archived_state_with_checkpoint(
            fixture.empty_candidate(),
            &fixture.repaired,
            fixture.snapshot("preservation-reversal"),
            |point| {
                if point == ArchivedRepairCheckpoint::AfterTransactionTriggers {
                    return Err(Error::Io(std::io::Error::other("force failed-candidate preservation")));
                }
                Ok(())
            },
        )
        .unwrap_err();

    let RepairError::CandidatePreservationIncomplete { outcome, .. } = repair_error(error) else {
        panic!("a successful preservation exchange reconciled to its original layout must be ambiguous");
    };
    assert_eq!(outcome, "ambiguous");
    assert_eq!(
        archived_state_repair_namespace_syscall_count(NamespaceMove::PreserveFailedCandidate),
        1
    );
    assert_eq!(directory_identity(&archive), old_wrapper);
    assert_repaired_snapshot(&staging, fixture.repaired.id, "preservation-reversal");
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
