use std::{fs, os::unix::fs::PermissionsExt as _, time::Duration};

use super::{
    super::{
        ActiveReblitBootTopologyIntentError, BootTopologyIntentBudget, BootTopologyIntentPolicy,
        MAX_BOOT_TOPOLOGY_SOURCE_BYTES, prepare_with_policy_and_checkpoint,
    },
    support::{ESP_PARTUUID, Fixture, authored_alias},
};

#[test]
fn same_byte_source_replacement_before_final_preparation_revalidation_is_rejected() {
    let fixture = Fixture::new();
    fixture.write_alias();
    let source = fixture.source_path();
    let displaced = fixture.root.join("etc/cast/displaced.glu");
    let expected = fs::read(&source).unwrap();
    let result =
        prepare_with_policy_and_checkpoint(&fixture.installation, BootTopologyIntentPolicy::production(), |_| {
            fs::rename(&source, &displaced).unwrap();
            fs::write(&source, &expected).unwrap();
            fs::set_permissions(&source, fs::Permissions::from_mode(0o644)).unwrap();
        });
    assert!(matches!(
        result,
        Err(ActiveReblitBootTopologyIntentError::Changed { .. })
    ));
}

#[test]
fn terminal_rebind_rejects_replacement_after_a_successful_evaluation() {
    let fixture = Fixture::new();
    fixture.write_alias();
    let prepared = fixture.prepare().unwrap();
    let source = fixture.source_path();
    let displaced = fixture.root.join("etc/cast/evaluated.glu");
    let expected = fs::read(&source).unwrap();
    let mut budget =
        BootTopologyIntentBudget::new(&fixture.installation, BootTopologyIntentPolicy::production()).unwrap();
    let result = prepared.revalidate_with_budget_and_checkpoints(
        &fixture.installation,
        &mut budget,
        || {
            fs::rename(&source, &displaced).unwrap();
            fs::write(&source, &expected).unwrap();
            fs::set_permissions(&source, fs::Permissions::from_mode(0o644)).unwrap();
        },
        || {},
    );
    assert!(matches!(
        result,
        Err(ActiveReblitBootTopologyIntentError::Changed { .. })
    ));
}

#[test]
fn second_complete_pass_rejects_same_inode_content_change_between_passes() {
    let fixture = Fixture::new();
    fixture.write_alias();
    let prepared = fixture.prepare().unwrap();
    let source = fixture.source_path();
    let mut budget =
        BootTopologyIntentBudget::new(&fixture.installation, BootTopologyIntentPolicy::production()).unwrap();
    let result = prepared.revalidate_with_budget_and_checkpoints(
        &fixture.installation,
        &mut budget,
        || {},
        || fixture.write_source(authored_alias("22222222-3333-4444-5555-666666666666")),
    );
    assert!(matches!(
        result,
        Err(ActiveReblitBootTopologyIntentError::Changed { .. })
    ));
    assert!(source.is_file());
}

#[test]
fn directory_chain_and_installation_root_substitution_are_rejected() {
    let directory_fixture = Fixture::new();
    directory_fixture.write_alias();
    let prepared = directory_fixture.prepare().unwrap();
    let cast = directory_fixture.root.join("etc/cast");
    let displaced = directory_fixture.root.join("etc/displaced-cast");
    let mut budget =
        BootTopologyIntentBudget::new(&directory_fixture.installation, BootTopologyIntentPolicy::production()).unwrap();
    let result = prepared.revalidate_with_budget_and_checkpoints(
        &directory_fixture.installation,
        &mut budget,
        || {},
        || {
            fs::rename(&cast, &displaced).unwrap();
            fs::create_dir(&cast).unwrap();
            fs::set_permissions(&cast, fs::Permissions::from_mode(0o755)).unwrap();
            fs::write(cast.join("boot-topology.glu"), authored_alias(ESP_PARTUUID)).unwrap();
            fs::set_permissions(cast.join("boot-topology.glu"), fs::Permissions::from_mode(0o644)).unwrap();
        },
    );
    assert!(matches!(
        result,
        Err(ActiveReblitBootTopologyIntentError::Changed { .. }) | Err(ActiveReblitBootTopologyIntentError::Io { .. })
    ));

    let root_fixture = Fixture::new();
    root_fixture.write_alias();
    let prepared = root_fixture.prepare().unwrap();
    let root = root_fixture.root.clone();
    let displaced = root.with_extension("displaced");
    let mut budget =
        BootTopologyIntentBudget::new(&root_fixture.installation, BootTopologyIntentPolicy::production()).unwrap();
    let result = prepared.revalidate_with_budget_and_checkpoints(
        &root_fixture.installation,
        &mut budget,
        || {},
        || {
            fs::rename(&root, &displaced).unwrap();
            fs::create_dir(&root).unwrap();
            fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
        },
    );
    assert!(matches!(
        result,
        Err(ActiveReblitBootTopologyIntentError::Installation(_))
    ));
}

#[test]
fn source_byte_bound_is_inclusive_and_production_ceiling_fails_before_evaluation() {
    let fixture = Fixture::new();
    let source = authored_alias(ESP_PARTUUID);
    fixture.write_source(&source);
    let exact = prepare_with_policy_and_checkpoint(
        &fixture.installation,
        BootTopologyIntentPolicy {
            max_source_bytes: source.len(),
            ..BootTopologyIntentPolicy::production()
        },
        |_| {},
    )
    .unwrap();
    assert_eq!(exact.source_text.len(), source.len());
    assert!(matches!(
        prepare_with_policy_and_checkpoint(
            &fixture.installation,
            BootTopologyIntentPolicy {
                max_source_bytes: source.len() - 1,
                ..BootTopologyIntentPolicy::production()
            },
            |_| {},
        ),
        Err(ActiveReblitBootTopologyIntentError::SourceBytesLimit { limit, actual, .. })
            if limit == source.len() - 1 && actual == source.len() as u64
    ));

    let oversized = Fixture::new();
    oversized.write_source(vec![b'x'; MAX_BOOT_TOPOLOGY_SOURCE_BYTES + 1]);
    assert!(matches!(
        oversized.prepare(),
        Err(ActiveReblitBootTopologyIntentError::SourceBytesLimit {
            limit: MAX_BOOT_TOPOLOGY_SOURCE_BYTES,
            actual,
            ..
        }) if actual == (MAX_BOOT_TOPOLOGY_SOURCE_BYTES + 1) as u64
    ));
}

#[test]
fn work_and_elapsed_time_bounds_are_exact_and_fail_closed() {
    let fixture = Fixture::new();
    fixture.write_alias();
    let observed = fixture.prepare().unwrap().preparation_work();
    let exact = prepare_with_policy_and_checkpoint(
        &fixture.installation,
        BootTopologyIntentPolicy {
            max_work: observed,
            ..BootTopologyIntentPolicy::production()
        },
        |_| {},
    )
    .unwrap();
    assert_eq!(exact.preparation_work(), observed);
    assert!(matches!(
        prepare_with_policy_and_checkpoint(
            &fixture.installation,
            BootTopologyIntentPolicy {
                max_work: observed - 1,
                ..BootTopologyIntentPolicy::production()
            },
            |_| {},
        ),
        Err(ActiveReblitBootTopologyIntentError::WorkLimit { limit, actual, .. })
            if limit == observed - 1 && actual == observed
    ));

    assert!(matches!(
        prepare_with_policy_and_checkpoint(
            &fixture.installation,
            BootTopologyIntentPolicy {
                timeout: Duration::ZERO,
                ..BootTopologyIntentPolicy::production()
            },
            |_| {},
        ),
        Err(ActiveReblitBootTopologyIntentError::DeadlineExceeded { .. })
    ));
    assert!(matches!(
        prepare_with_policy_and_checkpoint(
            &fixture.installation,
            BootTopologyIntentPolicy {
                timeout: Duration::MAX,
                ..BootTopologyIntentPolicy::production()
            },
            |_| {},
        ),
        Err(ActiveReblitBootTopologyIntentError::InvalidDeadline { .. })
    ));
}
