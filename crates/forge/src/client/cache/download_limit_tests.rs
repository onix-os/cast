use std::{
    io::{Cursor, Write as _},
    os::unix::fs::{PermissionsExt as _, symlink},
    sync::{Arc, Barrier},
    time::{Duration, Instant},
};

use stone::{StoneHeaderV1FileType, StoneWriter};

use super::*;

fn digest(bytes: &[u8]) -> u128 {
    let mut hasher = StoneDigestWriterHasher::new();
    {
        let mut writer = StoneDigestWriter::new(io::sink(), &mut hasher);
        writer.write_all(bytes).unwrap();
    }
    hasher.digest128()
}

fn private_tempdir() -> tempfile::TempDir {
    let temporary = tempfile::tempdir().unwrap();
    std::fs::set_permissions(
        temporary.path(),
        std::fs::Permissions::from_mode(PRIVATE_DIRECTORY_MODE),
    )
    .unwrap();
    temporary
}

fn sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn stage_asset(directory: &Directory, bytes: &[u8]) -> (NamedStageFile, FileFingerprint, u128) {
    let expected = digest(bytes);
    let mut stage = NamedStageFile::create(directory, ".asset-stage-").unwrap();
    stage.file.write_all(bytes).unwrap();
    stage.file.flush().unwrap();
    stage.file.sync_all().unwrap();
    let fingerprint = authenticate_asset_file(&mut stage.file, expected, bytes.len() as u64).unwrap();
    (stage, fingerprint, expected)
}

fn stone_with_content(bytes: &[u8]) -> Vec<u8> {
    let mut archive = Vec::new();
    let mut writer = StoneWriter::new(&mut archive, StoneHeaderV1FileType::Binary)
        .unwrap()
        .with_content(Cursor::new(Vec::new()), Some(bytes.len() as u64), 1)
        .unwrap();
    writer.add_content(&mut Cursor::new(bytes)).unwrap();
    writer.finalize().unwrap();
    archive
}

#[test]
fn declared_package_size_can_only_tighten_the_global_ceiling() {
    assert_eq!(package_download_limits(Some(42)).max_bytes, 42);
    assert_eq!(package_download_limits(None), request::DEFAULT_DOWNLOAD_LIMITS);
    assert_eq!(
        package_download_limits(Some(request::DEFAULT_DOWNLOAD_LIMITS.max_bytes + 1)).max_bytes,
        request::DEFAULT_DOWNLOAD_LIMITS.max_bytes
    );
}

#[tokio::test]
async fn cached_package_symlink_is_rejected_without_reading_target() {
    let directory = private_tempdir();
    let outside = directory.path().join("outside");
    let cached = directory.path().join("cached");
    std::fs::write(&outside, b"outside bytes").unwrap();
    symlink(&outside, &cached).unwrap();
    let directory = Directory::open_absolute(directory.path()).unwrap();

    assert!(
        authenticate_sha256_entry_async(
            &directory,
            OsStr::new("cached"),
            "irrelevant",
            None,
            request::DEFAULT_DOWNLOAD_LIMITS.max_bytes,
            None,
        )
        .await
        .is_err()
    );
    assert_eq!(std::fs::read(outside).unwrap(), b"outside bytes");
}

#[tokio::test]
async fn cached_package_fifo_is_rejected_without_blocking() {
    use nix::{sys::stat::Mode, unistd::mkfifo};

    let directory = private_tempdir();
    let cached = directory.path().join("cached");
    mkfifo(&cached, Mode::S_IRUSR | Mode::S_IWUSR).unwrap();
    let started = Instant::now();
    let directory = Directory::open_absolute(directory.path()).unwrap();

    assert!(
        authenticate_sha256_entry_async(
            &directory,
            OsStr::new("cached"),
            "irrelevant",
            None,
            request::DEFAULT_DOWNLOAD_LIMITS.max_bytes,
            None,
        )
        .await
        .is_err()
    );
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[tokio::test]
async fn cached_package_requires_the_exact_declared_size_at_n_and_n_plus_one() {
    let temporary = private_tempdir();
    for (name, bytes) in [("exact", b"abc".as_slice()), ("short", b"ab"), ("long", b"abcd")] {
        let path = temporary.path().join(name);
        std::fs::write(&path, bytes).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(PRIVATE_FILE_MODE)).unwrap();
    }
    let directory = Directory::open_absolute(temporary.path()).unwrap();
    let expected_hash = sha256(b"abc");

    authenticate_sha256_entry_async(
        &directory,
        OsStr::new("exact"),
        &expected_hash,
        Some(3),
        3,
        Some(PRIVATE_FILE_MODE),
    )
    .await
    .unwrap();
    for name in ["short", "long"] {
        assert!(
            authenticate_sha256_entry_async(
                &directory,
                OsStr::new(name),
                &expected_hash,
                Some(3),
                3,
                Some(PRIVATE_FILE_MODE),
            )
            .await
            .is_err()
        );
    }
}

#[test]
fn retained_download_descriptor_defeats_path_substitution_before_unpack() {
    let temporary = private_tempdir();
    let installation_root = temporary.path().join("root");
    std::fs::create_dir(&installation_root).unwrap();
    crate::test_support::prepare_private_installation_root(&installation_root);
    let installation = Installation::open(&installation_root, None).unwrap();
    std::fs::set_permissions(
        installation.cache_path(""),
        std::fs::Permissions::from_mode(PRIVATE_DIRECTORY_MODE),
    )
    .unwrap();
    std::fs::set_permissions(
        installation.assets_path(""),
        std::fs::Permissions::from_mode(PRIVATE_DIRECTORY_MODE),
    )
    .unwrap();
    let original_bytes = b"descriptor-authenticated package content";
    let archive = stone_with_content(original_bytes);
    let expected_sha256 = sha256(&archive);
    let download_path = installation.cache_path("retained-download.stone");
    std::fs::write(&download_path, &archive).unwrap();
    std::fs::set_permissions(&download_path, std::fs::Permissions::from_mode(PRIVATE_FILE_MODE)).unwrap();
    let mut descriptor = std::fs::OpenOptions::new().read(true).open(&download_path).unwrap();
    authenticate_sha256_file_sync(
        &mut descriptor,
        &expected_sha256,
        Some(archive.len() as u64),
        archive.len() as u64,
        Some(PRIVATE_FILE_MODE),
    )
    .unwrap();

    std::fs::rename(&download_path, installation.cache_path("detached-download.stone")).unwrap();
    std::fs::write(&download_path, b"hostile pathname replacement").unwrap();
    std::fs::set_permissions(&download_path, std::fs::Permissions::from_mode(PRIVATE_FILE_MODE)).unwrap();

    Download {
        path: download_path.clone(),
        file: descriptor,
        expected_sha256,
        expected_size: Some(archive.len() as u64),
        max_size: archive.len() as u64,
        installation: installation.clone(),
        was_cached: true,
    }
    .unpack(UnpackingInProgress::default(), |_| {})
    .unwrap();

    let asset = asset_path(&installation, &format!("{:02x}", digest(original_bytes)));
    assert_eq!(std::fs::read(asset).unwrap(), original_bytes);
    assert_eq!(std::fs::read(download_path).unwrap(), b"hostile pathname replacement");
}

#[test]
fn asset_publication_replaces_fifo_and_symlink_without_blocking_or_touching_target() {
    use nix::{sys::stat::Mode, unistd::mkfifo};

    let temporary = private_tempdir();
    let directory = Directory::open_absolute(temporary.path()).unwrap();
    let bytes = b"authenticated asset";
    let name = format!("{:02x}", digest(bytes));
    let final_path = temporary.path().join(&name);
    mkfifo(&final_path, Mode::S_IRUSR | Mode::S_IWUSR).unwrap();
    let (mut stage, fingerprint, expected) = stage_asset(&directory, bytes);
    let started = Instant::now();
    publish_asset_entry(
        &mut stage,
        &directory,
        OsStr::new(&name),
        fingerprint,
        expected,
        bytes.len() as u64,
    )
    .unwrap();
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(std::fs::read(&final_path).unwrap(), bytes);

    std::fs::remove_file(&final_path).unwrap();
    let outside = temporary.path().join("outside");
    std::fs::write(&outside, b"outside stays unchanged").unwrap();
    symlink(&outside, &final_path).unwrap();
    let (mut stage, fingerprint, expected) = stage_asset(&directory, bytes);
    publish_asset_entry(
        &mut stage,
        &directory,
        OsStr::new(&name),
        fingerprint,
        expected,
        bytes.len() as u64,
    )
    .unwrap();
    assert_eq!(std::fs::read(&final_path).unwrap(), bytes);
    assert_eq!(std::fs::read(outside).unwrap(), b"outside stays unchanged");
}

#[test]
fn asset_authentication_rejects_truncated_and_n_plus_one_entries() {
    let temporary = private_tempdir();
    let expected_bytes = b"abcd";
    let expected_digest = digest(expected_bytes);
    for (name, bytes) in [
        ("exact", expected_bytes.as_slice()),
        ("short", b"abc".as_slice()),
        ("long", b"abcde".as_slice()),
    ] {
        let path = temporary.path().join(name);
        std::fs::write(&path, bytes).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(PRIVATE_FILE_MODE)).unwrap();
    }
    let mut exact = std::fs::OpenOptions::new()
        .read(true)
        .open(temporary.path().join("exact"))
        .unwrap();
    authenticate_asset_file(&mut exact, expected_digest, 4).unwrap();
    for name in ["short", "long"] {
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .open(temporary.path().join(name))
            .unwrap();
        assert!(authenticate_asset_file(&mut file, expected_digest, 4).is_err());
    }
}

#[test]
fn competing_asset_publishers_reuse_one_verified_winner() {
    let temporary = private_tempdir();
    let directory_path = temporary.path().to_owned();
    let bytes = b"same content from competing publishers".to_vec();
    let name = format!("{:02x}", digest(&bytes));
    let barrier = Arc::new(Barrier::new(2));
    let mut workers = Vec::new();
    for _ in 0..2 {
        let directory_path = directory_path.clone();
        let bytes = bytes.clone();
        let name = name.clone();
        let barrier = Arc::clone(&barrier);
        workers.push(std::thread::spawn(move || {
            let directory = Directory::open_absolute(&directory_path).unwrap();
            let (mut stage, fingerprint, expected) = stage_asset(&directory, &bytes);
            barrier.wait();
            let mut winner = publish_asset_entry(
                &mut stage,
                &directory,
                OsStr::new(&name),
                fingerprint,
                expected,
                bytes.len() as u64,
            )
            .unwrap();
            let mut published = Vec::new();
            use std::io::Read as _;
            winner.read_to_end(&mut published).unwrap();
            published
        }));
    }
    for worker in workers {
        assert_eq!(worker.join().unwrap(), bytes);
    }
    let entries = std::fs::read_dir(&directory_path)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    assert_eq!(entries, [OsString::from(&name)]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn competing_download_publishers_reuse_one_verified_winner() {
    let temporary = private_tempdir();
    let destination = Directory::open_absolute(temporary.path()).unwrap();
    let bytes = b"one authenticated package download";
    let expected_hash = sha256(bytes);
    let first_stage = PrivateStageDirectory::create(&destination, ".download-stage-").unwrap();
    let second_stage = PrivateStageDirectory::create(&destination, ".download-stage-").unwrap();
    let staged_name = OsStr::new("package.stone");
    for stage in [&first_stage, &second_stage] {
        std::fs::write(stage.path.join(staged_name), bytes).unwrap();
        std::fs::set_permissions(
            stage.path.join(staged_name),
            std::fs::Permissions::from_mode(PRIVATE_FILE_MODE),
        )
        .unwrap();
        stage.require_inventory(&[staged_name]).unwrap();
    }
    let first_file = authenticate_sha256_entry_async(
        &first_stage.directory,
        staged_name,
        &expected_hash,
        Some(bytes.len() as u64),
        bytes.len() as u64,
        Some(PRIVATE_FILE_MODE),
    )
    .await
    .unwrap();
    let first_fingerprint = FileFingerprint::from_metadata(&first_file.metadata().unwrap());
    let second_file = authenticate_sha256_entry_async(
        &second_stage.directory,
        staged_name,
        &expected_hash,
        Some(bytes.len() as u64),
        bytes.len() as u64,
        Some(PRIVATE_FILE_MODE),
    )
    .await
    .unwrap();
    let second_fingerprint = FileFingerprint::from_metadata(&second_file.metadata().unwrap());

    let first = publish_download_entry_async(
        &first_stage.directory,
        staged_name,
        &destination,
        OsStr::new("winner.stone"),
        first_file,
        first_fingerprint,
        &expected_hash,
        Some(bytes.len() as u64),
        bytes.len() as u64,
    );
    let second = publish_download_entry_async(
        &second_stage.directory,
        staged_name,
        &destination,
        OsStr::new("winner.stone"),
        second_file,
        second_fingerprint,
        &expected_hash,
        Some(bytes.len() as u64),
        bytes.len() as u64,
    );
    let (first, second) = tokio::join!(first, second);
    for mut winner in [first.unwrap(), second.unwrap()] {
        use std::io::{Read as _, Seek as _};
        let mut actual = Vec::new();
        winner.seek(io::SeekFrom::Start(0)).unwrap();
        winner.read_to_end(&mut actual).unwrap();
        assert_eq!(actual, bytes);
    }
    assert_eq!(std::fs::read(temporary.path().join("winner.stone")).unwrap(), bytes);
}

#[test]
fn armed_publication_cleanup_removes_only_the_exact_moved_inode() {
    let temporary = private_tempdir();
    let directory = Directory::open_absolute(temporary.path()).unwrap();
    let path = temporary.path().join("published");
    std::fs::write(&path, b"first").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(PRIVATE_FILE_MODE)).unwrap();
    let fingerprint = FileFingerprint::from_metadata(&std::fs::metadata(&path).unwrap());
    let mut guard = PublishedEntryGuard::new(&directory, OsStr::new("published"), fingerprint).unwrap();
    guard.arm();
    drop(guard);
    assert!(!path.exists());

    std::fs::write(&path, b"old").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(PRIVATE_FILE_MODE)).unwrap();
    let fingerprint = FileFingerprint::from_metadata(&std::fs::metadata(&path).unwrap());
    let mut guard = PublishedEntryGuard::new(&directory, OsStr::new("published"), fingerprint).unwrap();
    guard.arm();
    std::fs::rename(&path, temporary.path().join("detached")).unwrap();
    std::fs::write(&path, b"replacement").unwrap();
    drop(guard);
    assert_eq!(std::fs::read(path).unwrap(), b"replacement");
}

#[test]
fn random_stages_clean_failure_without_truncating_legacy_part_file() {
    let temporary = private_tempdir();
    let directory = Directory::open_absolute(temporary.path()).unwrap();
    let legacy = temporary.path().join("asset.part");
    std::fs::write(&legacy, b"stale sentinel").unwrap();
    let stage = NamedStageFile::create(&directory, ".asset-stage-").unwrap();
    let staged_path = temporary.path().join(&stage.name);
    assert!(staged_path.exists());
    drop(stage);
    assert!(!staged_path.exists());
    assert_eq!(std::fs::read(legacy).unwrap(), b"stale sentinel");

    let anonymous = AnonymousFile::create(&directory, ".content-stage-").unwrap();
    let metadata = anonymous.file.metadata().unwrap();
    assert_eq!(metadata.nlink(), 0);
    assert_eq!(metadata.mode() & 0o7777, PRIVATE_FILE_MODE);

    let download_stage = PrivateStageDirectory::create(&directory, ".download-stage-").unwrap();
    let download_stage_path = download_stage.path.clone();
    std::fs::write(download_stage.path.join(".cast-download-stale"), b"stale").unwrap();
    drop(download_stage);
    assert!(!download_stage_path.exists());
}
