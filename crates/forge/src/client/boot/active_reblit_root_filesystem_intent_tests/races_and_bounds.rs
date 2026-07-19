use std::{
    cell::Cell,
    fs,
    os::unix::fs::PermissionsExt as _,
    rc::Rc,
    time::{Duration, Instant},
};

use super::{
    super::{
        ActiveReblitRootFilesystemIntentError, MAX_ROOT_FILESYSTEM_SOURCE_BYTES,
        PreparedActiveReblitRootFilesystemIntent, RootFilesystemIntentBudget, RootFilesystemIntentPolicy,
        normalization, prepare_with_policy_and_checkpoint, prepare_with_policy_until_and_clock,
    },
    support::{Fixture, ROOT_LOCATOR, authored_root},
};

#[test]
fn same_byte_source_replacement_before_final_preparation_revalidation_is_rejected() {
    let fixture = Fixture::new();
    fixture.write_root(ROOT_LOCATOR);
    let source = fixture.source_path();
    let displaced = fixture.root.join("etc/cast/displaced-root.glu");
    let expected = fs::read(&source).unwrap();
    let result =
        prepare_with_policy_and_checkpoint(&fixture.installation, RootFilesystemIntentPolicy::production(), |_| {
            fs::rename(&source, &displaced).unwrap();
            fs::write(&source, &expected).unwrap();
            fs::set_permissions(&source, fs::Permissions::from_mode(0o644)).unwrap();
        });
    assert!(matches!(
        result,
        Err(ActiveReblitRootFilesystemIntentError::Changed { .. })
    ));
}

#[test]
fn terminal_rebind_rejects_replacement_after_a_successful_evaluation() {
    let fixture = Fixture::new();
    fixture.write_root(ROOT_LOCATOR);
    let prepared = fixture.prepare().unwrap();
    let source = fixture.source_path();
    let displaced = fixture.root.join("etc/cast/evaluated-root.glu");
    let expected = fs::read(&source).unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut budget = RootFilesystemIntentBudget::new_until(
        &fixture.installation,
        RootFilesystemIntentPolicy::production(),
        deadline,
    )
    .unwrap();
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
        Err(ActiveReblitRootFilesystemIntentError::Changed { .. })
    ));
}

#[test]
fn second_complete_pass_rejects_same_inode_content_change_between_passes() {
    let fixture = Fixture::new();
    fixture.write_root(ROOT_LOCATOR);
    let prepared = fixture.prepare().unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut budget = RootFilesystemIntentBudget::new_until(
        &fixture.installation,
        RootFilesystemIntentPolicy::production(),
        deadline,
    )
    .unwrap();
    let result = prepared.revalidate_with_budget_and_checkpoints(
        &fixture.installation,
        &mut budget,
        || {},
        || fixture.write_root("PARTUUID=22222222-3333-4444-5555-666666666666"),
        || {},
    );
    assert!(matches!(
        result,
        Err(ActiveReblitRootFilesystemIntentError::Changed { .. })
    ));
}

#[test]
fn directory_chain_and_installation_root_substitution_are_rejected() {
    let directory_fixture = Fixture::new();
    directory_fixture.write_root(ROOT_LOCATOR);
    let prepared = directory_fixture.prepare().unwrap();
    let cast = directory_fixture.root.join("etc/cast");
    let displaced = directory_fixture.root.join("etc/displaced-cast");
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut budget = RootFilesystemIntentBudget::new_until(
        &directory_fixture.installation,
        RootFilesystemIntentPolicy::production(),
        deadline,
    )
    .unwrap();
    let result = prepared.revalidate_with_budget_and_checkpoints(
        &directory_fixture.installation,
        &mut budget,
        || {},
        || {
            fs::rename(&cast, &displaced).unwrap();
            fs::create_dir(&cast).unwrap();
            fs::set_permissions(&cast, fs::Permissions::from_mode(0o755)).unwrap();
            fs::write(cast.join("root-filesystem.glu"), authored_root(ROOT_LOCATOR)).unwrap();
            fs::set_permissions(cast.join("root-filesystem.glu"), fs::Permissions::from_mode(0o644)).unwrap();
        },
        || {},
    );
    assert!(matches!(
        result,
        Err(ActiveReblitRootFilesystemIntentError::Changed { .. })
            | Err(ActiveReblitRootFilesystemIntentError::Io { .. })
    ));

    let root_fixture = Fixture::new();
    root_fixture.write_root(ROOT_LOCATOR);
    let prepared = root_fixture.prepare().unwrap();
    let root = root_fixture.root.clone();
    let displaced = root.with_extension("displaced");
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut budget = RootFilesystemIntentBudget::new_until(
        &root_fixture.installation,
        RootFilesystemIntentPolicy::production(),
        deadline,
    )
    .unwrap();
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
        Err(ActiveReblitRootFilesystemIntentError::Installation(_))
    ));
}

#[test]
fn source_byte_bound_is_inclusive_and_production_ceiling_fails_before_evaluation() {
    let fixture = Fixture::new();
    let source = authored_root(ROOT_LOCATOR);
    fixture.write_source(&source);
    let exact = prepare_with_policy_and_checkpoint(
        &fixture.installation,
        RootFilesystemIntentPolicy {
            max_source_bytes: source.len(),
            ..RootFilesystemIntentPolicy::production()
        },
        |_| {},
    )
    .unwrap();
    assert_eq!(exact.source_text.len(), source.len());
    assert!(matches!(
        prepare_with_policy_and_checkpoint(
            &fixture.installation,
            RootFilesystemIntentPolicy {
                max_source_bytes: source.len() - 1,
                ..RootFilesystemIntentPolicy::production()
            },
            |_| {},
        ),
        Err(ActiveReblitRootFilesystemIntentError::SourceBytesLimit { limit, actual, .. })
            if limit == source.len() - 1 && actual == source.len() as u64
    ));

    let oversized = Fixture::new();
    oversized.write_source(vec![b'x'; MAX_ROOT_FILESYSTEM_SOURCE_BYTES + 1]);
    assert!(matches!(
        oversized.prepare(),
        Err(ActiveReblitRootFilesystemIntentError::SourceBytesLimit {
            limit: MAX_ROOT_FILESYSTEM_SOURCE_BYTES,
            actual,
            ..
        }) if actual == (MAX_ROOT_FILESYSTEM_SOURCE_BYTES + 1) as u64
    ));
}

#[test]
fn work_and_default_elapsed_time_bounds_are_exact_and_fail_closed() {
    let fixture = Fixture::new();
    fixture.write_root(ROOT_LOCATOR);
    let observed = fixture.prepare().unwrap().preparation_work();
    let exact = prepare_with_policy_and_checkpoint(
        &fixture.installation,
        RootFilesystemIntentPolicy {
            max_work: observed,
            ..RootFilesystemIntentPolicy::production()
        },
        |_| {},
    )
    .unwrap();
    assert_eq!(exact.preparation_work(), observed);
    assert!(matches!(
        prepare_with_policy_and_checkpoint(
            &fixture.installation,
            RootFilesystemIntentPolicy {
                max_work: observed - 1,
                ..RootFilesystemIntentPolicy::production()
            },
            |_| {},
        ),
        Err(ActiveReblitRootFilesystemIntentError::WorkLimit { limit, actual, .. })
            if limit == observed - 1 && actual == observed
    ));

    assert!(matches!(
        prepare_with_policy_and_checkpoint(
            &fixture.installation,
            RootFilesystemIntentPolicy {
                timeout: Duration::ZERO,
                ..RootFilesystemIntentPolicy::production()
            },
            |_| {},
        ),
        Err(ActiveReblitRootFilesystemIntentError::DeadlineExceeded { .. })
    ));
    assert!(matches!(
        prepare_with_policy_and_checkpoint(
            &fixture.installation,
            RootFilesystemIntentPolicy {
                timeout: Duration::MAX,
                ..RootFilesystemIntentPolicy::production()
            },
            |_| {},
        ),
        Err(ActiveReblitRootFilesystemIntentError::InvalidDeadline { .. })
    ));
}

#[test]
fn caller_owned_deadline_is_rejected_at_prepare_and_revalidate_entry() {
    let fixture = Fixture::new();
    fixture.write_root(ROOT_LOCATOR);
    let deadline = Instant::now() - Duration::from_nanos(1);

    let prepare_error = PreparedActiveReblitRootFilesystemIntent::prepare_until(&fixture.installation, deadline)
        .err()
        .expect("an expired caller deadline must reject preparation at entry");
    assert!(matches!(
        prepare_error,
        ActiveReblitRootFilesystemIntentError::DeadlineExceeded {
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
        ActiveReblitRootFilesystemIntentError::DeadlineExceeded {
            deadline: actual,
            remaining_at_admission: Duration::ZERO,
            ..
        } if actual == deadline
    ));
}

#[test]
fn caller_owned_deadline_is_rechecked_at_prepare_and_revalidate_completion() {
    let fixture = Fixture::new();
    fixture.write_root(ROOT_LOCATOR);
    let deadline = Instant::now() + Duration::from_secs(120);
    let admitted_at = deadline - Duration::from_secs(60);

    let prepare_complete = Rc::new(Cell::new(false));
    let prepare_hook_state = Rc::clone(&prepare_complete);
    let prepare_clock_state = Rc::clone(&prepare_complete);
    let prepare_error = prepare_with_policy_until_and_clock(
        &fixture.installation,
        RootFilesystemIntentPolicy::production(),
        deadline,
        move || prepare_hook_state.set(true),
        move || terminal_deadline_time(&prepare_clock_state, admitted_at, deadline),
    )
    .err()
    .expect("deadline expiry at the explicit preparation completion check must fail closed");
    assert!(prepare_complete.get());
    assert!(matches!(
        prepare_error,
        ActiveReblitRootFilesystemIntentError::DeadlineExceeded {
            deadline: actual,
            remaining_at_admission,
            ..
        } if actual == deadline && remaining_at_admission == Duration::from_secs(60)
    ));

    let prepared = fixture.prepare().unwrap();
    let revalidate_complete = Rc::new(Cell::new(false));
    let revalidate_clock_state = Rc::clone(&revalidate_complete);
    let mut budget = RootFilesystemIntentBudget::new_until_with_clock(
        &fixture.installation,
        RootFilesystemIntentPolicy::production(),
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
        ActiveReblitRootFilesystemIntentError::DeadlineExceeded {
            deadline: actual,
            remaining_at_admission,
            ..
        } if actual == deadline && remaining_at_admission == Duration::from_secs(60)
    ));
}

#[test]
fn normalized_materialization_rechecks_deadline_after_owned_tokens_exist() {
    let fixture = Fixture::new();
    let deadline = Instant::now() + Duration::from_secs(120);
    let admitted_at = deadline - Duration::from_secs(60);
    let materialized = Rc::new(Cell::new(false));
    let clock_state = Rc::clone(&materialized);
    let mut budget = RootFilesystemIntentBudget::new_until_with_clock(
        &fixture.installation,
        RootFilesystemIntentPolicy::production(),
        deadline,
        move || terminal_deadline_time(&clock_state, admitted_at, deadline),
    )
    .unwrap();
    let checkpoint_state = Rc::clone(&materialized);
    let error = normalization::materialize_root_argument_with_checkpoint(
        ROOT_LOCATOR.to_owned(),
        &mut budget,
        move |checkpoint| {
            assert_eq!(checkpoint, normalization::RootNormalizationCheckpoint::Materialized);
            checkpoint_state.set(true);
        },
    )
    .err()
    .expect("deadline expiry after owned-token materialization must fail closed");
    assert!(materialized.get());
    assert!(matches!(
        error,
        ActiveReblitRootFilesystemIntentError::DeadlineExceeded {
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
