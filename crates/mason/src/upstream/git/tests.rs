#[cfg(test)]
mod tests {
    use std::{collections::HashSet, os::unix::fs::symlink, process::Command};

    use super::*;

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

    fn source(url: Url) -> Git {
        Git {
            url,
            commit: "HEAD".to_owned(),
            name: "source".to_owned(),
            original_index: 0,
            materialization_sha256: None,
        }
    }

    fn create_repository(path: &Path, contents: &[u8]) -> String {
        fs::create_dir(path).unwrap();
        fixture_git(path, &["init", "--initial-branch=main"]);
        fixture_git(path, &["config", "user.name", "Cast Test"]);
        fixture_git(path, &["config", "user.email", "cast@example.invalid"]);
        fs::write(path.join("source.txt"), contents).unwrap();
        fixture_git(path, &["add", "source.txt"]);
        fixture_git(path, &["commit", "-m", "source"]);
        fixture_git(path, &["rev-parse", "HEAD"])
    }

    #[test]
    fn cache_identity_binds_the_complete_canonical_url() {
        let urls = [
            "https://alice:secret@example.invalid:8443/org/repo.git?transport=one#release",
            "http://alice:secret@example.invalid:8443/org/repo.git?transport=one#release",
            "https://bob:secret@example.invalid:8443/org/repo.git?transport=one#release",
            "https://alice:different@example.invalid:8443/org/repo.git?transport=one#release",
            "https://alice:secret@example.invalid:9443/org/repo.git?transport=one#release",
            "https://alice:secret@example.invalid:8443/other/repo.git?transport=one#release",
            "https://alice:secret@example.invalid:8443/org/repo.git?transport=two#release",
            "https://alice:secret@example.invalid:8443/org/repo.git?transport=one#other",
        ]
        .map(|url| Url::parse(url).unwrap());

        let names = urls
            .iter()
            .map(|url| {
                let name = source(url.clone()).directory_name();
                let name = name.to_str().unwrap();
                let expected_digest = format!("{:x}", Sha256::digest(url.as_str().as_bytes()));
                assert!(name.starts_with("repo-"));
                assert!(name.ends_with(&expected_digest));
                assert!(
                    name.bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
                );
                assert!(!name.contains("alice"));
                assert!(!name.contains("secret"));
                name.to_owned()
            })
            .collect::<HashSet<_>>();

        assert_eq!(names.len(), urls.len());
    }

    #[test]
    fn cache_identity_never_uses_unsafe_url_path_bytes() {
        let url = Url::parse("https://example.invalid/a/%2E%2E/%2Fbad%5Cname%00.git?path=/tmp/escape").unwrap();
        let name = source(url).directory_name();
        let name = name.to_str().unwrap();

        assert_eq!(Path::new(name).components().count(), 1);
        assert!(
            name.bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        );
        assert!(!matches!(name, "." | ".."));
    }

    #[tokio::test]
    async fn mismatched_cache_origin_is_rejected_and_repaired_before_reuse() {
        let temporary = tempfile::tempdir().unwrap();
        let requested_path = temporary.path().join("requested");
        let wrong_path = temporary.path().join("wrong");
        let requested_commit = create_repository(&requested_path, b"requested source\n");
        create_repository(&wrong_path, b"wrong source\n");
        let requested_url = Url::from_directory_path(&requested_path).unwrap();
        let wrong_url = Url::from_directory_path(&wrong_path).unwrap();
        let requested = source(requested_url.clone());
        let storage = temporary.path().join("storage");
        let cached_path = requested.stored_path(&storage);
        fs::create_dir_all(cached_path.parent().unwrap()).unwrap();
        gitwrap::Repository::clone_mirror(&cached_path, &wrong_url)
            .await
            .unwrap();

        match requested.stored(&storage).await {
            Err(Error::OriginMismatch { cache }) => assert_eq!(cache, cached_path),
            Err(error) => panic!("unexpected cache error: {error}"),
            Ok(_) => panic!("a mirror for another origin was accepted"),
        }

        let stored = requested.store(&storage, &ProgressBar::new(100)).await.unwrap();
        assert_eq!(
            stored.repo.get_remote_url("origin").await.unwrap(),
            requested_url.as_str()
        );
        assert_eq!(stored.resolved_hash, requested_commit);
    }

    #[tokio::test]
    async fn failed_cache_fetch_is_purged_before_a_later_retry() {
        let temporary = tempfile::tempdir().unwrap();
        let source_path = temporary.path().join("source");
        create_repository(&source_path, b"source\n");
        let requested = source(Url::from_directory_path(&source_path).unwrap());
        let storage = temporary.path().join("storage");
        requested.store(&storage, &ProgressBar::new(100)).await.unwrap();
        let cached_path = requested.stored_path(&storage);
        assert!(cached_path.is_dir());

        fs::remove_dir_all(&source_path).unwrap();
        assert!(requested.resolve(&storage, &ProgressBar::new(100)).await.is_err());
        assert!(
            !cached_path.exists(),
            "a failed in-place fetch must not leave a cache eligible for reuse"
        );
    }

    #[tokio::test]
    async fn same_url_sources_serialize_on_the_shared_cache() {
        let temporary = tempfile::tempdir().unwrap();
        let source_path = temporary.path().join("source");
        let commit = create_repository(&source_path, b"source\n");
        let source_url = Url::from_directory_path(&source_path).unwrap();
        let first = source(source_url.clone());
        let mut second = source(source_url);
        second.name = "second-materialization".to_owned();
        second.original_index = 1;
        let storage = temporary.path().join("storage");
        let first_progress = ProgressBar::new(100);
        let second_progress = ProgressBar::new(100);

        let (first_stored, second_stored) = tokio::join!(
            first.store(&storage, &first_progress),
            second.store(&storage, &second_progress),
        );

        assert_eq!(first_stored.unwrap().resolved_hash, commit);
        assert_eq!(second_stored.unwrap().resolved_hash, commit);
        assert_eq!(first.stored_path(&storage), second.stored_path(&storage));
    }

    #[test]
    fn cache_lock_rejects_parent_symlinks_and_detects_lock_inode_replacement() {
        let temporary = tempfile::tempdir().unwrap();
        let requested = source(Url::parse("https://example.invalid/source.git").unwrap());
        let storage = temporary.path().join("storage");
        let outside = temporary.path().join("outside");
        fs::create_dir(&storage).unwrap();
        fs::create_dir(&outside).unwrap();
        symlink(&outside, storage.join("git")).unwrap();
        assert!(requested.open_cache_lock(&storage).is_err());
        assert!(fs::read_dir(&outside).unwrap().next().is_none());

        fs::remove_file(storage.join("git")).unwrap();
        let opened = requested.open_cache_lock(&storage).unwrap();
        let lock_path = requested.cache_lock_path(&storage);

        fs::set_permissions(&lock_path, std::fs::Permissions::from_mode(0o640)).unwrap();
        let error = opened.verify_name().unwrap_err();
        assert!(error.to_string().contains("private mode"));
        fs::set_permissions(&lock_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        opened.verify_name().unwrap();

        let displaced = lock_path.with_extension("displaced");
        fs::rename(&lock_path, &displaced).unwrap();
        fs::write(&lock_path, b"replacement").unwrap();
        let error = opened.verify_name().unwrap_err();
        assert!(error.to_string().contains("inode"));
        assert_eq!(fs::read(&lock_path).unwrap(), b"replacement");
    }

    #[tokio::test]
    async fn live_cache_owner_is_waited_for_and_interrupted_marker_is_repaired() {
        let temporary = tempfile::tempdir().unwrap();
        let source_path = temporary.path().join("source");
        let commit = create_repository(&source_path, b"source\n");
        let requested = source(Url::from_directory_path(&source_path).unwrap());
        let storage = temporary.path().join("storage");
        requested.store(&storage, &ProgressBar::new(100)).await.unwrap();
        let live_lock = tokio::time::timeout(
            Duration::from_secs(2),
            requested.acquire_cache_lock(&storage, CacheLockMode::Exclusive),
        )
        .await
        .expect("the completed store must release its cache lock")
        .unwrap();
        let marker = requested.begin_cache_mutation(&storage).unwrap();
        let marker_path = marker.path.clone();

        let waiting_progress = ProgressBar::new(100);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), requested.store(&storage, &waiting_progress),)
                .await
                .is_err(),
            "a concurrent cache caller must wait instead of poisoning its peer through CacheBusy",
        );
        assert!(
            requested.stored_path(&storage).is_dir(),
            "a concurrent caller must not delete a mirror beneath the lock owner"
        );
        drop(live_lock); // model the mutation owner exiting unexpectedly
        drop(marker);

        assert!(matches!(
            requested.stored(&storage).await,
            Err(Error::IncompleteCache { .. })
        ));
        assert!(requested.stored_path(&storage).is_dir());
        assert!(marker_path.is_file());

        // Acquiring the exclusive lock proves that no cooperating mutation
        // owner is still live. Repair discards the marked mirror; it never
        // reuses potentially partial state.
        let repaired = requested.store(&storage, &ProgressBar::new(100)).await.unwrap();
        assert_eq!(repaired.resolved_hash, commit);
        assert!(!marker_path.exists());
    }

    #[test]
    fn verified_checkout_install_never_replaces_a_destination_symlink() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("staged");
        let destination = temporary.path().join("destination");
        let outside = temporary.path().join("outside");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("source.txt"), b"verified").unwrap();
        fs::create_dir(&outside).unwrap();
        symlink(&outside, &destination).unwrap();

        let error = rename_noreplace(&source, &destination).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert!(source.join("source.txt").is_file());
        assert!(fs::symlink_metadata(&destination).unwrap().file_type().is_symlink());
        assert!(fs::read_dir(outside).unwrap().next().is_none());
    }

    #[test]
    fn rejected_install_quarantine_refuses_to_move_a_replacement_inode() {
        let temporary = tempfile::tempdir().unwrap();
        let staged = temporary.path().join("staged");
        let destination = temporary.path().join("destination");
        let displaced = temporary.path().join("displaced-verified");
        fs::create_dir(&staged).unwrap();
        fs::write(staged.join("source.txt"), b"verified").unwrap();
        let installed = PinnedInstall::install(&staged, &destination).unwrap();

        fs::rename(&destination, &displaced).unwrap();
        fs::create_dir(&destination).unwrap();
        fs::write(destination.join("attacker.txt"), b"replacement").unwrap();
        let error = installed.quarantine().unwrap_err();

        assert!(error.to_string().contains("replacement Git checkout inode"));
        assert_eq!(fs::read(destination.join("attacker.txt")).unwrap(), b"replacement");
        assert_eq!(fs::read(displaced.join("source.txt")).unwrap(), b"verified");
        assert!(fs::read_dir(temporary.path()).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .as_bytes()
                .starts_with(b".cast-git-rejected-")
        }));
    }

    #[test]
    fn quarantine_cleanup_removes_empty_entries_and_reports_cleanup_failure() {
        let temporary = tempfile::tempdir().unwrap();
        let parent = open_directory(temporary.path()).unwrap();

        let (empty_name, empty) = create_quarantine_directory(&parent).unwrap();
        drop(empty);
        let source = io::Error::new(io::ErrorKind::Interrupted, "primary quarantine failure");
        let error = cleanup_quarantine_error(&parent, &empty_name, source);
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        assert_eq!(error.to_string(), "primary quarantine failure");
        assert_eq!(identity_at(&parent, &empty_name).unwrap(), None);

        let (nonempty_name, nonempty) = create_quarantine_directory(&parent).unwrap();
        let public_name = std::ffi::OsStr::from_bytes(nonempty_name.as_bytes());
        fs::write(temporary.path().join(public_name).join("retained"), b"data").unwrap();
        drop(nonempty);
        let source = io::Error::new(io::ErrorKind::Interrupted, "primary quarantine failure");
        let error = cleanup_quarantine_error(&parent, &nonempty_name, source);
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        assert!(
            error
                .to_string()
                .contains("removing the incomplete Git quarantine also failed")
        );
        assert!(temporary.path().join(public_name).join("retained").is_file());
    }

    #[test]
    fn rejected_install_quarantine_stays_with_the_pinned_parent_after_path_replacement() {
        let temporary = tempfile::tempdir().unwrap();
        let parent = temporary.path().join("parent");
        let moved_parent = temporary.path().join("moved-parent");
        let staged = temporary.path().join("staged");
        fs::create_dir(&parent).unwrap();
        fs::create_dir(&staged).unwrap();
        fs::write(staged.join("source.txt"), b"verified").unwrap();
        let destination = parent.join("destination");
        let installed = PinnedInstall::install(&staged, &destination).unwrap();

        fs::rename(&parent, &moved_parent).unwrap();
        fs::create_dir(&parent).unwrap();
        fs::create_dir(parent.join("destination")).unwrap();
        fs::write(parent.join("destination/attacker.txt"), b"replacement").unwrap();
        let reported = installed.quarantine().unwrap();

        assert_eq!(
            fs::read(parent.join("destination/attacker.txt")).unwrap(),
            b"replacement"
        );
        assert!(
            reported.to_string_lossy().contains("detached pinned Git quarantine"),
            "a replaced public parent must not produce a misleading live path: {reported:?}"
        );
        let quarantine = fs::read_dir(&moved_parent)
            .unwrap()
            .find_map(|entry| {
                let entry = entry.unwrap();
                entry
                    .file_name()
                    .as_bytes()
                    .starts_with(b".cast-git-rejected-")
                    .then(|| entry.path())
            })
            .expect("rejected checkout remains beneath the pinned original parent");
        assert_eq!(fs::read(quarantine.join("checkout/source.txt")).unwrap(), b"verified");
    }

    #[tokio::test]
    async fn failed_materialization_verification_leaves_no_checkout_or_staging_tree() {
        let temporary = tempfile::tempdir().unwrap();
        let source_path = temporary.path().join("source");
        let commit = create_repository(&source_path, b"locked source\n");
        let source_url = Url::from_directory_path(&source_path).unwrap();
        let mirror_path = temporary.path().join("mirror.git");
        let repo = gitwrap::Repository::clone_mirror(&mirror_path, &source_url)
            .await
            .unwrap();
        let stored = StoredGit {
            name: "source".to_owned(),
            was_cached: false,
            resolved_hash: commit,
            original_index: 0,
            materialization_sha256: Some("0".repeat(64)),
            repo,
        };
        let share_root = temporary.path().join("share");
        fs::create_dir(&share_root).unwrap();

        assert!(matches!(
            stored.share(&share_root.join("source"), 0).await,
            Err(Error::MaterializationDigestMismatch { .. })
        ));
        assert!(fs::read_dir(share_root).unwrap().next().is_none());
    }

    #[test]
    fn exported_git_tree_removes_only_git_administration_state() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join(".git");
        fs::create_dir_all(root.join(".git/objects")).unwrap();
        fs::create_dir_all(root.join("nested")).unwrap();
        fs::write(root.join("nested/.git"), b"gitdir: ../.git/modules/nested\n").unwrap();
        fs::write(root.join("regular"), b"regular").unwrap();
        symlink("regular", root.join("link")).unwrap();
        fs::write(root.join(".git-marker"), b"ordinary committed name").unwrap();

        materialization::remove_git_administration_bounded(&root).unwrap();

        assert!(root.is_dir());
        assert!(!root.join(".git").exists());
        assert!(!root.join("nested/.git").exists());
        assert!(
            fs::symlink_metadata(root.join("link"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(root.join(".git-marker").is_file());
    }
}
