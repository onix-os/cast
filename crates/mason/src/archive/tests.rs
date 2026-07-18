use std::{
    fs,
    io::Cursor,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    process::Command,
};

use flate2::{Compression, write::GzEncoder};
use tar::{Builder, Header};
use xz2::{
    stream::{Check, Filters, LzmaOptions, Stream},
    write::XzEncoder,
};

use super::*;

fn limits() -> ArchiveLimits {
    ArchiveLimits {
        compressed_bytes: 1024 * 1024,
        decoded_bytes: 1024 * 1024,
        xz_decoder_memory_bytes: 16 * 1024 * 1024,
        zstd_window_log_max: 20,
        entries: 64,
        path_bytes: 64 * 1024,
        path_depth: 16,
        one_path_bytes: 4095,
        link_bytes: 4095,
        extension_bytes: 64 * 1024,
        total_extension_bytes: 64 * 1024,
        file_logical_bytes: 1024 * 1024,
        file_physical_bytes: 1024 * 1024,
        total_logical_bytes: 1024 * 1024,
        total_physical_bytes: 1024 * 1024,
        sparse_extents: 0,
        materialized_nodes: 64,
        wall_time: Duration::from_secs(30),
    }
}

fn archive(build: impl FnOnce(&mut Builder<Vec<u8>>)) -> Vec<u8> {
    let mut builder = Builder::new(Vec::new());
    build(&mut builder);
    builder.finish().unwrap();
    builder.into_inner().unwrap()
}

fn archive_extension_entry_sizes(bytes: &[u8]) -> Vec<u64> {
    let mut archive = Archive::new(Cursor::new(bytes));
    let mut entries = archive.entries().unwrap().raw(true);
    let mut sizes = Vec::new();
    while let Some(entry) = entries.next() {
        let entry = entry.unwrap();
        let kind = entry.header().entry_type();
        if kind.is_pax_local_extensions() || kind.is_gnu_longname() || kind.is_gnu_longlink() {
            sizes.push(entry.size());
        }
    }
    sizes
}

fn append(builder: &mut Builder<Vec<u8>>, path: &str, kind: EntryType, link: Option<&str>, data: &[u8]) {
    let mut header = Header::new_ustar();
    header.set_path(path).unwrap();
    header.set_entry_type(kind);
    header.set_mode(if kind.is_dir() { 0o755 } else { 0o644 });
    header.set_size(data.len() as u64);
    if let Some(link) = link {
        header.set_link_name(link).unwrap();
    }
    header.set_cksum();
    builder.append(&header, Cursor::new(data)).unwrap();
}

fn append_raw_path(builder: &mut Builder<Vec<u8>>, path: &[u8], kind: EntryType, link: Option<&[u8]>, data: &[u8]) {
    assert!(path.len() <= 100);
    let mut header = Header::new_ustar();
    header.as_mut_bytes()[..100].fill(0);
    header.as_mut_bytes()[..path.len()].copy_from_slice(path);
    header.set_entry_type(kind);
    header.set_mode(0o644);
    header.set_size(data.len() as u64);
    if let Some(link) = link {
        assert!(link.len() <= 100);
        header.as_mut_bytes()[157..257].fill(0);
        header.as_mut_bytes()[157..157 + link.len()].copy_from_slice(link);
    }
    header.set_cksum();
    builder.append(&header, Cursor::new(data)).unwrap();
}

fn scan_result(bytes: &[u8], strip_components: usize, limits: ArchiveLimits) -> Result<ScanResult, Error> {
    let mut file = tempfile::tempfile().unwrap();
    file.write_all(bytes).unwrap();
    file.seek(SeekFrom::Start(0)).unwrap();
    scan_archive(
        &mut file,
        strip_components,
        limits,
        None,
        1_700_000_000,
        ArchiveDeadline::new(limits.wall_time),
    )
}

fn scan(bytes: &[u8], strip_components: usize, limits: ArchiveLimits) -> Result<Vec<ManifestEntry>, Error> {
    scan_result(bytes, strip_components, limits).map(|result| result.manifest)
}

#[test]
fn aggregate_compressed_n_plus_one_stops_before_the_next_fetch_boundary() {
    let mut aggregate = ArchiveSessionBudget::production().limits;
    aggregate.compressed_bytes = 4;
    let mut session = ArchiveSessionBudget::new(aggregate);
    let usage = ScanUsage {
        decoded_bytes: 0,
        entries: 0,
        path_bytes: 0,
        extension_bytes: 0,
        logical_bytes: 0,
        physical_bytes: 0,
        materialized_nodes: 0,
    };
    let mut fetches = 0;
    let mut prefetch = |session: &ArchiveSessionBudget| {
        session.remaining_compressed_bytes()?;
        fetches += 1;
        Ok::<_, Error>(())
    };

    prefetch(&session).unwrap();
    session.admit(4, usage).unwrap();
    assert!(matches!(
        prefetch(&session),
        Err(Error::LimitExceeded {
            resource: "derivation compressed archive bytes",
            limit: 4,
            ..
        })
    ));
    assert_eq!(fetches, 1);
}

fn two_files() -> Vec<u8> {
    archive(|builder| {
        append(builder, "root/a", EntryType::Regular, None, b"a");
        append(builder, "root/b", EntryType::Regular, None, b"b");
    })
}

fn install(bytes: &[u8], limits: ArchiveLimits) -> Result<(tempfile::TempDir, PathBuf), Error> {
    let root = crate::private_tempdir();
    let sources = root.path().join("sources");
    let build = root.path().join("build");
    fs::create_dir(&sources).unwrap();
    fs::create_dir(&build).unwrap();
    fs::create_dir(build.join("out")).unwrap();
    fs::write(sources.join("source.tar"), bytes).unwrap();
    let digest = hex::encode(Sha256::digest(bytes));
    let mut session = ArchiveSessionBudget::production();
    extract_locked_tar_with_limits(
        &sources,
        "source.tar",
        &digest,
        &build,
        "out",
        1,
        1_700_000_000,
        limits,
        &mut session,
    )?;
    let output = build.join("out");
    Ok((root, output))
}

fn assert_limit(bytes: &[u8], mutate: impl FnOnce(&mut ArchiveLimits), resource: &'static str) {
    let mut exact = limits();
    mutate(&mut exact);
    scan(bytes, 0, exact).unwrap();
    let mut exceeded = exact;
    match resource {
        "archive entries" => exceeded.entries -= 1,
        "archive path bytes" => exceeded.path_bytes -= 1,
        "archive path depth" => exceeded.path_depth -= 1,
        "one archive path bytes" => exceeded.one_path_bytes -= 1,
        "one file logical bytes" => exceeded.file_logical_bytes -= 1,
        "one file physical bytes" => exceeded.file_physical_bytes -= 1,
        "aggregate expanded logical bytes" => exceeded.total_logical_bytes -= 1,
        "aggregate expanded physical bytes" => exceeded.total_physical_bytes -= 1,
        "archive link bytes" => exceeded.link_bytes -= 1,
        "materialized archive nodes" => exceeded.materialized_nodes -= 1,
        _ => panic!("unknown test resource"),
    }
    assert!(matches!(
        scan(bytes, 0, exceeded),
        Err(Error::LimitExceeded { resource: found, .. }) if found == resource
    ));
}

#[test]
fn exact_entry_path_and_depth_limits_accept_n_and_reject_n_plus_one() {
    let entries = two_files();
    assert_limit(&entries, |value| value.entries = 2, "archive entries");
    assert_limit(&entries, |value| value.path_bytes = 12, "archive path bytes");
    assert_limit(
        &entries,
        |value| value.materialized_nodes = 3,
        "materialized archive nodes",
    );

    let depth = archive(|builder| append(builder, "a/b/c", EntryType::Regular, None, b""));
    assert_limit(&depth, |value| value.path_depth = 3, "archive path depth");
    assert_limit(
        &depth,
        |value| value.one_path_bytes = "a/b/c".len(),
        "one archive path bytes",
    );
}

#[test]
fn exact_per_file_and_aggregate_expansion_limits_accept_n_and_reject_n_plus_one() {
    let one = archive(|builder| append(builder, "root/file", EntryType::Regular, None, b"ab"));
    assert_limit(&one, |value| value.file_logical_bytes = 2, "one file logical bytes");
    assert_limit(&one, |value| value.file_physical_bytes = 2, "one file physical bytes");

    let two = two_files();
    assert_limit(
        &two,
        |value| value.total_logical_bytes = 2,
        "aggregate expanded logical bytes",
    );
    assert_limit(
        &two,
        |value| value.total_physical_bytes = 2,
        "aggregate expanded physical bytes",
    );
}

#[test]
fn derivation_archive_budget_accepts_exact_totals_and_rejects_the_next_extraction() {
    let bytes = two_files();
    let usage = scan_result(&bytes, 1, limits()).unwrap().usage;
    let aggregate = ArchiveAggregateLimits {
        extractions: 1,
        compressed_bytes: bytes.len() as u64,
        decoded_bytes: usage.decoded_bytes,
        entries: usage.entries,
        path_bytes: usage.path_bytes,
        extension_bytes: usage.extension_bytes,
        logical_bytes: usage.logical_bytes,
        physical_bytes: usage.physical_bytes,
        materialized_nodes: usage.materialized_nodes,
        wall_time: Duration::from_secs(30),
    };
    let mut exact = ArchiveSessionBudget::new(aggregate);
    exact.admit(bytes.len() as u64, usage).unwrap();
    assert!(matches!(
        exact.admit(bytes.len() as u64, usage),
        Err(Error::LimitExceeded {
            resource: "derivation archive extractions",
            actual: 2,
            limit: 1,
        })
    ));

    let mut too_small = ArchiveSessionBudget::new(ArchiveAggregateLimits {
        decoded_bytes: usage.decoded_bytes - 1,
        ..aggregate
    });
    assert!(matches!(
        too_small.admit(bytes.len() as u64, usage),
        Err(Error::LimitExceeded {
            resource: "derivation decoded archive bytes",
            ..
        })
    ));
}

#[test]
fn exact_link_limit_accepts_n_and_rejects_n_plus_one() {
    let bytes = archive(|builder| append(builder, "root/link", EntryType::Symlink, Some("target"), b""));
    assert_limit(&bytes, |value| value.link_bytes = 6, "archive link bytes");
}

#[test]
fn sparse_limit_zero_accepts_dense_and_rejects_the_first_sparse_extent() {
    let dense = archive(|builder| append(builder, "root/file", EntryType::Regular, None, b"x"));
    scan(&dense, 1, limits()).unwrap();

    let sparse = archive(|builder| {
        builder
            .append_pax_extensions([("GNU.sparse.map", b"0,1".as_slice())])
            .unwrap();
        append(builder, "root/file", EntryType::Regular, None, b"x");
    });
    assert!(matches!(scan(&sparse, 1, limits()), Err(Error::SparseEntry { .. })));
}

#[test]
fn traversal_absolute_paths_and_escaping_links_are_rejected() {
    for path in [
        b"/absolute".as_slice(),
        b"root/../escape",
        b"root//ambiguous",
        b"root/./ambiguous",
    ] {
        let bytes = archive(|builder| append_raw_path(builder, path, EntryType::Regular, None, b"x"));
        assert!(matches!(scan(&bytes, 1, limits()), Err(Error::UnsafePath { .. })));
    }

    // The target is valid relative to the authored archive path only if
    // the stripped top-level component is still considered. It escapes
    // from the final extraction path and must therefore be rejected.
    let symlink = archive(|builder| append(builder, "top/a/link", EntryType::Symlink, Some("../../outside"), b""));
    assert!(matches!(
        scan(&symlink, 1, limits()),
        Err(Error::EscapingSymlink { .. })
    ));

    let hardlink = archive(|builder| append_raw_path(builder, b"root/link", EntryType::Link, Some(b"../outside"), b""));
    assert!(matches!(scan(&hardlink, 1, limits()), Err(Error::UnsafePath { .. })));
}

#[test]
fn duplicate_and_parent_child_type_collisions_are_rejected() {
    let duplicate = archive(|builder| {
        append(builder, "root/file", EntryType::Regular, None, b"a");
        append(builder, "root/file", EntryType::Regular, None, b"b");
    });
    assert!(matches!(
        scan(&duplicate, 1, limits()),
        Err(Error::DuplicatePath { .. })
    ));

    let collision = archive(|builder| {
        append(builder, "root/node", EntryType::Regular, None, b"a");
        append(builder, "root/node/child", EntryType::Regular, None, b"b");
    });
    assert!(matches!(
        scan(&collision, 1, limits()),
        Err(Error::PathTypeCollision { .. })
    ));
}

#[test]
fn special_unknown_and_forward_hardlink_entries_are_rejected() {
    for kind in [
        EntryType::Fifo,
        EntryType::Char,
        EntryType::Block,
        EntryType::Continuous,
        EntryType::new(b's'),
    ] {
        let bytes = archive(|builder| append(builder, "root/node", kind, None, b""));
        assert!(matches!(
            scan(&bytes, 1, limits()),
            Err(Error::UnsupportedInodeType { .. })
        ));
    }

    let forward = archive(|builder| {
        append(builder, "root/link", EntryType::Link, Some("root/file"), b"");
        append(builder, "root/file", EntryType::Regular, None, b"x");
    });
    assert!(matches!(
        scan(&forward, 1, limits()),
        Err(Error::ForwardHardlink { .. })
    ));
}

#[test]
fn decoded_stream_limit_accepts_exact_n_and_rejects_n_plus_one() {
    let bytes = two_files();
    let mut exact = limits();
    exact.decoded_bytes = bytes.len() as u64;
    scan(&bytes, 1, exact).unwrap();
    exact.decoded_bytes -= 1;
    assert!(scan(&bytes, 1, exact).is_err());
}

#[test]
fn plain_gzip_xz_and_zstd_tar_streams_share_one_parser_and_publication_boundary() {
    let plain = two_files();
    let mut gzip = GzEncoder::new(Vec::new(), Compression::default());
    gzip.write_all(&plain).unwrap();
    let gzip = gzip.finish().unwrap();
    let mut xz = XzEncoder::new(Vec::new(), 6);
    xz.write_all(&plain).unwrap();
    let xz = xz.finish().unwrap();
    // Keep this fixture inside the deliberately small test decoder
    // ceiling. `encode_all` inherits zstd's default window, which is not
    // part of this test's contract and can legitimately exceed 2^20 even
    // for a tiny input. The dedicated boundary test below proves that
    // oversized frames are rejected.
    let mut zstd = zstd::stream::Encoder::new(Vec::new(), 3).unwrap();
    zstd.window_log(limits().zstd_window_log_max).unwrap();
    zstd.include_contentsize(false).unwrap();
    zstd.write_all(&plain).unwrap();
    let zstd = zstd.finish().unwrap();

    for bytes in [&plain, &gzip, &xz, &zstd] {
        assert_eq!(scan(bytes, 1, limits()).unwrap().len(), 2);
        let (_root, output) = install(bytes, limits()).unwrap();
        assert_eq!(fs::read(output.join("a")).unwrap(), b"a");
        assert_eq!(fs::read(output.join("b")).unwrap(), b"b");
    }
}

#[test]
fn xz_decoder_memory_and_zstd_window_limits_accept_the_minimum_and_reject_less() {
    let plain = two_files();
    let mut options = LzmaOptions::new_preset(0).unwrap();
    options.dict_size(1024 * 1024);
    let mut filters = Filters::new();
    filters.lzma2(&options);
    let stream = Stream::new_stream_encoder(&filters, Check::Crc64).unwrap();
    let mut encoder = XzEncoder::new_stream(Vec::new(), stream);
    encoder.write_all(&plain).unwrap();
    let xz = encoder.finish().unwrap();

    let mut lower = 1u64;
    let mut upper = 16 * 1024 * 1024u64;
    while lower < upper {
        let midpoint = lower + (upper - lower) / 2;
        let mut candidate = limits();
        candidate.xz_decoder_memory_bytes = midpoint;
        if scan(&xz, 1, candidate).is_ok() {
            upper = midpoint;
        } else {
            lower = midpoint + 1;
        }
    }
    let mut exact = limits();
    exact.xz_decoder_memory_bytes = lower;
    scan(&xz, 1, exact).unwrap();
    exact.xz_decoder_memory_bytes -= 1;
    assert!(scan(&xz, 1, exact).is_err());

    let mut encoder = zstd::stream::Encoder::new(Vec::new(), 1).unwrap();
    encoder.window_log(20).unwrap();
    encoder.include_contentsize(false).unwrap();
    encoder.write_all(&plain).unwrap();
    let zstd = encoder.finish().unwrap();
    let minimum = (10..=30)
        .find(|window| {
            let mut candidate = limits();
            candidate.zstd_window_log_max = *window;
            scan(&zstd, 1, candidate).is_ok()
        })
        .expect("test zstd frame must fit a finite decoder window");
    let mut exact = limits();
    exact.zstd_window_log_max = minimum;
    scan(&zstd, 1, exact).unwrap();
    exact.zstd_window_log_max -= 1;
    assert!(scan(&zstd, 1, exact).is_err());
}

#[test]
fn pax_size_is_rejected_and_extension_budgets_are_aggregate() {
    let pax_size = archive(|builder| {
        builder.append_pax_extensions([("size", b"1".as_slice())]).unwrap();
        append(builder, "root/file", EntryType::Regular, None, b"x");
    });
    assert!(matches!(
        scan(&pax_size, 1, limits()),
        Err(Error::UnsupportedPaxKey { .. })
    ));

    let long_path = b"root/this-is-a-long-extension-path";
    let extensions = archive(|builder| {
        builder.append_pax_extensions([("path", long_path.as_slice())]).unwrap();
        append(builder, "placeholder", EntryType::Regular, None, b"x");
        builder
            .append_pax_extensions([("comment", b"bounded".as_slice())])
            .unwrap();
        append(builder, "root/second", EntryType::Regular, None, b"y");
    });
    let admitted = scan(&extensions, 1, limits()).unwrap();
    assert_eq!(join_components(&admitted[0].path), b"this-is-a-long-extension-path");

    let extension_sizes = archive_extension_entry_sizes(&extensions);
    let total = extension_sizes.iter().sum::<u64>();
    let largest = *extension_sizes.iter().max().unwrap();
    let mut exact = limits();
    exact.extension_bytes = largest;
    exact.total_extension_bytes = total;
    scan(&extensions, 1, exact).unwrap();
    exact.total_extension_bytes -= 1;
    assert!(matches!(
        scan(&extensions, 1, exact),
        Err(Error::LimitExceeded {
            resource: "aggregate archive extension bytes",
            ..
        })
    ));
}

#[test]
fn unsupported_compressed_container_has_no_external_fallback() {
    let mut bytes = b"BZh".to_vec();
    bytes.extend_from_slice(&[0; 1024]);
    assert!(matches!(
        scan(&bytes, 0, limits()),
        Err(Error::UnsupportedArchiveCompression)
    ));
}

#[test]
fn verified_manifest_is_published_with_safe_links_and_exact_contents() {
    let bytes = archive(|builder| {
        append(builder, "root", EntryType::Directory, None, b"");
        append(builder, "root/file", EntryType::Regular, None, b"payload");
        append(builder, "root/symlink", EntryType::Symlink, Some("file"), b"");
        append(builder, "root/hardlink", EntryType::Link, Some("root/file"), b"");
    });
    let (_root, output) = install(&bytes, limits()).unwrap();
    assert_eq!(fs::read(output.join("file")).unwrap(), b"payload");
    assert_eq!(fs::read_link(output.join("symlink")).unwrap(), Path::new("file"));
    assert_eq!(
        fs::metadata(output.join("file")).unwrap().ino(),
        fs::metadata(output.join("hardlink")).unwrap().ino()
    );
    assert!(!output.with_file_name(".cast-archive-stage").exists());
}

#[test]
fn implicit_directories_stage_root_and_symlinks_have_normalized_metadata() {
    let epoch = 1_700_000_000;
    let bytes = archive(|builder| {
        append(builder, "root/nested/file", EntryType::Regular, None, b"payload");
        append(builder, "root/nested/link", EntryType::Symlink, Some("file"), b"");
    });
    let (_root, output) = install(&bytes, limits()).unwrap();
    let output_metadata = fs::symlink_metadata(&output).unwrap();
    let nested_metadata = fs::symlink_metadata(output.join("nested")).unwrap();
    let link_metadata = fs::symlink_metadata(output.join("nested/link")).unwrap();

    assert_eq!(output_metadata.mode() & 0o777, 0o755);
    assert_eq!(nested_metadata.mode() & 0o777, 0o755);
    assert_eq!(output_metadata.mtime(), epoch);
    assert_eq!(nested_metadata.mtime(), epoch);
    assert_eq!(link_metadata.mtime(), epoch);
    assert_eq!(fs::read(output.join("nested/link")).unwrap(), b"payload");
}

#[test]
fn compressed_input_limit_accepts_exact_n_and_rejects_n_plus_one() {
    let bytes = two_files();
    let mut exact = limits();
    exact.compressed_bytes = bytes.len() as u64;
    install(&bytes, exact).unwrap();
    exact.compressed_bytes -= 1;
    assert!(matches!(
        install(&bytes, exact),
        Err(Error::LimitExceeded {
            resource: "compressed archive bytes",
            ..
        })
    ));
}

#[test]
fn zero_wall_time_is_rejected_at_the_first_checkpoint() {
    let mut expired = limits();
    expired.wall_time = Duration::ZERO;
    assert!(matches!(
        scan(&two_files(), 1, expired),
        Err(Error::WallTimeExceeded { limit }) if limit == Duration::ZERO
    ));
}

#[test]
fn archive_snapshot_is_immutable_before_either_parser_pass() {
    let bytes = two_files();
    let mut source = tempfile::tempfile().unwrap();
    source.write_all(&bytes).unwrap();
    source.seek(SeekFrom::Start(0)).unwrap();
    let digest = hex::encode(Sha256::digest(&bytes));
    let mut snapshot = sealed_archive_snapshot(
        source,
        &digest,
        bytes.len() as u64,
        ArchiveDeadline::new(Duration::from_secs(30)),
    )
    .unwrap();

    assert!(snapshot.write_all(b"mutation").is_err());
    assert!(snapshot.set_len(0).is_err());
    snapshot.seek(SeekFrom::Start(0)).unwrap();
    let mut retained = Vec::new();
    snapshot.read_to_end(&mut retained).unwrap();
    assert_eq!(retained, bytes);
}

#[test]
fn failed_stage_open_removes_the_just_created_empty_stage() {
    let bytes = two_files();
    let root = crate::private_tempdir();
    let sources = root.path().join("sources");
    let build = root.path().join("build");
    fs::create_dir(&sources).unwrap();
    fs::create_dir(&build).unwrap();
    fs::write(sources.join("source.tar"), &bytes).unwrap();
    let digest = hex::encode(Sha256::digest(&bytes));

    TEST_FAIL_STAGE_OPEN.with(|fail| fail.set(true));
    let mut session = ArchiveSessionBudget::production();
    let result = extract_locked_tar_with_limits(
        &sources,
        "source.tar",
        &digest,
        &build,
        "out",
        1,
        1_700_000_000,
        limits(),
        &mut session,
    );
    TEST_FAIL_STAGE_OPEN.with(|fail| fail.set(false));

    assert!(result.is_err());
    assert!(fs::read_dir(&build).unwrap().next().is_none());
}

#[test]
fn post_rename_durability_failure_never_purges_the_published_tree() {
    let bytes = two_files();
    let root = crate::private_tempdir();
    let sources = root.path().join("sources");
    let build = root.path().join("build");
    fs::create_dir(&sources).unwrap();
    fs::create_dir(&build).unwrap();
    fs::create_dir(build.join("out")).unwrap();
    fs::write(sources.join("source.tar"), &bytes).unwrap();
    let digest = hex::encode(Sha256::digest(&bytes));

    TEST_FAIL_PUBLISH_AFTER_RENAME.with(|fail| fail.set(true));
    let mut session = ArchiveSessionBudget::production();
    let result = extract_locked_tar_with_limits(
        &sources,
        "source.tar",
        &digest,
        &build,
        "out",
        1,
        1_700_000_000,
        limits(),
        &mut session,
    );
    TEST_FAIL_PUBLISH_AFTER_RENAME.with(|fail| fail.set(false));

    assert!(result.is_err());
    assert_eq!(fs::read(build.join("out/a")).unwrap(), b"a");
    assert_eq!(fs::read(build.join("out/b")).unwrap(), b"b");
    assert_eq!(
        fs::read_dir(&build)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>(),
        ["out"]
    );
}

#[test]
fn restrictive_umask_cannot_change_nested_destination_metadata() {
    const CHILD: &str = "CAST_ARCHIVE_UMASK_CHILD";
    const TEST: &str = "archive::tests::restrictive_umask_cannot_change_nested_destination_metadata";
    if std::env::var_os(CHILD).is_none() {
        let output = Command::new(std::env::current_exe().unwrap())
            .arg(TEST)
            .arg("--exact")
            .arg("--nocapture")
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "restrictive-umask archive child failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }

    let bytes = two_files();
    let root = crate::private_tempdir();
    let sources = root.path().join("sources");
    let build = root.path().join("build");
    fs::create_dir(&sources).unwrap();
    fs::create_dir(&build).unwrap();
    fs::write(sources.join("source.tar"), &bytes).unwrap();
    let digest = hex::encode(Sha256::digest(&bytes));
    let mut session = ArchiveSessionBudget::production();

    // Apply the hostile mask only after creating the trusted test inputs.
    // This keeps the setup readable while ensuring every destination
    // parent and extracted node is created under the restrictive mask.
    unsafe { libc::umask(0o777) };
    extract_locked_tar_with_limits(
        &sources,
        "source.tar",
        &digest,
        &build,
        "nested/parents/out",
        1,
        1_700_000_000,
        limits(),
        &mut session,
    )
    .unwrap();

    for directory in ["nested", "nested/parents", "nested/parents/out"] {
        let metadata = fs::metadata(build.join(directory)).unwrap();
        assert_eq!(metadata.mode() & 0o777, 0o755, "{directory}");
        assert_eq!(metadata.mtime(), 1_700_000_000, "{directory}");
    }
    assert_eq!(fs::read(build.join("nested/parents/out/a")).unwrap(), b"a");
}

#[test]
fn ordinary_archive_extraction_is_procfs_independent() {
    let bytes = archive(|builder| {
        append(builder, "root/nested/a", EntryType::Regular, None, b"a");
    });
    TEST_PROC_CHMOD_RECOVERIES.with(|recoveries| recoveries.set(0));

    let (_root, output) = install(&bytes, limits()).unwrap();

    assert_eq!(fs::read(output.join("nested/a")).unwrap(), b"a");
    TEST_PROC_CHMOD_RECOVERIES.with(|recoveries| {
        assert_eq!(
            recoveries.get(),
            0,
            "ordinary archive extraction must use readable-fd fchmod and remain valid in a ProcPolicy::None sandbox"
        );
    });
}

#[test]
fn stage_cleanup_budgets_accept_exact_boundaries_and_reject_n_plus_one() {
    let root = crate::private_tempdir();
    let directory = File::open(root.path()).unwrap();
    let device = directory.metadata().unwrap().dev();

    let mut entries = StageCleanupBudget::new(device);
    entries.entries = STAGE_CLEANUP_MAX_ENTRIES - 1;
    entries.entry(0).unwrap();
    assert_eq!(entries.entries, STAGE_CLEANUP_MAX_ENTRIES);
    assert!(entries.entry(0).is_err());

    let mut operations = StageCleanupBudget::new(device);
    operations.operations = STAGE_CLEANUP_MAX_OPERATIONS - 1;
    operations.operation().unwrap();
    assert_eq!(operations.operations, STAGE_CLEANUP_MAX_OPERATIONS);
    assert!(operations.operation().is_err());

    let mut names = StageCleanupBudget::new(device);
    names.name_bytes = STAGE_CLEANUP_MAX_NAME_BYTES - 1;
    names.entry(1).unwrap();
    assert_eq!(names.name_bytes, STAGE_CLEANUP_MAX_NAME_BYTES);
    assert!(names.entry(1).is_err());

    let mut depth = StageCleanupBudget::new(device);
    purge_directory_contents(&directory, &mut depth, STAGE_CLEANUP_MAX_DEPTH).unwrap();
    assert!(purge_directory_contents(&directory, &mut depth, STAGE_CLEANUP_MAX_DEPTH + 1).is_err());

    let mut deadline = StageCleanupBudget::new(device);
    deadline.deadline = Instant::now() - Duration::from_nanos(1);
    assert!(deadline.require_limits().is_err());
}

#[test]
fn partial_private_stage_is_removed_after_extraction_failure() {
    let bytes = two_files();
    let root = crate::private_tempdir();
    let sources = root.path().join("sources");
    let build = root.path().join("build");
    fs::create_dir(&sources).unwrap();
    fs::create_dir(&build).unwrap();
    fs::create_dir(build.join("out")).unwrap();
    fs::write(sources.join("source.tar"), &bytes).unwrap();
    let digest = hex::encode(Sha256::digest(&bytes));

    TEST_STAGE_WRITES_BEFORE_FAILURE.with(|remaining| remaining.set(1));
    let mut session = ArchiveSessionBudget::production();
    let result = extract_locked_tar_with_limits(
        &sources,
        "source.tar",
        &digest,
        &build,
        "out",
        1,
        1_700_000_000,
        limits(),
        &mut session,
    );
    TEST_STAGE_WRITES_BEFORE_FAILURE.with(|remaining| remaining.set(u64::MAX));

    assert!(result.is_err());
    assert!(fs::read_dir(build.join("out")).unwrap().next().is_none());
    let build_entries = fs::read_dir(&build)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    assert_eq!(build_entries, ["out"]);
}
