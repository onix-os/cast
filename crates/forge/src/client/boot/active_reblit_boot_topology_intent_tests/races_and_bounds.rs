use std::{
    cell::Cell,
    fs,
    os::unix::fs::PermissionsExt as _,
    rc::Rc,
    time::{Duration, Instant},
};

use super::{
    super::{
        ActiveReblitBootTopologyIntentError, BootTopologyIntentBudget, BootTopologyIntentPolicy,
        MAX_BOOT_TOPOLOGY_SOURCE_BYTES, PreparedActiveReblitBootTopologyIntent, prepare_with_policy_and_checkpoint,
        prepare_with_policy_until_and_clock,
    },
    support::{ESP_PARTUUID, Fixture, authored_alias, authored_alias_at},
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
        || {},
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
        || {},
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
        || {},
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
fn mount_selector_byte_component_byte_and_component_count_bounds_are_exact() {
    let exact_total = mount_selector_with_total_bytes(4_095);
    let over_total = mount_selector_with_total_bytes(4_096);
    assert_eq!(exact_total.len(), 4_095);
    assert_eq!(over_total.len(), 4_096);
    assert_mount_selector_accepted(&exact_total);
    assert_mount_selector_rejected(&over_total, 4_096);

    let exact_component = format!("/synthetic/{}", "c".repeat(255));
    let over_component = format!("/synthetic/{}", "c".repeat(256));
    assert_mount_selector_accepted(&exact_component);
    assert_mount_selector_rejected(&over_component, over_component.len());

    let exact_components = format!("/{}", vec!["c"; 128].join("/"));
    let over_components = format!("/{}", vec!["c"; 129].join("/"));
    assert_mount_selector_accepted(&exact_components);
    assert_mount_selector_rejected(&over_components, over_components.len());
}

fn mount_selector_with_total_bytes(total: usize) -> String {
    let mut remaining = total - 1;
    let mut components = Vec::new();
    while remaining > 0 {
        let component_bytes = remaining.min(255);
        components.push("c".repeat(component_bytes));
        remaining -= component_bytes;
        if remaining > 0 {
            remaining -= 1;
        }
    }
    format!("/{}", components.join("/"))
}

fn assert_mount_selector_accepted(mount_point: &str) {
    let fixture = Fixture::new();
    fixture.write_source(authored_alias_at(ESP_PARTUUID, mount_point));
    fixture.prepare().unwrap();
}

fn assert_mount_selector_rejected(mount_point: &str, actual_bytes: usize) {
    let fixture = Fixture::new();
    fixture.write_source(authored_alias_at(ESP_PARTUUID, mount_point));
    assert!(matches!(
        fixture.prepare(),
        Err(ActiveReblitBootTopologyIntentError::InvalidMountPointSelector {
            field: "esp.mount_point",
            actual_bytes: actual,
            ..
        }) if actual == actual_bytes
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

#[test]
fn caller_owned_deadline_is_rejected_at_prepare_and_revalidate_entry() {
    let fixture = Fixture::new();
    fixture.write_alias();
    let deadline = Instant::now() - Duration::from_nanos(1);

    let prepare_error = PreparedActiveReblitBootTopologyIntent::prepare_until(&fixture.installation, deadline)
        .err()
        .expect("an expired caller deadline must reject preparation at entry");
    assert!(matches!(
        prepare_error,
        ActiveReblitBootTopologyIntentError::DeadlineExceeded {
            deadline: actual,
            remaining_at_admission: Duration::ZERO,
            ..
        } if actual == deadline
    ));

    let prepared = fixture.prepare().unwrap();
    let revalidate_error = prepared
        .revalidate_until(&fixture.installation, deadline)
        .err()
        .expect("an expired caller deadline must reject revalidation at entry");
    assert!(matches!(
        revalidate_error,
        ActiveReblitBootTopologyIntentError::DeadlineExceeded {
            deadline: actual,
            remaining_at_admission: Duration::ZERO,
            ..
        } if actual == deadline
    ));
}

#[test]
fn caller_owned_deadline_is_rechecked_at_prepare_and_revalidate_completion() {
    let fixture = Fixture::new();
    fixture.write_alias();
    let deadline = Instant::now() + Duration::from_secs(120);
    let admitted_at = deadline - Duration::from_secs(60);

    let prepare_complete = Rc::new(Cell::new(false));
    let prepare_hook_state = Rc::clone(&prepare_complete);
    let prepare_clock_state = Rc::clone(&prepare_complete);
    let prepare_error = prepare_with_policy_until_and_clock(
        &fixture.installation,
        BootTopologyIntentPolicy::production(),
        deadline,
        move || prepare_hook_state.set(true),
        move || terminal_deadline_time(&prepare_clock_state, admitted_at, deadline),
    )
    .err()
    .expect("deadline expiry at the explicit preparation completion check must fail closed");
    assert!(prepare_complete.get());
    assert!(matches!(
        prepare_error,
        ActiveReblitBootTopologyIntentError::DeadlineExceeded {
            deadline: actual,
            remaining_at_admission,
            ..
        } if actual == deadline && remaining_at_admission == Duration::from_secs(60)
    ));

    let prepared = fixture.prepare().unwrap();
    let revalidate_complete = Rc::new(Cell::new(false));
    let revalidate_clock_state = Rc::clone(&revalidate_complete);
    let mut budget = BootTopologyIntentBudget::new_until_with_clock(
        &fixture.installation,
        BootTopologyIntentPolicy::production(),
        deadline,
        move || terminal_deadline_time(&revalidate_clock_state, admitted_at, deadline),
    )
    .unwrap();
    let revalidate_hook_state = Rc::clone(&revalidate_complete);
    let revalidate_error = prepared
        .revalidate_with_budget_and_checkpoints(
            &fixture.installation,
            &mut budget,
            || {},
            || {},
            move || revalidate_hook_state.set(true),
        )
        .err()
        .expect("deadline expiry at the explicit revalidation completion check must fail closed");
    assert!(revalidate_complete.get());
    assert!(matches!(
        revalidate_error,
        ActiveReblitBootTopologyIntentError::DeadlineExceeded {
            deadline: actual,
            remaining_at_admission,
            ..
        } if actual == deadline && remaining_at_admission == Duration::from_secs(60)
    ));
}

fn terminal_deadline_time(complete: &Cell<bool>, admitted_at: Instant, deadline: Instant) -> Instant {
    if complete.get() {
        deadline + Duration::from_nanos(1)
    } else {
        admitted_at
    }
}
