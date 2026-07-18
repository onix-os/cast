#[cfg(test)]
mod fixture_import_tests {
    use std::os::unix::fs::{MetadataExt as _, symlink};

    use sha2::{Digest as _, Sha256};
    use stone_recipe::derivation::LockedSource;
    use forge::runtime;

    use super::*;
    use crate::upstream::{
        Upstream, import_locked_archive_fixture, import_locked_git_fixture, locked_upstreams, sync_locked,
    };

    const EPOCH: i64 = 1_700_000_000;
    const COMMIT: &str = "4f124a6f438b061a836e332d67e803a69a7bf2d3";
    const MATERIALIZATION: &str = "4ee9bc28310196671f067634c0cbce03f21eca3a1a7b18be2fd2f808bc0c0e2c";

    fn bundle() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join(
            "../../tests/fixtures/gluon/execution/git-bundles/cast-multiple-sources-protocol-1.0.0.bundle",
        )
    }

    fn locked_git() -> LockedSource {
        LockedSource::Git {
            order: 1,
            url: "https://fixtures.invalid/sources/cast-multiple-sources-protocol.git".to_owned(),
            requested_ref: COMMIT.to_owned(),
            commit: COMMIT.to_owned(),
            materialization_sha256: MATERIALIZATION.to_owned(),
            directory: "vendor-protocol".to_owned(),
        }
    }

    fn git_for(source: &LockedSource) -> Git {
        let upstreams = locked_upstreams(std::slice::from_ref(source)).unwrap();
        let [Upstream::Git(git)] = upstreams.as_slice() else {
            panic!("fixture source stopped being Git-kind");
        };
        git.clone()
    }

    fn assert_no_cache(source: &LockedSource, storage: &Path) {
        let git = git_for(source);
        assert!(!git.stored_path(storage).exists());
        assert!(!git.mutation_marker_path(storage).exists());
    }

    #[test]
    fn exact_import_reopens_and_syncs_through_the_production_cache_path() {
        let temporary = crate::private_tempdir();
        let source = locked_git();
        let storage = temporary.path().join("upstreams");
        let shared = temporary.path().join("shared");
        import_locked_git_fixture(&source, &storage, &bundle(), EPOCH).unwrap();

        let git = git_for(&source);
        let (stored, has_commit) = runtime::block_on(git.stored(&storage)).unwrap();
        assert!(has_commit);
        assert_eq!(stored.resolved_hash, COMMIT);
        drop(stored);
        sync_locked(std::slice::from_ref(&source), &storage, &shared, EPOCH).unwrap();

        let root = shared.join("vendor-protocol");
        assert_eq!(
            fs::read(root.join("include/vendor_protocol.h")).unwrap(),
            fs::read(
                Path::new(env!("CARGO_MANIFEST_DIR")).join(
                    "../../tests/fixtures/gluon/execution/git-source-trees/\
                     cast-multiple-sources-protocol-1.0.0/include/vendor_protocol.h",
                ),
            )
            .unwrap()
        );
        assert!(!root.join(".git").exists());
        let metadata = fs::metadata(root.join("include/vendor_protocol.h")).unwrap();
        assert_eq!(metadata.mode() & 0o7777, 0o644);
        assert_eq!(metadata.mtime(), EPOCH);
        assert_eq!(metadata.mtime_nsec(), 0);
    }

    #[test]
    fn wrong_variant_commit_and_materialization_identities_fail_before_publication() {
        let temporary = crate::private_tempdir();
        let archive = LockedSource::Archive {
            order: 0,
            url: "https://fixtures.invalid/source.tar".to_owned(),
            sha256: hex::encode(Sha256::digest(b"archive")),
            filename: "source.tar".to_owned(),
        };
        assert!(matches!(
            import_locked_git_fixture(&archive, &temporary.path().join("variant"), &bundle(), EPOCH),
            Err(crate::upstream::Error::FixtureImportRequiresGit)
        ));
        assert!(matches!(
            import_locked_archive_fixture(&locked_git(), &temporary.path().join("archive"), &bundle()),
            Err(crate::upstream::Error::FixtureImportRequiresArchive)
        ));

        let mut non_full = locked_git();
        let LockedSource::Git { commit, .. } = &mut non_full else {
            unreachable!()
        };
        *commit = "main".to_owned();
        let non_full_storage = temporary.path().join("non-full");
        assert!(import_locked_git_fixture(&non_full, &non_full_storage, &bundle(), EPOCH).is_err());
        assert_no_cache(&non_full, &non_full_storage);

        let mut wrong_commit = locked_git();
        let LockedSource::Git { commit, .. } = &mut wrong_commit else {
            unreachable!()
        };
        *commit = "0123456789abcdef0123456789abcdef01234567".to_owned();
        let wrong_commit_storage = temporary.path().join("wrong-commit");
        assert!(import_locked_git_fixture(&wrong_commit, &wrong_commit_storage, &bundle(), EPOCH).is_err());
        assert_no_cache(&wrong_commit, &wrong_commit_storage);

        let mut wrong_digest = locked_git();
        let LockedSource::Git {
            materialization_sha256,
            ..
        } = &mut wrong_digest
        else {
            unreachable!()
        };
        *materialization_sha256 = "0".repeat(64);
        let wrong_digest_storage = temporary.path().join("wrong-digest");
        let error = import_locked_git_fixture(&wrong_digest, &wrong_digest_storage, &bundle(), EPOCH).unwrap_err();
        assert!(matches!(
            error,
            crate::upstream::Error::Git(Error::MaterializationDigestMismatch { index: 1, .. })
        ));
        assert_no_cache(&wrong_digest, &wrong_digest_storage);
    }

    #[test]
    fn unsafe_bundle_files_and_corrupt_bytes_never_publish_a_cache() {
        let temporary = crate::private_tempdir();
        let source = locked_git();
        let corrupt = temporary.path().join("corrupt.bundle");
        fs::write(&corrupt, b"not a Git bundle\n").unwrap();
        let empty = temporary.path().join("empty.bundle");
        fs::write(&empty, []).unwrap();
        let oversized = temporary.path().join("oversized.bundle");
        fs::write(&oversized, vec![0_u8; 1024 * 1024 + 1]).unwrap();
        let linked_source = temporary.path().join("linked-source.bundle");
        fs::copy(bundle(), &linked_source).unwrap();
        let linked = temporary.path().join("linked.bundle");
        fs::hard_link(&linked_source, &linked).unwrap();
        let symlinked = temporary.path().join("symlink.bundle");
        symlink(bundle(), &symlinked).unwrap();
        let directory = temporary.path().join("directory.bundle");
        fs::create_dir(&directory).unwrap();
        let missing = temporary.path().join("missing.bundle");

        for (index, fixture) in [corrupt, empty, oversized, linked, symlinked, directory, missing]
            .into_iter()
            .enumerate()
        {
            let storage = temporary.path().join(format!("unsafe-{index}"));
            assert!(import_locked_git_fixture(&source, &storage, &fixture, EPOCH).is_err());
            assert_no_cache(&source, &storage);
        }
    }

    #[test]
    fn existing_cache_marker_and_post_publication_failure_are_never_adopted() {
        let temporary = crate::private_tempdir();
        let source = locked_git();
        let git = git_for(&source);

        let occupied_storage = temporary.path().join("occupied");
        let occupied_cache = git.stored_path(&occupied_storage);
        fs::create_dir_all(&occupied_cache).unwrap();
        fs::write(occupied_cache.join("sentinel"), b"foreign cache\n").unwrap();
        assert!(import_locked_git_fixture(&source, &occupied_storage, &bundle(), EPOCH).is_err());
        assert_eq!(fs::read(occupied_cache.join("sentinel")).unwrap(), b"foreign cache\n");

        let marked_storage = temporary.path().join("marked");
        let marker = git.mutation_marker_path(&marked_storage);
        fs::create_dir_all(marker.parent().unwrap()).unwrap();
        fs::write(&marker, b"incomplete\n").unwrap();
        assert!(matches!(
            import_locked_git_fixture(&source, &marked_storage, &bundle(), EPOCH),
            Err(crate::upstream::Error::Git(Error::IncompleteCache { .. }))
        ));
        assert!(!git.stored_path(&marked_storage).exists());
        assert!(marker.exists());

        let failed_storage = temporary.path().join("post-publication");
        let failed_cache = git.stored_path(&failed_storage);
        arm_fixture_import_post_publication_failure(failed_cache.clone());
        assert!(import_locked_git_fixture(&source, &failed_storage, &bundle(), EPOCH).is_err());
        let failed_marker = git.mutation_marker_path(&failed_storage);
        assert!(failed_cache.is_dir());
        assert!(failed_marker.is_file());
        assert!(matches!(
            runtime::block_on(git.stored(&failed_storage)),
            Err(Error::IncompleteCache { .. })
        ));
        assert!(matches!(
            import_locked_git_fixture(&source, &failed_storage, &bundle(), EPOCH),
            Err(crate::upstream::Error::Git(Error::IncompleteCache { .. }))
        ));
        assert!(failed_cache.is_dir());
        assert!(failed_marker.is_file());
    }
}
