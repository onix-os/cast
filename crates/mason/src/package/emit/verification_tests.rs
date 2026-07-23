use std::os::unix::fs::PermissionsExt;

use super::*;

fn test_sink(root: &Path, names: &[(&str, u64)]) -> ArtifactSink {
    ArtifactSink::new(
        root,
        names
            .iter()
            .map(|(name, max_bytes)| ArtifactSpec {
                name: (*name).to_owned(),
                max_bytes: *max_bytes,
            })
            .collect(),
    )
    .unwrap()
}

fn direct_names(root: &Path) -> Vec<String> {
    let mut names = std::fs::read_dir(root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect::<Vec<_>>();
    names.sort();
    names
}

#[test]
fn artifact_sink_publishes_only_the_exact_read_only_set() {
    let root = tempfile::tempdir().unwrap();
    let mut sink = test_sink(root.path(), &[("a.stone", 32), ("manifest.bin", 32)]);
    sink.writer("a.stone").unwrap().write_all(b"stone").unwrap();
    sink.writer("manifest.bin").unwrap().write_all(b"manifest").unwrap();

    sink.commit().unwrap();

    assert_eq!(direct_names(root.path()), ["a.stone", "manifest.bin"]);
    assert_eq!(std::fs::read(root.path().join("a.stone")).unwrap(), b"stone");
    assert_eq!(std::fs::read(root.path().join("manifest.bin")).unwrap(), b"manifest");
    for name in ["a.stone", "manifest.bin"] {
        let metadata = std::fs::symlink_metadata(root.path().join(name)).unwrap();
        assert!(metadata.file_type().is_file());
        assert_eq!(metadata.nlink(), 1);
        assert_eq!(metadata.permissions().mode() & 0o7777, 0o444);
    }
}

#[test]
fn real_contentful_stone_emission_survives_transactional_staging() {
    let input = tempfile::tempdir().unwrap();
    let largest = input.path().join("usr/bin/largest");
    let equal_a = input.path().join("usr/bin/equal-a");
    let equal_b = input.path().join("usr/bin/equal-b");
    std::fs::create_dir_all(largest.parent().unwrap()).unwrap();
    std::fs::write(&largest, b"the largest contentful stone payload").unwrap();
    std::fs::write(&equal_a, b"aaaa").unwrap();
    std::fs::write(&equal_b, b"bbbb").unwrap();
    let mut collector = collect::Collector::new(input.path());
    collector
        .add_rule("*", "out", stone_recipe::derivation::PathRuleKind::Any)
        .unwrap();
    let mut hasher = stone::StoneDigestWriterHasher::new();
    // Deliberately feed both layout targets and content sizes in a
    // non-canonical order. The emitter, not traversal accident, owns the
    // Stone wire order.
    let infos = [&equal_b, &largest, &equal_a]
        .into_iter()
        .map(|source| collector.path(source, &mut hasher).unwrap())
        .collect::<Vec<_>>();
    let mut bucket = analysis::Bucket::default();
    bucket.paths.extend(infos);
    let plan = test_derivation_plan();
    let definition = ResolvedOutput::default();
    let package = Package::new_with_architecture(
        "example",
        &plan.package,
        &definition,
        bucket,
        NonZeroU64::new(1).unwrap(),
        Architecture::X86_64,
        1,
    );
    let filename = package.filename();
    let output = tempfile::tempdir().unwrap();
    let mut sink = test_sink(output.path(), &[(filename.as_str(), MAX_STONE_ARTIFACT_BYTES)]);

    emit_package(
        &mut sink,
        &package,
        &plan.provenance.recipe.sha256,
        &plan.derivation_id(),
    )
    .unwrap();
    sink.commit().unwrap();

    let mut stone = File::open(output.path().join(filename)).unwrap();
    let payloads = forge::util::stone_payloads(&mut stone).unwrap();
    assert!(payloads.iter().any(|payload| payload.meta().is_some()));
    let layouts = payloads.iter().find_map(|payload| payload.layout()).unwrap();
    assert_eq!(
        layouts
            .body
            .iter()
            .map(|record| record.file.target())
            .collect::<Vec<_>>(),
        ["bin/equal-a", "bin/equal-b", "bin/largest"]
    );
    let indices = payloads.iter().find_map(|payload| payload.index()).unwrap();
    assert!(indices.body.windows(2).all(|pair| {
        let left_size = pair[0].end - pair[0].start;
        let right_size = pair[1].end - pair[1].start;
        left_size > right_size || (left_size == right_size && pair[0].digest < pair[1].digest)
    }));
    assert_eq!(indices.body[0].end - indices.body[0].start, 36);
    assert_eq!(indices.body[1].end - indices.body[1].start, 4);
    assert_eq!(indices.body[2].end - indices.body[2].start, 4);
    assert!(payloads.iter().any(|payload| payload.content().is_some()));
}

#[test]
fn bounded_artifact_failure_removes_every_owned_name() {
    let root = tempfile::tempdir().unwrap();
    let mut sink = test_sink(root.path(), &[("bounded.stone", 4)]);

    let error = sink.writer("bounded.stone").unwrap().write_all(b"12345").unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::FileTooLarge);
    sink.abort().unwrap();

    assert!(direct_names(root.path()).is_empty());
}

#[test]
fn bounded_artifact_seek_accepts_exact_limit_and_rejects_limit_plus_one() {
    let root = tempfile::tempdir().unwrap();
    let mut sink = test_sink(root.path(), &[("bounded.stone", 4)]);
    let writer = sink.writer("bounded.stone").unwrap();

    writer.write_all(b"1234").unwrap();
    assert_eq!(writer.seek(SeekFrom::Start(4)).unwrap(), 4);
    assert_eq!(writer.file.metadata().unwrap().len(), 4);
    assert_eq!(
        writer.seek(SeekFrom::Start(5)).unwrap_err().kind(),
        io::ErrorKind::FileTooLarge
    );
    assert_eq!(
        writer.seek(SeekFrom::Current(1)).unwrap_err().kind(),
        io::ErrorKind::FileTooLarge
    );
    assert_eq!(
        writer.seek(SeekFrom::End(1)).unwrap_err().kind(),
        io::ErrorKind::FileTooLarge
    );
    assert_eq!(writer.write(b"5").unwrap_err().kind(), io::ErrorKind::FileTooLarge);
    assert_eq!(writer.file.metadata().unwrap().len(), 4);

    sink.abort().unwrap();
    assert!(direct_names(root.path()).is_empty());
}

#[test]
fn publication_collision_after_one_rename_rolls_back_owned_final() {
    let root = tempfile::tempdir().unwrap();
    let root_path = root.path().to_owned();
    let mut sink = test_sink(root.path(), &[("a.stone", 32), ("b.stone", 32)]);
    sink.writer("a.stone").unwrap().write_all(b"owned-a").unwrap();
    sink.writer("b.stone").unwrap().write_all(b"owned-b").unwrap();

    let result = sink.commit_with_hook(|index, _| {
        if index == 0 {
            std::fs::write(root_path.join("b.stone"), b"foreign-blocker").unwrap();
        }
    });

    assert!(result.is_err());
    assert_eq!(direct_names(root.path()), ["b.stone"]);
    assert_eq!(std::fs::read(root.path().join("b.stone")).unwrap(), b"foreign-blocker");
}

#[test]
fn staged_same_size_mutation_immediately_before_rename_is_rejected() {
    let root = tempfile::tempdir().unwrap();
    let mut sink = test_sink(root.path(), &[("a.stone", 32)]);
    sink.writer("a.stone").unwrap().write_all(b"owned-bytes").unwrap();

    let result = sink.commit_with_hooks(
        |_, path| {
            let before = std::fs::metadata(path).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644)).unwrap();
            let mut file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
            file.write_all(b"other-bytes").unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o444)).unwrap();
            file.set_times(
                std::fs::FileTimes::new()
                    .set_accessed(before.accessed().unwrap())
                    .set_modified(before.modified().unwrap()),
            )
            .unwrap();
        },
        |_, _| {},
    );

    assert!(matches!(result, Err(ArtifactError::DigestChanged { .. })));
    assert!(direct_names(root.path()).is_empty());
}

#[test]
fn final_inode_swap_is_detected_without_deleting_the_replacement() {
    let root = tempfile::tempdir().unwrap();
    let mut sink = test_sink(root.path(), &[("a.stone", 32), ("b.stone", 32)]);
    sink.writer("a.stone").unwrap().write_all(b"owned-a").unwrap();
    sink.writer("b.stone").unwrap().write_all(b"owned-b").unwrap();

    let result = sink.commit_with_hook(|index, path| {
        if index == 0 {
            std::fs::remove_file(path).unwrap();
            std::fs::write(path, b"foreign-replacement").unwrap();
        }
    });

    assert!(matches!(result, Err(ArtifactError::Rollback { .. })));
    assert_eq!(direct_names(root.path()), ["a.stone"]);
    assert_eq!(
        std::fs::read(root.path().join("a.stone")).unwrap(),
        b"foreign-replacement"
    );
}

#[test]
fn same_inode_truncation_after_publication_is_detected_and_removed() {
    let root = tempfile::tempdir().unwrap();
    let mut sink = test_sink(root.path(), &[("a.stone", 32)]);
    sink.writer("a.stone").unwrap().write_all(b"owned-bytes").unwrap();

    let result = sink.commit_with_hook(|_, path| {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644)).unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(path)
            .unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o444)).unwrap();
    });

    assert!(matches!(result, Err(ArtifactError::ArtifactChanged { .. })));
    assert!(direct_names(root.path()).is_empty());
}

#[test]
fn same_inode_same_size_overwrite_after_publication_is_detected_and_removed() {
    let root = tempfile::tempdir().unwrap();
    let mut sink = test_sink(root.path(), &[("a.stone", 32)]);
    sink.writer("a.stone").unwrap().write_all(b"owned-bytes").unwrap();

    let result = sink.commit_with_hook(|_, path| {
        let metadata_before = std::fs::symlink_metadata(path).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644)).unwrap();
        std::fs::write(path, b"other-bytes").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o444)).unwrap();
        let metadata_after = std::fs::symlink_metadata(path).unwrap();
        assert_eq!(metadata_before.ino(), metadata_after.ino());
        assert_eq!(metadata_before.len(), metadata_after.len());
    });

    assert!(matches!(result, Err(ArtifactError::ArtifactChanged { .. })));
    assert!(direct_names(root.path()).is_empty());
}

#[test]
fn replaced_public_root_is_rejected_and_only_the_pinned_root_is_cleaned() {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("artifacts");
    let moved = parent.path().join("moved-artifacts");
    std::fs::create_dir(&root).unwrap();
    let mut sink = test_sink(&root, &[("a.stone", 32)]);
    sink.writer("a.stone").unwrap().write_all(b"owned").unwrap();

    std::fs::rename(&root, &moved).unwrap();
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("sentinel"), b"do not delete").unwrap();

    assert!(sink.commit().is_err());
    assert!(direct_names(&moved).is_empty());
    assert_eq!(direct_names(&root), ["sentinel"]);
    assert_eq!(std::fs::read(root.join("sentinel")).unwrap(), b"do not delete");
}

#[test]
fn preexisting_artifact_root_entries_are_never_reused_or_removed() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("a.stone"), b"preexisting").unwrap();

    assert!(ArtifactSink::new(root.path(), vec![ArtifactSpec::stone("a.stone".to_owned())]).is_err());
    assert_eq!(direct_names(root.path()), ["a.stone"]);
    assert_eq!(std::fs::read(root.path().join("a.stone")).unwrap(), b"preexisting");
}

#[test]
fn emitter_rejects_a_path_replaced_after_collection() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("file");
    std::fs::write(&path, b"payload").unwrap();
    let mut collector = collect::Collector::new(root.path());
    collector
        .add_rule("*", "out", stone_recipe::derivation::PathRuleKind::Any)
        .unwrap();
    let info = collector
        .path(&path, &mut stone::StoneDigestWriterHasher::new())
        .unwrap();
    std::fs::rename(&path, root.path().join("old")).unwrap();
    std::fs::write(&path, b"payload").unwrap();

    assert!(matches!(verify_paths(&[info]), Err(Error::VerifiedInput { .. })));
}

#[test]
fn duplicate_normalized_layout_targets_are_rejected_before_emission() {
    let root = tempfile::tempdir().unwrap();
    let usr = root.path().join("usr/bin/tool");
    let root_bin = root.path().join("bin/tool");
    std::fs::create_dir_all(usr.parent().unwrap()).unwrap();
    std::fs::create_dir_all(root_bin.parent().unwrap()).unwrap();
    std::fs::write(&usr, b"usr").unwrap();
    std::fs::write(&root_bin, b"root").unwrap();
    let mut collector = collect::Collector::new(root.path());
    collector
        .add_rule("*", "out", stone_recipe::derivation::PathRuleKind::Any)
        .unwrap();
    let mut hasher = stone::StoneDigestWriterHasher::new();
    let usr = collector.path(&usr, &mut hasher).unwrap();
    let root_bin = collector.path(&root_bin, &mut hasher).unwrap();
    let plan = test_derivation_plan();
    let definition = ResolvedOutput::default();
    let package = |name, path| {
        let mut bucket = analysis::Bucket::default();
        bucket.paths.push(path);
        Package::new_with_architecture(
            name,
            &plan.package,
            &definition,
            bucket,
            NonZeroU64::new(1).unwrap(),
            Architecture::X86_64,
            1,
        )
    };
    let packages = [package("first", usr), package("second", root_bin)];
    assert!(matches!(
        verify_unique_layout_targets(&packages),
        Err(Error::DuplicateLayoutTarget { .. })
    ));
}

#[test]
fn reserved_system_metadata_target_is_rejected_before_artifact_sink_creation() {
    let input = tempfile::tempdir().unwrap();
    let reserved_path = input.path().join("usr/.cast-tree-id/forged-child");
    std::fs::create_dir_all(reserved_path.parent().unwrap()).unwrap();
    std::fs::write(&reserved_path, b"forged marker").unwrap();

    let mut collector = collect::Collector::new(input.path());
    collector
        .add_rule("*", "out", stone_recipe::derivation::PathRuleKind::Any)
        .unwrap();
    let reserved = collector
        .path(&reserved_path, &mut stone::StoneDigestWriterHasher::new())
        .unwrap();
    let sealed = collector.seal().unwrap();

    let plan = test_derivation_plan();
    let definition = ResolvedOutput::default();
    let mut bucket = analysis::Bucket::default();
    bucket.paths.push(reserved);
    let package = Package::new_with_architecture(
        "reserved-owner",
        &plan.package,
        &definition,
        bucket,
        NonZeroU64::new(1).unwrap(),
        Architecture::X86_64,
        1,
    );

    let artifact_root = tempfile::tempdir().unwrap();

    assert!(matches!(
        emit_frozen(
            artifact_root.path(),
            &plan.package,
            &plan.provenance.recipe.sha256,
            std::iter::empty(),
            Architecture::X86_64,
            &[package],
            &plan.derivation_id(),
            &sealed,
        ),
        Err(Error::ReservedLayoutTarget {
            target,
            package,
            path,
        }) if target == "/usr/.cast-tree-id/forged-child"
            && package == "reserved-owner"
            && path == reserved_path
    ));
    assert!(direct_names(artifact_root.path()).is_empty());
}

#[test]
fn near_system_metadata_names_remain_legal_for_mason_layouts() {
    let root = tempfile::tempdir().unwrap();
    let plan = test_derivation_plan();
    let definition = ResolvedOutput::default();
    let mut collector = collect::Collector::new(root.path());
    collector
        .add_rule("*", "out", stone_recipe::derivation::PathRuleKind::Any)
        .unwrap();
    let mut hasher = stone::StoneDigestWriterHasher::new();
    let mut bucket = analysis::Bucket::default();

    let near_names = ["usr/.cast-tree-id-old", "usr/.stateID.old/child"];
    for relative in near_names {
        let path = root.path().join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"ordinary package data").unwrap();
    }
    for relative in near_names {
        let path = root.path().join(relative);
        bucket.paths.push(collector.path(&path, &mut hasher).unwrap());
    }

    let package = Package::new_with_architecture(
        "near-names",
        &plan.package,
        &definition,
        bucket,
        NonZeroU64::new(1).unwrap(),
        Architecture::X86_64,
        1,
    );
    verify_unique_layout_targets(&[package]).unwrap();
}

#[test]
fn non_directory_normalized_ancestor_is_rejected_before_emission() {
    let root = tempfile::tempdir().unwrap();
    let normalized_ancestor = root.path().join("usr/bin");
    let descendant = root.path().join("bin/tool");
    std::fs::create_dir_all(normalized_ancestor.parent().unwrap()).unwrap();
    std::fs::create_dir_all(descendant.parent().unwrap()).unwrap();
    std::fs::write(&normalized_ancestor, b"not a directory").unwrap();
    std::fs::write(&descendant, b"payload").unwrap();
    let mut collector = collect::Collector::new(root.path());
    collector
        .add_rule("*", "out", stone_recipe::derivation::PathRuleKind::Any)
        .unwrap();
    let mut hasher = stone::StoneDigestWriterHasher::new();
    let ancestor = collector.path(&normalized_ancestor, &mut hasher).unwrap();
    let descendant = collector.path(&descendant, &mut hasher).unwrap();
    let plan = test_derivation_plan();
    let definition = ResolvedOutput::default();
    let package = |name, path| {
        let mut bucket = analysis::Bucket::default();
        bucket.paths.push(path);
        Package::new_with_architecture(
            name,
            &plan.package,
            &definition,
            bucket,
            NonZeroU64::new(1).unwrap(),
            Architecture::X86_64,
            1,
        )
    };
    let packages = [package("ancestor", ancestor), package("descendant", descendant)];

    assert!(matches!(
        verify_unique_layout_targets(&packages),
        Err(Error::AncestorLayoutTarget {
            ref ancestor,
            ref descendant,
            ..
        }) if ancestor == "/bin" && descendant == "/bin/tool"
    ));
}

#[test]
fn directory_normalized_ancestor_may_own_descendants() {
    let root = tempfile::tempdir().unwrap();
    let normalized_ancestor = root.path().join("usr/bin");
    let descendant = root.path().join("bin/tool");
    std::fs::create_dir_all(&normalized_ancestor).unwrap();
    std::fs::create_dir_all(descendant.parent().unwrap()).unwrap();
    std::fs::write(&descendant, b"payload").unwrap();
    let mut collector = collect::Collector::new(root.path());
    collector
        .add_rule("*", "out", stone_recipe::derivation::PathRuleKind::Any)
        .unwrap();
    let mut hasher = stone::StoneDigestWriterHasher::new();
    let ancestor = collector.path(&normalized_ancestor, &mut hasher).unwrap();
    let descendant = collector.path(&descendant, &mut hasher).unwrap();
    let plan = test_derivation_plan();
    let definition = ResolvedOutput::default();
    let package = |name, path| {
        let mut bucket = analysis::Bucket::default();
        bucket.paths.push(path);
        Package::new_with_architecture(
            name,
            &plan.package,
            &definition,
            bucket,
            NonZeroU64::new(1).unwrap(),
            Architecture::X86_64,
            1,
        )
    };
    let packages = [package("ancestor", ancestor), package("descendant", descendant)];

    verify_unique_layout_targets(&packages).unwrap();
}

#[test]
fn content_emission_preserves_the_primary_writer_error() {
    let path = Path::new("/verified/input");
    let write_result = Err(StoneWriteError::Io(io::Error::other("primary writer failure")));
    let verify_result = Err(collect::Error::TreeChanged {
        path: path.to_owned(),
        detail: "consequential short read",
    });

    assert!(matches!(
        finish_content_write(path, write_result, verify_result),
        Err(Error::StoneBinaryWriter { .. })
    ));
}
