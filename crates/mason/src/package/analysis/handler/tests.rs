use std::{
    collections::BTreeSet,
    fs,
    io::Write as _,
    os::fd::AsRawFd,
    os::unix::fs::symlink,
    path::Path,
    time::{Duration, Instant},
};

use nix::fcntl::{FcntlArg, FdFlag, fcntl};
use nix::{sys::stat::Mode, unistd::mkfifo};
use stone::StoneDigestWriterHasher;
use stone_recipe::derivation::{ExecutablePlan, PathRuleKind, RelationKind, RelationPlan};

use super::execution::{
    AnalyzerContainment, AnalyzerExecutionError, AnalyzerLimits, AnalyzerPipe, analyzer_cleanup_deadline,
    analyzer_descendant_signal_target, bounded_analyzer_limit, checked_output_with_limits, contained_output,
    join_analyzer_pipe_readers_until, read_analyzer_pipe, with_analyzer_cleanup,
};
use super::input::AnalyzerInputError;
use super::sandbox::{SandboxSnapshot, empty_sandbox_directory, sandbox_directory_entries, verify_sandbox_inventory};
use super::*;
use crate::package::{collect::Collector, test_derivation_plan};

fn collect_path(root: &Path, path: &Path) -> PathInfo {
    let mut collector = Collector::new(root);
    collector.add_rule("*", "fixture", PathRuleKind::Any).unwrap();
    let mut hasher = StoneDigestWriterHasher::new();
    collector.path(path, &mut hasher).unwrap()
}

fn pkg_config_program() -> PathBuf {
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .map(|directory| directory.join("pkg-config"))
        .find(|path| path.is_file())
        .expect("focused analyzer tests require pkg-config")
}

fn test_limits(stdout_bytes: usize, stderr_bytes: usize, wall_timeout: Duration) -> AnalyzerLimits {
    AnalyzerLimits {
        wall_timeout,
        stdout_bytes,
        stderr_bytes,
    }
}

fn checked_test_output(mut command: Command, limits: AnalyzerLimits) -> Result<Output, BoxError> {
    checked_output_with_limits(&mut command, limits)
}

fn execution_error(error: &BoxError) -> &AnalyzerExecutionError {
    error
        .downcast_ref::<AnalyzerExecutionError>()
        .expect("expected a structured analyzer execution error")
}

#[test]
fn analyzer_limit_never_raises_an_inherited_soft_limit() {
    let inherited = nix::libc::rlimit {
        rlim_cur: 16,
        rlim_max: 128,
    };
    assert_eq!(
        bounded_analyzer_limit(inherited, 64),
        nix::libc::rlimit {
            rlim_cur: 16,
            rlim_max: 64,
        }
    );

    let inherited = nix::libc::rlimit {
        rlim_cur: 96,
        rlim_max: 128,
    };
    assert_eq!(
        bounded_analyzer_limit(inherited, 64),
        nix::libc::rlimit {
            rlim_cur: 64,
            rlim_max: 64,
        }
    );

    let inherited = nix::libc::rlimit {
        rlim_cur: 24,
        rlim_max: 48,
    };
    assert_eq!(bounded_analyzer_limit(inherited, 64), inherited);
}

#[test]
fn verified_input_accepts_exact_limit_and_rejects_one_byte_over() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("input.pc");
    fs::write(&path, b"12345678").unwrap();
    let info = collect_path(root.path(), &path);

    let input = VerifiedAnalyzerInput::from_path_info(&info, 8).unwrap();
    assert_eq!(input.read_all(8).unwrap(), b"12345678");

    let over_root = tempfile::tempdir().unwrap();
    let over_path = over_root.path().join("input.pc");
    fs::write(&over_path, b"123456789").unwrap();
    let over = collect_path(over_root.path(), &over_path);
    let error = VerifiedAnalyzerInput::from_path_info(&over, 8).unwrap_err();
    assert!(matches!(
        error.downcast_ref::<AnalyzerInputError>(),
        Some(AnalyzerInputError::TooLarge { size: 9, limit: 8, .. })
    ));
}

#[test]
fn verified_input_is_sealed_against_write_truncate_and_growth() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("input.pc");
    fs::write(&path, b"immutable").unwrap();
    let info = collect_path(root.path(), &path);
    let input = VerifiedAnalyzerInput::from_path_info(&info, 9).unwrap();
    let required = nix::libc::F_SEAL_WRITE | nix::libc::F_SEAL_GROW | nix::libc::F_SEAL_SHRINK | nix::libc::F_SEAL_SEAL;
    // SAFETY: input owns this live memfd.
    let seals = unsafe { nix::libc::fcntl(input.file.as_raw_fd(), nix::libc::F_GET_SEALS) };
    assert_eq!(seals & required, required);

    let mut read_only = input.try_clone().unwrap();
    assert!(read_only.write_all(b"x").is_err());
    assert!(read_only.set_len(0).is_err());
    let byte = b'x';
    // SAFETY: the pointer and descriptor are live; the seal must reject
    // this write rather than altering the authenticated bytes.
    assert_eq!(
        unsafe { nix::libc::pwrite(input.file.as_raw_fd(), (&byte as *const u8).cast(), 1, 0) },
        -1
    );
    // SAFETY: input owns this live memfd.
    assert_eq!(unsafe { nix::libc::ftruncate(input.file.as_raw_fd(), 0) }, -1);
    // SAFETY: input owns this live memfd.
    assert_eq!(unsafe { nix::libc::ftruncate(input.file.as_raw_fd(), 10) }, -1);

    assert_eq!(input.read_all(9).unwrap(), b"immutable");
}

#[test]
fn private_regular_file_sandbox_is_available_to_child_and_removed() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("input.pc");
    fs::write(&path, b"child-visible").unwrap();
    let info = collect_path(root.path(), &path);
    let input = VerifiedAnalyzerInput::from_path_info(&info, 13).unwrap();
    let sandbox = ExternalAnalyzerInput::new(&input, &info.target_path, "input.pc", ".pkgconfig").unwrap();
    let sandbox_path = sandbox.working_directory().to_owned();
    let mut command = analyzer_command("/bin/cat");
    command.current_dir(sandbox.working_directory()).arg(sandbox.path());

    let operation = checked_output_for(&info, command);
    let output = sandbox.finish(&info, operation).unwrap();
    assert_eq!(output.stdout, b"child-visible");
    assert!(!sandbox_path.exists());
}

#[test]
fn changed_or_expanded_sandbox_is_rejected_then_removed() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("input.pc");
    fs::write(&path, b"authenticated").unwrap();
    let info = collect_path(root.path(), &path);
    let input = VerifiedAnalyzerInput::from_path_info(&info, 13).unwrap();
    let sandbox = ExternalAnalyzerInput::new(&input, &info.target_path, "input.pc", ".pkgconfig").unwrap();
    let sandbox_path = sandbox.working_directory().to_owned();
    let mut command = analyzer_command("/bin/sh");
    command.current_dir(sandbox.working_directory()).args([
        "-c",
        "chmod 700 .; chmod 600 input.pc; printf corrupted > input.pc; : > extra",
    ]);

    let operation = checked_output_for(&info, command);
    assert!(sandbox.finish(&info, operation).is_err());
    assert!(!sandbox_path.exists());
    assert_eq!(fs::read(path).unwrap(), b"authenticated");
}

#[test]
fn sandbox_inventory_accepts_exact_entry_limit_and_rejects_one_over() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("first"), b"one").unwrap();
    let directory = StdOpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_CLOEXEC | nix::libc::O_DIRECTORY | nix::libc::O_NOFOLLOW)
        .open(root.path())
        .unwrap();

    assert_eq!(
        sandbox_directory_entries(&directory, 1, analyzer_cleanup_deadline())
            .unwrap()
            .len(),
        1
    );
    fs::write(root.path().join("second"), b"two").unwrap();
    let error = sandbox_directory_entries(&directory, 1, analyzer_cleanup_deadline()).unwrap_err();
    assert!(error.to_string().contains("1-entry enumeration limit"), "{error}");
}

#[test]
fn sandbox_cleanup_accepts_exact_depth_and_rejects_one_over() {
    fn nested(root: &Path, depth: usize) {
        let mut current = root.to_owned();
        for _ in 0..depth {
            current.push("d");
            fs::create_dir(&current).unwrap();
        }
    }

    let exact = tempfile::tempdir().unwrap();
    nested(exact.path(), SANDBOX_CLEANUP_DEPTH_LIMIT);
    let exact_directory = StdOpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_CLOEXEC | nix::libc::O_DIRECTORY | nix::libc::O_NOFOLLOW)
        .open(exact.path())
        .unwrap();
    let mut remaining = SANDBOX_CLEANUP_ENTRY_LIMIT;
    empty_sandbox_directory(&exact_directory, &mut remaining, 0, analyzer_cleanup_deadline()).unwrap();
    assert_eq!(fs::read_dir(exact.path()).unwrap().count(), 0);

    let over = tempfile::tempdir().unwrap();
    nested(over.path(), SANDBOX_CLEANUP_DEPTH_LIMIT + 1);
    let over_directory = StdOpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_CLOEXEC | nix::libc::O_DIRECTORY | nix::libc::O_NOFOLLOW)
        .open(over.path())
        .unwrap();
    let mut remaining = SANDBOX_CLEANUP_ENTRY_LIMIT;
    let error = empty_sandbox_directory(&over_directory, &mut remaining, 0, analyzer_cleanup_deadline()).unwrap_err();
    assert!(error.to_string().contains("depth limit"), "{error}");
}

#[test]
fn unfinished_detached_sandbox_is_retained_without_deleting_replacement() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("input.pc");
    fs::write(&path, b"authenticated").unwrap();
    let info = collect_path(root.path(), &path);
    let input = VerifiedAnalyzerInput::from_path_info(&info, 13).unwrap();
    let sandbox = ExternalAnalyzerInput::new(&input, &info.target_path, "input.pc", ".pkgconfig").unwrap();
    let original_path = sandbox.working_directory().to_owned();
    let detached_path = original_path.with_file_name(format!(
        "{}.detached",
        original_path.file_name().unwrap().to_string_lossy()
    ));

    fs::rename(&original_path, &detached_path).unwrap();
    fs::set_permissions(&detached_path, Permissions::from_mode(0o700)).unwrap();
    fs::rename(detached_path.join("input.pc"), detached_path.join("renamed-input")).unwrap();
    fs::create_dir(detached_path.join("nested")).unwrap();
    fs::write(detached_path.join("nested/payload"), b"extra").unwrap();

    fs::create_dir(&original_path).unwrap();
    fs::write(original_path.join("replacement-marker"), b"do-not-delete").unwrap();

    let expected_after_mutation = SandboxSnapshot::from_metadata(&sandbox.directory_file.metadata().unwrap());
    assert!(
        verify_sandbox_inventory(
            &sandbox.directory_file,
            &original_path,
            OsStr::new("input.pc"),
            expected_after_mutation,
        )
        .is_err(),
        "inventory must be read from the pinned directory, not the clean replacement path"
    );

    // Drop is the fail-safe path: no explicit finish is called.
    drop(sandbox);

    assert_eq!(
        fs::read(original_path.join("replacement-marker")).unwrap(),
        b"do-not-delete"
    );
    fs::set_permissions(&detached_path, Permissions::from_mode(0o700)).unwrap();
    assert_eq!(fs::read(detached_path.join("renamed-input")).unwrap(), b"authenticated");
    assert_eq!(fs::read(detached_path.join("nested/payload")).unwrap(), b"extra");

    fs::remove_file(detached_path.join("renamed-input")).unwrap();
    fs::remove_file(detached_path.join("nested/payload")).unwrap();
    fs::remove_dir(detached_path.join("nested")).unwrap();
    fs::remove_dir(&detached_path).unwrap();
    fs::remove_file(original_path.join("replacement-marker")).unwrap();
    fs::remove_dir(&original_path).unwrap();
}

#[test]
fn finished_detached_sandbox_is_emptied_without_deleting_replacement() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("input.pc");
    fs::write(&path, b"authenticated").unwrap();
    let info = collect_path(root.path(), &path);
    let input = VerifiedAnalyzerInput::from_path_info(&info, 13).unwrap();
    let sandbox = ExternalAnalyzerInput::new(&input, &info.target_path, "input.pc", ".pkgconfig").unwrap();
    let original_path = sandbox.working_directory().to_owned();
    let detached_path = original_path.with_file_name(format!(
        "{}.detached-finished",
        original_path.file_name().unwrap().to_string_lossy()
    ));

    fs::rename(&original_path, &detached_path).unwrap();
    fs::set_permissions(&detached_path, Permissions::from_mode(0o700)).unwrap();
    fs::create_dir(detached_path.join("nested")).unwrap();
    fs::write(detached_path.join("nested/payload"), b"extra").unwrap();
    fs::create_dir(&original_path).unwrap();
    fs::write(original_path.join("replacement-marker"), b"do-not-delete").unwrap();

    let operation: Result<(), BoxError> = Err(Box::new(std::io::Error::other("fixture failure")));
    assert!(sandbox.finish(&info, operation).is_err());

    assert_eq!(
        fs::read(original_path.join("replacement-marker")).unwrap(),
        b"do-not-delete"
    );
    fs::set_permissions(&detached_path, Permissions::from_mode(0o700)).unwrap();
    assert_eq!(fs::read_dir(&detached_path).unwrap().count(), 0);

    fs::remove_dir(&detached_path).unwrap();
    fs::remove_file(original_path.join("replacement-marker")).unwrap();
    fs::remove_dir(&original_path).unwrap();
}

#[test]
fn pkg_config_pcfiledir_uses_logical_install_path_not_random_sandbox() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("usr/lib/pkgconfig/demo.pc");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, b"Name: demo\nDescription: demo\nVersion: 1\n").unwrap();
    let info = collect_path(root.path(), &path);
    let input = VerifiedAnalyzerInput::from_path_info(&info, 256).unwrap();
    let sandbox = ExternalAnalyzerInput::new(&input, &info.target_path, "input.pc", ".pkgconfig").unwrap();
    let mut command = analyzer_command(pkg_config_program().to_str().unwrap());
    command
        .current_dir(sandbox.working_directory())
        .args(["--define-variable=pcfiledir=/usr/lib/pkgconfig", "--variable=pcfiledir"])
        .arg(sandbox.path())
        .env("LC_ALL", "C");

    let operation = checked_output_for(&info, command);
    let output = sandbox.finish(&info, operation).unwrap();
    assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), "/usr/lib/pkgconfig");
}

#[test]
fn pkg_config_handler_runs_real_tool_without_original_or_proc_path() {
    let install = tempfile::tempdir().unwrap();
    let path = install.path().join("usr/lib/pkgconfig/demo.pc");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let content = b"Name: demo\nDescription: demo\nVersion: 1\n";
    fs::write(&path, content).unwrap();

    let mut collector = Collector::new(install.path());
    collector.add_rule("*", "fixture", PathRuleKind::Any).unwrap();
    let mut hasher = StoneDigestWriterHasher::new();
    let mut info = collector.path(&path, &mut hasher).unwrap();
    let mut plan = test_derivation_plan();
    let program = pkg_config_program();
    plan.analysis.tools.pkg_config = Some(ExecutablePlan {
        path: program.to_string_lossy().into_owned(),
        requirement: RelationPlan {
            kind: RelationKind::Binary,
            name: "pkg-config".to_owned(),
        },
    });
    let mut providers = BTreeSet::new();
    let mut dependencies = BTreeSet::new();
    let mut bucket = BucketMut {
        providers: &mut providers,
        dependencies: &mut dependencies,
        analysis: &plan.analysis,
        install_root: install.path(),
    };

    let response = pkg_config(&mut bucket, &mut info).unwrap();
    assert!(matches!(response.decision, Decision::NextHandler));
    assert_eq!(
        providers,
        BTreeSet::from([Provider::new(Kind::PkgConfig, "demo").unwrap()])
    );
    assert!(dependencies.is_empty());
    assert_eq!(fs::read(path).unwrap(), content);
}

#[test]
fn compressman_declares_regular_output_then_collector_publishes_it() {
    let install = tempfile::tempdir().unwrap();
    let path = install.path().join("usr/share/man/man1/demo.1");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let content = b"deterministic manual page\n";
    fs::write(&path, content).unwrap();

    let mut collector = Collector::new(install.path());
    collector.add_rule("*", "fixture", PathRuleKind::Any).unwrap();
    let mut hasher = StoneDigestWriterHasher::new();
    let mut info = collector.path(&path, &mut hasher).unwrap();
    let mut plan = test_derivation_plan();
    plan.analysis.compress_man = true;
    let mut providers = BTreeSet::new();
    let mut dependencies = BTreeSet::new();
    let mut bucket = BucketMut {
        providers: &mut providers,
        dependencies: &mut dependencies,
        analysis: &plan.analysis,
        install_root: install.path(),
    };

    let response = compressman(&mut bucket, &mut info).unwrap();
    let compressed = path.with_added_extension("zst");
    assert_eq!(compressed, path.parent().unwrap().join("demo.1.zst"));
    assert!(matches!(
        &response.decision,
        Decision::ReplaceFile { newpath } if newpath == &compressed
    ));
    assert_eq!(response.publications.len(), 1);
    assert!(
        !compressed.exists(),
        "handler published outside the collector transaction"
    );
    assert_eq!(fs::read(&path).unwrap(), content);

    let published = collector
        .publish_generated(&response.publications, &mut hasher)
        .unwrap();
    assert_eq!(published.len(), 1);
    assert!(!path.parent().unwrap().join("demo.1..zst").exists());
    let decoded = zstd::stream::decode_all(fs::File::open(&compressed).unwrap()).unwrap();
    assert_eq!(decoded, content);
    collector.seal().unwrap();
}

#[test]
fn compressman_symlink_publication_does_not_eagerly_mutate_its_target() {
    let install = tempfile::tempdir().unwrap();
    let directory = install.path().join("usr/share/man/man1");
    fs::create_dir_all(&directory).unwrap();
    let target = directory.join("demo.1");
    let link = directory.join("alias.1");
    let content = b"target manual page\n";
    fs::write(&target, content).unwrap();
    symlink("demo.1", &link).unwrap();

    let mut collector = Collector::new(install.path());
    collector.add_rule("*", "fixture", PathRuleKind::Any).unwrap();
    let mut hasher = StoneDigestWriterHasher::new();
    let mut link_info = collector.path(&link, &mut hasher).unwrap();
    let mut plan = test_derivation_plan();
    plan.analysis.compress_man = true;
    let mut providers = BTreeSet::new();
    let mut dependencies = BTreeSet::new();
    let mut bucket = BucketMut {
        providers: &mut providers,
        dependencies: &mut dependencies,
        analysis: &plan.analysis,
        install_root: install.path(),
    };

    let link_response = compressman(&mut bucket, &mut link_info).unwrap();
    let compressed_link = link.with_added_extension("zst");
    let compressed_target = target.with_added_extension("zst");
    assert_eq!(compressed_link, directory.join("alias.1.zst"));
    assert_eq!(compressed_target, directory.join("demo.1.zst"));
    assert!(!compressed_link.exists());
    assert!(!compressed_target.exists());
    collector
        .publish_generated(&link_response.publications, &mut hasher)
        .unwrap();
    assert_eq!(fs::read_link(&compressed_link).unwrap(), Path::new("demo.1.zst"));
    assert!(!directory.join("alias.1..zst").exists());
    assert!(!directory.join("demo.1..zst").exists());
    assert!(
        !compressed_target.exists(),
        "symlink handling eagerly wrote a different collected path"
    );

    let mut target_info = collector.path(&target, &mut hasher).unwrap();
    let target_response = compressman(&mut bucket, &mut target_info).unwrap();
    collector
        .publish_generated(&target_response.publications, &mut hasher)
        .unwrap();
    assert_eq!(
        zstd::stream::decode_all(fs::File::open(&compressed_target).unwrap()).unwrap(),
        content
    );
    collector.seal().unwrap();
}

#[test]
fn pkg_config_dependency_output_requires_complete_records() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("input.pc");
    fs::write(&path, b"fixture").unwrap();
    let info = collect_path(root.path(), &path);

    assert!(parse_pkg_config_dependencies(&info, "").unwrap().is_empty());
    assert_eq!(
        parse_pkg_config_dependencies(
            &info,
            "plain\nexact = 1\ndifferent != 1\nolder < 2\nnewer > 3\nmaximum <= 4\nminimum >= 5\n",
        )
        .unwrap(),
        ["plain", "exact", "different", "older", "newer", "maximum", "minimum",]
    );

    for malformed in [
        "\n",
        "   \n",
        "demo ???\n",
        "demo >=\n",
        "demo >= 1 trailing\n",
        "demo>=1\n",
        "demo\n\nother\n",
    ] {
        assert!(
            parse_pkg_config_dependencies(&info, malformed).is_err(),
            "accepted malformed pkg-config output {malformed:?}"
        );
    }
}

#[test]
fn frozen_root_regular_lookup_stays_beneath_its_descriptor_anchor() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("frozen-root");
    let pkgconfig = root.join("usr/lib32/pkgconfig");
    fs::create_dir_all(&pkgconfig).unwrap();
    fs::write(pkgconfig.join("external.pc"), b"external").unwrap();
    symlink("external.pc", pkgconfig.join("link.pc")).unwrap();
    let anchor = open_frozen_root_anchor(&root).unwrap();

    assert!(descriptor_root_contains_regular_target(&anchor, Path::new("/usr/lib32/pkgconfig/external.pc")).unwrap());
    assert!(!descriptor_root_contains_regular_target(&anchor, Path::new("/usr/lib32/pkgconfig/link.pc")).unwrap());
    assert!(!descriptor_root_contains_regular_target(&anchor, Path::new("/usr/lib32/pkgconfig/missing.pc")).unwrap());

    let displaced = temporary.path().join("displaced-root");
    fs::rename(&root, &displaced).unwrap();
    fs::create_dir(&root).unwrap();
    assert!(
        descriptor_root_contains_regular_target(&anchor, Path::new("/usr/lib32/pkgconfig/external.pc")).unwrap(),
        "lookup followed a replaced root pathname instead of its descriptor"
    );
    assert!(descriptor_root_contains_regular_target(&anchor, Path::new("../external.pc")).is_err());
}

#[test]
fn symlink_and_fifo_never_become_analyzer_inputs() {
    let symlink_root = tempfile::tempdir().unwrap();
    let outside = tempfile::NamedTempFile::new().unwrap();
    let link = symlink_root.path().join("input.pc");
    symlink(outside.path(), &link).unwrap();
    let link_info = collect_path(symlink_root.path(), &link);
    assert!(VerifiedAnalyzerInput::from_path_info(&link_info, 64).is_err());

    let fifo_root = tempfile::tempdir().unwrap();
    let fifo = fifo_root.path().join("input.pc");
    mkfifo(&fifo, Mode::S_IRUSR | Mode::S_IWUSR).unwrap();
    // A FIFO is refused during collection itself and never even becomes a
    // PathInfo, so it can never reach analyzer-input verification.
    let started = Instant::now();
    let mut collector = Collector::new(fifo_root.path());
    collector.add_rule("*", "fixture", PathRuleKind::Any).unwrap();
    let mut hasher = StoneDigestWriterHasher::new();
    assert!(collector.path(&fifo, &mut hasher).is_err());
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[test]
fn path_replacement_never_becomes_analyzer_input() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("input.pc");
    let displaced = root.path().join("displaced");
    fs::write(&path, b"original").unwrap();
    let info = collect_path(root.path(), &path);
    fs::rename(&path, displaced).unwrap();
    fs::write(&path, b"attacker").unwrap();

    assert!(VerifiedAnalyzerInput::from_path_info(&info, 64).is_err());
}

#[test]
fn analyzer_commands_have_no_ambient_environment_stdin_or_descriptors() {
    let environment = checked_output(analyzer_command("/usr/bin/env")).unwrap();
    assert!(environment.stdout.is_empty());

    let inherited = tempfile::tempfile().unwrap();
    let inherited_fd = inherited.as_raw_fd();
    fcntl(inherited_fd, FcntlArg::F_SETFD(FdFlag::empty())).unwrap();

    let mut command = analyzer_command("/bin/sh");
    command.args(["-c", &format!("test ! -e /proc/self/fd/{inherited_fd} && ! read value")]);

    checked_output(command).unwrap();
}

#[test]
fn analyzer_command_failure_is_rejected_even_with_partial_stdout() {
    let mut command = analyzer_command("/bin/sh");
    command.args(["-c", "printf partial-output; printf analyzer-failed >&2; exit 9"]);

    let error = checked_output(command).unwrap_err().to_string();

    assert!(error.contains("exit status: 9"), "{error}");
    assert!(error.contains("analyzer-failed"), "{error}");
}

#[test]
fn analyzer_output_accepts_each_pipe_at_its_exact_byte_limit() {
    let mut command = analyzer_command("/bin/sh");
    command.args(["-c", "printf 12345678; printf abcdefgh >&2"]);

    let output = checked_test_output(command, test_limits(8, 8, Duration::from_secs(2))).unwrap();

    assert_eq!(output.stdout, b"12345678");
    assert_eq!(output.stderr, b"abcdefgh");
}

#[test]
fn analyzer_stdout_rejects_one_byte_over_limit() {
    let mut command = analyzer_command("/bin/sh");
    command.args(["-c", "printf 123456789"]);

    let error = checked_test_output(command, test_limits(8, 8, Duration::from_secs(2))).unwrap_err();

    assert!(matches!(
        execution_error(&error),
        AnalyzerExecutionError::OutputLimit {
            pipe: AnalyzerPipe::Stdout,
            limit: 8,
            ..
        }
    ));
}

#[test]
fn analyzer_stderr_rejects_one_byte_over_limit() {
    let mut command = analyzer_command("/bin/sh");
    command.args(["-c", "printf abcdefghi >&2"]);

    let error = checked_test_output(command, test_limits(8, 8, Duration::from_secs(2))).unwrap_err();

    assert!(matches!(
        execution_error(&error),
        AnalyzerExecutionError::OutputLimit {
            pipe: AnalyzerPipe::Stderr,
            limit: 8,
            ..
        }
    ));
}

#[test]
fn sleeping_analyzer_times_out_and_its_background_process_is_cleaned_up() {
    let temporary = tempfile::tempdir().unwrap();
    let marker = temporary.path().join("delayed-write");
    let mut command = analyzer_command("/bin/sh");
    command.env("MARKER", &marker).args([
        "-c",
        "(/bin/sleep 0.2; printf escaped > \"$MARKER\") & exec /bin/sleep 30",
    ]);

    let started = Instant::now();
    let error = checked_test_output(command, test_limits(64, 64, Duration::from_millis(50))).unwrap_err();

    assert!(matches!(
        execution_error(&error),
        AnalyzerExecutionError::Timeout {
            timeout,
            ..
        } if *timeout == Duration::from_millis(50)
    ));
    assert!(started.elapsed() < Duration::from_secs(2));
    thread::sleep(Duration::from_millis(400));
    assert!(!marker.exists());
}

#[test]
fn background_analyzer_pipe_holder_cannot_hang_packaging() {
    let mut command = analyzer_command("/bin/sh");
    command.args(["-c", "printf direct-output; (/bin/sleep 30) &"]);

    let started = Instant::now();
    let output = checked_output(command).unwrap();

    assert_eq!(output.stdout, b"direct-output");
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn background_analyzer_cannot_mutate_after_direct_child_exit() {
    let temporary = tempfile::tempdir().unwrap();
    let marker = temporary.path().join("delayed-write");
    let mut command = analyzer_command("/bin/sh");
    command
        .env("MARKER", &marker)
        .args(["-c", "(sleep 0.2; printf escaped > \"$MARKER\") &"]);

    checked_output(command).unwrap();
    thread::sleep(Duration::from_millis(500));

    assert!(!marker.exists());
}

#[test]
fn pipe_reader_cleanup_has_a_finite_deadline() {
    let (stdout_reader, stdout_writer) = std::os::unix::net::UnixStream::pair().unwrap();
    let (stderr_reader, stderr_writer) = std::os::unix::net::UnixStream::pair().unwrap();
    let (events, _received) = mpsc::channel();
    let readers = [
        read_analyzer_pipe(stdout_reader, AnalyzerPipe::Stdout, 64, events.clone()).unwrap(),
        read_analyzer_pipe(stderr_reader, AnalyzerPipe::Stderr, 64, events).unwrap(),
    ];
    let started = Instant::now();
    let error = join_analyzer_pipe_readers_until(
        readers,
        "blocked pipe fixture",
        Instant::now() + Duration::from_millis(25),
    )
    .unwrap_err();

    assert!(matches!(error, AnalyzerExecutionError::ReaderCleanupTimeout { .. }));
    assert!(started.elapsed() < Duration::from_secs(1));
    drop(stdout_writer);
    drop(stderr_writer);
}

#[test]
fn analyzer_operation_and_cleanup_failures_are_both_preserved() {
    let operation = AnalyzerExecutionError::Timeout {
        invocation: "fixture".to_owned(),
        timeout: Duration::from_millis(10),
    };
    let cleanup = Err(AnalyzerExecutionError::ReaderCleanupTimeout {
        invocation: "fixture".to_owned(),
        timeout: Duration::from_millis(20),
    });

    let combined = with_analyzer_cleanup(operation, cleanup);
    assert!(matches!(
        combined,
        AnalyzerExecutionError::OperationCleanup {
            operation,
            cleanup,
        } if matches!(*operation, AnalyzerExecutionError::Timeout { .. })
            && matches!(*cleanup, AnalyzerExecutionError::ReaderCleanupTimeout { .. })
    ));
}

#[test]
fn production_containment_is_rejected_before_analyzer_spawn() {
    if getpid().as_raw() == 1 {
        return;
    }
    let temporary = tempfile::tempdir().unwrap();
    let marker = temporary.path().join("spawned");
    let mut command = analyzer_command("/bin/sh");
    command
        .env("MARKER", &marker)
        .args(["-c", "printf spawned > \"$MARKER\""]);

    let error = contained_output(
        &mut command,
        AnalyzerContainment::PidNamespace,
        test_limits(64, 64, Duration::from_secs(1)),
        "containment fixture",
    )
    .unwrap_err();

    assert!(matches!(error, AnalyzerExecutionError::Containment { .. }));
    assert!(!marker.exists());
}

#[test]
fn production_analyzer_cleanup_targets_the_complete_pid_namespace() {
    assert_eq!(
        analyzer_descendant_signal_target(AnalyzerContainment::PidNamespace, Pid::from_raw(1234)),
        Pid::from_raw(-1)
    );
}

#[test]
fn production_handlers_do_not_embed_analyzer_program_selection() {
    let production = |source: &'static str| source.split("#[cfg(test)]").next().unwrap();
    let sources = [
        production(include_str!("../handler.rs")),
        production(include_str!("execution.rs")),
        production(include_str!("input.rs")),
        production(include_str!("sandbox.rs")),
        production(include_str!("python.rs")),
        production(include_str!("elf.rs")),
    ];

    for source in sources {
        assert!(
            !source.contains("/proc/"),
            "production analyzer input depends on procfs"
        );
        assert!(
            !source.contains("command_path"),
            "production analyzer passes inherited-descriptor path"
        );
        for forbidden in [
            "/usr/bin/pkg-config",
            "/usr/bin/python3",
            "/usr/bin/llvm-objcopy",
            "/usr/bin/llvm-strip",
            "/usr/bin/objcopy",
            "/usr/bin/strip",
            "AnalysisToolchain",
        ] {
            assert!(!source.contains(forbidden), "production analyzer embeds {forbidden}");
        }
    }
}
