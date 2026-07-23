use std::{
    ffi::OsStr,
    fs,
    os::unix::fs::{MetadataExt as _, PermissionsExt as _},
    path::{Path, PathBuf},
};

use super::*;
use crate::{
    client::{
        active_reblit_bls_renderer::BoundActiveReblitBlsPublicationPlan,
        active_reblit_boot_publication_preflight::ActiveReblitBootPublicationPreflightError,
        active_reblit_boot_sync_staging::{
            ActiveReblitBootSyncCompletePersistenceError,
            ActiveReblitBootSyncCompleteValidationError,
            ActiveReblitBootSyncCompletionReconciliationError,
            ActiveReblitBootSyncPromotedValidationError,
            DurableActiveReblitBootSyncCompletionRecord,
            arm_before_completion_journal_reopen,
        },
        CoordinatorActiveStateReservation,
    },
    db::state::BootPublicationReceiptPromotionError,
    linux_fs::descriptor_boot_namespace::BootNamespaceDestinationState,
    transition_journal::{
        PublicBindingRevalidationBoundary, TransitionJournalStore,
        arm_next_displaced_unlink_fault, arm_next_temporary_sync_fault,
        arm_next_update_exchange_fault,
        arm_next_update_final_directory_sync_fault,
        arm_next_update_first_directory_sync_fault,
        arm_public_binding_revalidation_callback,
        assert_displaced_unlink_fault_consumed,
        assert_public_binding_revalidation_callback_consumed,
        assert_temporary_sync_fault_consumed,
        assert_update_exchange_fault_consumed,
        assert_update_final_directory_sync_fault_consumed,
        assert_update_first_directory_sync_fault_consumed, encode,
    },
};

macro_rules! promote_alias_for_completion {
    ($staged:expr, $client:expr, $plan:expr, $root:expr) => {{
        let terminal = publish_terminal_alias!($staged, $client, $plan, $root);
        let assessments = arm_exact_alias_assessments($root, 4);
        let promoted = terminal
            .promote_terminal_receipt($client)
            .expect("promote terminal receipt before completion");
        assert_eq!(fixture_boot_namespace_assessments_remaining(), 0);
        drop(assessments);
        promoted
            .try_into_cleaned()
            .expect("exact alias completion fixture requires no cleanup")
    }};
}

macro_rules! publication_snapshot {
    ($plan:expr, $root:expr) => {{
        ($plan)
            .outputs()
            .map(|output| {
                let relative = output.relative_path().to_owned();
                let path = ($root).join(&relative);
                let metadata = fs::metadata(&path).expect("inspect published output");
                (
                    relative,
                    fs::read(&path).expect("read published output"),
                    metadata.permissions().mode(),
                    metadata.ino(),
                )
            })
            .collect::<Vec<_>>()
    }};
}

fn canonical_journal(installation: &Installation) -> PathBuf {
    installation.root.join(".cast/journal/state-transition")
}

fn load_journal_record(installation: &Installation) -> TransitionRecord {
    let cast = installation.retained_mutable_cast_directory().unwrap();
    let journal = TransitionJournalStore::open_in_retained_cast(
        cast,
        &installation.root,
    )
    .unwrap();
    journal
        .load_revalidated_retained_cast(cast)
        .unwrap()
        .expect("completion journal remains present")
}

fn assert_clean_journal_inventory(installation: &Installation) {
    let mut names = fs::read_dir(installation.root.join(".cast/journal"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    names.sort();
    assert_eq!(
        names,
        [
            OsStr::new("state-transition").to_owned(),
            OsStr::new("state-transition.lock").to_owned(),
        ],
        "completion left journal residue",
    );
}

fn replace_file_identity(canonical: &Path, displaced: &Path) {
    let bytes = fs::read(canonical).unwrap();
    let mode = fs::metadata(canonical).unwrap().permissions().mode();
    fs::rename(canonical, displaced).unwrap();
    fs::write(canonical, bytes).unwrap();
    fs::set_permissions(canonical, fs::Permissions::from_mode(mode)).unwrap();
    fs::remove_file(displaced).unwrap();
}

fn first_output_path(
    plan: &impl CompletionTestPlan,
    publication_root: &Path,
) -> PathBuf {
    publication_root.join(plan.first_relative_path())
}

trait CompletionTestPlan {
    fn first_relative_path(&self) -> &Path;
}

impl<'input, 'topology_view, 'topology_authority, 'attempt, 'stone, 'roots>
    CompletionTestPlan
    for BoundActiveReblitBlsPublicationPlan<
        'input,
        'topology_view,
        'topology_authority,
        'attempt,
        'stone,
        'roots,
    >
{
    fn first_relative_path(&self) -> &Path {
        self.outputs()
            .next()
            .expect("completion fixture has outputs")
            .relative_path()
    }
}

fn reset_and_assert_no_legacy_boot_effect() {
    crate::client::boot::reset_boot_synchronize_attempt_count();
}

fn assert_no_legacy_boot_effect() {
    assert_eq!(crate::client::boot::boot_synchronize_attempt_count(), 0);
}

#[path = "completion/success.rs"]
mod success;
#[path = "completion/deadline.rs"]
mod deadline;
#[path = "completion/reconciliation.rs"]
mod reconciliation;
#[path = "completion/drift.rs"]
mod drift;
#[path = "completion/commit_decision.rs"]
mod commit_decision;
#[path = "completion/commit_cleanup.rs"]
mod commit_cleanup;

const EXPECTED_BEHAVIORAL_SCENARIO_COUNT: usize = 36;

#[test]
fn completion_behavioral_scenario_inventory_is_exactly_thirty_six() {
    let module_counts = [
        success::SCENARIO_COUNT,
        deadline::SCENARIO_COUNT,
        reconciliation::SCENARIO_COUNT,
        drift::SCENARIO_COUNT,
        commit_decision::SCENARIO_COUNT,
        commit_cleanup::SCENARIO_COUNT,
    ];
    assert_eq!(module_counts, [3, 1, 7, 10, 11, 4]);
    assert_eq!(
        module_counts.into_iter().sum::<usize>(),
        EXPECTED_BEHAVIORAL_SCENARIO_COUNT,
    );
}
