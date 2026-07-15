struct AssetCopyFixture {
    _temporary: tempfile::TempDir,
    installation: Installation,
    pool: AssetPool,
    source: PathBuf,
    source_path: PathBuf,
    output: PathBuf,
    output_parent: fs::File,
    digest: u128,
}

fn asset_copy_fixture(bytes: &[u8]) -> AssetCopyFixture {
    let temporary = tempfile::tempdir().unwrap();
    let installation = test_installation(temporary.path());
    let digest = xxhash_rust::xxh3::xxh3_128(bytes);
    let source_path = cache::asset_path(&installation, &format!("{digest:02x}"));
    fs::create_dir_all(source_path.parent().unwrap()).unwrap();
    fs::write(&source_path, bytes).unwrap();
    fs::set_permissions(&source_path, Permissions::from_mode(0o640)).unwrap();
    let source = source_path
        .strip_prefix(installation.assets_path("v2"))
        .unwrap()
        .to_owned();
    let pool = AssetPool::open(&installation).unwrap();
    let output_directory = temporary.path().join("output");
    fs::create_dir(&output_directory).unwrap();
    let output_parent = fs::File::open(&output_directory).unwrap();
    let output = output_directory.join("copied");
    AssetCopyFixture {
        _temporary: temporary,
        installation,
        pool,
        source,
        source_path,
        output,
        output_parent,
        digest,
    }
}

fn copy_fixture_asset(fixture: &AssetCopyFixture) -> Result<(), Error> {
    copy_asset(
        &fixture.pool,
        &fixture.source,
        fixture.digest,
        fixture.output_parent.as_raw_fd(),
        "copied",
        nix::libc::S_IFREG | 0o755,
        None,
        Some(Instant::now() + Duration::from_secs(10)),
    )
}

#[test]
fn frozen_copy_manifest_counts_output_inodes_and_enforces_exact_byte_limit() {
    let first = xxhash_rust::xxh3::xxh3_128(b"first frozen asset");
    let second = xxhash_rust::xxh3::xxh3_128(b"second frozen asset");
    let manifest = FrozenCopyManifest::from_digests_with_limit(
        [EMPTY_FILE_DIGEST, first, first, second],
        8,
        |digest| match digest {
            digest if digest == first => Ok(3),
            digest if digest == second => Ok(2),
            _ => unreachable!(),
        },
    )
    .unwrap();
    assert_eq!(
        manifest.total_bytes, 8,
        "duplicate digest must be charged per output inode"
    );
    assert_eq!(manifest.lengths.len(), 2, "empty files consume no cache-manifest entry");

    assert!(matches!(
        FrozenCopyManifest::from_digests_with_limit([first, first, second], 7, |digest| {
            if digest == first { Ok(3) } else { Ok(2) }
        }),
        Err(Error::FrozenMaterializationTotalByteLimit { limit: 7, actual: 8 })
    ));

    let mut total = 7;
    account_frozen_blit_bytes(&mut total, 1, 8).unwrap();
    assert_eq!(total, 8);
    assert!(matches!(
        account_frozen_blit_bytes(&mut total, 1, 8),
        Err(Error::FrozenMaterializationTotalByteLimit { limit: 8, actual: 9 })
    ));
    assert_eq!(total, 8, "a rejected N+1 byte must not mutate accounting");

    let mut overflow = u64::MAX;
    assert!(matches!(
        account_frozen_blit_bytes(&mut overflow, 1, u64::MAX),
        Err(Error::FrozenMaterializationTotalByteLimit {
            limit: u64::MAX,
            actual: u64::MAX
        })
    ));
    assert_eq!(overflow, u64::MAX);
}

#[test]
fn frozen_capability_retry_timeout_remains_a_materialization_timeout() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("file");
    fs::write(&path, b"deadline proof").unwrap();
    let file = fs::File::open(&path).unwrap();
    let error = open_frozen_normalization_readonly(
        file.file(),
        Path::new("/file"),
        Instant::now() - Duration::from_millis(1),
    )
    .unwrap_err();
    assert!(matches!(error, Error::FrozenMaterializationTimeout { .. }));
}

#[test]
fn independent_copy_rejects_length_changed_after_byte_preflight_before_creation() {
    let original = b"preflight length";
    let fixture = asset_copy_fixture(original);
    let manifest = FrozenCopyManifest::from_digests_with_limit([fixture.digest], original.len() as u64, |_| {
        Ok(original.len() as u64)
    })
    .unwrap();
    fs::write(&fixture.source_path, b"longer bytes after preflight").unwrap();

    let result = copy_asset(
        &fixture.pool,
        &fixture.source,
        fixture.digest,
        fixture.output_parent.as_raw_fd(),
        "copied",
        nix::libc::S_IFREG | 0o755,
        Some(&manifest),
        Some(Instant::now() + Duration::from_secs(10)),
    );
    assert!(matches!(
        result,
        Err(Error::FrozenMaterializationAssetLengthChanged { .. })
    ));
    assert!(!fixture.output.exists());
}

#[test]
fn independent_copy_rejects_replaced_asset_pool_and_removes_partial_target() {
    let fixture = asset_copy_fixture(b"authenticated cache bytes");
    let asset_pool = fixture.installation.assets_path("v2");
    let detached = fixture.installation.assets_path("v2-detached");
    fs::rename(&asset_pool, &detached).unwrap();
    fs::create_dir(&asset_pool).unwrap();

    assert!(copy_fixture_asset(&fixture).is_err());
    assert!(!fixture.output.exists());
}

#[test]
fn independent_copy_rejects_symlinked_asset_component() {
    let fixture = asset_copy_fixture(b"component traversal bytes");
    let first = fixture.source.components().next().unwrap().as_os_str();
    let component = fixture.installation.assets_path("v2").join(first);
    let detached = fixture.installation.assets_path("v2").join("detached-component");
    fs::rename(&component, &detached).unwrap();
    symlink(&detached, &component).unwrap();

    assert!(copy_fixture_asset(&fixture).is_err());
    assert!(!fixture.output.exists());
}

#[test]
fn independent_copy_rejects_fifo_without_blocking() {
    let fixture = asset_copy_fixture(b"fifo placeholder");
    fs::remove_file(&fixture.source_path).unwrap();
    nix::unistd::mkfifo(&fixture.source_path, Mode::from_bits_truncate(0o600)).unwrap();

    let started = Instant::now();
    assert!(copy_fixture_asset(&fixture).is_err());
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(!fixture.output.exists());
}

#[test]
fn independent_copy_rejects_final_symlink_and_directory() {
    let fixture = asset_copy_fixture(b"non-regular placeholder");
    fs::remove_file(&fixture.source_path).unwrap();
    symlink("missing", &fixture.source_path).unwrap();
    assert!(copy_fixture_asset(&fixture).is_err());
    assert!(!fixture.output.exists());

    fs::remove_file(&fixture.source_path).unwrap();
    fs::create_dir(&fixture.source_path).unwrap();
    assert!(copy_fixture_asset(&fixture).is_err());
    assert!(!fixture.output.exists());
}

#[test]
fn independent_copy_rejects_digest_mismatch_and_removes_target() {
    let fixture = asset_copy_fixture(b"digest-bound bytes");
    let result = copy_asset(
        &fixture.pool,
        &fixture.source,
        fixture.digest ^ 1,
        fixture.output_parent.as_raw_fd(),
        "copied",
        nix::libc::S_IFREG | 0o755,
        None,
        Some(Instant::now() + Duration::from_secs(10)),
    );

    assert!(result.is_err());
    assert!(!fixture.output.exists());
}

#[test]
fn independent_copy_rejects_source_replacement_after_open() {
    let fixture = asset_copy_fixture(b"pinned original bytes");
    let detached = fixture.source_path.with_extension("detached");
    let mut replaced = false;
    let result = copy_asset_with_checkpoint(
        &fixture.pool,
        &fixture.source,
        fixture.digest,
        fixture.output_parent.as_raw_fd(),
        "copied",
        nix::libc::S_IFREG | 0o755,
        None,
        Some(Instant::now() + Duration::from_secs(10)),
        |checkpoint| {
            if checkpoint == AssetCopyCheckpoint::SourceOpened && !replaced {
                fs::rename(&fixture.source_path, &detached).unwrap();
                fs::write(&fixture.source_path, b"hostile replacement").unwrap();
                replaced = true;
            }
        },
    );

    assert!(result.is_err());
    assert!(!fixture.output.exists(), "copy failure was {result:#?}");
}

#[test]
fn independent_copy_rejects_source_mutation_after_streaming() {
    let original = b"original stable bytes";
    let fixture = asset_copy_fixture(original);
    let mut mutated = false;
    let result = copy_asset_with_checkpoint(
        &fixture.pool,
        &fixture.source,
        fixture.digest,
        fixture.output_parent.as_raw_fd(),
        "copied",
        nix::libc::S_IFREG | 0o755,
        None,
        Some(Instant::now() + Duration::from_secs(10)),
        |checkpoint| {
            if checkpoint == AssetCopyCheckpoint::BytesCopied && !mutated {
                fs::write(&fixture.source_path, b"mutated hostile bytes").unwrap();
                mutated = true;
            }
        },
    );

    assert!(result.is_err());
    assert!(!fixture.output.exists(), "copy failure was {result:#?}");
}

#[test]
fn exact_copy_accepts_n_and_rejects_n_minus_or_plus_one() {
    let temporary = tempfile::tempdir().unwrap();
    let bytes = vec![0x5a; ASSET_COPY_BUFFER_BYTES * 2];
    let digest = xxhash_rust::xxh3::xxh3_128(&bytes);
    let source_path = temporary.path().join("source");
    fs::write(&source_path, &bytes).unwrap();

    let source = fs::File::open(&source_path).unwrap();
    let target = fs::File::create(temporary.path().join("exact")).unwrap();
    copy_fd_exact(
        source.as_raw_fd(),
        target.as_raw_fd(),
        bytes.len() as u64,
        digest,
        Some(Instant::now() + Duration::from_secs(10)),
    )
    .unwrap();
    drop(target);
    assert_eq!(fs::read(temporary.path().join("exact")).unwrap(), bytes);

    let source = fs::File::open(&source_path).unwrap();
    let target = fs::File::create(temporary.path().join("short-bound")).unwrap();
    assert!(
        copy_fd_exact(
            source.as_raw_fd(),
            target.as_raw_fd(),
            bytes.len() as u64 - 1,
            digest,
            Some(Instant::now() + Duration::from_secs(10)),
        )
        .is_err()
    );

    let source = fs::File::open(&source_path).unwrap();
    let target = fs::File::create(temporary.path().join("long-bound")).unwrap();
    assert!(
        copy_fd_exact(
            source.as_raw_fd(),
            target.as_raw_fd(),
            bytes.len() as u64 + 1,
            digest,
            Some(Instant::now() + Duration::from_secs(10)),
        )
        .is_err()
    );
}

#[test]
fn independent_copy_never_unlinks_preexisting_hardlink_target() {
    let fixture = asset_copy_fixture(b"exclusive destination bytes");
    let sentinel = fixture.output.with_extension("sentinel");
    fs::write(&sentinel, b"sentinel").unwrap();
    fs::hard_link(&sentinel, &fixture.output).unwrap();
    let before = fs::metadata(&sentinel).unwrap();

    assert!(copy_fixture_asset(&fixture).is_err());
    assert_eq!(fs::read(&fixture.output).unwrap(), b"sentinel");
    let after = fs::metadata(&fixture.output).unwrap();
    assert_eq!((after.dev(), after.ino()), (before.dev(), before.ino()));
    assert_eq!(after.nlink(), 2);
}
