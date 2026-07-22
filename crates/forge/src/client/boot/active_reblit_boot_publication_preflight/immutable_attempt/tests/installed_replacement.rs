use std::{
    fs,
    os::{
        fd::AsRawFd as _,
        unix::fs::{MetadataExt as _, PermissionsExt as _},
    },
    path::{Path, PathBuf},
};

use super::*;
use crate::{
    boot_publication::{
        BootPublicationOutputProvenanceClaim, BootPublicationSha256,
    },
    client::{
        active_reblit_bls_renderer::BoundActiveReblitBlsPublication,
        active_reblit_boot_publication_receipt::BorrowedActiveReblitBootPublicationProvenanceClaim,
        active_reblit_installed_boot_publication_delta::{
            ActiveReblitBootPublicationDeltaAction,
            ClassifiedActiveReblitBootPublicationDeltaEntry,
        },
        active_reblit_mounted_boot_topology::AliasFixture,
    },
    db::state::BootPublicationReceiptPromotionOutcome,
    transition_journal::{CodecError, Phase},
};

#[derive(Debug)]
struct MaterializedPriorOutput {
    relative_path: PathBuf,
    bytes: Vec<u8>,
    length: u64,
    xxh3: u128,
    sha256: [u8; 32],
    inode: u64,
}

fn system_triggers_complete(mut record: TransitionRecord) -> TransitionRecord {
    loop {
        match record.forward_successor(None) {
            Ok(successor) => record = successor,
            Err(CodecError::ExplicitBootSyncStartedSuccessorRequired) => {
                assert_eq!(record.phase, Phase::SystemTriggersComplete);
                return record;
            }
            Err(error) => panic!("construct prior receipt predecessor: {error}"),
        }
    }
}

fn read_output_bytes(
    output: &BoundActiveReblitBlsPublication<'_, '_>,
) -> Vec<u8> {
    if let Some(bytes) = output.generated_bytes() {
        return bytes.to_vec();
    }
    let asset = output
        .sealed_asset()
        .unwrap()
        .expect("sealed publication output retains its exact source");
    let mut bytes = vec![0_u8; output.expected_length() as usize];
    let mut offset = 0usize;
    while offset < bytes.len() {
        let read = unsafe {
            nix::libc::pread(
                asset.descriptor().as_raw_fd(),
                bytes[offset..].as_mut_ptr().cast(),
                bytes.len() - offset,
                offset as nix::libc::off_t,
            )
        };
        assert!(read > 0, "read exact sealed publication source");
        offset += read as usize;
    }
    bytes
}

fn materialize_prior_output(
    root: &Path,
    output: &BoundActiveReblitBlsPublication<'_, '_>,
) -> MaterializedPriorOutput {
    let relative_path = output.relative_path().to_owned();
    let destination = root.join(&relative_path);
    fs::create_dir_all(destination.parent().unwrap()).unwrap();
    support::set_safe_publication_parents(root, &relative_path);
    let bytes = read_output_bytes(output);
    fs::write(&destination, &bytes).unwrap();
    fs::set_permissions(
        &destination,
        fs::Permissions::from_mode(output.mode()),
    )
    .unwrap();
    MaterializedPriorOutput {
        relative_path,
        bytes,
        length: output.expected_length(),
        xxh3: output.expected_digest(),
        sha256: *output.expected_content_identity().as_bytes(),
        inode: fs::metadata(destination).unwrap().ino(),
    }
}

fn classified_action_at(
    entries: &[ClassifiedActiveReblitBootPublicationDeltaEntry],
    relative_path: &Path,
) -> ActiveReblitBootPublicationDeltaAction {
    let relative_path = relative_path.to_str().unwrap();
    entries
        .iter()
        .find(|entry| entry.relative_path() == relative_path)
        .unwrap_or_else(|| panic!("classified delta omitted {relative_path}"))
        .action()
}

#[test]
fn authentic_installed_delta_executes_all_desired_actions_and_defers_cleanup() {
    let deadline = render_support::future_deadline();
    let fixture = render_support::RenderFixture::new(
        render_support::StateSpec::one_kernel("6.12"),
        vec![render_support::StateSpec::one_kernel("5.15")],
    );
    let client = support::staging_client(&fixture, fixture.state_db.clone());
    let stone = match PreparedActiveReblitStoneBootInputs::prepare_until(
        &client.installation,
        &fixture.state_db,
        &fixture.layout_db,
        &fixture.head,
        deadline,
    )
    .unwrap()
    {
        ActiveReblitStoneBootInputsOutcome::Ready(stone) => stone,
        ActiveReblitStoneBootInputsOutcome::NotApplicable(reason) => {
            panic!("replacement fixture must be bootable: {reason:?}")
        }
    };
    let topology_fixture =
        AliasFixture::stable().expect("alias topology fixture must prepare");
    support::set_safe_directory(topology_fixture.publication_root());

    let (
        prior_fingerprint,
        prior_head_entry,
        prior_fallback,
        prior_loader_control,
        prior_history_entry,
    ) = {
        let topology_prepared = topology_fixture
            .prepare_for_installation_until(&client.installation, deadline)
            .unwrap();
        let topology = topology_prepared
            .revalidate_until(&client.installation, deadline)
            .unwrap();
        let roots = PreparedActiveReblitBootStateRoots::prepare_until(
            &client.installation,
            &fixture.head_usr,
            fixture.head.id,
            stone.state_ids(),
            deadline,
        )
        .unwrap();
        let prepared = PreparedActiveReblitBootRenderInputs::prepare_until(
            &stone,
            &roots,
            &client.installation,
            deadline,
        )
        .unwrap();
        let local_policy = PreparedActiveReblitLocalBootPolicy::prepare_until(
            &client.installation,
            deadline,
        )
        .unwrap();
        let root_intent = PreparedActiveReblitRootFilesystemIntent::prepare_until(
            &client.installation,
            deadline,
        )
        .unwrap();
        let inputs = prepared
            .revalidate_until(
                &fixture.state_db,
                &fixture.layout_db,
                &client.installation,
                &local_policy,
                &root_intent,
                deadline,
            )
            .unwrap();
        let rendered = RenderedActiveReblitBlsRequests::render(&inputs).unwrap();
        let plan = rendered.into_publication_plan(&topology).unwrap();
        let inventory = plan.prepare_desired_publication_inventory().unwrap();
        let claims = inventory
            .outputs()
            .iter()
            .map(|output| {
                let path = output.relative_path().to_str().unwrap();
                let claim = if path.starts_with("loader/entries/head-6.12-")
                    || path.starts_with("loader/entries/history0-5.15-")
                    || path == "EFI/Boot/BOOTX64.EFI"
                {
                    BootPublicationOutputProvenanceClaim::ClaimedPublishedByCast
                } else if path == "loader/loader.conf" {
                    BootPublicationOutputProvenanceClaim::BorrowedFirstAdoption
                } else {
                    BootPublicationOutputProvenanceClaim::UnclaimedAbsent
                };
                BorrowedActiveReblitBootPublicationProvenanceClaim::new(
                    output.root(),
                    output.relative_path(),
                    BootPublicationSha256::from_bytes(
                        *output.content_identity().as_bytes(),
                    ),
                    claim,
                )
            })
            .collect::<Vec<_>>();
        let mut prior_predecessor = support::preparing_record();
        prior_predecessor.transition_id = TransitionId::parse(
            "fedcba9876543210fedcba9876543210",
        )
        .unwrap();
        let prior_predecessor = system_triggers_complete(prior_predecessor);
        let receipt = plan
            .prepare_complete_boot_publication_receipt(
                &inventory,
                &prior_predecessor,
                None,
                &claims,
            )
            .unwrap();
        fixture
            .state_db
            .stage_boot_publication_receipt(&receipt)
            .unwrap();
        assert_eq!(
            fixture
                .state_db
                .promote_boot_publication_receipt(&receipt, deadline)
                .unwrap(),
            BootPublicationReceiptPromotionOutcome::Promoted,
        );

        let mut head_entry = None;
        let mut fallback = None;
        let mut loader_control = None;
        let mut history_entry = None;
        for output in plan.outputs() {
            let path = output.relative_path().to_str().unwrap();
            let slot = if path.starts_with("loader/entries/head-6.12-") {
                Some(&mut head_entry)
            } else if path == "EFI/Boot/BOOTX64.EFI" {
                Some(&mut fallback)
            } else if path == "loader/loader.conf" {
                Some(&mut loader_control)
            } else if path.starts_with("loader/entries/history0-5.15-") {
                Some(&mut history_entry)
            } else {
                None
            };
            if let Some(slot) = slot {
                assert!(slot.is_none(), "selected prior output twice: {path}");
                *slot = Some(materialize_prior_output(
                    topology_fixture.publication_root(),
                    &output,
                ));
            }
        }
        (
            receipt.fingerprint(),
            head_entry.expect("prior plan has a head entry"),
            fallback.expect("prior plan has a fallback bootloader"),
            loader_control.expect("prior plan has loader control"),
            history_entry.expect("prior plan has a history entry"),
        )
    };

    fixture.exclude_history(0);
    fixture.write_local("40-generation.cmdline", b"generation=b");
    let topology_prepared = topology_fixture
        .prepare_for_installation_until(&client.installation, deadline)
        .unwrap();
    let topology = topology_prepared
        .revalidate_until(&client.installation, deadline)
        .unwrap();
    let roots = PreparedActiveReblitBootStateRoots::prepare_until(
        &client.installation,
        &fixture.head_usr,
        fixture.head.id,
        stone.state_ids(),
        deadline,
    )
    .unwrap();
    let prepared = PreparedActiveReblitBootRenderInputs::prepare_until(
        &stone,
        &roots,
        &client.installation,
        deadline,
    )
    .unwrap();
    let local_policy = PreparedActiveReblitLocalBootPolicy::prepare_until(
        &client.installation,
        deadline,
    )
    .unwrap();
    let root_intent = PreparedActiveReblitRootFilesystemIntent::prepare_until(
        &client.installation,
        deadline,
    )
    .unwrap();
    let inputs = prepared
        .revalidate_until(
            &fixture.state_db,
            &fixture.layout_db,
            &client.installation,
            &local_policy,
            &root_intent,
            deadline,
        )
        .unwrap();
    let rendered = RenderedActiveReblitBlsRequests::render(&inputs).unwrap();
    let plan = rendered.into_publication_plan(&topology).unwrap();
    let inventory = plan.prepare_desired_publication_inventory().unwrap();
    let replacement_output = plan
        .outputs()
        .find(|output| output.relative_path() == prior_head_entry.relative_path)
        .expect("new plan retains the head entry path");
    let replacement_bytes = read_output_bytes(&replacement_output);
    assert_ne!(replacement_bytes, prior_head_entry.bytes);
    let replacement_length = replacement_output.expected_length();
    let replacement_xxh3 = replacement_output.expected_digest();
    let replacement_sha256 =
        *replacement_output.expected_content_identity().as_bytes();
    drop(replacement_output);

    let (journal, predecessor, binding) =
        support::exact_boot_sync_journal(&client.installation);
    let staging_assessment = arm_fixture_boot_namespace_assessments([
        FixtureBootNamespaceAssessment::new(
            BootTargetRole::Esp,
            topology_fixture.publication_root().to_owned(),
        ),
    ]);
    let staged = client
        .stage_active_reblit_boot_sync(
            &plan,
            &inventory,
            journal,
            predecessor,
            binding,
        )
        .unwrap();
    assert_eq!(fixture_boot_namespace_assessments_remaining(), 0);
    drop(staging_assessment);
    assert_eq!(
        staged.receipt().body().committed_predecessor(),
        Some(prior_fingerprint),
    );
    let expected_record = staged.record().clone();
    let pending_fingerprint = staged.receipt_fingerprint();
    {
        let fresh = staged.revalidate_against(&client).unwrap();
        let entries = fresh.classified_delta().entries();
        assert_eq!(
            classified_action_at(entries, &prior_head_entry.relative_path),
            ActiveReblitBootPublicationDeltaAction::ReplaceOwnedDesired,
        );
        assert_eq!(
            classified_action_at(entries, &prior_fallback.relative_path),
            ActiveReblitBootPublicationDeltaAction::RetainOwnedDesired,
        );
        assert_eq!(
            classified_action_at(entries, &prior_loader_control.relative_path),
            ActiveReblitBootPublicationDeltaAction::PreserveBorrowedDesired,
        );
        assert_eq!(
            classified_action_at(entries, &prior_history_entry.relative_path),
            ActiveReblitBootPublicationDeltaAction::DeleteOwnedStaleAfterPromotion,
        );
        assert!(entries.iter().any(|entry| {
            entry.action()
                == ActiveReblitBootPublicationDeltaAction::PublishDesired
        }));
    }

    let root = topology_fixture.publication_root().to_owned();
    let aggregate = arm_fixture_boot_namespace_assessments(
        (0..3).map(|_| {
            FixtureBootNamespaceAssessment::new(BootTargetRole::Esp, root.clone())
        }),
    );
    let replacement_assessment =
        arm_fixture_owned_replacement_assessments(root.clone(), 1, 4);
    let leaf = arm_fixture_immutable_leaf_assessments(
        root.clone(),
        plan.publication_count() - 1,
    );
    let terminal = staged
        .attempt_immutable_boot_publication(&client)
        .unwrap();
    assert_eq!(fixture_boot_namespace_assessments_remaining(), 0);
    assert_eq!(fixture_immutable_leaf_assessments_remaining(), 0);
    assert_eq!(fixture_owned_replacement_assessments_remaining(), 0);
    assert_eq!(fixture_owned_replacement_validations_remaining(), 4);
    drop(leaf);
    drop(aggregate);

    assert_eq!(terminal.publication_count(), plan.publication_count());
    assert_eq!(terminal.replaced_count(), 1);
    assert_eq!(terminal.already_exact_count(), 2);
    assert_eq!(terminal.published_count(), plan.publication_count() - 3);
    assert_eq!(terminal.evidence().len(), plan.publication_count());
    assert!(terminal.promoted_cleanup_required());
    let mut replacement_index = None;
    for (index, (evidence, output)) in
        terminal.evidence().iter().zip(plan.outputs()).enumerate()
    {
        assert_eq!(evidence.plan_index(), index);
        let expected_action = if output.relative_path()
            == prior_head_entry.relative_path
        {
            replacement_index = Some(index);
            ActiveReblitBootPublicationDeltaAction::ReplaceOwnedDesired
        } else if output.relative_path() == prior_fallback.relative_path {
            ActiveReblitBootPublicationDeltaAction::RetainOwnedDesired
        } else if output.relative_path() == prior_loader_control.relative_path {
            ActiveReblitBootPublicationDeltaAction::PreserveBorrowedDesired
        } else {
            ActiveReblitBootPublicationDeltaAction::PublishDesired
        };
        assert_eq!(evidence.action(), expected_action);
        assert_ne!(
            evidence.action(),
            ActiveReblitBootPublicationDeltaAction::DeleteOwnedStaleAfterPromotion,
        );
    }
    let replacement_index = replacement_index.expect("replacement evidence exists");
    let (
        sidecar_leaf,
        installed_inode,
        replacement_inode,
        replacement_owner,
    ) = {
        let evidence = &terminal.evidence()[replacement_index];
        assert_eq!(evidence.installed_length(), Some(prior_head_entry.length));
        assert_eq!(evidence.installed_xxh3(), Some(prior_head_entry.xxh3));
        assert_eq!(evidence.installed_sha256(), Some(prior_head_entry.sha256));
        assert_eq!(evidence.length(), replacement_length);
        assert_eq!(evidence.xxh3(), replacement_xxh3);
        assert_eq!(evidence.sha256(), replacement_sha256);
        assert!(evidence.owner_matches_receipt(pending_fingerprint));
        let authority = evidence
            .replacement_authority()
            .expect("replacement retains exact authority");
        (
            authority.sidecar_leaf().to_owned(),
            authority.installed_file_inode(),
            authority.replacement_file_inode(),
            authority.owner(),
        )
    };
    assert_eq!(replacement_owner.as_bytes(), *pending_fingerprint.as_bytes());
    assert_eq!(installed_inode, prior_head_entry.inode);
    let canonical = root.join(&prior_head_entry.relative_path);
    let sidecar = canonical.parent().unwrap().join(&sidecar_leaf);
    assert_eq!(fs::read(&canonical).unwrap(), replacement_bytes);
    assert_eq!(fs::metadata(&canonical).unwrap().ino(), replacement_inode);
    assert_eq!(fs::read(&sidecar).unwrap(), prior_head_entry.bytes);
    assert_eq!(fs::metadata(&sidecar).unwrap().ino(), installed_inode);
    let stale = root.join(&prior_history_entry.relative_path);
    assert_eq!(fs::read(&stale).unwrap(), prior_history_entry.bytes);
    assert_eq!(fs::metadata(&stale).unwrap().ino(), prior_history_entry.inode);
    assert_eq!(
        terminal
            .staged
            .revalidate_against(&client)
            .unwrap()
            .record(),
        &expected_record,
    );

    let promotion_assessments = arm_fixture_boot_namespace_assessments(
        (0..4).map(|_| {
            FixtureBootNamespaceAssessment::new(BootTargetRole::Esp, root.clone())
        }),
    );
    let promoted = terminal.promote_terminal_receipt(&client).unwrap();
    assert_eq!(fixture_boot_namespace_assessments_remaining(), 0);
    assert_eq!(fixture_owned_replacement_validations_remaining(), 0);
    drop(promotion_assessments);
    drop(replacement_assessment);
    assert_eq!(
        promoted.database_outcome(),
        BootPublicationReceiptPromotionOutcome::Promoted,
    );
    assert_eq!(promoted.receipt_fingerprint(), pending_fingerprint);
    assert_eq!(promoted.replaced_count(), 1);
    assert!(promoted.promoted_cleanup_required());
    let promoted_authority = promoted.evidence()[replacement_index]
        .replacement_authority()
        .expect("promotion retains replacement authority");
    assert_eq!(promoted_authority.sidecar_leaf(), sidecar_leaf);
    assert_eq!(promoted_authority.installed_file_inode(), installed_inode);
    assert_eq!(promoted_authority.replacement_file_inode(), replacement_inode);
    assert_eq!(promoted_authority.owner(), replacement_owner);
    let receipt_state = fixture.state_db.boot_publication_receipt_state().unwrap();
    assert_eq!(receipt_state.head().committed(), Some(pending_fingerprint));
    assert!(receipt_state.head().pending().is_none());
    assert!(receipt_state.pending().is_none());
    assert_eq!(
        receipt_state.committed().unwrap().fingerprint(),
        pending_fingerprint,
    );
    assert_eq!(fs::metadata(&sidecar).unwrap().ino(), installed_inode);
    assert_eq!(fs::metadata(&stale).unwrap().ino(), prior_history_entry.inode);

    let journal_path = fixture
        .installation
        .root
        .join(".cast/journal/state-transition");
    let journal_inode = fs::metadata(&journal_path).unwrap().ino();
    let promoted = match promoted.try_into_cleaned() {
        Err(promoted) => promoted,
        Ok(_) => panic!("replacement and owned-stale cleanup must remain mandatory"),
    };
    assert_eq!(promoted.replaced_count(), 1);
    assert!(promoted.promoted_cleanup_required());
    let retained_authority = promoted.evidence()[replacement_index]
        .replacement_authority()
        .expect("rejected cleanup conversion retains replacement authority");
    assert_eq!(retained_authority.sidecar_leaf(), sidecar_leaf);
    assert_eq!(retained_authority.installed_file_inode(), installed_inode);
    assert_eq!(retained_authority.replacement_file_inode(), replacement_inode);
    assert_eq!(retained_authority.owner(), replacement_owner);
    assert_eq!(fs::metadata(&journal_path).unwrap().ino(), journal_inode);
    assert_eq!(fs::metadata(&sidecar).unwrap().ino(), installed_inode);
    assert_eq!(fs::read(&sidecar).unwrap(), prior_head_entry.bytes);
    assert_eq!(fs::metadata(&stale).unwrap().ino(), prior_history_entry.inode);
    assert_eq!(fs::read(&stale).unwrap(), prior_history_entry.bytes);
    drop(promoted);
    topology_fixture.assert_outside_unchanged();
}
