use std::{
    fs,
    os::unix::fs::{MetadataExt as _, PermissionsExt as _},
    path::Path,
};

use super::*;
use super::super::receipt_promotion::*;
use crate::{
    client::{
        active_reblit_bls_renderer::arm_bound_plan_collision_drift,
        active_reblit_boot_namespace_inputs::ActiveReblitBootNamespaceInputError,
        active_reblit_boot_publication_preflight::fixture_assessment::{
            FixtureBootNamespaceAssessment,
            FixtureBootNamespaceAssessmentGuard,
            arm as arm_fixture_boot_namespace_assessments,
            remaining as fixture_boot_namespace_assessments_remaining,
        },
        active_reblit_mounted_boot_topology::{
            ActiveReblitBootPublicationTargetsError,
            ActiveReblitMountedBootTopologyCaptureError,
            BootTargetRole,
            arm_fixture_immutable_leaf_assessments,
            fixture_immutable_leaf_assessments_remaining,
        },
    },
    db::state::{
        BootPublicationReceiptPromotionOutcome,
        BootPublicationReceiptState,
    },
    transition_journal::{Phase, TransitionJournalStore},
};

use super::support::with_staged_alias_attempt;

fn arm_exact_alias_assessments(
    root: &Path,
    count: usize,
) -> FixtureBootNamespaceAssessmentGuard {
    arm_fixture_boot_namespace_assessments(
        (0..count).map(|_| {
            FixtureBootNamespaceAssessment::new(BootTargetRole::Esp, root.to_owned())
        }),
    )
}

fn set_safe_publication_parents(root: &Path, relative_path: &Path) {
    fs::set_permissions(root, fs::Permissions::from_mode(0o755)).unwrap();
    let mut directory = root.to_owned();
    for component in relative_path.parent().unwrap().components() {
        directory.push(component.as_os_str());
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o755)).unwrap();
    }
}

fn assert_promoted_state(
    state: &BootPublicationReceiptState,
    fingerprint: BootPublicationReceiptFingerprint,
) {
    assert_eq!(state.head().committed(), Some(fingerprint));
    assert!(state.head().pending().is_none());
    assert!(state.pending().is_none());
    assert_eq!(state.committed().unwrap().fingerprint(), fingerprint);
}

macro_rules! publish_terminal_alias {
    ($staged:expr, $client:expr, $plan:expr, $root:expr) => {{
        let aggregate = arm_exact_alias_assessments($root, 2);
        let leaf = arm_fixture_immutable_leaf_assessments(
            ($root).to_owned(),
            ($plan).publication_count(),
        );
        let terminal = ($staged)
            .attempt_immutable_boot_publication($client)
            .expect("terminal immutable publication");
        assert_eq!(fixture_boot_namespace_assessments_remaining(), 0);
        assert_eq!(fixture_immutable_leaf_assessments_remaining(), 0);
        drop(leaf);
        drop(aggregate);
        terminal
    }};
}

#[path = "tests/admission.rs"]
mod admission;
#[path = "tests/database_reporting.rs"]
mod database_reporting;
#[path = "tests/fail_stop.rs"]
mod fail_stop;
#[path = "tests/last_boundary.rs"]
mod last_boundary;
#[path = "tests/pre_promotion_integrity.rs"]
mod pre_promotion_integrity;
#[path = "tests/success.rs"]
mod success;
