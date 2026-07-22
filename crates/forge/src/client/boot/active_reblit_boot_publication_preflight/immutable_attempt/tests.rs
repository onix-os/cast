use std::fs;

use super::*;
use crate::{
    Installation, db, repository, state,
    boot_publication::{
        BootPublicationOutputProvenanceClaim, BootPublicationReceiptFingerprint,
        BootPublicationSha256,
    },
    client::{
        active_reblit_bls_renderer::RenderedActiveReblitBlsRequests,
        active_reblit_boot_inputs::{
            ActiveReblitStoneBootInputsOutcome, PreparedActiveReblitStoneBootInputs,
        },
        active_reblit_local_boot_policy::PreparedActiveReblitLocalBootPolicy,
        active_reblit_boot_publication_receipt::BorrowedActiveReblitBootPublicationProvenanceClaim,
        active_reblit_boot_render_inputs::PreparedActiveReblitBootRenderInputs,
        active_reblit_root_filesystem_intent::PreparedActiveReblitRootFilesystemIntent,
        active_reblit_desired_publication::PreparedActiveReblitDesiredPublicationInventory,
        active_reblit_mounted_boot_topology::{
            AliasFixture, arm_fixture_immutable_leaf_assessments,
            fixture_immutable_leaf_assessments_remaining,
        },
    },
    linux_fs::mount_namespace::{
        FixtureRetainedBootFilePublicationFault,
        RetainedBootFilePublicationError,
        arm_retained_boot_file_publication_fault,
    },
    state::TransitionId,
    transition_identity::PreparedActiveReblitBootStateRoots,
    transition_journal::{
        BootId, CodecError, MountNamespaceIdentity, Operation, Phase, Previous,
        PreviousOrigin, QuarantineName, RuntimeEpoch, RuntimeTreeIdentity,
        TransitionJournalRecordBinding, TransitionJournalStore, TransitionRecord,
        TreeToken,
    },
};

use super::super::fixture_assessment::{
    FixtureBootNamespaceAssessment, FixtureBootNamespaceMutation,
    arm as arm_fixture_boot_namespace_assessments,
    remaining as fixture_boot_namespace_assessments_remaining,
};

#[path = "../../active_reblit_boot_render_inputs_tests/support.rs"]
mod render_support;
#[path = "tests/support.rs"]
mod support;
#[path = "tests/routing.rs"]
mod routing;
#[path = "tests/integration.rs"]
mod integration;
#[path = "tests/durable_state.rs"]
mod durable_state;
#[path = "tests/failures.rs"]
mod failures;
#[path = "receipt_promotion/tests.rs"]
mod receipt_promotion;
