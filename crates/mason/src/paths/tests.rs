use std::{
    cell::{Cell, RefCell},
    ffi::OsStr,
    os::unix::fs::{MetadataExt, PermissionsExt, symlink},
};

use super::*;
use crate::package::test_derivation_plan;

fn test_paths(root: &tempfile::TempDir, plan: &DerivationPlan) -> Paths {
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let recipe =
        Recipe::load(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples/gluon/stone.glu")).unwrap();
    let output = root.path().join("output");
    util::ensure_dir_exists(&output).unwrap();
    Paths::new(&recipe, plan.layout.clone(), root.path(), output).unwrap()
}

#[test]
fn preserve_existing_workspace_leaf_policy_pins_without_chmodding_directory_race_winner() {
    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let forge = root.path().join("forge");
    std::fs::create_dir(&forge).unwrap();
    std::fs::set_permissions(&forge, std::fs::Permissions::from_mode(0o555)).unwrap();
    let before = std::fs::symlink_metadata(&forge).unwrap();

    let prepared =
        prepare_private_workspace_root_with_policy(&forge, WorkspaceRootLeafPolicy::PreserveExisting).unwrap();

    let after = std::fs::symlink_metadata(&forge).unwrap();
    assert_eq!(prepared, forge);
    assert_eq!((after.dev(), after.ino()), (before.dev(), before.ino()));
    assert_eq!(after.permissions().mode() & 0o7777, 0o555);
}

#[test]
fn missing_workspace_root_rejects_existing_symlink_without_touching_target() {
    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let target = root.path().join("unrelated");
    std::fs::create_dir(&target).unwrap();
    std::fs::write(target.join("keep"), b"untouched").unwrap();
    let target_before = std::fs::symlink_metadata(&target).unwrap();
    let forge = root.path().join("forge");
    symlink(&target, &forge).unwrap();

    let error = prepare_missing_private_workspace_root(&forge).unwrap_err();

    assert_ne!(error.kind(), io::ErrorKind::NotFound);
    assert!(std::fs::symlink_metadata(&forge).unwrap().file_type().is_symlink());
    let target_after = std::fs::symlink_metadata(&target).unwrap();
    assert_eq!(
        (target_after.dev(), target_after.ino()),
        (target_before.dev(), target_before.ino())
    );
    assert_eq!(std::fs::read(target.join("keep")).unwrap(), b"untouched");
}

#[test]
fn preserve_existing_workspace_leaf_policy_rejects_non_directory_race_winners_unchanged() {
    for kind in ["symlink", "file"] {
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let target = root.path().join("unrelated");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("keep"), b"untouched").unwrap();
        let forge = root.path().join("forge");
        match kind {
            "symlink" => symlink(&target, &forge).unwrap(),
            "file" => std::fs::write(&forge, b"not a directory").unwrap(),
            _ => unreachable!(),
        }
        let before = std::fs::symlink_metadata(&forge).unwrap();

        let error =
            prepare_private_workspace_root_with_policy(&forge, WorkspaceRootLeafPolicy::PreserveExisting).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "{kind}");
        let after = std::fs::symlink_metadata(&forge).unwrap();
        assert_eq!((after.dev(), after.ino()), (before.dev(), before.ino()), "{kind}");
        assert_eq!(std::fs::read(target.join("keep")).unwrap(), b"untouched", "{kind}");
        if kind == "file" {
            assert_eq!(std::fs::read(&forge).unwrap(), b"not a directory");
        } else {
            assert_eq!(std::fs::read_link(&forge).unwrap(), target);
        }
    }
}

#[test]
fn frozen_workspaces_are_keyed_by_the_complete_derivation_id() {
    let root = tempfile::tempdir().unwrap();
    let first_plan = test_derivation_plan();
    let mut second_plan = first_plan.clone();
    second_plan.source_date_epoch += 1;
    second_plan.validate().unwrap();
    assert_eq!(first_plan.package, second_plan.package);
    assert_ne!(first_plan.derivation_id(), second_plan.derivation_id());

    let mut first = test_paths(&root, &first_plan);
    let mut second = test_paths(&root, &second_plan);
    first.bind_to_plan(&first_plan).unwrap();
    second.bind_to_plan(&second_plan).unwrap();

    assert!(!first.rootfs().host.exists());
    assert!(!second.rootfs().host.exists());
    assert!(first.rootfs().host.parent().unwrap().is_dir());
    assert_ne!(first.rootfs().host, second.rootfs().host);
    assert_ne!(first.build().host, second.build().host);
    assert_ne!(first.artefacts().host, second.artefacts().host);
    assert_eq!(
        first.rootfs().host.file_name().and_then(|name| name.to_str()),
        Some(first_plan.derivation_id().as_str())
    );
    assert_eq!(
        second.rootfs().host.file_name().and_then(|name| name.to_str()),
        Some(second_plan.derivation_id().as_str())
    );
    first.require_plan(&first_plan).unwrap();
    assert!(first.require_plan(&second_plan).is_err());
}

#[test]
fn paths_remain_recipe_keyed_until_the_frozen_plan_is_bound() {
    let root = tempfile::tempdir().unwrap();
    let plan = test_derivation_plan();
    let paths = test_paths(&root, &plan);

    assert!(matches!(&paths.id, Id::Recipe(_)));
    assert!(paths.require_plan(&plan).is_err());
}

#[test]
fn invalid_recipe_identity_is_rejected_before_host_paths_are_created() {
    let root = tempfile::tempdir().unwrap();
    let plan = test_derivation_plan();
    let mut recipe =
        Recipe::load(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples/gluon/stone.glu")).unwrap();
    recipe.declaration.meta.pname = "/tmp/cast-path-escape".to_owned();
    let output = root.path().join("output");
    util::ensure_dir_exists(&output).unwrap();

    let error = Paths::new(&recipe, plan.layout, root.path(), output).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    assert!(!root.path().join("root").exists());
}

#[test]
#[allow(deprecated)]
fn execution_guard_exclusively_locks_the_derivation_path() {
    let root = tempfile::tempdir().unwrap();
    let plan = test_derivation_plan();
    let mut paths = test_paths(&root, &plan);
    paths.bind_to_plan(&plan).unwrap();

    let guard = paths.acquire_execution_lock(&plan).unwrap();
    assert_eq!(guard.path(), paths.execution_lock_path(&plan).unwrap());
    let contender = File::options()
        .create(true)
        .write(true)
        .truncate(false)
        .open(guard.path())
        .unwrap();
    assert_eq!(
        flock(contender.as_raw_fd(), FlockArg::LockExclusiveNonblock),
        Err(nix::errno::Errno::EWOULDBLOCK)
    );

    drop(guard);
    flock(contender.as_raw_fd(), FlockArg::LockExclusiveNonblock).unwrap();
}

#[test]
fn frozen_packaging_permit_authorizes_only_its_exact_descriptor_free_binding() {
    let root = tempfile::tempdir().unwrap();
    let plan = test_derivation_plan();
    let mut paths = test_paths(&root, &plan);
    paths.bind_to_plan(&plan).unwrap();
    let execution_lock = paths.acquire_execution_lock(&plan).unwrap();
    let binding = paths.frozen_packaging_binding(&plan).unwrap();
    let permit = paths
        .issue_frozen_packaging_permit(&execution_lock, &plan)
        .unwrap();

    assert_eq!(binding.workspace, root.path().canonicalize().unwrap());
    let workspace = std::fs::metadata(root.path()).unwrap();
    assert_eq!(binding.workspace_identity, (workspace.dev(), workspace.ino()));
    assert_eq!(binding.derivation_id, plan.derivation_id());
    assert_eq!(binding.lock_path, paths.execution_lock_path(&plan).unwrap());
    permit.require_for(&binding).unwrap();
    paths.require_execution_lock(&execution_lock, &plan).unwrap();
}

#[test]
fn frozen_packaging_permit_rejects_other_workspace_and_derivation_bindings() {
    let root = tempfile::tempdir().unwrap();
    let other_root = tempfile::tempdir().unwrap();
    let plan = test_derivation_plan();
    let mut paths = test_paths(&root, &plan);
    let mut other_workspace = test_paths(&other_root, &plan);
    paths.bind_to_plan(&plan).unwrap();
    other_workspace.bind_to_plan(&plan).unwrap();
    let execution_lock = paths.acquire_execution_lock(&plan).unwrap();
    let permit = paths
        .issue_frozen_packaging_permit(&execution_lock, &plan)
        .unwrap();

    let other_workspace_binding = other_workspace.frozen_packaging_binding(&plan).unwrap();
    assert!(permit.require_for(&other_workspace_binding).is_err());

    let mut other_plan = plan.clone();
    other_plan.source_date_epoch += 1;
    other_plan.validate().unwrap();
    let mut other_derivation = test_paths(&root, &other_plan);
    other_derivation.bind_to_plan(&other_plan).unwrap();
    let other_derivation_binding = other_derivation.frozen_packaging_binding(&other_plan).unwrap();
    assert!(permit.require_for(&other_derivation_binding).is_err());
}

#[test]
fn frozen_packaging_permit_issuance_revalidates_the_complete_lock_first() {
    let root = tempfile::tempdir().unwrap();
    let plan = test_derivation_plan();
    let mut paths = test_paths(&root, &plan);
    paths.bind_to_plan(&plan).unwrap();
    let execution_lock = paths.acquire_execution_lock(&plan).unwrap();
    let lock_path = execution_lock.path().to_owned();

    std::fs::remove_file(&lock_path).unwrap();
    std::fs::write(&lock_path, b"replacement").unwrap();
    std::fs::set_permissions(&lock_path, std::fs::Permissions::from_mode(0o600)).unwrap();

    let error = paths
        .issue_frozen_packaging_permit(&execution_lock, &plan)
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    assert!(error.to_string().contains("execution lock is not one private regular file"));
}

#[test]
fn execution_lock_immediately_times_out_under_real_contention() {
    let root = tempfile::tempdir().unwrap();
    let plan = test_derivation_plan();
    let mut paths = test_paths(&root, &plan);
    paths.bind_to_plan(&plan).unwrap();
    let guard = paths.acquire_execution_lock(&plan).unwrap();

    let deadline = Instant::now();
    let waits = Cell::new(0usize);
    let error = paths
        .acquire_execution_lock_until(&plan, deadline, || deadline, |_| waits.set(waits.get() + 1))
        .unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert!(error.to_string().contains("workspace execution gate"));
    assert_eq!(waits.get(), 0);
    paths.require_execution_lock(&guard, &plan).unwrap();
}

#[test]
fn execution_lock_overshoot_never_attempts_after_the_deadline() {
    let started = Instant::now();
    let remaining = Duration::from_millis(7);
    let deadline = started + remaining;
    let clock = Cell::new(started);
    let attempts = Cell::new(0usize);
    let waits = RefCell::new(Vec::new());
    let mut now = || clock.get();
    let mut wait = |duration| {
        waits.borrow_mut().push(duration);
        clock.set(deadline + Duration::from_nanos(1));
    };

    let error = lock_exclusive_until_with(
        "overshoot test lock",
        Path::new("overshoot"),
        deadline,
        &mut now,
        &mut wait,
        || {
            attempts.set(attempts.get() + 1);
            Ok(false)
        },
    )
    .unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert_eq!(attempts.get(), 1);
    assert_eq!(waits.into_inner(), vec![remaining]);
}

#[test]
fn execution_lock_partial_final_interval_retries_once_at_the_exact_deadline() {
    let started = Instant::now();
    let remaining = Duration::from_millis(7);
    assert!(remaining < EXECUTION_LOCK_RETRY_INTERVAL);
    let deadline = started + remaining;
    let clock = Cell::new(started);
    let attempts = Cell::new(0usize);
    let waits = RefCell::new(Vec::new());
    let mut now = || clock.get();
    let mut wait = |duration| {
        waits.borrow_mut().push(duration);
        clock.set(clock.get() + duration);
    };

    lock_exclusive_until_with(
        "partial interval test lock",
        Path::new("partial"),
        deadline,
        &mut now,
        &mut wait,
        || {
            attempts.set(attempts.get() + 1);
            Ok(attempts.get() == 2)
        },
    )
    .unwrap();

    assert_eq!(clock.get(), deadline);
    assert_eq!(attempts.get(), 2);
    assert_eq!(waits.into_inner(), vec![remaining]);
}

#[test]
#[allow(deprecated)]
fn execution_lock_workspace_and_derivation_contention_share_one_deadline() {
    let root = tempfile::tempdir().unwrap();
    let plan = test_derivation_plan();
    let mut paths = test_paths(&root, &plan);
    paths.bind_to_plan(&plan).unwrap();

    let workspace_holder = open_directory_nofollow(&paths.host_root).unwrap();
    flock(workspace_holder.as_raw_fd(), FlockArg::LockExclusiveNonblock).unwrap();
    let workspace_holder = RefCell::new(Some(workspace_holder));

    let lock_dir = paths
        .prepare_private_host_directory(&paths.execution_lock_dir())
        .unwrap();
    let leaf = execution_lock_leaf(plan.derivation_id().as_str()).unwrap();
    let derivation_holder = open_or_create_execution_lock_file(&lock_dir, &leaf).unwrap();
    flock(derivation_holder.as_raw_fd(), FlockArg::LockExclusiveNonblock).unwrap();

    let started = Instant::now();
    let deadline = started + EXECUTION_LOCK_RETRY_INTERVAL.saturating_mul(2);
    let clock = Cell::new(started);
    let waits = RefCell::new(Vec::new());
    let error = paths
        .acquire_execution_lock_until(
            &plan,
            deadline,
            || clock.get(),
            |duration| {
                waits.borrow_mut().push(duration);
                clock.set(clock.get() + duration);
                drop(workspace_holder.borrow_mut().take());
            },
        )
        .unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert!(error.to_string().contains("derivation execution lock"));
    assert_eq!(clock.get(), deadline);
    assert_eq!(
        waits.into_inner(),
        vec![EXECUTION_LOCK_RETRY_INTERVAL, EXECUTION_LOCK_RETRY_INTERVAL]
    );

    drop(derivation_holder);
    let guard = paths.acquire_execution_lock(&plan).unwrap();
    paths.require_execution_lock(&guard, &plan).unwrap();
}

#[test]
fn execution_lock_expired_before_the_second_lock_cannot_acquire_it() {
    let root = tempfile::tempdir().unwrap();
    let plan = test_derivation_plan();
    let mut paths = test_paths(&root, &plan);
    paths.bind_to_plan(&plan).unwrap();

    let started = Instant::now();
    let deadline = started + EXECUTION_LOCK_RETRY_INTERVAL;
    let past_deadline = deadline + Duration::from_nanos(1);
    let clock_reads = Cell::new(0usize);
    let waits = Cell::new(0usize);
    let error = paths
        .acquire_execution_lock_until(
            &plan,
            deadline,
            || {
                let read = clock_reads.get();
                clock_reads.set(read + 1);
                if read == 0 { started } else { past_deadline }
            },
            |_| waits.set(waits.get() + 1),
        )
        .unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert!(error.to_string().contains("derivation execution lock"));
    assert_eq!(clock_reads.get(), 2);
    assert_eq!(waits.get(), 0);

    // The derivation lock was free. A successful result above would prove
    // that an attempt was incorrectly made after the shared deadline.
    let guard = paths.acquire_execution_lock(&plan).unwrap();
    paths.require_execution_lock(&guard, &plan).unwrap();
}

#[test]
fn execution_lock_retry_can_acquire_at_the_exact_deadline_and_retains_the_lock() {
    let root = tempfile::tempdir().unwrap();
    let plan = test_derivation_plan();
    let mut paths = test_paths(&root, &plan);
    paths.bind_to_plan(&plan).unwrap();
    let original = RefCell::new(Some(paths.acquire_execution_lock(&plan).unwrap()));

    let started = Instant::now();
    let deadline = started + EXECUTION_LOCK_RETRY_INTERVAL;
    let clock = Cell::new(started);
    let waits = Cell::new(0usize);
    let replacement = paths
        .acquire_execution_lock_until(
            &plan,
            deadline,
            || clock.get(),
            |duration| {
                waits.set(waits.get() + 1);
                clock.set(clock.get() + duration);
                drop(original.borrow_mut().take());
            },
        )
        .unwrap();

    assert_eq!(waits.get(), 1);
    assert_eq!(clock.get(), deadline);
    assert!(original.into_inner().is_none());
    paths.require_execution_lock(&replacement, &plan).unwrap();

    let contender = File::options()
        .create(true)
        .write(true)
        .truncate(false)
        .open(replacement.path())
        .unwrap();
    assert_eq!(
        flock(contender.as_raw_fd(), FlockArg::LockExclusiveNonblock),
        Err(nix::errno::Errno::EWOULDBLOCK)
    );
}

#[test]
fn execution_lock_non_contention_error_is_not_reported_as_timeout() {
    struct InvalidDescriptor;

    impl AsRawFd for InvalidDescriptor {
        fn as_raw_fd(&self) -> RawFd {
            -1
        }
    }

    let current = Instant::now();
    let mut now = || current;
    let mut wait = |_| panic!("a non-contention flock error must not be retried");
    let error = lock_exclusive_until(
        &InvalidDescriptor,
        "invalid test lock",
        Path::new("invalid"),
        current,
        &mut now,
        &mut wait,
    )
    .unwrap_err();

    assert_ne!(error.kind(), io::ErrorKind::TimedOut);
    assert_eq!(error.raw_os_error(), Some(nix::libc::EBADF));
}

#[test]
fn execution_lock_retries_interruption_without_treating_it_as_contention() {
    let current = Instant::now();
    let attempts = Cell::new(0usize);
    let waits = Cell::new(0usize);
    let mut now = || current;
    let mut wait = |_| waits.set(waits.get() + 1);

    lock_exclusive_until_with(
        "interrupted test lock",
        Path::new("interrupted"),
        current + EXECUTION_LOCK_RETRY_INTERVAL,
        &mut now,
        &mut wait,
        || {
            attempts.set(attempts.get() + 1);
            if attempts.get() == 1 {
                Err(io::Error::from(io::ErrorKind::Interrupted))
            } else {
                Ok(true)
            }
        },
    )
    .unwrap();

    assert_eq!(attempts.get(), 2);
    assert_eq!(waits.get(), 0);
}

#[test]
fn execution_lock_repeated_interruptions_stop_at_the_deadline_without_waiting() {
    let started = Instant::now();
    let deadline = started + EXECUTION_LOCK_RETRY_INTERVAL;
    let clock = Cell::new(started);
    let attempts = Cell::new(0usize);
    let waits = Cell::new(0usize);
    let mut now = || clock.get();
    let mut wait = |_| waits.set(waits.get() + 1);

    let error = lock_exclusive_until_with(
        "repeated interruption test lock",
        Path::new("repeated-interruption"),
        deadline,
        &mut now,
        &mut wait,
        || {
            attempts.set(attempts.get() + 1);
            if attempts.get() == 2 {
                clock.set(deadline);
            }
            Err(io::Error::from(io::ErrorKind::Interrupted))
        },
    )
    .unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert_eq!(attempts.get(), 2);
    assert_eq!(waits.get(), 0);
}

#[test]
fn execution_lock_name_accepts_exact_name_max_and_rejects_name_max_plus_one() {
    let maximum_id = MAX_EXECUTION_LOCK_NAME_BYTES - EXECUTION_LOCK_SUFFIX.len();
    let accepted = execution_lock_leaf(&"a".repeat(maximum_id)).unwrap();
    assert_eq!(accepted.to_bytes().len(), MAX_EXECUTION_LOCK_NAME_BYTES);

    let error = execution_lock_leaf(&"a".repeat(maximum_id + 1)).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    assert!(execution_lock_leaf("").is_err());
    assert!(execution_lock_leaf("not/a-component").is_err());
    assert!(execution_lock_leaf("not\0a-component").is_err());
}

#[test]
fn execution_lock_rejects_fifo_without_blocking() {
    let root = tempfile::tempdir().unwrap();
    let plan = test_derivation_plan();
    let mut paths = test_paths(&root, &plan);
    paths.bind_to_plan(&plan).unwrap();
    let lock_path = paths.execution_lock_path(&plan).unwrap();
    let lock_path_c = CString::new(lock_path.as_os_str().as_bytes()).unwrap();
    // SAFETY: the test path is one live NUL-terminated string.
    assert_eq!(unsafe { nix::libc::mkfifo(lock_path_c.as_ptr(), 0o600) }, 0);

    let (send, receive) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        send.send(paths.acquire_execution_lock(&plan).map(drop)).unwrap();
    });
    let error = receive
        .recv_timeout(Duration::from_secs(2))
        .expect("O_NONBLOCK must prevent a hostile FIFO from hanging lock acquisition")
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
}

#[test]
fn execution_lock_rejects_symlink_and_multiple_link_regular_file() {
    let root = tempfile::tempdir().unwrap();
    let plan = test_derivation_plan();
    let mut paths = test_paths(&root, &plan);
    paths.bind_to_plan(&plan).unwrap();
    let lock_path = paths.execution_lock_path(&plan).unwrap();
    let outside = root.path().join("outside-lock");
    std::fs::write(&outside, b"outside").unwrap();
    std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o600)).unwrap();
    symlink(&outside, &lock_path).unwrap();

    assert!(paths.acquire_execution_lock(&plan).is_err());
    assert_eq!(std::fs::read(&outside).unwrap(), b"outside");

    std::fs::remove_file(&lock_path).unwrap();
    std::fs::write(&lock_path, b"").unwrap();
    std::fs::set_permissions(&lock_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let alias = root.path().join("lock-alias");
    std::fs::hard_link(&lock_path, &alias).unwrap();
    let error = paths.acquire_execution_lock(&plan).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    assert_eq!(std::fs::metadata(&lock_path).unwrap().nlink(), 2);
}

#[test]
fn execution_lock_path_replacement_invalidates_guard_and_cannot_overlap_a_second_guard() {
    let root = tempfile::tempdir().unwrap();
    let plan = test_derivation_plan();
    let mut paths = test_paths(&root, &plan);
    paths.bind_to_plan(&plan).unwrap();
    let guard = paths.acquire_execution_lock(&plan).unwrap();
    let lock_path = guard.path().to_owned();

    std::fs::remove_file(&lock_path).unwrap();
    std::fs::write(&lock_path, b"replacement").unwrap();
    std::fs::set_permissions(&lock_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(paths.require_execution_lock(&guard, &plan).is_err());

    let contender_paths = paths.clone();
    let contender_plan = plan.clone();
    let (send, receive) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        send.send(contender_paths.acquire_execution_lock(&contender_plan))
            .unwrap();
    });
    assert!(
        matches!(
            receive.recv_timeout(Duration::from_millis(100)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ),
        "the stable workspace gate must prevent overlapping guards after pathname replacement"
    );

    drop(guard);
    let replacement_guard = receive
        .recv_timeout(Duration::from_secs(2))
        .expect("the contender must proceed after the original stable gate is released")
        .unwrap();
    paths.require_execution_lock(&replacement_guard, &plan).unwrap();
}

#[test]
fn frozen_scratch_is_atomically_replaced_and_bounded_cleanup_never_follows_links() {
    let root = tempfile::tempdir().unwrap();
    let plan = test_derivation_plan();
    let mut paths = test_paths(&root, &plan);
    paths.bind_to_plan(&plan).unwrap();
    let scratch = paths.artefacts().host;
    let outside = root.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("sentinel"), b"outside").unwrap();

    let leaf = CString::new(scratch.file_name().unwrap().as_bytes()).unwrap();
    let stale = stale_leaf_name(&leaf).unwrap();
    let stale_path = scratch.parent().unwrap().join(OsStr::from_bytes(stale.to_bytes()));
    std::fs::create_dir(&stale_path).unwrap();
    std::fs::set_permissions(&stale_path, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(stale_path.join("interrupted-retry"), b"stale").unwrap();
    symlink(&outside, stale_path.join("outside-link")).unwrap();

    let first = paths.prepare_fresh_private_host_directory(&scratch).unwrap();
    assert!(!stale_path.exists());
    assert_eq!(std::fs::read(outside.join("sentinel")).unwrap(), b"outside");
    let first_identity = directory_identity(&first).unwrap();
    std::fs::create_dir(scratch.join("nested")).unwrap();
    std::fs::write(scratch.join("nested/file"), b"stale").unwrap();
    symlink(&outside, scratch.join("nested/outside-link")).unwrap();

    let second = paths.prepare_fresh_private_host_directory(&scratch).unwrap();
    assert_ne!(first_identity, directory_identity(&second).unwrap());
    assert!(std::fs::read_dir(&scratch).unwrap().next().is_none());
    assert_eq!(
        std::fs::metadata(&scratch).unwrap().permissions().mode() & 0o7777,
        0o700
    );
    assert_eq!(std::fs::read(outside.join("sentinel")).unwrap(), b"outside");
    assert!(!stale_path.exists());

    paths.remove_private_host_directory(&scratch).unwrap();
    assert!(!scratch.exists());
    paths.remove_private_host_directory(&scratch).unwrap();
}

#[test]
fn frozen_scratch_rejects_unsafe_existing_leaf_without_renaming_or_chmoding_it() {
    let root = tempfile::tempdir().unwrap();
    let plan = test_derivation_plan();
    let mut paths = test_paths(&root, &plan);
    paths.bind_to_plan(&plan).unwrap();
    let scratch = paths.build().host;
    std::fs::create_dir(&scratch).unwrap();
    std::fs::set_permissions(&scratch, std::fs::Permissions::from_mode(0o770)).unwrap();
    std::fs::write(scratch.join("must-survive"), b"unsafe").unwrap();

    let error = paths.prepare_fresh_private_host_directory(&scratch).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    assert_eq!(
        std::fs::metadata(&scratch).unwrap().permissions().mode() & 0o7777,
        0o770
    );
    assert_eq!(std::fs::read(scratch.join("must-survive")).unwrap(), b"unsafe");
}

#[test]
fn frozen_private_source_rejects_a_symlinked_parent_without_touching_its_target() {
    let root = tempfile::tempdir().unwrap();
    let plan = test_derivation_plan();
    let mut paths = test_paths(&root, &plan);
    paths.bind_to_plan(&plan).unwrap();
    let outside = root.path().join("outside-cache");
    std::fs::create_dir(&outside).unwrap();
    symlink(&outside, root.path().join("derivations")).unwrap();
    let cache = paths.derivation_cache_host(plan.derivation_id().as_str(), "ccache");

    assert!(paths.prepare_private_host_directory(&cache).is_err());
    assert!(std::fs::read_dir(outside).unwrap().next().is_none());
}

#[test]
fn retained_workspace_descriptor_detects_path_substitution() {
    let outer = tempfile::tempdir().unwrap();
    let workspace = outer.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::set_permissions(&workspace, std::fs::Permissions::from_mode(0o700)).unwrap();
    let plan = test_derivation_plan();
    let recipe =
        Recipe::load(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples/gluon/stone.glu")).unwrap();
    let output = outer.path().join("output");
    std::fs::create_dir(&output).unwrap();
    let paths = Paths::new(&recipe, plan.layout, &workspace, output).unwrap();

    std::fs::rename(&workspace, outer.path().join("detached-workspace")).unwrap();
    std::fs::create_dir(&workspace).unwrap();
    std::fs::set_permissions(&workspace, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(paths.frozen_workspace_anchor().is_err());
}

#[test]
fn purge_budgets_accept_each_exact_boundary_and_reject_n_plus_one() {
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut entries = PurgeBudget {
        entries: MAX_PURGE_ENTRIES - 1,
        operations: 0,
        name_bytes: 0,
        deadline,
        device: 0,
    };
    entries.account(0, true).unwrap();
    assert_eq!(entries.entries, MAX_PURGE_ENTRIES);
    assert!(entries.account(0, true).is_err());

    let mut operations = PurgeBudget {
        entries: 0,
        operations: MAX_PURGE_OPERATIONS - 1,
        name_bytes: 0,
        deadline,
        device: 0,
    };
    operations.account(0, false).unwrap();
    assert_eq!(operations.operations, MAX_PURGE_OPERATIONS);
    assert!(operations.account(0, false).is_err());

    let mut names = PurgeBudget {
        entries: 0,
        operations: 0,
        name_bytes: MAX_PURGE_NAME_BYTES - 1,
        deadline,
        device: 0,
    };
    names.account(1, true).unwrap();
    assert_eq!(names.name_bytes, MAX_PURGE_NAME_BYTES);
    assert!(names.account(1, true).is_err());

    require_purge_depth(MAX_PURGE_DEPTH).unwrap();
    assert!(require_purge_depth(MAX_PURGE_DEPTH + 1).is_err());
}
