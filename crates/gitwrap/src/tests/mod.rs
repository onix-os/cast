use std::{
    io::Write,
    os::unix::fs::PermissionsExt as _,
    path::Path,
    process::{Command, Stdio},
    time::Duration,
};

use tokio::io::AsyncWriteExt;

use super::*;

fn test_limits() -> Limits {
    Limits {
        wall_timeout: Duration::from_secs(2),
        termination_timeout: Duration::from_secs(2),
        stdout_bytes: 1024 * 1024,
        stderr_bytes: 1024 * 1024,
        progress_segment_bytes: 4096,
        repository_bytes: 64 * 1024 * 1024,
        repository_entries: 100_000,
        open_files: 256,
        address_space_bytes: 512 * 1024 * 1024,
        quota_poll_interval: Duration::from_millis(5),
    }
}

fn contained_test_command(script: &str, limits: Limits) -> process::Command {
    let mut command = process::Command::new("/bin/sh");
    command.arg("-c").arg(script).env_clear();
    constrain_process(&mut command, limits);
    command
}

fn process_exists(pid: i32) -> bool {
    let result = unsafe { nix::libc::kill(pid, 0) };
    result == 0 || std_io::Error::last_os_error().raw_os_error() != Some(nix::libc::ESRCH)
}

#[tokio::test]
async fn stdout_limit_accepts_exact_n_and_rejects_n_plus_one() {
    let (mut exact_writer, exact_reader) = io::duplex(16);
    exact_writer.write_all(b"1234").await.unwrap();
    drop(exact_writer);
    assert_eq!(read_bounded(exact_reader, "stdout", 4).await.unwrap(), b"1234");

    let (mut oversized_writer, oversized_reader) = io::duplex(16);
    oversized_writer.write_all(b"12345").await.unwrap();
    drop(oversized_writer);
    let error = read_bounded(oversized_reader, "stdout", 4).await.unwrap_err();
    assert!(error.limit_exceeded());
    assert!(error.to_string().contains("4-byte output limit"));
}

#[tokio::test]
async fn progress_segment_limit_accepts_exact_n_and_rejects_n_plus_one() {
    let (mut exact_writer, exact_reader) = io::duplex(16);
    exact_writer.write_all(b"1234\r").await.unwrap();
    drop(exact_writer);
    ProgressParser::new(exact_reader, 5, 4).parse(|_| {}).await.unwrap();

    let (mut oversized_writer, oversized_reader) = io::duplex(16);
    oversized_writer.write_all(b"12345\r").await.unwrap();
    drop(oversized_writer);
    let error = ProgressParser::new(oversized_reader, 6, 4)
        .parse(|_| {})
        .await
        .unwrap_err();
    assert!(error.limit_exceeded());
    assert!(error.to_string().contains("progress record"));
}

#[tokio::test]
async fn public_progress_backpressure_never_blocks_stderr_supervision() {
    let (progress, mut receiver) = mpsc::channel(1);
    progress
        .try_send(FetchProgress {
            percent: 0,
            speed: "queued".to_owned(),
        })
        .unwrap();
    let (mut writer, reader) = io::duplex(256);
    writer
        .write_all(
            b"Receiving objects:  25% (1/4), 1.00 MiB | 1.00 MiB/s\rReceiving objects:  50% (2/4), 2.00 MiB | 2.00 MiB/s\r",
        )
        .await
        .unwrap();
    drop(writer);

    timeout(
        Duration::from_millis(100),
        ProgressParser::new(reader, 1024, 256).parse(progress_callback(progress)),
    )
    .await
    .expect("a full progress channel must never stall parsing")
    .unwrap();
    assert_eq!(receiver.recv().await.unwrap().speed, "queued");
    assert!(receiver.try_recv().is_err(), "backpressured updates are dropped");
}

#[test]
fn repository_limits_accept_exact_n_and_reject_n_plus_one() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("repository");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("one"), vec![0_u8; 4096]).unwrap();

    let generous = test_limits();
    let usage = verify_repository_usage(&root, generous).unwrap();
    let exact_bytes = usage.logical_bytes.max(usage.allocated_bytes);
    let exact = Limits {
        repository_bytes: exact_bytes,
        repository_entries: 1,
        ..generous
    };
    assert_eq!(verify_repository_usage(&root, exact).unwrap(), usage);

    fs::write(root.join("two"), b"x").unwrap();
    let error = verify_repository_usage(&root, exact).unwrap_err();
    assert!(error.limit_exceeded());
    assert!(error.to_string().contains("filesystem entries"));

    fs::remove_file(root.join("two")).unwrap();
    fs::write(root.join("one"), vec![0_u8; 4097]).unwrap();
    let error = verify_repository_usage(&root, exact).unwrap_err();
    assert!(error.limit_exceeded());
    assert!(error.to_string().contains("bytes"));
}

#[test]
fn strict_entry_quota_rejects_n_plus_one_without_sampling_slack() {
    let temporary = tempfile::tempdir().unwrap();
    fs::write(temporary.path().join("one"), b"").unwrap();
    let mut limits = test_limits();
    limits.repository_entries = 1;
    let one = verify_repository_usage(temporary.path(), limits).unwrap();
    assert_eq!(one.entries, 1);

    fs::write(temporary.path().join("two"), b"").unwrap();
    let error = verify_repository_usage(temporary.path(), limits).unwrap_err();
    assert!(error.limit_exceeded());
    assert!(error.to_string().contains("filesystem entries"));
}

#[test]
fn live_scan_may_retry_a_vanished_name_but_strict_scan_fails_closed() {
    let temporary = tempfile::tempdir().unwrap();
    let root = open_directory(temporary.path()).unwrap();
    let vanished = CString::new("vanished").unwrap();

    assert!(scan_metadata_at(&root, &vanished, ScanMode::Live).unwrap().is_none());
    let error = scan_metadata_at(&root, &vanished, ScanMode::Strict).unwrap_err();
    assert!(error.limit_exceeded());
    assert!(error.to_string().contains("changed during strict quota"));
}

#[test]
fn live_scan_allows_initial_absence_without_building_a_strict_inventory() {
    let temporary = tempfile::tempdir().unwrap();
    let missing = temporary.path().join("not-created-yet");
    let mut limits = test_limits();
    limits.address_space_bytes = 1;

    let absent = RepositoryUsageScanner::new(&missing, limits, ScanMode::Live).unwrap();
    assert!(absent.directories.is_empty());
    let error = match RepositoryUsageScanner::new(&missing, limits, ScanMode::Strict) {
        Err(error) => error,
        Ok(_) => panic!("strict scan accepted a missing mandatory root"),
    };
    assert!(error.limit_exceeded());

    let root = temporary.path().join("created");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("entry"), b"contents").unwrap();
    let mut scanner = RepositoryUsageScanner::new(&root, limits, ScanMode::Live).unwrap();
    while !scanner.advance(1, None).unwrap() {}
    assert_eq!(scanner.snapshot.usage.entries, 1);
    assert!(scanner.snapshot.entries.is_empty());
    assert_eq!(scanner.snapshot_bytes, 0);
}

#[test]
fn strict_relative_path_allocation_is_prechecked_against_snapshot_budget() {
    let temporary = tempfile::tempdir().unwrap();
    let cursor = DirectoryCursor::open(temporary.path()).unwrap();
    let error = cursor.child_relative(b"four", 3, 100).unwrap_err();
    assert!(error.limit_exceeded());
    assert!(error.to_string().contains("100-byte memory budget"));
}

#[test]
fn strict_two_snapshot_verification_rejects_same_name_inode_replacement() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("repository");
    fs::create_dir(&root).unwrap();
    let entry = root.join("entry");
    let old_entry = temporary.path().join("old-entry");
    fs::write(&entry, b"same-size").unwrap();
    let limits = test_limits();
    let deadline = Instant::now() + limits.wall_timeout;
    let mut pass = 0_u8;

    let error = verify_two_repository_snapshots(|| {
        if pass == 1 {
            fs::rename(&entry, &old_entry).unwrap();
            fs::write(&entry, b"same-size").unwrap();
        }
        pass += 1;
        scan_repository_path_strict(&root, limits, deadline)
    })
    .unwrap_err();
    assert!(error.limit_exceeded());
    assert!(error.to_string().contains("changed during strict quota"));
}

#[test]
fn descriptor_rooted_quota_scan_never_follows_nested_or_root_symlinks() {
    use std::os::unix::fs::symlink;

    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("repository");
    let outside = temporary.path().join("outside");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("large"), vec![0_u8; 1024 * 1024]).unwrap();
    symlink(&outside, root.join("link")).unwrap();

    let usage = verify_repository_usage(&root, test_limits()).unwrap();
    assert!(usage.logical_bytes < 1024 * 1024);
    let root_link = temporary.path().join("repository-link");
    symlink(&root, &root_link).unwrap();
    let error = verify_repository_usage(&root_link, test_limits()).unwrap_err();
    assert!(error.to_string().contains("not an ordinary directory"));
}

#[test]
fn quota_scan_rejects_nesting_before_exhausting_parent_descriptors() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("repository");
    fs::create_dir(&root).unwrap();
    fs::create_dir(root.join("nested")).unwrap();
    let mut limits = test_limits();
    limits.open_files = 34;

    let error = verify_repository_usage(&root, limits).unwrap_err();
    assert!(error.limit_exceeded());
    assert!(error.to_string().contains("descriptor budget"));
}

#[test]
fn quota_scan_rejects_a_budget_too_small_for_one_cursor() {
    let temporary = tempfile::tempdir().unwrap();
    let mut limits = test_limits();
    limits.open_files = 33;

    let error = verify_repository_usage(temporary.path(), limits).unwrap_err();
    assert!(error.limit_exceeded());
    assert!(error.to_string().contains("0-directory descriptor budget"));
}

#[test]
fn quota_scanner_reserves_descriptors_already_open_in_the_parent() {
    assert_eq!(scanner_cursor_capacity(256, 100, 32, 2), 62);
    assert_eq!(scanner_cursor_capacity(132, 100, 32, 2), 0);
    assert_eq!(scanner_cursor_capacity(131, 100, 32, 2), 0);
}

#[tokio::test]
async fn repository_rejects_a_replaced_public_path_while_root_is_pinned() {
    let temporary = tempfile::tempdir().unwrap();
    let source = temporary.path().join("source");
    fs::create_dir(&source).unwrap();
    fixture_git(&source, &["init", "--initial-branch=main"]);
    fixture_git(&source, &["config", "user.name", "Gitwrap Test"]);
    fixture_git(&source, &["config", "user.email", "gitwrap@example.invalid"]);
    fs::write(source.join("source.txt"), b"source\n").unwrap();
    fixture_git(&source, &["add", "source.txt"]);
    fixture_git(&source, &["commit", "-m", "source"]);

    let destination = temporary.path().join("mirror.git");
    let mut limits = test_limits();
    limits.repository_bytes = 1024 * 1024;
    let source_url = Url::from_directory_path(&source).unwrap();
    let repository = Repository::clone_mirror_with_limits(&destination, &source_url, limits)
        .await
        .unwrap();

    let moved = temporary.path().join("moved-mirror.git");
    fs::rename(&destination, &moved).unwrap();
    fs::create_dir(&destination).unwrap();
    fs::write(destination.join("replacement"), vec![0_u8; 2 * 1024 * 1024]).unwrap();
    assert!(verify_repository_usage(&destination, limits).is_err());

    let error = repository.get_remote_url("origin").await.unwrap_err();
    assert!(error.to_string().contains("no longer names"));
    assert_eq!(
        fixture_git(&moved, &["remote", "get-url", "origin"]),
        source_url.as_str(),
    );
}

#[test]
fn quota_scan_uses_the_subprocess_absolute_deadline() {
    let temporary = tempfile::tempdir().unwrap();
    let error = verify_repository_usage_before(temporary.path(), test_limits(), Instant::now()).unwrap_err();
    assert!(error.timed_out());
}

#[tokio::test]
async fn oversized_cached_mirror_is_rejected_before_git_is_started() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("cached.git");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("untrusted"), vec![0_u8; 8192]).unwrap();
    let usage = verify_repository_usage(&root, test_limits()).unwrap();
    let mut limits = test_limits();
    limits.repository_bytes = usage.logical_bytes.max(usage.allocated_bytes) - 1;

    let error = Repository::open_bare_with_limits(&root, limits).await.unwrap_err();
    assert!(error.limit_exceeded());
    assert!(!error.run_failed(), "Git should not inspect an oversized cache");
    assert!(root.join("untrusted").is_file());
}

#[tokio::test]
async fn remote_url_mutation_is_rejected_when_it_crosses_repository_quota() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("repository");
    fs::create_dir(&root).unwrap();
    fixture_git(&root, &["init", "--initial-branch=main"]);
    fixture_git(&root, &["remote", "add", "origin", "https://example.invalid/a"]);
    let mut limits = test_limits();
    let usage = verify_repository_usage(&root, limits).unwrap();
    limits.repository_bytes = usage.logical_bytes.max(usage.allocated_bytes);
    let repository = Repository {
        path: root.clone(),
        limits,
        identity: Some(RepositoryIdentity::from_directory(&open_repository_directory(&root).unwrap()).unwrap()),
        mirror: None,
    };
    let large_url = format!("https://example.invalid/{}", "x".repeat(8192));

    let error = repository.set_remote_url("origin", &large_url).await.unwrap_err();
    assert!(error.limit_exceeded(), "unexpected error: {error}");
}

#[tokio::test]
async fn failed_public_fetch_never_deletes_a_caller_owned_repository() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("caller-owned");
    fs::create_dir(&root).unwrap();
    fixture_git(&root, &["init", "--initial-branch=main"]);
    let missing_remote = Url::from_file_path(temporary.path().join("missing-remote"))
        .unwrap()
        .to_string();
    fixture_git(&root, &["remote", "add", "origin", &missing_remote]);
    let repository = Repository {
        path: root.clone(),
        limits: test_limits(),
        identity: Some(RepositoryIdentity::from_directory(&open_repository_directory(&root).unwrap()).unwrap()),
        mirror: None,
    };

    let (progress, _receiver) = mpsc::channel(1);
    let error = repository.fetch_progress(progress).await.unwrap_err();
    assert!(error.run_failed());
    assert!(root.join(".git").is_dir());
}

#[tokio::test]
async fn timeout_kills_and_reaps_the_complete_process_group() {
    let temporary = tempfile::tempdir().unwrap();
    let pid_file = temporary.path().join("descendant.pid");
    let mut limits = test_limits();
    limits.wall_timeout = Duration::from_millis(250);
    let mut command = contained_test_command("sleep 30 & echo $! > \"$PID_FILE\"; wait", limits);
    command.env("PID_FILE", &pid_file);

    let error = run_command(command, limits, None, None::<fn(FetchProgress)>)
        .await
        .unwrap_err();
    assert!(error.timed_out(), "unexpected error: {error}");
    let descendant: i32 = fs::read_to_string(&pid_file).unwrap().trim().parse().unwrap();
    assert!(!process_exists(descendant), "descendant {descendant} survived timeout");
}

#[tokio::test]
async fn successful_parent_cannot_leave_a_background_pipe_holder() {
    let temporary = tempfile::tempdir().unwrap();
    let pid_file = temporary.path().join("descendant.pid");
    let mut limits = test_limits();
    limits.wall_timeout = Duration::from_secs(5);
    let mut command = contained_test_command("sleep 30 & echo $! > \"$PID_FILE\"", limits);
    command.env("PID_FILE", &pid_file);

    let started = Instant::now();
    let output = run_command(command, limits, None, None::<fn(FetchProgress)>)
        .await
        .unwrap();
    assert!(output.status.success());
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "background pipe holder consumed the outer wall deadline"
    );
    let descendant: i32 = fs::read_to_string(&pid_file).unwrap().trim().parse().unwrap();
    assert!(
        !process_exists(descendant),
        "descendant {descendant} survived successful parent exit"
    );
}

#[tokio::test]
async fn output_limit_kills_descendants_and_never_exposes_stderr_secrets() {
    let temporary = tempfile::tempdir().unwrap();
    let pid_file = temporary.path().join("descendant.pid");
    let mut limits = test_limits();
    limits.stdout_bytes = 4;
    let mut command = contained_test_command("sleep 30 & echo $! > \"$PID_FILE\"; printf 12345; wait", limits);
    command.env("PID_FILE", &pid_file);
    let error = run_command(command, limits, None, None::<fn(FetchProgress)>)
        .await
        .unwrap_err();
    assert!(error.limit_exceeded(), "unexpected error: {error}");
    let descendant: i32 = fs::read_to_string(&pid_file).unwrap().trim().parse().unwrap();
    assert!(
        !process_exists(descendant),
        "descendant {descendant} survived output rejection"
    );

    let secret = "https://alice:secret@example.invalid/repository.git";
    let command = contained_test_command(&format!("echo '{secret}' >&2; exit 7"), test_limits());
    let error = run_command(command, test_limits(), None, None::<fn(FetchProgress)>)
        .await
        .unwrap_err();
    assert!(error.run_failed());
    assert!(!error.to_string().contains("alice"));
    assert!(!error.to_string().contains("secret"));
}

#[tokio::test]
async fn child_boundary_disables_core_dumps_and_caps_open_files() {
    let mut limits = test_limits();
    limits.open_files = 64;
    let mut inherited = nix::libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    assert_eq!(
        unsafe { nix::libc::getrlimit(nix::libc::RLIMIT_NOFILE, &mut inherited) },
        0
    );
    let expected_open_files = inherited.rlim_cur.min(64);
    let mut inherited_address_space = nix::libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    assert_eq!(
        unsafe { nix::libc::getrlimit(nix::libc::RLIMIT_AS, &mut inherited_address_space) },
        0
    );
    let expected_address_space = inherited_address_space
        .rlim_cur
        .min(rlim_from_u64(limits.address_space_bytes))
        / 1024;
    let command = contained_test_command(
        "printf '%s %s %s' \"$(ulimit -c)\" \"$(ulimit -n)\" \"$(ulimit -v)\"",
        limits,
    );

    let output = run_command(command, limits, None, None::<fn(FetchProgress)>)
        .await
        .unwrap();
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("0 {expected_open_files} {expected_address_space}")
    );
}

#[tokio::test]
async fn incremental_quota_scan_does_not_starve_a_full_stdout_pipe() {
    let temporary = tempfile::tempdir().unwrap();
    let repository = temporary.path().join("repository");
    fs::create_dir(&repository).unwrap();
    for index in 0..2048 {
        fs::write(repository.join(format!("entry-{index}")), b"").unwrap();
    }
    let mut limits = test_limits();
    limits.stdout_bytes = 512 * 1024;
    limits.quota_poll_interval = Duration::from_millis(1);
    let command = contained_test_command("dd if=/dev/zero bs=65536 count=4 2>/dev/null; sleep 0.05", limits);

    let output = run_command(
        command,
        limits,
        Some(MonitoredRepository::Path(repository)),
        None::<fn(FetchProgress)>,
    )
    .await
    .unwrap();
    assert_eq!(output.stdout.len(), 4 * 65536);
}

#[tokio::test]
async fn oversized_clone_is_rejected_without_final_or_staging_state() {
    let temporary = tempfile::tempdir().unwrap();
    let source = temporary.path().join("source");
    fs::create_dir(&source).unwrap();
    fixture_git(&source, &["init", "--initial-branch=main"]);
    fixture_git(&source, &["config", "user.name", "Gitwrap Test"]);
    fixture_git(&source, &["config", "user.email", "gitwrap@example.invalid"]);
    fs::write(source.join("large"), vec![0_u8; 32 * 1024]).unwrap();
    fixture_git(&source, &["add", "large"]);
    fixture_git(&source, &["commit", "-m", "large"]);

    let destination = temporary.path().join("mirror.git");
    let mut limits = test_limits();
    limits.repository_bytes = 4096;
    let error = Repository::clone_mirror_with_limits(&destination, &Url::from_directory_path(&source).unwrap(), limits)
        .await
        .unwrap_err();
    assert!(
        error.limit_exceeded() || error.run_failed(),
        "unexpected error: {error}"
    );
    assert!(!destination.exists());
    assert!(fs::read_dir(temporary.path()).unwrap().all(|entry| !entry
        .unwrap()
        .file_name()
        .to_string_lossy()
        .starts_with(".gitwrap-mirror-")));
}

#[tokio::test]
async fn published_mirror_and_credential_config_are_owner_private() {
    let temporary = tempfile::tempdir().unwrap();
    let source = temporary.path().join("source");
    fs::create_dir(&source).unwrap();
    fixture_git(&source, &["init", "--initial-branch=main"]);
    fixture_git(&source, &["config", "user.name", "Gitwrap Test"]);
    fixture_git(&source, &["config", "user.email", "gitwrap@example.invalid"]);
    fs::write(source.join("source.txt"), b"source\n").unwrap();
    fixture_git(&source, &["add", "source.txt"]);
    fixture_git(&source, &["commit", "-m", "source"]);

    let destination = temporary.path().join("mirror.git");
    Repository::clone_mirror_with_limits(&destination, &Url::from_directory_path(&source).unwrap(), test_limits())
        .await
        .unwrap();

    assert_eq!(fs::metadata(&destination).unwrap().mode() & 0o777, 0o700);
    assert_eq!(fs::metadata(destination.join("config")).unwrap().mode() & 0o777, 0o600);
}

#[tokio::test]
async fn private_mirror_strips_hostile_local_config_before_open_and_fetch() {
    let temporary = tempfile::tempdir().unwrap();
    let source = temporary.path().join("source");
    fs::create_dir(&source).unwrap();
    fixture_git(&source, &["init", "--initial-branch=main"]);
    fixture_git(&source, &["config", "user.name", "Gitwrap Test"]);
    fixture_git(&source, &["config", "user.email", "gitwrap@example.invalid"]);
    fs::write(source.join("source.txt"), b"source\n").unwrap();
    fixture_git(&source, &["add", "source.txt"]);
    fixture_git(&source, &["commit", "-m", "source"]);

    let origin = Url::from_directory_path(&source).unwrap();
    let destination = temporary.path().join("mirror.git");
    Repository::clone_mirror_with_limits(&destination, &origin, test_limits())
        .await
        .unwrap();
    let canonical = canonical_mirror_config(&origin, ObjectFormat::Sha1).unwrap();
    let included = temporary.path().join("included-config");
    let sentinel = temporary.path().join("credential-helper-ran");
    fs::write(
        &included,
        format!(
            "[credential]\n\thelper = !touch {}\n[url \"custom-helper://attacker/\"]\n\tinsteadOf = {}\n",
            sentinel.display(),
            origin.as_str()
        ),
    )
    .unwrap();
    let mut hostile = canonical.clone();
    hostile.extend_from_slice(
        format!(
            "[include]\n\tpath = {}\n[credential]\n\thelper = !touch {}\n[core]\n\tsshCommand = touch {}\n",
            included.display(),
            sentinel.display(),
            sentinel.display()
        )
        .as_bytes(),
    );
    fs::write(destination.join("config"), &hostile).unwrap();
    fs::set_permissions(&destination, std::fs::Permissions::from_mode(0o755)).unwrap();
    fs::set_permissions(destination.join("config"), std::fs::Permissions::from_mode(0o644)).unwrap();

    let repository = Repository::open_private_mirror_with_limits(&destination, &origin, test_limits())
        .await
        .unwrap();
    assert_eq!(fs::read(destination.join("config")).unwrap(), canonical);
    assert_eq!(fs::metadata(&destination).unwrap().mode() & 0o777, 0o700);
    assert_eq!(fs::metadata(destination.join("config")).unwrap().mode() & 0o777, 0o600);
    assert!(!sentinel.exists());

    fs::write(destination.join("config"), &hostile).unwrap();
    let (progress, _receiver) = mpsc::channel(1);
    repository.fetch_progress(progress).await.unwrap();
    assert_eq!(fs::read(destination.join("config")).unwrap(), canonical);
    assert!(!sentinel.exists());
}

#[tokio::test]
async fn private_mirror_origin_is_checked_before_config_is_rewritten() {
    let temporary = tempfile::tempdir().unwrap();
    let source = temporary.path().join("source");
    let wrong_source = temporary.path().join("wrong-source");
    fs::create_dir(&source).unwrap();
    fs::create_dir(&wrong_source).unwrap();
    fixture_git(&source, &["init", "--bare"]);
    fixture_git(&wrong_source, &["init", "--bare"]);
    let origin = Url::from_directory_path(&source).unwrap();
    let wrong_origin = Url::from_directory_path(&wrong_source).unwrap();
    let destination = temporary.path().join("mirror.git");
    Repository::clone_mirror_with_limits(&destination, &origin, test_limits())
        .await
        .unwrap();
    fixture_git(&destination, &["remote", "set-url", "origin", wrong_origin.as_str()]);

    let error = Repository::open_private_mirror_with_limits(&destination, &origin, test_limits())
        .await
        .unwrap_err();
    assert!(error.mirror_origin_mismatch());
    assert_eq!(
        fixture_git(&destination, &["remote", "get-url", "origin"]),
        wrong_origin.as_str()
    );
}

#[tokio::test]
async fn unknown_remote_helper_schemes_and_option_like_arguments_are_rejected_before_spawn() {
    let temporary = tempfile::tempdir().unwrap();
    let destination = temporary.path().join("mirror.git");
    let error = Repository::clone_mirror_with_limits(
        &destination,
        &Url::parse("custom-helper://example.invalid/repository").unwrap(),
        test_limits(),
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("not allowed"));
    assert!(!destination.exists());

    let error = Repository::clone_mirror_with_limits(
        &destination,
        &Url::parse("http://example.invalid/repository").unwrap(),
        test_limits(),
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("not allowed"));
    assert!(!destination.exists());

    let error = Repository::clone_mirror_with_limits(
        &destination,
        &Url::parse("git://example.invalid/repository").unwrap(),
        test_limits(),
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("not allowed"));
    assert!(!destination.exists());

    let repository = null_repository();
    assert!(repository.has_commit("--batch").await.is_err());
    assert!(repository.get_remote_url("--all").await.is_err());
    assert!(repository.set_remote_url("--add", "value").await.is_err());
    assert!(repository.checkout("--orphan").await.is_err());
    assert!(repository.contains_gitlinks("--long").await.is_err());
}

#[tokio::test]
async fn sha256_object_format_commit_ids_are_accepted() {
    let temporary = tempfile::tempdir().unwrap();
    let source = temporary.path().join("source");
    fs::create_dir(&source).unwrap();
    fixture_git(&source, &["init", "--object-format=sha256", "--initial-branch=main"]);
    fixture_git(&source, &["config", "user.name", "Gitwrap Test"]);
    fixture_git(&source, &["config", "user.email", "gitwrap@example.invalid"]);
    fs::write(source.join("source.txt"), b"source\n").unwrap();
    fixture_git(&source, &["add", "source.txt"]);
    fixture_git(&source, &["commit", "-m", "source"]);

    let destination = temporary.path().join("mirror.git");
    let repository =
        Repository::clone_mirror_with_limits(&destination, &Url::from_directory_path(&source).unwrap(), test_limits())
            .await
            .unwrap();
    let commit = repository.peel_commit("HEAD").await.unwrap();
    assert_eq!(commit.len(), 64);
    assert!(commit.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')));
}

fn fixture_git(repository: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(arguments)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn fixture_git_with_input(repository: &Path, arguments: &[&str], input: &[u8]) -> String {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.as_mut().unwrap().write_all(input).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "git {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

#[tokio::test]
async fn clone_to_skips_an_uncheckoutable_default_head() {
    let temporary = tempfile::tempdir().unwrap();
    let repository_path = temporary.path().join("repository");
    fs::create_dir(&repository_path).unwrap();
    fixture_git(&repository_path, &["init", "--initial-branch=main"]);
    fixture_git(&repository_path, &["config", "user.name", "Gitwrap Test"]);
    fixture_git(&repository_path, &["config", "user.email", "gitwrap@example.invalid"]);

    fs::write(repository_path.join("source.txt"), b"locked source\n").unwrap();
    fixture_git(&repository_path, &["add", "source.txt"]);
    fixture_git(&repository_path, &["commit", "-m", "locked source"]);
    let locked_commit = fixture_git(&repository_path, &["rev-parse", "HEAD"]);

    fs::write(repository_path.join("invalid-head"), b"invalid default head\n").unwrap();
    let invalid_blob = fixture_git(&repository_path, &["hash-object", "-w", "invalid-head"]);
    let invalid_tree_entry = format!("100644 blob {invalid_blob}\t.git\n");
    let invalid_tree = fixture_git_with_input(&repository_path, &["mktree"], invalid_tree_entry.as_bytes());
    let invalid_head = fixture_git(
        &repository_path,
        &[
            "commit-tree",
            &invalid_tree,
            "-p",
            &locked_commit,
            "-m",
            "uncheckoutable default head",
        ],
    );
    fixture_git(&repository_path, &["update-ref", "refs/heads/main", &invalid_head]);

    let ordinary_clone = temporary.path().join("ordinary-clone");
    let ordinary_result = run_git(
        [
            OsStr::new("clone"),
            OsStr::new("--no-hardlinks"),
            OsStr::new("--no-recurse-submodules"),
            repository_path.as_os_str(),
            ordinary_clone.as_os_str(),
        ],
        Limits::DEFAULT,
    )
    .await;
    assert!(
        ordinary_result.is_err(),
        "the fixture's default HEAD must be uncheckoutable"
    );

    let repository = Repository {
        path: repository_path.clone(),
        limits: Limits::DEFAULT,
        identity: Some(
            RepositoryIdentity::from_directory(&open_repository_directory(&repository_path).unwrap()).unwrap(),
        ),
        mirror: None,
    };
    let clone_path = temporary.path().join("locked-clone");
    let cloned = repository.clone_to(&clone_path).await.unwrap();
    assert_eq!(
        fixture_git(cloned.path(), &["rev-parse", "HEAD"]),
        invalid_head,
        "the clone must retain the unrelated default HEAD"
    );

    cloned.checkout(&locked_commit).await.unwrap();
    assert_eq!(fs::read(clone_path.join("source.txt")).unwrap(), b"locked source\n");
    assert_eq!(fixture_git(cloned.path(), &["rev-parse", "HEAD"]), locked_commit);
}

#[tokio::test]
async fn gitlinks_are_detected_without_materializing_submodules() {
    let temporary = tempfile::tempdir().unwrap();
    let repository_path = temporary.path().join("repository");
    fs::create_dir(&repository_path).unwrap();
    fixture_git(&repository_path, &["init", "--initial-branch=main"]);
    fixture_git(&repository_path, &["config", "user.name", "Gitwrap Test"]);
    fixture_git(&repository_path, &["config", "user.email", "gitwrap@example.invalid"]);
    fs::write(repository_path.join("source.txt"), b"locked source\n").unwrap();
    fixture_git(&repository_path, &["add", "source.txt"]);
    fixture_git(&repository_path, &["commit", "-m", "source"]);

    let repository = Repository {
        path: repository_path.clone(),
        limits: Limits::DEFAULT,
        identity: Some(
            RepositoryIdentity::from_directory(&open_repository_directory(&repository_path).unwrap()).unwrap(),
        ),
        mirror: None,
    };
    let source_commit = fixture_git(&repository_path, &["rev-parse", "HEAD"]);
    assert!(!repository.contains_gitlinks(&source_commit).await.unwrap());

    let cache_info = format!("160000,{source_commit},vendor/dependency");
    fixture_git(&repository_path, &["update-index", "--add", "--cacheinfo", &cache_info]);
    fixture_git(&repository_path, &["commit", "-m", "gitlink"]);
    let gitlink_commit = fixture_git(&repository_path, &["rev-parse", "HEAD"]);

    assert!(repository.contains_gitlinks(&gitlink_commit).await.unwrap());
}

#[tokio::test]
async fn annotated_tags_are_peeled_to_the_commit_object() {
    let temporary = tempfile::tempdir().unwrap();
    let repository_path = temporary.path().join("repository");
    fs::create_dir(&repository_path).unwrap();
    fixture_git(&repository_path, &["init", "--initial-branch=main"]);
    fixture_git(&repository_path, &["config", "user.name", "Gitwrap Test"]);
    fixture_git(&repository_path, &["config", "user.email", "gitwrap@example.invalid"]);
    fs::write(repository_path.join("source.txt"), b"locked source\n").unwrap();
    fixture_git(&repository_path, &["add", "source.txt"]);
    fixture_git(&repository_path, &["commit", "-m", "source"]);
    fixture_git(
        &repository_path,
        &["tag", "--annotate", "v1", "--message", "release v1"],
    );

    let commit = fixture_git(&repository_path, &["rev-parse", "HEAD"]);
    let tag_object = fixture_git(&repository_path, &["rev-parse", "v1"]);
    assert_ne!(tag_object, commit, "the fixture must use an annotated tag object");

    let repository = Repository {
        path: repository_path.clone(),
        limits: Limits::DEFAULT,
        identity: Some(
            RepositoryIdentity::from_directory(&open_repository_directory(&repository_path).unwrap()).unwrap(),
        ),
        mirror: None,
    };
    let peeled = repository.peel_commit("v1").await.unwrap();

    assert_eq!(peeled, commit);
    assert_eq!(peeled.len(), 40);
    assert!(peeled.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')));
}
