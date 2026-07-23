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
    exact_boot_sync_journal_for_state(installation, None)
}

pub(super) fn exact_boot_sync_journal_for_state(
    installation: &Installation,
    state: Option<state::Id>,
) -> (
    TransitionJournalStore,
    TransitionRecord,
    TransitionJournalRecordBinding,
) {
    let mut predecessor = preparing_record();
    if let Some(state) = state {
        predecessor.candidate.id = Some(i32::from(state));
        predecessor.previous.id = Some(i32::from(state));
    }
    persist_exact_boot_sync_journal(installation, predecessor, 0)
}

pub(super) fn exact_boot_sync_journal_for_commit_decision_route(
    installation: &Installation,
    state: state::Id,
    run_system_triggers: bool,
    pre_boot_generation_offset: u64,
) -> (
    TransitionJournalStore,
    TransitionRecord,
    TransitionJournalRecordBinding,
) {
    let candidate_store = crate::tree_marker::TreeMarkerStore::open_path(
        &installation.root.join("usr"),
    )
    .unwrap();
    let candidate_marker = candidate_store.adopt_or_create_before_journal().unwrap();
    let previous_store = crate::tree_marker::TreeMarkerStore::open_path(
        &installation.staging_path("usr"),
    )
    .unwrap();
    let previous_marker = previous_store.adopt_or_create_before_journal().unwrap();

    let mut predecessor = preparing_record();
    predecessor.options.run_system_triggers = run_system_triggers;
    predecessor.creation_epoch = RuntimeEpoch::capture().unwrap();
    predecessor.candidate.id = Some(i32::from(state));
    predecessor.candidate.tree_token = candidate_marker.token().clone();
    predecessor.candidate.usr_runtime_identity =
        RuntimeTreeIdentity::capture_directory(candidate_store.retained_directory()).unwrap();
    predecessor.previous.id = Some(i32::from(state));
    predecessor.previous.tree_token = previous_marker.token().clone();
    predecessor.previous.usr_runtime_identity =
        RuntimeTreeIdentity::capture_directory(previous_store.retained_directory()).unwrap();
    persist_exact_boot_sync_journal(
        installation,
        predecessor,
        pre_boot_generation_offset,
    )
}

fn persist_exact_boot_sync_journal(
    installation: &Installation,
    mut predecessor: TransitionRecord,
    pre_boot_generation_offset: u64,
) -> (
    TransitionJournalStore,
    TransitionRecord,
    TransitionJournalRecordBinding,
) {
    let cast = installation.retained_mutable_cast_directory().unwrap();
    let journal = TransitionJournalStore::open_in_retained_cast(cast, &installation.root).unwrap();
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
    let expected_phase = if predecessor.options.run_system_triggers {
        Phase::SystemTriggersComplete
    } else {
        Phase::RootLinksComplete
    };
    assert_eq!(predecessor.phase, expected_phase);
    if pre_boot_generation_offset != 0 {
        predecessor.generation = predecessor
            .generation
            .checked_add(pre_boot_generation_offset)
            .unwrap();
        fs::write(
            installation.root.join(".cast/journal/state-transition"),
            crate::transition_journal::encode(&predecessor).unwrap(),
        )
        .unwrap();
    }
    let binding = journal.record_binding(cast, &predecessor).unwrap();
    (journal, predecessor, binding)
}

pub(super) fn commit_decision_fixture() -> render_support::RenderFixture {
    let mut fixture = render_support::simple_fixture();
    let source = preparing_record();
    let exact = fixture
        .state_db
        .add_with_transition(
            &source.transition_id,
            &fixture.head.selections,
            Some("commit decision head"),
            None,
        )
        .unwrap();
    let provenance = db::state::MetadataProvenance::from_outputs(
        b"NAME=Commit Decision Test\nID=commit-decision-test\n",
        b"let system = { hostname = \"commit-decision-test\" } in system\n",
    );
    fixture
        .state_db
        .insert_fresh_metadata_provenance_if_transition_matches(
            exact.id,
            &source.transition_id,
            &provenance,
        )
        .unwrap();
    fixture
        .state_db
        .clear_transition_if_matches(exact.id, &source.transition_id)
        .unwrap();
    fixture.state_db.remove(&fixture.head.id).unwrap();

    fs::write(
        fixture.installation.root.join("usr/.stateID"),
        exact.id.to_string(),
    )
    .unwrap();
    fixture.installation.active_state = Some(exact.id);
    fixture.head = exact;

    let staging_usr = fixture.installation.staging_path("usr");
    fs::create_dir(&staging_usr).unwrap();
    set_safe_directory(&staging_usr);
    fs::write(staging_usr.join(".stateID"), fixture.head.id.to_string()).unwrap();
    let previous_store = crate::tree_marker::TreeMarkerStore::open_path(&staging_usr).unwrap();
    let previous_marker = previous_store.adopt_or_create_before_journal().unwrap();

    let replacement = fixture.installation.state_quarantine_dir().join(format!(
        "replaced-active-reblit-wrapper-{}-{}-0",
        i32::from(fixture.head.id),
        previous_marker.token().as_str(),
    ));
    fs::create_dir(&replacement).unwrap();
    use std::os::unix::fs::{PermissionsExt as _, symlink};
    fs::set_permissions(&replacement, fs::Permissions::from_mode(0o700)).unwrap();

    const ROOT_ABI: [(&str, &str); 5] = [
        ("bin", "usr/bin"),
        ("sbin", "usr/sbin"),
        ("lib", "usr/lib"),
        ("lib32", "usr/lib32"),
        ("lib64", "usr/lib"),
    ];
    for (name, target) in ROOT_ABI {
        symlink(target, fixture.installation.root.join(name)).unwrap();
        symlink(
            target,
            fixture.installation.isolation_dir().join(name),
        )
        .unwrap();
    }
    fixture
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
            $setup_claims:ident, $setup_predecessor:ident, $setup_deadline:ident
            $(, $setup_topology_fixture:ident)?| $setup:block,
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
        #[allow(unused_variables)]
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
            $(let $setup_topology_fixture = &$topology_fixture;)?
            $setup
        })?
        let staging_preflight = arm_fixture_boot_namespace_assessments([
            FixtureBootNamespaceAssessment::new(
                BootTargetRole::Esp,
                $topology_fixture.publication_root().to_owned(),
            ),
        ]);
        let $staged = $client
            .stage_active_reblit_boot_sync(
                &$plan,
                &$inventory,
                journal,
                predecessor,
                binding,
            )
            .unwrap();
        drop(staging_preflight);
        let $expected_record = $staged.record().clone();
        let $fingerprint = $staged.receipt_fingerprint();
        $body
        $topology_fixture.assert_outside_unchanged();
    }};
}

pub(super) use with_staged_alias_attempt;
