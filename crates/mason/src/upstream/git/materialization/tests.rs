use std::{
    ffi::OsString,
    os::unix::{
        ffi::OsStringExt,
        fs::{MetadataExt, PermissionsExt, symlink},
        net::UnixListener,
    },
};

use nix::{sys::stat::Mode, unistd::mkfifo};

use super::*;

const EPOCH: i64 = 1_700_000_000;

#[test]
fn empty_tree_has_a_stable_golden_digest() {
    let root = tempfile::tempdir().unwrap();
    assert_eq!(
        normalize_and_hash(root.path(), EPOCH).unwrap(),
        "badf0db4c0a8fd2cde62d7893df1313fcdaca41f8b9cab21c2e58f53c033c908"
    );
}

#[test]
fn trusted_descriptor_root_path_is_accepted_without_weakening_descendant_traversal() {
    let temporary = tempfile::tempdir().unwrap();
    let held_root = temporary.path().join("held");
    let checkout = held_root.join("checkout");
    fs::create_dir_all(&checkout).unwrap();
    fs::write(checkout.join("source"), b"descriptor rooted").unwrap();
    fs::create_dir(checkout.join(".git")).unwrap();
    fs::write(checkout.join(".git/config"), b"admin").unwrap();
    let held = fs::File::open(&held_root).unwrap();
    let descriptor_checkout = PathBuf::from(format!("/proc/{}/fd/{}/checkout", std::process::id(), held.as_raw_fd()));

    remove_git_administration_descriptor_path_bounded(&descriptor_checkout).unwrap();
    let digest = normalize_and_hash_descriptor_path(&descriptor_checkout, EPOCH).unwrap();

    assert_eq!(digest.len(), 64);
    assert_eq!(fs::read(checkout.join("source")).unwrap(), b"descriptor rooted");
    assert!(!checkout.join(".git").exists());

    let staged = held_root.join("staged");
    let installed = held_root.join("installed");
    fs::create_dir(&staged).unwrap();
    fs::write(staged.join("source"), b"sealed descriptor root").unwrap();
    let descriptor_staged = PathBuf::from(format!("/proc/{}/fd/{}/staged", std::process::id(), held.as_raw_fd()));
    let proof =
        normalize_and_seal_descriptor_path_with_limits(&descriptor_staged, EPOCH, MaterializationLimits::default())
            .unwrap();
    fs::rename(&staged, &installed).unwrap();
    let descriptor_installed = PathBuf::from(format!(
        "/proc/{}/fd/{}/installed",
        std::process::id(),
        held.as_raw_fd()
    ));
    proof.verify_installed_descriptor_path(&descriptor_installed).unwrap();
}

#[test]
fn order_non_utf8_permissions_and_timestamps_normalize_identically() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let raw_name = OsString::from_vec(b"non-utf8-\xff".to_vec());
    create_equivalent_tree(first.path(), &raw_name, false, 111);
    create_equivalent_tree(second.path(), &raw_name, true, 222);

    let first_hash = normalize_and_hash(first.path(), EPOCH).unwrap();
    let second_hash = normalize_and_hash(second.path(), EPOCH).unwrap();
    assert_eq!(first_hash, second_hash);

    for root in [first.path(), second.path()] {
        assert_mode(root, DIRECTORY_MODE);
        assert_mode(&root.join("nested"), DIRECTORY_MODE);
        assert_mode(&root.join(&raw_name), REGULAR_MODE);
        assert_mode(&root.join("executable"), EXECUTABLE_MODE);
        for path in [
            root.to_owned(),
            root.join("nested"),
            root.join(&raw_name),
            root.join("executable"),
            root.join("link"),
        ] {
            assert_timestamp(&path, EPOCH);
        }
    }
}

#[test]
fn content_path_mode_type_and_symlink_target_are_semantic() {
    let baseline = digest_with(|_| {});
    let mutations = [
        digest_with(|root| fs::write(root.join("regular"), b"bravo").unwrap()),
        digest_with(|root| fs::write(root.join("regular"), b"longer").unwrap()),
        digest_with(|root| fs::rename(root.join("regular"), root.join("renamed")).unwrap()),
        digest_with(|root| fs::set_permissions(root.join("regular"), Permissions::from_mode(0o755)).unwrap()),
        digest_with(|root| {
            fs::remove_file(root.join("link")).unwrap();
            symlink("executable", root.join("link")).unwrap();
        }),
        digest_with(|root| {
            fs::remove_file(root.join("kind")).unwrap();
            fs::create_dir(root.join("kind")).unwrap();
        }),
    ];
    for mutation in mutations {
        assert_ne!(mutation, baseline);
    }
}

#[test]
fn entry_added_after_the_canonical_hash_is_rejected() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("original"), b"original").unwrap();

    let result = normalize_with_hook(root.path(), |point| {
        if point == HookPoint::AfterCanonicalHash {
            let added = root.path().join("added");
            fs::write(&added, b"added").unwrap();
            fs::set_permissions(added, Permissions::from_mode(REGULAR_MODE)).unwrap();
        }
    });

    assert!(matches!(result, Err(Error::TreeChanged)));
    assert!(root.path().join("added").exists());
}

#[test]
fn same_length_content_change_after_the_canonical_hash_is_rejected() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    fs::write(&source, b"alpha").unwrap();

    let result = normalize_with_hook(root.path(), |point| {
        if point == HookPoint::AfterCanonicalHash {
            fs::write(&source, b"bravo").unwrap();
        }
    });

    assert!(matches!(result, Err(Error::TreeChanged)));
    assert_eq!(fs::read(source).unwrap(), b"bravo");
}

#[test]
fn ancestor_symlink_swap_cannot_escape_the_audited_root() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("tree");
    let moved = temporary.path().join("moved-outside");
    let nested = root.join("nested");
    let source = nested.join("source");
    fs::create_dir_all(&nested).unwrap();
    fs::write(&source, b"outside sentinel").unwrap();
    fs::set_permissions(&source, Permissions::from_mode(0o600)).unwrap();
    let old = filetime::FileTime::from_unix_time(123, 0);
    filetime::set_file_times(&source, old, old).unwrap();

    let result = normalize_with_hook(&root, |point| {
        if point == HookPoint::AfterAudit {
            fs::rename(&nested, &moved).unwrap();
            symlink(&moved, &nested).unwrap();
        }
    });

    assert!(result.is_err());
    let escaped = moved.join("source");
    assert_mode(&escaped, 0o600);
    assert_eq!(fs::metadata(escaped).unwrap().mtime(), 123);
}

#[test]
fn hard_links_reject_the_whole_tree_before_mutation() {
    let root = tempfile::tempdir().unwrap();
    let original = root.path().join("a-original");
    fs::write(&original, b"shared").unwrap();
    fs::set_permissions(&original, Permissions::from_mode(0o600)).unwrap();
    let old = filetime::FileTime::from_unix_time(123, 0);
    filetime::set_file_times(&original, old, old).unwrap();
    fs::hard_link(&original, root.path().join("b-link")).unwrap();

    assert!(matches!(
        normalize_and_hash(root.path(), EPOCH),
        Err(Error::UnexpectedLinkCount { links: 2, .. })
    ));
    assert_mode(&original, 0o600);
    assert_eq!(fs::metadata(&original).unwrap().mtime(), 123);
}

#[test]
fn fifos_and_sockets_are_rejected_before_mutation() {
    let fifo_root = tempfile::tempdir().unwrap();
    let fifo_sentinel = fifo_root.path().join("a-sentinel");
    fs::write(&fifo_sentinel, b"sentinel").unwrap();
    fs::set_permissions(&fifo_sentinel, Permissions::from_mode(0o600)).unwrap();
    mkfifo(&fifo_root.path().join("z-fifo"), Mode::S_IRUSR | Mode::S_IWUSR).unwrap();
    assert!(matches!(
        normalize_and_hash(fifo_root.path(), EPOCH),
        Err(Error::UnsupportedFileType { kind: "FIFO", .. })
    ));
    assert_mode(&fifo_sentinel, 0o600);

    let socket_root = tempfile::tempdir().unwrap();
    let socket_sentinel = socket_root.path().join("a-sentinel");
    fs::write(&socket_sentinel, b"sentinel").unwrap();
    fs::set_permissions(&socket_sentinel, Permissions::from_mode(0o600)).unwrap();
    let _listener = match UnixListener::bind(socket_root.path().join("z-socket")) {
        Ok(listener) => listener,
        // Some test sandboxes prohibit AF_UNIX creation. FIFO coverage
        // above still proves special inodes are rejected without opening
        // them; exercise the socket branch whenever the host permits it.
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => return,
        Err(error) => panic!("failed to create Unix socket fixture: {error}"),
    };
    assert!(matches!(
        normalize_and_hash(socket_root.path(), EPOCH),
        Err(Error::UnsupportedFileType { kind: "socket", .. })
    ));
    assert_mode(&socket_sentinel, 0o600);
}

#[test]
fn symlinks_are_hashed_and_timestamped_without_following_targets() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("tree");
    let outside = temporary.path().join("outside");
    fs::create_dir(&root).unwrap();
    fs::write(&outside, b"outside").unwrap();
    fs::set_permissions(&outside, Permissions::from_mode(0o600)).unwrap();
    let old = filetime::FileTime::from_unix_time(123, 0);
    filetime::set_file_times(&outside, old, old).unwrap();
    symlink("../outside", root.join("link")).unwrap();

    normalize_and_hash(&root, EPOCH).unwrap();

    assert!(
        fs::symlink_metadata(root.join("link"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_timestamp(&root.join("link"), EPOCH);
    assert_mode(&outside, 0o600);
    assert_eq!(fs::metadata(&outside).unwrap().mtime(), 123);
}

#[test]
fn entry_limit_accepts_exact_n_and_rejects_n_plus_one_before_mutation() {
    let exact = tempfile::tempdir().unwrap();
    fs::write(exact.path().join("a"), b"a").unwrap();
    fs::write(exact.path().join("b"), b"b").unwrap();
    let mut limits = MaterializationLimits::default();
    limits.max_entries = 2;
    normalize_and_hash_with_limits(exact.path(), EPOCH, limits).unwrap();

    let over = tempfile::tempdir().unwrap();
    for name in ["a", "b", "c"] {
        let path = over.path().join(name);
        fs::write(&path, name).unwrap();
        fs::set_permissions(&path, Permissions::from_mode(0o600)).unwrap();
    }
    assert_limit(
        normalize_and_hash_with_limits(over.path(), EPOCH, limits),
        "total entries",
        2,
        3,
    );
    assert_mode(&over.path().join("a"), 0o600);
}

#[test]
fn depth_name_and_path_limits_have_exact_boundaries() {
    let exact_depth = tempfile::tempdir().unwrap();
    fs::create_dir_all(exact_depth.path().join("a/b")).unwrap();
    let mut limits = MaterializationLimits::default();
    limits.max_depth = 2;
    normalize_and_hash_with_limits(exact_depth.path(), EPOCH, limits).unwrap();

    let over_depth = tempfile::tempdir().unwrap();
    fs::create_dir_all(over_depth.path().join("a/b/c")).unwrap();
    assert_limit(
        normalize_and_hash_with_limits(over_depth.path(), EPOCH, limits),
        "entry depth",
        2,
        3,
    );

    let exact_name = tempfile::tempdir().unwrap();
    fs::write(exact_name.path().join("abc"), b"").unwrap();
    limits = MaterializationLimits::default();
    limits.max_name_bytes = 3;
    normalize_and_hash_with_limits(exact_name.path(), EPOCH, limits).unwrap();

    let over_name = tempfile::tempdir().unwrap();
    fs::write(over_name.path().join("abcd"), b"").unwrap();
    assert_limit(
        normalize_and_hash_with_limits(over_name.path(), EPOCH, limits),
        "entry name bytes",
        3,
        4,
    );

    let exact_path = tempfile::tempdir().unwrap();
    fs::create_dir(exact_path.path().join("a")).unwrap();
    fs::write(exact_path.path().join("a/b"), b"").unwrap();
    limits = MaterializationLimits::default();
    limits.max_path_bytes = 3;
    normalize_and_hash_with_limits(exact_path.path(), EPOCH, limits).unwrap();

    let over_path = tempfile::tempdir().unwrap();
    fs::create_dir(over_path.path().join("a")).unwrap();
    fs::write(over_path.path().join("a/bb"), b"").unwrap();
    assert_limit(
        normalize_and_hash_with_limits(over_path.path(), EPOCH, limits),
        "relative path bytes",
        3,
        4,
    );
}

#[test]
fn file_and_regular_aggregate_limits_have_exact_boundaries() {
    let exact_file = tempfile::tempdir().unwrap();
    fs::write(exact_file.path().join("source"), b"abc").unwrap();
    let mut limits = MaterializationLimits::default();
    limits.max_file_bytes = 3;
    normalize_and_hash_with_limits(exact_file.path(), EPOCH, limits).unwrap();

    let over_file = tempfile::tempdir().unwrap();
    fs::write(over_file.path().join("source"), b"abcd").unwrap();
    assert_limit(
        normalize_and_hash_with_limits(over_file.path(), EPOCH, limits),
        "regular file bytes",
        3,
        4,
    );

    let exact_total = tempfile::tempdir().unwrap();
    fs::write(exact_total.path().join("a"), b"a").unwrap();
    fs::write(exact_total.path().join("b"), b"bc").unwrap();
    limits = MaterializationLimits::default();
    limits.max_total_regular_bytes = 3;
    normalize_and_hash_with_limits(exact_total.path(), EPOCH, limits).unwrap();

    let over_total = tempfile::tempdir().unwrap();
    fs::write(over_total.path().join("a"), b"a").unwrap();
    fs::write(over_total.path().join("b"), b"bcd").unwrap();
    assert_limit(
        normalize_and_hash_with_limits(over_total.path(), EPOCH, limits),
        "total regular file bytes",
        3,
        4,
    );
}

#[test]
fn symlink_and_all_allocation_aggregates_have_exact_boundaries() {
    let exact_target = tempfile::tempdir().unwrap();
    symlink("abc", exact_target.path().join("link")).unwrap();
    let mut limits = MaterializationLimits::default();
    limits.max_symlink_target_bytes = 3;
    normalize_and_hash_with_limits(exact_target.path(), EPOCH, limits).unwrap();

    let over_target = tempfile::tempdir().unwrap();
    symlink("abcd", over_target.path().join("link")).unwrap();
    assert_limit(
        normalize_and_hash_with_limits(over_target.path(), EPOCH, limits),
        "symlink target bytes",
        3,
        4,
    );

    let exact_names = tempfile::tempdir().unwrap();
    fs::write(exact_names.path().join("a"), b"").unwrap();
    fs::write(exact_names.path().join("bb"), b"").unwrap();
    limits = MaterializationLimits::default();
    limits.max_total_name_bytes = 3;
    normalize_and_hash_with_limits(exact_names.path(), EPOCH, limits).unwrap();

    let over_names = tempfile::tempdir().unwrap();
    fs::write(over_names.path().join("a"), b"").unwrap();
    fs::write(over_names.path().join("bbb"), b"").unwrap();
    assert_limit(
        normalize_and_hash_with_limits(over_names.path(), EPOCH, limits),
        "total entry name bytes",
        3,
        4,
    );

    let exact_paths = tempfile::tempdir().unwrap();
    fs::write(exact_paths.path().join("a"), b"").unwrap();
    fs::write(exact_paths.path().join("bb"), b"").unwrap();
    limits = MaterializationLimits::default();
    limits.max_total_path_bytes = 3;
    normalize_and_hash_with_limits(exact_paths.path(), EPOCH, limits).unwrap();

    let over_paths = tempfile::tempdir().unwrap();
    fs::write(over_paths.path().join("a"), b"").unwrap();
    fs::write(over_paths.path().join("bbb"), b"").unwrap();
    assert_limit(
        normalize_and_hash_with_limits(over_paths.path(), EPOCH, limits),
        "total relative path bytes",
        3,
        4,
    );

    let exact_links = tempfile::tempdir().unwrap();
    symlink("a", exact_links.path().join("one")).unwrap();
    symlink("bb", exact_links.path().join("two")).unwrap();
    limits = MaterializationLimits::default();
    limits.max_total_symlink_target_bytes = 3;
    normalize_and_hash_with_limits(exact_links.path(), EPOCH, limits).unwrap();

    let over_links = tempfile::tempdir().unwrap();
    symlink("a", over_links.path().join("one")).unwrap();
    symlink("bbb", over_links.path().join("two")).unwrap();
    assert_limit(
        normalize_and_hash_with_limits(over_links.path(), EPOCH, limits),
        "total symlink target bytes",
        3,
        4,
    );
}

#[test]
fn zero_deadline_and_impossible_symlink_capacity_fail_structurally() {
    let empty = tempfile::tempdir().unwrap();
    let mut limits = MaterializationLimits::default();
    limits.max_duration = Duration::ZERO;
    assert!(matches!(
        normalize_and_hash_with_limits(empty.path(), EPOCH, limits),
        Err(Error::DurationExceeded { .. })
    ));

    let linked = tempfile::tempdir().unwrap();
    symlink("a", linked.path().join("link")).unwrap();
    limits = MaterializationLimits::default();
    limits.max_symlink_target_bytes = usize::MAX;
    assert!(matches!(
        normalize_and_hash_with_limits(linked.path(), EPOCH, limits),
        Err(Error::ArithmeticOverflow {
            resource: "symlink target bytes",
            ..
        })
    ));

    limits.max_symlink_target_bytes = usize::MAX - 1;
    assert!(matches!(
        normalize_and_hash_with_limits(linked.path(), EPOCH, limits),
        Err(Error::Allocation {
            resource: "symlink target bytes",
            requested: usize::MAX,
            ..
        })
    ));
}

#[test]
fn deep_and_wide_trees_use_iterative_bounded_traversal() {
    let deep = tempfile::tempdir().unwrap();
    let mut cursor = deep.path().to_owned();
    for _ in 0..200 {
        cursor.push("d");
        fs::create_dir(&cursor).unwrap();
    }
    fs::write(cursor.join("leaf"), b"deep").unwrap();
    normalize_and_hash(deep.path(), EPOCH).unwrap();

    let wide = tempfile::tempdir().unwrap();
    for index in 0..512 {
        fs::write(wide.path().join(format!("entry-{index:04}")), b"wide").unwrap();
    }
    assert!(
        open_descriptors_beneath(wide.path()).is_empty(),
        "wide fixture unexpectedly had an open descriptor before traversal"
    );
    normalize_and_hash(wide.path(), EPOCH).unwrap();
    let leaked = open_descriptors_beneath(wide.path());
    assert!(leaked.is_empty(), "descriptor leak beneath wide fixture: {leaked:?}");
}

#[test]
fn sealed_materialization_survives_rename_and_rejects_path_replacement_or_mutation() {
    let temporary = tempfile::tempdir().unwrap();
    let source = temporary.path().join("source");
    let installed = temporary.path().join("installed");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("payload"), b"sealed").unwrap();
    let proof = normalize_and_seal_with_limits(&source, EPOCH, MaterializationLimits::default()).unwrap();
    assert_eq!(proof.digest().len(), 64);
    fs::rename(&source, &installed).unwrap();
    proof.verify_installed(&installed).unwrap();

    let source = temporary.path().join("source-two");
    let installed = temporary.path().join("installed-two");
    let held = temporary.path().join("held-two");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("payload"), b"sealed").unwrap();
    let proof = normalize_and_seal_with_limits(&source, EPOCH, MaterializationLimits::default()).unwrap();
    fs::rename(&source, &installed).unwrap();
    fs::rename(&installed, &held).unwrap();
    fs::create_dir(&installed).unwrap();
    fs::write(installed.join("payload"), b"sealed").unwrap();
    assert!(matches!(
        proof.verify_installed(&installed),
        Err(Error::EntryChanged(_))
    ));

    let source = temporary.path().join("source-three");
    let installed = temporary.path().join("installed-three");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("payload"), b"sealed").unwrap();
    let proof = normalize_and_seal_with_limits(&source, EPOCH, MaterializationLimits::default()).unwrap();
    fs::rename(&source, &installed).unwrap();
    fs::write(installed.join("payload"), b"mutate").unwrap();
    assert!(proof.verify_installed(&installed).is_err());
}

#[test]
fn sealed_post_install_verification_reuses_the_original_deadline() {
    let temporary = tempfile::tempdir().unwrap();
    let source = temporary.path().join("source");
    let installed = temporary.path().join("installed");
    fs::create_dir(&source).unwrap();
    let mut proof = normalize_and_seal_with_limits(&source, EPOCH, MaterializationLimits::default()).unwrap();
    fs::rename(&source, &installed).unwrap();
    proof.deadline.limit = Duration::ZERO;
    assert!(matches!(
        proof.verify_installed(&installed),
        Err(Error::DurationExceeded { .. })
    ));
}

#[test]
fn bounded_administration_removal_preserves_authored_neighbors_and_never_follows_links() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join(".git");
    let outside = temporary.path().join("outside");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("sentinel"), b"outside").unwrap();
    fs::create_dir_all(root.join("nested/.git/objects")).unwrap();
    fs::write(root.join("nested/.git/config"), b"admin").unwrap();
    fs::write(root.join(".git"), b"gitdir: elsewhere").unwrap();
    fs::write(root.join(".git-marker"), b"authored").unwrap();
    symlink(&outside, root.join("linked")).unwrap();
    symlink(&outside, root.join("linked-admin.git")).unwrap();
    fs::create_dir(root.join("other")).unwrap();
    symlink(&outside, root.join("other/.git")).unwrap();

    remove_git_administration_bounded(&root).unwrap();

    assert!(root.is_dir(), "an export root named .git must survive");
    assert!(!root.join(".git").exists());
    assert!(!root.join("nested/.git").exists());
    assert!(!root.join("other/.git").exists());
    assert_eq!(fs::read(root.join(".git-marker")).unwrap(), b"authored");
    assert_eq!(fs::read(outside.join("sentinel")).unwrap(), b"outside");
    assert!(
        fs::symlink_metadata(root.join("linked"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(
        fs::symlink_metadata(root.join("linked-admin.git"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn administration_removal_is_bounded_iterative_and_handles_special_git_entries() {
    let exact = tempfile::tempdir().unwrap();
    fs::create_dir(exact.path().join(".git")).unwrap();
    fs::write(exact.path().join(".git/config"), b"config").unwrap();
    let mut limits = MaterializationLimits::default();
    limits.max_entries = 2;
    remove_git_administration_with_limits(exact.path(), limits, false).unwrap();
    assert!(!exact.path().join(".git").exists());

    let over = tempfile::tempdir().unwrap();
    fs::create_dir(over.path().join(".git")).unwrap();
    fs::write(over.path().join(".git/config"), b"config").unwrap();
    fs::write(over.path().join("ordinary"), b"ordinary").unwrap();
    assert_limit(
        remove_git_administration_with_limits(over.path(), limits, false).map(|()| String::new()),
        "total entries",
        2,
        3,
    );
    assert!(over.path().join(".git").exists(), "preflight failure must not mutate");

    let deep = tempfile::tempdir().unwrap();
    let mut cursor = deep.path().join(".git");
    fs::create_dir(&cursor).unwrap();
    for _ in 0..200 {
        cursor.push("d");
        fs::create_dir(&cursor).unwrap();
    }
    fs::write(cursor.join("leaf"), b"leaf").unwrap();
    remove_git_administration_bounded(deep.path()).unwrap();
    assert!(!deep.path().join(".git").exists());

    let special = tempfile::tempdir().unwrap();
    mkfifo(&special.path().join(".git"), Mode::S_IRUSR | Mode::S_IWUSR).unwrap();
    remove_git_administration_bounded(special.path()).unwrap();
    assert!(!special.path().join(".git").exists());
}

fn create_equivalent_tree(root: &Path, raw_name: &OsString, reverse: bool, timestamp: i64) {
    fs::create_dir(root.join("nested")).unwrap();
    let files = if reverse {
        vec![
            (PathBuf::from("executable"), b"execute".as_slice()),
            (PathBuf::from(raw_name), b"raw".as_slice()),
        ]
    } else {
        vec![
            (PathBuf::from(raw_name), b"raw".as_slice()),
            (PathBuf::from("executable"), b"execute".as_slice()),
        ]
    };
    for (path, bytes) in files {
        fs::write(root.join(path), bytes).unwrap();
    }
    symlink(raw_name, root.join("link")).unwrap();

    fs::set_permissions(root, Permissions::from_mode(if reverse { 0o777 } else { 0o700 })).unwrap();
    fs::set_permissions(
        root.join("nested"),
        Permissions::from_mode(if reverse { 0o775 } else { 0o700 }),
    )
    .unwrap();
    fs::set_permissions(
        root.join(raw_name),
        Permissions::from_mode(if reverse { 0o664 } else { 0o600 }),
    )
    .unwrap();
    fs::set_permissions(
        root.join("executable"),
        Permissions::from_mode(if reverse { 0o777 } else { 0o711 }),
    )
    .unwrap();

    let old = filetime::FileTime::from_unix_time(timestamp, 0);
    for path in [
        root.to_owned(),
        root.join("nested"),
        root.join(raw_name),
        root.join("executable"),
    ] {
        filetime::set_file_times(path, old, old).unwrap();
    }
    filetime::set_symlink_file_times(root.join("link"), old, old).unwrap();
}

fn digest_with(mutate: impl FnOnce(&Path)) -> String {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("regular"), b"alpha").unwrap();
    fs::write(root.path().join("executable"), b"execute").unwrap();
    fs::set_permissions(root.path().join("executable"), Permissions::from_mode(0o755)).unwrap();
    fs::write(root.path().join("kind"), b"kind").unwrap();
    symlink("regular", root.path().join("link")).unwrap();
    mutate(root.path());
    normalize_and_hash(root.path(), EPOCH).unwrap()
}

fn normalize_with_hook(root: &Path, hook: impl FnMut(HookPoint)) -> Result<String, Error> {
    let limits = MaterializationLimits::default();
    let deadline = Deadline::new(limits.max_duration);
    let root = RootHandle::open(root)?;
    normalize_and_hash_with(&root, EPOCH, limits, &deadline, hook).map(|(digest, _)| digest)
}

fn assert_limit<T>(result: Result<T, Error>, resource: &'static str, limit: u64, actual: u64) {
    assert!(
        matches!(
            result,
            Err(Error::LimitExceeded {
                resource: found_resource,
                limit: found_limit,
                actual: found_actual,
                ..
            }) if found_resource == resource && found_limit == limit && found_actual == actual
        ),
        "expected {resource} limit {limit} with actual {actual}"
    );
}

fn open_descriptors_beneath(root: &Path) -> Vec<(OsString, PathBuf)> {
    let mut descriptors = fs::read_dir("/proc/self/fd")
        .unwrap()
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let target = fs::read_link(entry.path()).ok()?;
            target.starts_with(root).then(|| (entry.file_name(), target))
        })
        .collect::<Vec<_>>();
    descriptors.sort();
    descriptors
}

fn assert_mode(path: &Path, expected: u32) {
    assert_eq!(fs::symlink_metadata(path).unwrap().mode() & 0o7777, expected);
}

fn assert_timestamp(path: &Path, expected: i64) {
    let metadata = fs::symlink_metadata(path).unwrap();
    assert_eq!(metadata.atime(), expected);
    assert_eq!(metadata.atime_nsec(), 0);
    assert_eq!(metadata.mtime(), expected);
    assert_eq!(metadata.mtime_nsec(), 0);
}
