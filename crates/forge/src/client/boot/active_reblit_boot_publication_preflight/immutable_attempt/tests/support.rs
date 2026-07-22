use super::*;
use std::path::Path;

pub(super) fn set_safe_directory(path: &Path) {
    use std::os::unix::fs::PermissionsExt as _;

    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

pub(super) fn set_safe_publication_parents(
    root: &Path,
    relative_path: &Path,
) {
    set_safe_directory(root);
    let mut directory = root.to_owned();
    for component in relative_path.parent().unwrap().components() {
        directory.push(component.as_os_str());
        set_safe_directory(&directory);
    }
}

pub(super) fn preparing_record() -> TransitionRecord {
    TransitionRecord::preparing(
        TransitionId::parse("0123456789abcdef0123456789abcdef").unwrap(),
        RuntimeEpoch {
            boot_id: BootId::parse("01234567-89ab-4cde-8f01-23456789abcd").unwrap(),
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
            tree_token: TreeToken::parse("b".repeat(TreeToken::TEXT_LENGTH)).unwrap(),
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

pub(super) fn exact_boot_sync_journal(
    installation: &Installation,
) -> (
    TransitionJournalStore,
    TransitionRecord,
    TransitionJournalRecordBinding,
) {
    let cast = installation.retained_mutable_cast_directory().unwrap();
    let journal = TransitionJournalStore::open_in_retained_cast(cast, &installation.root).unwrap();
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

pub(super) fn claim_bindings(
    inventory: &PreparedActiveReblitDesiredPublicationInventory,
) -> Vec<BorrowedActiveReblitBootPublicationProvenanceClaim<'_>> {
    inventory
        .outputs()
        .iter()
        .map(|output| {
            BorrowedActiveReblitBootPublicationProvenanceClaim::new(
                output.root(),
                output.relative_path(),
                BootPublicationSha256::from_bytes(*output.content_identity().as_bytes()),
                BootPublicationOutputProvenanceClaim::UnclaimedAbsent,
            )
        })
        .collect()
}

pub(super) fn staging_client(
    fixture: &render_support::RenderFixture,
    state_db: db::state::Database,
) -> Client {
    let repositories = repository::Manager::with_explicit(
        "immutable-publication-attempt-test",
        repository::Map::default(),
        fixture.installation.clone(),
    )
    .unwrap();
    Client {
        registry: crate::client::build_repository_registry(&repositories),
        install_db: db::meta::Database::new(":memory:").unwrap(),
        state_db,
        layout_db: fixture.layout_db.clone(),
        config: None,
        repositories,
        scope: crate::client::Scope::Stateful,
        installation: fixture.installation.clone(),
    }
}

pub(super) fn assert_pending_boot_sync_started(
    database: &db::state::Database,
    installation: &Installation,
    expected_record: &TransitionRecord,
    fingerprint: BootPublicationReceiptFingerprint,
) {
    let receipt_state = database.boot_publication_receipt_state().unwrap();
    assert_eq!(receipt_state.pending().unwrap().fingerprint(), fingerprint);
    assert!(receipt_state.head().committed().is_none());
    assert_eq!(expected_record.phase, Phase::BootSyncStarted);
    let cast = installation.retained_mutable_cast_directory().unwrap();
    let journal = TransitionJournalStore::open_in_retained_cast(cast, &installation.root).unwrap();
    assert_eq!(
        journal.load_revalidated_retained_cast(cast).unwrap(),
        Some(expected_record.clone()),
    );
}

macro_rules! with_staged_alias_attempt {
    (
        $(before_stage |$setup_client:ident, $setup_plan:ident, $setup_inventory:ident,
            $setup_claims:ident, $setup_predecessor:ident, $setup_deadline:ident| $setup:block,
        )?
        |$fixture:ident, $topology_fixture:ident, $plan:ident, $inventory:ident,
            $client:ident, $staged:ident, $expected_record:ident, $fingerprint:ident| $body:block
    ) => {{
        let deadline = render_support::future_deadline();
        let $fixture = render_support::simple_fixture();
        let $client = support::staging_client(&$fixture, $fixture.state_db.clone());
        let stone = match PreparedActiveReblitStoneBootInputs::prepare_until(
            &$client.installation,
            &$fixture.state_db,
            &$fixture.layout_db,
            &$fixture.head,
            deadline,
        )
        .unwrap()
        {
            ActiveReblitStoneBootInputsOutcome::Ready(stone) => stone,
            ActiveReblitStoneBootInputsOutcome::NotApplicable(reason) => {
                panic!("attempt fixture must be bootable: {reason:?}")
            }
        };
        let roots = PreparedActiveReblitBootStateRoots::prepare_until(
            &$client.installation,
            &$fixture.head_usr,
            $fixture.head.id,
            stone.state_ids(),
            deadline,
        )
        .unwrap();
        let prepared = render_support::prepare_static(&$fixture, &stone, &roots);
        let local_policy = PreparedActiveReblitLocalBootPolicy::prepare_until(
            &$client.installation,
            deadline,
        )
        .unwrap();
        let root_intent = PreparedActiveReblitRootFilesystemIntent::prepare_until(
            &$client.installation,
            deadline,
        )
        .unwrap();
        let inputs = prepared
            .revalidate_until(
                &$fixture.state_db,
                &$fixture.layout_db,
                &$client.installation,
                &local_policy,
                &root_intent,
                deadline,
            )
            .unwrap();
        let $topology_fixture = AliasFixture::stable().expect("alias topology fixture must prepare");
        support::set_safe_directory($topology_fixture.publication_root());
        let topology_prepared = $topology_fixture
            .prepare_for_installation_until(&$client.installation, deadline)
            .unwrap();
        let topology = topology_prepared
            .revalidate_until(&$client.installation, deadline)
            .unwrap();
        let rendered = RenderedActiveReblitBlsRequests::render(&inputs).unwrap();
        let $plan = rendered.into_publication_plan(&topology).unwrap();
        let $inventory = $plan.prepare_desired_publication_inventory().unwrap();
        let claims = support::claim_bindings(&$inventory);
        let (journal, predecessor, binding) =
            support::exact_boot_sync_journal(&$client.installation);
        $({
            let $setup_client = &$client;
            let $setup_plan = &$plan;
            let $setup_inventory = &$inventory;
            let $setup_claims = &claims;
            let $setup_predecessor = &predecessor;
            let $setup_deadline = deadline;
            $setup
        })?
        let $staged = $client
            .stage_active_reblit_boot_sync(
                &$plan,
                &$inventory,
                &claims,
                journal,
                predecessor,
                binding,
            )
            .unwrap();
        let $expected_record = $staged.record().clone();
        let $fingerprint = $staged.receipt_fingerprint();
        $body
        $topology_fixture.assert_outside_unchanged();
    }};
}

pub(super) use with_staged_alias_attempt;
