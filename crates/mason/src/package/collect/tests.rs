use std::{
    ffi::CString,
    fs::Permissions,
    io::{ErrorKind, Read as _},
    os::unix::{
        ffi::{OsStrExt, OsStringExt},
        fs::{PermissionsExt, symlink},
        net::UnixListener,
    },
    sync::atomic::{AtomicBool, Ordering},
};

use fs_err as fs;
use glob::Pattern;

use super::*;

fn add_rule(collector: &mut Collector, pattern: &str, package: &str, kind: PathRuleKind) {
    collector.add_rule(pattern, package, kind).unwrap();
}

fn all_collector(root: &Path) -> Collector {
    let mut collector = Collector::new(root);
    add_rule(&mut collector, "*", "out", PathRuleKind::Any);
    collector
}

fn collector_with_limits(root: &Path, limits: CollectionLimits) -> Collector {
    let mut collector = Collector::new_with_limits(root, limits);
    add_rule(&mut collector, "*", "out", PathRuleKind::Any);
    collector
}

fn write_file(root: &Path, relative: &str, mode: u32) -> PathBuf {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, b"payload").unwrap();
    fs::set_permissions(&path, Permissions::from_mode(mode)).unwrap();
    path
}

#[test]
fn raw_glob_candidates_and_reverse_rule_precedence_select_the_output() {
    let root = tempfile::tempdir().unwrap();
    let path = write_file(root.path(), "usr/share/[literal]", 0o644);
    let mut collector = Collector::new(root.path());
    add_rule(&mut collector, "*", "fallback", PathRuleKind::Any);
    add_rule(
        &mut collector,
        &Pattern::escape("/usr/share/[literal]"),
        "lower-priority",
        PathRuleKind::Any,
    );
    add_rule(
        &mut collector,
        &Pattern::escape("/usr/share/[literal]"),
        "highest-priority",
        PathRuleKind::Any,
    );

    let info = collector.path(&path, &mut StoneDigestWriterHasher::new()).unwrap();
    assert_eq!(info.target_path, Path::new("/usr/share/[literal]"));
    assert_eq!(info.package.as_ref(), "highest-priority");
}

#[test]
fn rule_limits_accept_n_reject_n_plus_one_and_compile_once() {
    let root = tempfile::tempdir().unwrap();
    let mut limits = CollectionLimits::default();
    limits.max_rules = 2;
    limits.max_rule_pattern_bytes = 4;
    limits.max_rule_package_bytes = 3;
    limits.max_total_rule_pattern_bytes = 8;
    limits.max_total_rule_package_bytes = 6;
    let mut collector = Collector::new_with_limits(root.path(), limits);
    collector.add_rule("/a", "out", PathRuleKind::Any).unwrap();
    collector.add_rule("/b", "out", PathRuleKind::Any).unwrap();
    assert!(Arc::ptr_eq(&collector.rules[0].package, &collector.rules[1].package));
    assert!(matches!(
        collector.add_rule("/c", "out", PathRuleKind::Any),
        Err(Error::LimitExceeded {
            resource: "collection rules",
            limit: 2,
            actual: 3,
            ..
        })
    ));

    let mut limits = CollectionLimits::default();
    limits.max_rule_pattern_bytes = 3;
    let mut collector = Collector::new_with_limits(root.path(), limits);
    collector.add_rule("abc", "p", PathRuleKind::Any).unwrap();
    assert!(matches!(
        collector.add_rule("abcd", "p", PathRuleKind::Any),
        Err(Error::LimitExceeded {
            resource: "rule pattern bytes",
            limit: 3,
            actual: 4,
            ..
        })
    ));

    let mut limits = CollectionLimits::default();
    limits.max_rule_package_bytes = 3;
    let mut collector = Collector::new_with_limits(root.path(), limits);
    collector.add_rule("*", "pkg", PathRuleKind::Any).unwrap();
    assert!(matches!(
        collector.add_rule("*", "pkgs", PathRuleKind::Any),
        Err(Error::LimitExceeded {
            resource: "rule package bytes",
            limit: 3,
            actual: 4,
            ..
        })
    ));

    let mut limits = CollectionLimits::default();
    limits.max_total_rule_pattern_bytes = 2;
    limits.max_total_rule_package_bytes = 2;
    let mut collector = Collector::new_with_limits(root.path(), limits);
    collector.add_rule("a", "p", PathRuleKind::Any).unwrap();
    collector.add_rule("b", "q", PathRuleKind::Any).unwrap();
    assert!(matches!(
        collector.add_rule("c", "r", PathRuleKind::Any),
        Err(Error::LimitExceeded {
            resource: "total rule pattern bytes",
            limit: 2,
            actual: 3,
            ..
        })
    ));

    let mut limits = CollectionLimits::default();
    limits.max_total_rule_package_bytes = 2;
    let mut collector = Collector::new_with_limits(root.path(), limits);
    collector.add_rule("a", "p", PathRuleKind::Any).unwrap();
    collector.add_rule("b", "q", PathRuleKind::Any).unwrap();
    assert!(matches!(
        collector.add_rule("c", "r", PathRuleKind::Any),
        Err(Error::LimitExceeded {
            resource: "total rule package bytes",
            limit: 2,
            actual: 3,
            ..
        })
    ));

    let mut collector = Collector::new(root.path());
    assert!(matches!(
        collector.add_rule("[", "out", PathRuleKind::Any),
        Err(Error::InvalidRulePattern { .. })
    ));
}

#[test]
fn non_utf8_paths_and_symlink_targets_are_rejected_without_lossy_layouts() {
    let root = tempfile::tempdir().unwrap();
    let invalid_name = OsString::from_vec(vec![b'f', 0x80]);
    fs::write(root.path().join(&invalid_name), b"data").unwrap();
    assert!(matches!(
        all_collector(root.path()).enumerate_paths(None, &mut StoneDigestWriterHasher::new()),
        Err(Error::NonUtf8Path { .. })
    ));

    let root = tempfile::tempdir().unwrap();
    let invalid_target = OsString::from_vec(vec![b't', 0x80]);
    symlink(&invalid_target, root.path().join("link")).unwrap();
    assert!(matches!(
        all_collector(root.path()).enumerate_paths(None, &mut StoneDigestWriterHasher::new()),
        Err(Error::NonUtf8SymlinkTarget { .. })
    ));
}

#[test]
fn every_path_reuses_the_single_interned_package_label() {
    let root = tempfile::tempdir().unwrap();
    for index in 0..128 {
        fs::write(root.path().join(format!("file-{index:03}")), b"x").unwrap();
    }
    let paths = all_collector(root.path())
        .enumerate_paths(None, &mut StoneDigestWriterHasher::new())
        .unwrap();
    let package = &paths.first().unwrap().package;

    assert_eq!(paths.len(), 128);
    assert!(paths.iter().all(|path| Arc::ptr_eq(package, &path.package)));
}

#[test]
fn executable_rules_require_a_regular_file_with_an_execute_bit() {
    let root = tempfile::tempdir().unwrap();
    let executable = write_file(root.path(), "usr/bin/tool", 0o751);
    let regular = write_file(root.path(), "usr/bin/data", 0o644);
    let mut collector = Collector::new(root.path());
    add_rule(&mut collector, "*", "out", PathRuleKind::Any);
    add_rule(&mut collector, "/usr/bin/*", "executables", PathRuleKind::Executable);
    let mut hasher = StoneDigestWriterHasher::new();

    assert_eq!(
        collector.path(&executable, &mut hasher).unwrap().package.as_ref(),
        "executables"
    );
    assert_eq!(collector.path(&regular, &mut hasher).unwrap().package.as_ref(), "out");
}

#[test]
fn symlink_rules_use_lstat_and_enumeration_does_not_follow_linked_directories() {
    let root = tempfile::tempdir().unwrap();
    let external = tempfile::tempdir().unwrap();
    write_file(external.path(), "nested/file", 0o644);
    let linked_dir = root.path().join("linked-dir");
    symlink(external.path().join("nested"), &linked_dir).unwrap();
    let broken = root.path().join("broken");
    symlink(root.path().join("missing"), &broken).unwrap();

    let mut collector = Collector::new(root.path());
    add_rule(&mut collector, "*", "out", PathRuleKind::Any);
    add_rule(&mut collector, "/*", "links", PathRuleKind::Symlink);
    let paths = collector
        .enumerate_paths(None, &mut StoneDigestWriterHasher::new())
        .unwrap();

    assert_eq!(
        paths.iter().map(|path| path.target_path.as_path()).collect::<Vec<_>>(),
        [Path::new("/broken"), Path::new("/linked-dir")]
    );
    assert!(paths.iter().all(|path| path.package.as_ref() == "links"));
    assert!(
        paths
            .iter()
            .all(|path| matches!(path.layout.file, StonePayloadLayoutFile::Symlink(..)))
    );
}

#[test]
fn special_rules_match_unix_domain_sockets() {
    let root = tempfile::tempdir().unwrap();
    let socket = root.path().join("run/service.sock");
    fs::create_dir_all(socket.parent().unwrap()).unwrap();
    let _listener = match UnixListener::bind(&socket) {
        Ok(listener) => listener,
        Err(error) if error.kind() == ErrorKind::PermissionDenied => return,
        Err(error) => panic!("failed to create Unix socket fixture: {error}"),
    };
    let mut collector = Collector::new(root.path());
    add_rule(&mut collector, "*", "out", PathRuleKind::Any);
    add_rule(&mut collector, "/run/*", "special", PathRuleKind::Special);

    let info = collector.path(&socket, &mut StoneDigestWriterHasher::new()).unwrap();
    assert_eq!(info.package.as_ref(), "special");
    assert!(matches!(info.layout.file, StonePayloadLayoutFile::Socket(..)));
}

#[test]
fn collection_has_no_implicit_fallback_output() {
    let root = tempfile::tempdir().unwrap();
    let regular = write_file(root.path(), "usr/bin/data", 0o644);
    let mut collector = Collector::new(root.path());
    add_rule(&mut collector, "/usr/bin/*", "executables", PathRuleKind::Executable);

    assert!(matches!(
        collector.path(&regular, &mut StoneDigestWriterHasher::new()),
        Err(Error::NoMatchingRule { .. })
    ));
}

#[test]
fn entry_depth_name_path_file_and_aggregate_limits_accept_n_and_reject_n_plus_one() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir_all(root.path().join("d")).unwrap();
    fs::write(root.path().join("d/a"), b"1234").unwrap();
    fs::write(root.path().join("b"), b"56").unwrap();

    let mut limits = CollectionLimits::default();
    limits.max_entries = 3;
    limits.max_depth = 2;
    limits.max_name_bytes = 1;
    limits.max_path_bytes = 3;
    limits.max_total_name_bytes = 3;
    limits.max_total_path_bytes = 5;
    limits.max_file_bytes = 4;
    limits.max_total_regular_bytes = 6;
    collector_with_limits(root.path(), limits)
        .enumerate_paths(None, &mut StoneDigestWriterHasher::new())
        .unwrap();

    for (resource, mutate) in [
        ("total entries", 0usize),
        ("path depth", 1),
        ("entry name bytes", 2),
        ("entry path bytes", 3),
        ("regular file bytes", 4),
        ("total regular file bytes", 5),
        ("total entry name bytes", 6),
        ("total entry path bytes", 7),
    ] {
        let mut rejected = limits;
        match mutate {
            0 => rejected.max_entries -= 1,
            1 => rejected.max_depth -= 1,
            2 => rejected.max_name_bytes = 0,
            3 => rejected.max_path_bytes -= 1,
            4 => rejected.max_file_bytes -= 1,
            5 => rejected.max_total_regular_bytes -= 1,
            6 => rejected.max_total_name_bytes -= 1,
            7 => rejected.max_total_path_bytes -= 1,
            _ => unreachable!(),
        }
        let error = collector_with_limits(root.path(), rejected)
            .enumerate_paths(None, &mut StoneDigestWriterHasher::new())
            .unwrap_err();
        assert!(
            matches!(error, Error::LimitExceeded { resource: actual, .. } if actual == resource),
            "expected {resource}, got {error:?}"
        );
    }
}

#[test]
fn symlink_target_limit_accepts_n_and_rejects_n_plus_one() {
    let root = tempfile::tempdir().unwrap();
    symlink("12345678", root.path().join("link")).unwrap();
    symlink("1234", root.path().join("link2")).unwrap();
    let mut limits = CollectionLimits::default();
    limits.max_symlink_target_bytes = 8;
    limits.max_total_symlink_target_bytes = 12;
    collector_with_limits(root.path(), limits)
        .enumerate_paths(None, &mut StoneDigestWriterHasher::new())
        .unwrap();
    limits.max_symlink_target_bytes = 7;
    assert!(matches!(
        collector_with_limits(root.path(), limits).enumerate_paths(None, &mut StoneDigestWriterHasher::new()),
        Err(Error::LimitExceeded {
            resource: "symlink target bytes",
            limit: 7,
            actual: 8,
            ..
        })
    ));
    limits.max_symlink_target_bytes = 8;
    limits.max_total_symlink_target_bytes = 11;
    assert!(matches!(
        collector_with_limits(root.path(), limits).enumerate_paths(None, &mut StoneDigestWriterHasher::new()),
        Err(Error::LimitExceeded {
            resource: "total symlink target bytes",
            limit: 11,
            actual: 12,
            ..
        })
    ));
}

#[test]
fn traversal_is_deterministic_iterative_and_preserves_empty_special_directories() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("z"), b"z").unwrap();
    fs::write(root.path().join("a"), b"a").unwrap();
    let mut current = root.path().to_owned();
    for _ in 0..300 {
        current.push("d");
        fs::create_dir(&current).unwrap();
    }
    fs::write(current.join("leaf"), b"leaf").unwrap();
    fs::create_dir(root.path().join("empty")).unwrap();
    fs::create_dir(root.path().join("special")).unwrap();
    fs::set_permissions(root.path().join("special"), Permissions::from_mode(0o700)).unwrap();

    let mut limits = CollectionLimits::default();
    limits.max_depth = 512;
    limits.max_path_bytes = 1024;
    let first = collector_with_limits(root.path(), limits)
        .enumerate_paths(None, &mut StoneDigestWriterHasher::new())
        .unwrap();
    let second = collector_with_limits(root.path(), limits)
        .enumerate_paths(None, &mut StoneDigestWriterHasher::new())
        .unwrap();
    let first_paths = first.iter().map(|path| path.target_path.clone()).collect::<Vec<_>>();
    let second_paths = second.iter().map(|path| path.target_path.clone()).collect::<Vec<_>>();
    assert_eq!(first_paths, second_paths);
    assert_eq!(first_paths.first().unwrap(), Path::new("/a"));
    assert!(first_paths.contains(&PathBuf::from("/empty")));
    assert!(first_paths.contains(&PathBuf::from("/special")));
    assert!(first_paths.iter().any(|path| path.ends_with("leaf")));
}

#[test]
fn collection_deadline_is_finite() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("file"), b"data").unwrap();
    let mut limits = CollectionLimits::default();
    limits.max_duration = Duration::ZERO;
    assert!(matches!(
        collector_with_limits(root.path(), limits).enumerate_paths(None, &mut StoneDigestWriterHasher::new()),
        Err(Error::DurationExceeded { .. })
    ));
}

#[test]
fn shared_deadline_survives_collection_and_expires_before_emission() {
    let root = tempfile::tempdir().unwrap();
    let path = write_file(root.path(), "file", 0o644);
    let mut limits = CollectionLimits::default();
    limits.max_duration = Duration::from_millis(100);
    let collector = collector_with_limits(root.path(), limits);
    let info = collector.path(&path, &mut StoneDigestWriterHasher::new()).unwrap();

    std::thread::sleep(Duration::from_millis(150));

    assert!(matches!(info.open_verified(), Err(Error::DurationExceeded { .. })));
}

#[test]
fn sparse_large_file_is_hashed_at_n_and_rejected_at_n_plus_one() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("large");
    let bytes = 2 * MIB;
    File::create(&path).unwrap().set_len(bytes).unwrap();
    let mut limits = CollectionLimits::default();
    limits.max_file_bytes = bytes;
    limits.max_total_regular_bytes = bytes;
    collector_with_limits(root.path(), limits)
        .enumerate_paths(None, &mut StoneDigestWriterHasher::new())
        .unwrap();

    limits.max_file_bytes = bytes - 1;
    assert!(matches!(
        collector_with_limits(root.path(), limits).enumerate_paths(None, &mut StoneDigestWriterHasher::new()),
        Err(Error::LimitExceeded {
            resource: "regular file bytes",
            limit,
            actual,
            ..
        }) if limit == bytes - 1 && actual == bytes
    ));
}

#[test]
fn returned_paths_do_not_retain_one_descriptor_per_directory() {
    const CHILD_ENV: &str = "MASON_COLLECTOR_FD_REGRESSION_CHILD";
    const TEST_NAME: &str = "package::collect::tests::returned_paths_do_not_retain_one_descriptor_per_directory";
    if std::env::var_os(CHILD_ENV).is_none() {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([TEST_NAME, "--exact", "--test-threads=1"])
            .env(CHILD_ENV, "1")
            .status()
            .unwrap();
        assert!(status.success(), "isolated descriptor regression failed: {status}");
        return;
    }

    let root = tempfile::tempdir().unwrap();
    for index in 0..256 {
        let directory = root.path().join(format!("d{index:03}"));
        fs::create_dir(&directory).unwrap();
        fs::write(directory.join("file"), b"x").unwrap();
    }
    let descriptor_count = || fs::read_dir("/proc/self/fd").unwrap().count();
    let before = descriptor_count();
    let collector = all_collector(root.path());
    let paths = collector
        .enumerate_paths(None, &mut StoneDigestWriterHasher::new())
        .unwrap();
    let after = descriptor_count();

    assert_eq!(paths.iter().filter(|path| path.is_file()).count(), 256);
    assert!(
        after <= before + 8,
        "collected paths retained {} unexpected descriptors",
        after.saturating_sub(before)
    );
}

#[test]
fn verified_reader_rejects_in_place_changes_during_content_emission() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("file");
    fs::write(&path, vec![b'a'; HASH_BUFFER_BYTES * 2]).unwrap();
    let collector = all_collector(root.path());
    let info = collector.path(&path, &mut StoneDigestWriterHasher::new()).unwrap();
    let mut reader = info.open_verified().unwrap();
    let mut prefix = vec![0u8; HASH_BUFFER_BYTES];
    reader.read_exact(&mut prefix).unwrap();

    fs::write(&path, vec![b'b'; HASH_BUFFER_BYTES * 2]).unwrap();
    let mut suffix = Vec::new();
    reader.read_to_end(&mut suffix).unwrap();

    assert!(matches!(
        reader.finish(),
        Err(Error::ContentHashChanged { .. } | Error::TreeChanged { .. })
    ));
}

#[test]
fn replacing_the_collector_root_invalidates_collected_paths() {
    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("root");
    fs::create_dir(&root).unwrap();
    let path = write_file(&root, "file", 0o644);
    let collector = all_collector(&root);
    let info = collector.path(&path, &mut StoneDigestWriterHasher::new()).unwrap();
    fs::rename(&root, base.path().join("old-root")).unwrap();
    fs::create_dir(&root).unwrap();
    fs::write(root.join("file"), b"payload").unwrap();

    assert!(matches!(info.verify_unchanged(), Err(Error::TreeChanged { .. })));
}

#[test]
fn in_place_change_replacement_directory_swap_and_fifo_masquerade_are_rejected() {
    fn run_race(action: impl Fn(&Path) + Send + Sync + 'static, point: TestPoint) -> Error {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("file"), b"original").unwrap();
        let fired = Arc::new(AtomicBool::new(false));
        let fired_hook = Arc::clone(&fired);
        let mut collector = all_collector(root.path());
        collector.hook = Some(Arc::new(move |actual, path| {
            if actual == point && !fired_hook.swap(true, Ordering::SeqCst) {
                action(path);
            }
        }));
        let error = collector
            .enumerate_paths(None, &mut StoneDigestWriterHasher::new())
            .unwrap_err();
        assert!(fired.load(Ordering::SeqCst));
        error
    }

    assert!(matches!(
        run_race(
            |path| fs::write(path, b"changed!").unwrap(),
            TestPoint::AfterRegularOpen
        ),
        Error::TreeChanged { .. }
    ));
    assert!(matches!(
        run_race(
            |path| {
                let old = path.with_extension("old");
                fs::rename(path, old).unwrap();
                fs::write(path, b"replacement").unwrap();
            },
            TestPoint::AfterRegularHash
        ),
        Error::TreeChanged { .. }
    ));
    assert!(matches!(
        run_race(
            |path| {
                fs::remove_file(path).unwrap();
                let name = CString::new(path.as_os_str().as_bytes()).unwrap();
                // SAFETY: name is a live NUL-terminated pathname.
                assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
            },
            TestPoint::AfterEntryHandle
        ),
        Error::TreeChanged { .. }
    ));

    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("dir")).unwrap();
    fs::write(root.path().join("dir/file"), b"data").unwrap();
    let fired = Arc::new(AtomicBool::new(false));
    let fired_hook = Arc::clone(&fired);
    let root_path = root.path().to_owned();
    let mut collector = all_collector(root.path());
    collector.hook = Some(Arc::new(move |point, path| {
        if point == TestPoint::AfterDirectoryOpen && path.ends_with("dir") && !fired_hook.swap(true, Ordering::SeqCst) {
            fs::rename(path, root_path.join("old-dir")).unwrap();
            fs::create_dir(path).unwrap();
        }
    }));
    assert!(matches!(
        collector.enumerate_paths(None, &mut StoneDigestWriterHasher::new()),
        Err(Error::TreeChanged { .. })
    ));
}

#[test]
fn collected_file_replacement_is_rejected_before_verified_emission() {
    let root = tempfile::tempdir().unwrap();
    let path = write_file(root.path(), "file", 0o644);
    let collector = all_collector(root.path());
    let info = collector.path(&path, &mut StoneDigestWriterHasher::new()).unwrap();
    fs::rename(&path, root.path().join("old")).unwrap();
    fs::write(&path, b"payload").unwrap();

    assert!(matches!(info.open_verified(), Err(Error::TreeChanged { .. })));
}

#[test]
fn complete_inventory_witnesses_ignored_entries_and_an_empty_root() {
    let root = tempfile::tempdir().unwrap();
    let included = write_file(root.path(), "included", 0o644);
    let ignored = write_file(root.path(), "ignored", 0o644);
    let collector = all_collector(root.path());
    let paths = collector
        .enumerate_paths(None, &mut StoneDigestWriterHasher::new())
        .unwrap();
    assert_eq!(paths.len(), 2);
    drop(paths.into_iter().find(|path| path.path == ignored));
    fs::write(&ignored, b"changed").unwrap();
    assert!(matches!(collector.seal(), Err(Error::TreeChanged { .. })));
    assert!(included.exists());

    let root = tempfile::tempdir().unwrap();
    let collector = all_collector(root.path());
    assert!(
        collector
            .enumerate_paths(None, &mut StoneDigestWriterHasher::new())
            .unwrap()
            .is_empty()
    );
    fs::write(root.path().join("late"), b"late").unwrap();
    assert!(matches!(collector.seal(), Err(Error::TreeChanged { .. })));
}

#[test]
fn generated_admission_is_exact_batched_and_allows_only_declared_ancestors() {
    let root = tempfile::tempdir().unwrap();
    let collector = all_collector(root.path());
    collector
        .enumerate_paths(None, &mut StoneDigestWriterHasher::new())
        .unwrap();
    let first = write_file(root.path(), "new/a", 0o644);
    let second = write_file(root.path(), "new/b", 0o644);
    let paths = collector
        .paths(&[first.clone(), second.clone()], &mut StoneDigestWriterHasher::new())
        .unwrap();
    assert_eq!(paths.len(), 2);
    collector.seal().unwrap().verify().unwrap();

    let root = tempfile::tempdir().unwrap();
    let collector = all_collector(root.path());
    collector
        .enumerate_paths(None, &mut StoneDigestWriterHasher::new())
        .unwrap();
    let declared = write_file(root.path(), "nested/generated/leaf", 0o644);
    write_file(root.path(), "nested/generated/extra", 0o644);
    assert!(matches!(
        collector.paths(&[declared], &mut StoneDigestWriterHasher::new()),
        Err(Error::TreeChanged { .. })
    ));
    assert!(matches!(collector.seal(), Err(Error::InventoryPoisoned)));
}

#[test]
fn generated_admission_rejects_existing_terminals_and_duplicate_declarations() {
    let root = tempfile::tempdir().unwrap();
    let existing = write_file(root.path(), "existing", 0o644);
    let collector = all_collector(root.path());
    collector
        .enumerate_paths(None, &mut StoneDigestWriterHasher::new())
        .unwrap();
    assert!(matches!(
        collector.paths(&[existing], &mut StoneDigestWriterHasher::new()),
        Err(Error::ExistingAdmission { .. })
    ));

    let root = tempfile::tempdir().unwrap();
    let collector = all_collector(root.path());
    collector
        .enumerate_paths(None, &mut StoneDigestWriterHasher::new())
        .unwrap();
    let generated = write_file(root.path(), "generated", 0o644);
    assert!(matches!(
        collector.paths(&[generated.clone(), generated], &mut StoneDigestWriterHasher::new()),
        Err(Error::DuplicateAdmission { .. })
    ));

    let root = tempfile::tempdir().unwrap();
    let mut collector = Collector::new(root.path());
    add_rule(&mut collector, "/initial", "out", PathRuleKind::Any);
    collector
        .enumerate_paths(None, &mut StoneDigestWriterHasher::new())
        .unwrap();
    let generated = write_file(root.path(), "generated", 0o644);
    assert!(matches!(
        collector.paths(&[generated], &mut StoneDigestWriterHasher::new()),
        Err(Error::NoMatchingRule { .. })
    ));
    assert!(matches!(collector.seal(), Err(Error::InventoryPoisoned)));
}

#[test]
fn every_complete_rescan_enforces_exact_aggregate_limits() {
    let root = tempfile::tempdir().unwrap();
    write_file(root.path(), "one", 0o644);
    write_file(root.path(), "two", 0o644);
    let mut limits = CollectionLimits::default();
    limits.max_entries = 2;
    let collector = collector_with_limits(root.path(), limits);
    collector
        .enumerate_paths(None, &mut StoneDigestWriterHasher::new())
        .unwrap();
    collector.seal().unwrap();

    let root = tempfile::tempdir().unwrap();
    write_file(root.path(), "one", 0o644);
    let mut limits = CollectionLimits::default();
    limits.max_entries = 1;
    let collector = collector_with_limits(root.path(), limits);
    collector
        .enumerate_paths(None, &mut StoneDigestWriterHasher::new())
        .unwrap();
    let generated = write_file(root.path(), "two", 0o644);
    assert!(matches!(
        collector.paths(&[generated], &mut StoneDigestWriterHasher::new()),
        Err(Error::LimitExceeded {
            resource: "total entries",
            limit: 1,
            actual: 2,
            ..
        })
    ));
}

#[test]
fn declaration_trie_accepts_exact_remaining_entries_and_rejects_n_plus_one_before_admission() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir_all(root.path().join("a/b")).unwrap();
    let mut limits = CollectionLimits::default();
    limits.max_entries = 3;
    let collector = collector_with_limits(root.path(), limits);
    collector
        .enumerate_paths(None, &mut StoneDigestWriterHasher::new())
        .unwrap();
    let generated = write_file(root.path(), "a/b/c", 0o644);
    collector
        .paths(&[generated], &mut StoneDigestWriterHasher::new())
        .unwrap();
    collector.seal().unwrap();

    let root = tempfile::tempdir().unwrap();
    fs::create_dir_all(root.path().join("a/b")).unwrap();
    let collector = collector_with_limits(root.path(), limits);
    collector
        .enumerate_paths(None, &mut StoneDigestWriterHasher::new())
        .unwrap();
    let generated = write_file(root.path(), "a/b/c/d", 0o644);
    assert!(matches!(
        collector.paths(&[generated], &mut StoneDigestWriterHasher::new()),
        Err(Error::LimitExceeded {
            resource: "total entries",
            limit: 3,
            actual: 4,
            ..
        })
    ));
    assert!(matches!(collector.seal(), Err(Error::InventoryPoisoned)));
}

#[test]
fn sealed_inventory_rejects_late_add_delete_metadata_and_replacement() {
    for action in 0..4 {
        let root = tempfile::tempdir().unwrap();
        let path = write_file(root.path(), "file", 0o644);
        let collector = all_collector(root.path());
        collector
            .enumerate_paths(None, &mut StoneDigestWriterHasher::new())
            .unwrap();
        let sealed = collector.seal().unwrap();
        match action {
            0 => fs::write(root.path().join("added"), b"added").unwrap(),
            1 => fs::remove_file(&path).unwrap(),
            2 => fs::set_permissions(&path, Permissions::from_mode(0o600)).unwrap(),
            3 => {
                fs::rename(&path, root.path().join("old")).unwrap();
                fs::write(&path, b"payload").unwrap();
            }
            _ => unreachable!(),
        }
        assert!(matches!(sealed.verify(), Err(Error::TreeChanged { .. })));
    }
}

#[test]
fn sealed_phase_rejects_late_admission() {
    let root = tempfile::tempdir().unwrap();
    write_file(root.path(), "file", 0o644);
    let collector = all_collector(root.path());
    collector
        .enumerate_paths(None, &mut StoneDigestWriterHasher::new())
        .unwrap();
    collector.seal().unwrap();
    let generated = write_file(root.path(), "generated", 0o644);
    assert!(matches!(
        collector.paths(&[generated], &mut StoneDigestWriterHasher::new()),
        Err(Error::InvalidInventoryPhase { phase: "sealed", .. })
    ));
}

#[test]
fn regular_target_lookup_is_inventory_only_and_phase_checked() {
    let root = tempfile::tempdir().unwrap();
    let regular = write_file(root.path(), "usr/lib/pkgconfig/lib.pc", 0o644);
    symlink("lib.pc", root.path().join("usr/lib/pkgconfig/link.pc")).unwrap();
    let collector = all_collector(root.path());
    let paths = collector
        .enumerate_paths(None, &mut StoneDigestWriterHasher::new())
        .unwrap();
    let info = paths
        .iter()
        .find(|info| info.target_path == Path::new("/usr/lib/pkgconfig/lib.pc"))
        .unwrap();
    assert!(
        info.inventory_contains_regular_target(Path::new("/usr/lib/pkgconfig/lib.pc"))
            .unwrap()
    );
    assert!(
        !info
            .inventory_contains_regular_target(Path::new("/usr/lib/pkgconfig/link.pc"))
            .unwrap()
    );
    assert!(
        !info
            .inventory_contains_regular_target(Path::new("/usr/lib/pkgconfig/missing.pc"))
            .unwrap()
    );
    fs::remove_file(regular).unwrap();
    assert!(
        info.inventory_contains_regular_target(Path::new("/usr/lib/pkgconfig/lib.pc"))
            .unwrap()
    );
    assert!(matches!(collector.seal(), Err(Error::TreeChanged { .. })));
    assert!(matches!(
        info.inventory_contains_regular_target(Path::new("/usr/lib/pkgconfig/lib.pc")),
        Err(Error::InventoryPoisoned)
    ));
}
