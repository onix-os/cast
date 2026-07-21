use std::{
    fs,
    os::unix::fs::{MetadataExt as _, PermissionsExt as _},
};

use super::*;
use crate::{
    boot_publication::{
        BootPublicationOutputProvenanceClaim, BootPublicationSha256,
    },
    client::{
        active_reblit_bls_renderer::{
            RenderedActiveReblitBlsRequests, arm_bound_plan_collision_drift,
        },
        active_reblit_boot_inputs::PreparedActiveReblitStoneBootInputs,
        active_reblit_boot_render_inputs::PreparedActiveReblitBootRenderInputs,
        active_reblit_mounted_boot_topology::AliasFixture,
    },
    db, repository,
    state::{self, TransitionId},
    transition_journal::{
        BootId, MountNamespaceIdentity, Previous, PreviousOrigin,
        QuarantineName, RuntimeEpoch, RuntimeTreeIdentity, TreeToken,
        arm_next_displaced_unlink_fault, arm_next_temporary_sync_fault,
        arm_next_update_exchange_fault,
        arm_next_update_final_directory_sync_fault,
        arm_next_update_first_directory_sync_fault,
        assert_displaced_unlink_fault_consumed,
        assert_temporary_sync_fault_consumed,
        assert_update_exchange_fault_consumed,
        assert_update_final_directory_sync_fault_consumed,
        assert_update_first_directory_sync_fault_consumed,
    },
};

#[allow(unused_qualifications)]
#[path = "active_reblit_boot_render_inputs_tests/support.rs"]
mod support;

fn preparing_record() -> TransitionRecord {
    TransitionRecord::preparing(
        TransitionId::parse("0123456789abcdef0123456789abcdef").unwrap(),
        RuntimeEpoch {
            boot_id: BootId::parse(
                "01234567-89ab-4cde-8f01-23456789abcd",
            )
            .unwrap(),
            mount_namespace: MountNamespaceIdentity {
                st_dev: 30,
                inode: 31,
            },
        },
        Operation::ActiveReblit,
        Some(42),
        TreeToken::parse("a".repeat(TreeToken::TEXT_LENGTH)).unwrap(),
        RuntimeTreeIdentity {
            st_dev: 10,
            inode: 11,
            mount_id: 12,
        },
        Previous {
            id: Some(42),
            tree_token: TreeToken::parse(
                "b".repeat(TreeToken::TEXT_LENGTH),
            )
            .unwrap(),
            usr_runtime_identity: RuntimeTreeIdentity {
                st_dev: 10,
                inode: 13,
                mount_id: 12,
            },
            origin: PreviousOrigin::ActiveReblitCorrupt,
        },
        true,
        true,
        QuarantineName::parse("failed-0123456789abcdef").unwrap(),
    )
    .unwrap()
}

fn exact_boot_sync_journal(
    installation: &Installation,
) -> (
    TransitionJournalStore,
    TransitionRecord,
    TransitionJournalRecordBinding,
) {
    let cast = installation.retained_mutable_cast_directory().unwrap();
    let journal = TransitionJournalStore::open_in_retained_cast(
        cast,
        &installation.root,
    )
    .unwrap();
    let mut predecessor = preparing_record();
    journal.create(&predecessor).unwrap();
    loop {
        match predecessor.forward_successor(None) {
            Ok(successor) => {
                journal.advance(&predecessor, &successor).unwrap();
                predecessor = successor;
            }
            Err(CodecError::ExplicitBootSyncStartedSuccessorRequired) => break,
            Err(error) => panic!("construct exact pre-boot record: {error}"),
        }
    }
    assert_eq!(predecessor.phase, Phase::SystemTriggersComplete);
    let binding = journal.record_binding(cast, &predecessor).unwrap();
    (journal, predecessor, binding)
}

fn claim_bindings<'inventory>(
    inventory: &'inventory PreparedActiveReblitDesiredPublicationInventory,
    claim: BootPublicationOutputProvenanceClaim,
) -> Vec<BorrowedActiveReblitBootPublicationProvenanceClaim<'inventory>> {
    inventory
        .outputs()
        .iter()
        .map(|output| {
            BorrowedActiveReblitBootPublicationProvenanceClaim::new(
                output.root(),
                output.relative_path(),
                BootPublicationSha256::from_bytes(
                    *output.content_identity().as_bytes(),
                ),
                claim,
            )
        })
        .collect()
}

fn mismatched_claim_bindings(
    inventory: &PreparedActiveReblitDesiredPublicationInventory,
) -> Vec<BorrowedActiveReblitBootPublicationProvenanceClaim<'_>> {
    inventory
        .outputs()
        .iter()
        .enumerate()
        .map(|(index, output)| {
            let content = if index == 0 {
                BootPublicationSha256::from_bytes([0x99; 32])
            } else {
                BootPublicationSha256::from_bytes(
                    *output.content_identity().as_bytes(),
                )
            };
            BorrowedActiveReblitBootPublicationProvenanceClaim::new(
                output.root(),
                output.relative_path(),
                content,
                BootPublicationOutputProvenanceClaim::UnclaimedAbsent,
            )
        })
        .collect()
}

fn assert_exact_database_receipt(
    database: &Database,
    receipt: &CanonicalBootPublicationReceipt,
) {
    let state = database.boot_publication_receipt_state().unwrap();
    let pending = state.pending().unwrap();
    assert_eq!(pending.fingerprint(), receipt.fingerprint());
    assert_eq!(pending.body(), receipt.body());
    assert_eq!(pending.canonical_body(), receipt.canonical_body());
    assert_eq!(
        state.receipt_pair_for(receipt.body().transition_id()),
        Some(receipt_pair(receipt)),
    );
}

fn assert_exact_journal_record(
    installation: &Installation,
    expected: &TransitionRecord,
) {
    let cast = installation.retained_mutable_cast_directory().unwrap();
    let reopened = TransitionJournalStore::open_in_retained_cast(
        cast,
        &installation.root,
    )
    .unwrap();
    assert_eq!(
        reopened.load_revalidated_retained_cast(cast).unwrap(),
        Some(expected.clone()),
    );
}

fn staging_client(
    fixture: &support::RenderFixture,
    state_db: Database,
) -> Client {
    let repositories = repository::Manager::with_explicit(
        "boot-sync-staging-test",
        repository::Map::default(),
        fixture.installation.clone(),
    )
    .unwrap();
    Client {
        registry: super::super::build_repository_registry(&repositories),
        install_db: db::meta::Database::new(":memory:").unwrap(),
        state_db,
        layout_db: fixture.layout_db.clone(),
        config: None,
        repositories,
        scope: super::super::Scope::Stateful,
        installation: fixture.installation.clone(),
    }
}

macro_rules! with_bound_staging_plan {
    (|$fixture:ident, $plan:ident, $inventory:ident, $claims:ident| $body:block) => {{
        let deadline = support::future_deadline();
        let $fixture = support::simple_fixture();
        let stone = $fixture.stone();
        let roots = $fixture.roots(&stone);
        let prepared = support::prepare_static(&$fixture, &stone, &roots);
        let local_policy = $fixture.local_policy();
        let root_intent = $fixture.root_intent();
        let inputs = prepared
            .revalidate_until(
                &$fixture.state_db,
                &$fixture.layout_db,
                &$fixture.installation,
                &local_policy,
                &root_intent,
                deadline,
            )
            .unwrap();
        let topology_fixture =
            AliasFixture::stable().expect("alias topology fixture must prepare");
        let topology_prepared = topology_fixture
            .prepare_for_installation_until(
                &$fixture.installation,
                deadline,
            )
            .unwrap();
        let topology = topology_prepared
            .revalidate_until(&$fixture.installation, deadline)
            .unwrap();
        let rendered = RenderedActiveReblitBlsRequests::render(&inputs).unwrap();
        let $plan = rendered.into_publication_plan(&topology).unwrap();
        let $inventory = $plan.prepare_desired_publication_inventory().unwrap();
        let $claims = claim_bindings(
            &$inventory,
            BootPublicationOutputProvenanceClaim::UnclaimedAbsent,
        );
        $body
        topology_fixture.assert_outside_unchanged();
    }};
}

macro_rules! with_cross_installation_staging_plan {
    (|$fixture:ident, $plan:ident, $inventory:ident, $claims:ident| $body:block) => {{
        let deadline = support::future_deadline();
        let $fixture = support::simple_fixture();
        let stone = $fixture.stone();
        let roots = $fixture.roots(&stone);
        let prepared = support::prepare_static(&$fixture, &stone, &roots);
        let local_policy = $fixture.local_policy();
        let root_intent = $fixture.root_intent();
        let inputs = prepared
            .revalidate_until(
                &$fixture.state_db,
                &$fixture.layout_db,
                &$fixture.installation,
                &local_policy,
                &root_intent,
                deadline,
            )
            .unwrap();
        let topology_fixture =
            AliasFixture::stable().expect("alias topology fixture must prepare");
        let topology_prepared = topology_fixture.prepare_until(deadline).unwrap();
        let topology = topology_prepared
            .revalidate_until(topology_fixture.installation(), deadline)
            .unwrap();
        let rendered = RenderedActiveReblitBlsRequests::render(&inputs).unwrap();
        let $plan = rendered.into_publication_plan(&topology).unwrap();
        let $inventory = $plan.prepare_desired_publication_inventory().unwrap();
        let $claims = claim_bindings(
            &$inventory,
            BootPublicationOutputProvenanceClaim::UnclaimedAbsent,
        );
        $body
        topology_fixture.assert_outside_unchanged();
    }};
}

#[test]
fn success_derives_and_stages_exact_receipt_then_retains_successor_binding() {
    crate::client::boot::reset_boot_synchronize_attempt_count();
    with_bound_staging_plan!(|fixture, plan, inventory, claims| {
        let client = staging_client(&fixture, fixture.state_db.clone());
        let (journal, predecessor, binding) =
            exact_boot_sync_journal(&fixture.installation);
        let expected = plan
            .prepare_complete_boot_publication_receipt(
                &inventory,
                &predecessor,
                fixture
                    .state_db
                    .boot_publication_receipt_state()
                    .unwrap()
                    .head()
                    .committed(),
                &claims,
            )
            .unwrap();

        let staged = stage_with_retained_stores(
            &fixture.installation,
            &fixture.state_db,
            &plan,
            &inventory,
            &claims,
            journal,
            predecessor,
            binding,
        )
        .unwrap();

        assert_eq!(staged.record().phase, Phase::BootSyncStarted);
        assert_eq!(staged.receipt(), &expected);
        assert_eq!(staged.receipt_fingerprint(), expected.fingerprint());
        assert_eq!(
            staged.record().boot_publication_receipt_correlation().unwrap(),
            Some(receipt_pair(staged.receipt())),
        );
        assert_eq!(
            staged.database_outcome(),
            BootPublicationReceiptStageOutcome::Staged,
        );
        assert_exact_database_receipt(&fixture.state_db, &expected);
        let fresh = staged.revalidate_against(&client).unwrap();
        assert_eq!(fresh.record(), staged.record());
        assert_eq!(fresh.receipt(), staged.receipt());
        assert_eq!(fresh.receipt_fingerprint(), staged.receipt_fingerprint());
        drop(fresh);
        let (journal, record, binding) = staged.into_parts();
        let cast = fixture
            .installation
            .retained_mutable_cast_directory()
            .unwrap();
        assert!(journal.has_record_binding(cast, &binding, &record).unwrap());
        assert_eq!(crate::client::boot::boot_synchronize_attempt_count(), 0);
    });
}

#[test]
fn fresh_revalidation_rejects_a_mixed_client_before_reading_effect_evidence() {
    crate::client::boot::reset_boot_synchronize_attempt_count();
    with_bound_staging_plan!(|fixture, plan, inventory, claims| {
        let mismatched_client = staging_client(
            &fixture,
            Database::new(":memory:").unwrap(),
        );
        let (journal, predecessor, binding) =
            exact_boot_sync_journal(&fixture.installation);
        let expected = plan
            .prepare_complete_boot_publication_receipt(
                &inventory,
                &predecessor,
                None,
                &claims,
            )
            .unwrap();

        let staged = stage_with_retained_stores(
            &fixture.installation,
            &fixture.state_db,
            &plan,
            &inventory,
            &claims,
            journal,
            predecessor,
            binding,
        )
        .unwrap();

        assert!(matches!(
            staged.revalidate_against(&mismatched_client),
            Err(ActiveReblitBootSyncFreshValidationError::ClientCapabilityMismatch),
        ));
        assert_exact_database_receipt(&fixture.state_db, &expected);
        assert_eq!(crate::client::boot::boot_synchronize_attempt_count(), 0);
    });
}

#[test]
fn fresh_revalidation_rejects_successor_inode_drift_without_boot_effects() {
    crate::client::boot::reset_boot_synchronize_attempt_count();
    with_bound_staging_plan!(|fixture, plan, inventory, claims| {
        let client = staging_client(&fixture, fixture.state_db.clone());
        let (journal, predecessor, binding) =
            exact_boot_sync_journal(&fixture.installation);
        let expected = plan
            .prepare_complete_boot_publication_receipt(
                &inventory,
                &predecessor,
                None,
                &claims,
            )
            .unwrap();
        let staged = stage_with_retained_stores(
            &fixture.installation,
            &fixture.state_db,
            &plan,
            &inventory,
            &claims,
            journal,
            predecessor,
            binding,
        )
        .unwrap();
        let successor = staged.record().clone();
        let canonical = fixture
            .installation
            .root
            .join(".cast/journal/state-transition");
        let displaced = fixture
            .installation
            .root
            .join("fresh-validation-displaced-successor");
        let bytes = fs::read(&canonical).unwrap();
        fs::rename(&canonical, &displaced).unwrap();
        fs::write(&canonical, &bytes).unwrap();
        fs::set_permissions(&canonical, fs::Permissions::from_mode(0o600)).unwrap();

        assert!(matches!(
            staged.revalidate_against(&client),
            Err(ActiveReblitBootSyncFreshValidationError::Evidence(
                ActiveReblitBootSyncPostAdvanceValidationError::SuccessorBindingChanged,
            )),
        ));
        assert_exact_database_receipt(&fixture.state_db, &expected);
        assert_eq!(crate::client::boot::boot_synchronize_attempt_count(), 0);
        drop(staged);
        assert_exact_journal_record(&fixture.installation, &successor);
    });
}

#[test]
fn fresh_revalidation_rejects_pending_body_drift_without_boot_effects() {
    crate::client::boot::reset_boot_synchronize_attempt_count();
    with_bound_staging_plan!(|fixture, plan, inventory, claims| {
        let client = staging_client(&fixture, fixture.state_db.clone());
        let (journal, predecessor, binding) =
            exact_boot_sync_journal(&fixture.installation);
        let staged = stage_with_retained_stores(
            &fixture.installation,
            &fixture.state_db,
            &plan,
            &inventory,
            &claims,
            journal,
            predecessor,
            binding,
        )
        .unwrap();
        let successor = staged.record().clone();
        fixture
            .state_db
            .delete_boot_publication_receipt_body_for_test(
                staged.receipt_fingerprint(),
            );

        assert!(matches!(
            staged.revalidate_against(&client),
            Err(ActiveReblitBootSyncFreshValidationError::Evidence(
                ActiveReblitBootSyncPostAdvanceValidationError::ReceiptState(
                    ActiveReblitBootSyncReceiptStateError::Load(
                        BootPublicationReceiptStateError::DanglingReference { .. },
                    ),
                ),
            )),
        ));
        assert_eq!(crate::client::boot::boot_synchronize_attempt_count(), 0);
        drop(staged);
        assert_exact_journal_record(&fixture.installation, &successor);
    });
}

#[test]
fn exact_internally_derived_pre_staged_retry_is_read_only_and_advances() {
    with_bound_staging_plan!(|fixture, plan, inventory, claims| {
        let (journal, predecessor, binding) =
            exact_boot_sync_journal(&fixture.installation);
        let expected = plan
            .prepare_complete_boot_publication_receipt(
                &inventory,
                &predecessor,
                None,
                &claims,
            )
            .unwrap();
        assert_eq!(
            fixture
                .state_db
                .stage_boot_publication_receipt(&expected)
                .unwrap(),
            BootPublicationReceiptStageOutcome::Staged,
        );

        let staged = stage_with_retained_stores(
            &fixture.installation,
            &fixture.state_db,
            &plan,
            &inventory,
            &claims,
            journal,
            predecessor,
            binding,
        )
        .unwrap();

        assert_eq!(
            staged.database_outcome(),
            BootPublicationReceiptStageOutcome::AlreadyStaged,
        );
        assert_eq!(staged.record().phase, Phase::BootSyncStarted);
        assert_exact_database_receipt(&fixture.state_db, &expected);
    });
}

#[test]
fn unbound_provenance_inputs_fail_before_database_or_journal_change() {
    with_bound_staging_plan!(|fixture, plan, inventory, _claims| {
        let bad_claims = mismatched_claim_bindings(&inventory);
        let (journal, predecessor, binding) =
            exact_boot_sync_journal(&fixture.installation);
        let expected_predecessor = predecessor.clone();

        let error = stage_with_retained_stores(
            &fixture.installation,
            &fixture.state_db,
            &plan,
            &inventory,
            &bad_claims,
            journal,
            predecessor,
            binding,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            ActiveReblitBootSyncStagingError::ReceiptMapping(
                ActiveReblitBootPublicationReceiptError::ProvenanceClaimBindingMismatch {
                    index: 0,
                },
            ),
        ));
        assert!(
            fixture
                .state_db
                .boot_publication_receipt_state()
                .unwrap()
                .pending()
                .is_none(),
        );
        assert_exact_journal_record(
            &fixture.installation,
            &expected_predecessor,
        );
    });
}

#[test]
fn cross_installation_bound_plan_is_rejected_before_database_staging() {
    with_cross_installation_staging_plan!(|fixture, plan, inventory, claims| {
        let (journal, predecessor, binding) =
            exact_boot_sync_journal(&fixture.installation);
        let expected_predecessor = predecessor.clone();

        let error = stage_with_retained_stores(
            &fixture.installation,
            &fixture.state_db,
            &plan,
            &inventory,
            &claims,
            journal,
            predecessor,
            binding,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            ActiveReblitBootSyncStagingError::PlanInstallationMismatch,
        ));
        assert!(
            fixture
                .state_db
                .boot_publication_receipt_state()
                .unwrap()
                .pending()
                .is_none(),
        );
        assert_exact_journal_record(
            &fixture.installation,
            &expected_predecessor,
        );
    });
}

#[test]
fn conflicting_internally_derived_pending_receipt_does_not_advance() {
    with_bound_staging_plan!(|fixture, plan, inventory, claims| {
        let conflicting_claims = claim_bindings(
            &inventory,
            BootPublicationOutputProvenanceClaim::ClaimedPublishedByCast,
        );
        let (journal, predecessor, binding) =
            exact_boot_sync_journal(&fixture.installation);
        let expected_predecessor = predecessor.clone();
        let conflict = plan
            .prepare_complete_boot_publication_receipt(
                &inventory,
                &predecessor,
                None,
                &conflicting_claims,
            )
            .unwrap();
        fixture
            .state_db
            .stage_boot_publication_receipt(&conflict)
            .unwrap();

        let error = stage_with_retained_stores(
            &fixture.installation,
            &fixture.state_db,
            &plan,
            &inventory,
            &claims,
            journal,
            predecessor,
            binding,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            ActiveReblitBootSyncStagingError::DatabaseStage(
                BootPublicationReceiptStateError::PendingConflict { .. },
            ),
        ));
        assert_exact_database_receipt(&fixture.state_db, &conflict);
        assert_exact_journal_record(
            &fixture.installation,
            &expected_predecessor,
        );
    });
}

#[test]
fn dangling_pending_body_fails_database_admission_before_advancing() {
    with_bound_staging_plan!(|fixture, plan, inventory, claims| {
        let (journal, predecessor, binding) =
            exact_boot_sync_journal(&fixture.installation);
        let expected_predecessor = predecessor.clone();
        let expected = plan
            .prepare_complete_boot_publication_receipt(
                &inventory,
                &predecessor,
                None,
                &claims,
            )
            .unwrap();
        fixture
            .state_db
            .stage_boot_publication_receipt(&expected)
            .unwrap();
        fixture
            .state_db
            .delete_boot_publication_receipt_body_for_test(
                expected.fingerprint(),
            );

        let error = stage_with_retained_stores(
            &fixture.installation,
            &fixture.state_db,
            &plan,
            &inventory,
            &claims,
            journal,
            predecessor,
            binding,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            ActiveReblitBootSyncStagingError::DatabaseAdmission(
                BootPublicationReceiptStateError::DanglingReference { .. },
            ),
        ));
        assert_exact_journal_record(
            &fixture.installation,
            &expected_predecessor,
        );
    });
}

#[test]
fn every_journal_update_fault_is_classified_as_exact_predecessor_or_successor() {
    type FaultArm = fn();
    type FaultAssert = fn();
    let cases: [
        (
            FaultArm,
            FaultAssert,
            DurableActiveReblitBootSyncRecord,
        );
        5
    ] = [
        (
            arm_next_temporary_sync_fault,
            assert_temporary_sync_fault_consumed,
            DurableActiveReblitBootSyncRecord::Predecessor,
        ),
        (
            arm_next_update_exchange_fault,
            assert_update_exchange_fault_consumed,
            DurableActiveReblitBootSyncRecord::Predecessor,
        ),
        (
            arm_next_update_first_directory_sync_fault,
            assert_update_first_directory_sync_fault_consumed,
            DurableActiveReblitBootSyncRecord::BootSyncStarted,
        ),
        (
            arm_next_displaced_unlink_fault,
            assert_displaced_unlink_fault_consumed,
            DurableActiveReblitBootSyncRecord::BootSyncStarted,
        ),
        (
            arm_next_update_final_directory_sync_fault,
            assert_update_final_directory_sync_fault_consumed,
            DurableActiveReblitBootSyncRecord::BootSyncStarted,
        ),
    ];

    for (arm, assert_consumed, durable) in cases {
        with_bound_staging_plan!(|fixture, plan, inventory, claims| {
            let (journal, predecessor, binding) =
                exact_boot_sync_journal(&fixture.installation);
            let expected = plan
                .prepare_complete_boot_publication_receipt(
                    &inventory,
                    &predecessor,
                    None,
                    &claims,
                )
                .unwrap();
            arm();

            let error = stage_with_retained_stores(
                &fixture.installation,
                &fixture.state_db,
                &plan,
                &inventory,
                &claims,
                journal,
                predecessor,
                binding,
            )
            .unwrap_err();

            assert_consumed();
            assert!(matches!(
                error,
                ActiveReblitBootSyncStagingError::JournalAdvance {
                    durable: actual,
                    ..
                } if actual == durable,
            ));
            assert_exact_database_receipt(&fixture.state_db, &expected);
        });
    }
}

#[test]
fn post_advance_successor_inode_substitution_is_fail_stop_boot_sync_started() {
    with_bound_staging_plan!(|fixture, plan, inventory, claims| {
        let (journal, predecessor, binding) =
            exact_boot_sync_journal(&fixture.installation);
        let expected = plan
            .prepare_complete_boot_publication_receipt(
                &inventory,
                &predecessor,
                None,
                &claims,
            )
            .unwrap();
        let successor = predecessor
            .boot_sync_started_successor(receipt_pair(&expected))
            .unwrap();
        let canonical = fixture
            .installation
            .root
            .join(".cast/journal/state-transition");
        let displaced = fixture
            .installation
            .root
            .join("boot-sync-successor-displaced");
        let callback_canonical = canonical.clone();
        let callback_displaced = displaced.clone();
        arm_after_successful_advance_before_validation(move || {
            let bytes = fs::read(&callback_canonical).unwrap();
            fs::rename(&callback_canonical, &callback_displaced).unwrap();
            fs::write(&callback_canonical, bytes).unwrap();
            fs::set_permissions(
                &callback_canonical,
                fs::Permissions::from_mode(0o600),
            )
            .unwrap();
        });

        let error = stage_with_retained_stores(
            &fixture.installation,
            &fixture.state_db,
            &plan,
            &inventory,
            &claims,
            journal,
            predecessor,
            binding,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            ActiveReblitBootSyncStagingError::PostAdvanceValidation {
                durable: DurableActiveReblitBootSyncRecord::BootSyncStarted,
                validation:
                    ActiveReblitBootSyncPostAdvanceValidationError::SuccessorBindingChanged,
            },
        ));
        assert_eq!(fs::read(&canonical).unwrap(), fs::read(&displaced).unwrap());
        let canonical_metadata = fs::symlink_metadata(&canonical).unwrap();
        let displaced_metadata = fs::symlink_metadata(&displaced).unwrap();
        assert_ne!(
            (canonical_metadata.dev(), canonical_metadata.ino()),
            (displaced_metadata.dev(), displaced_metadata.ino()),
        );
        assert_exact_database_receipt(&fixture.state_db, &expected);
        assert_exact_journal_record(&fixture.installation, &successor);
    });
}

#[test]
fn bound_plan_drift_after_staging_never_reaches_boot_sync_started() {
    with_bound_staging_plan!(|fixture, plan, inventory, claims| {
        let (journal, predecessor, binding) =
            exact_boot_sync_journal(&fixture.installation);
        let expected_predecessor = predecessor.clone();
        let expected = plan
            .prepare_complete_boot_publication_receipt(
                &inventory,
                &predecessor,
                None,
                &claims,
            )
            .unwrap();
        arm_after_receipt_stage_before_final_rederivation(
            arm_bound_plan_collision_drift,
        );

        let error = stage_with_retained_stores(
            &fixture.installation,
            &fixture.state_db,
            &plan,
            &inventory,
            &claims,
            journal,
            predecessor,
            binding,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            ActiveReblitBootSyncStagingError::ReceiptRederivation(
                ActiveReblitBootPublicationReceiptError::CollisionDomainDrift,
            ),
        ));
        assert_exact_database_receipt(&fixture.state_db, &expected);
        assert_exact_journal_record(
            &fixture.installation,
            &expected_predecessor,
        );
    });
}
