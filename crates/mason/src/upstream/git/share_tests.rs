#[cfg(test)]
mod share_tests {
    use std::{os::unix::fs::MetadataExt as _, process::Command};

    use super::*;
    use crate::upstream::share_root::ShareRoot;

    fn bounded_git(repository: &Path, arguments: &[&str]) -> String {
        let output = Command::new("timeout")
            .arg("10s")
            .arg("git")
            .arg("-C")
            .arg(repository)
            .args(arguments)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "bounded git {arguments:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    async fn stored_git(temporary: &Path, source_date_epoch: i64) -> StoredGit {
        let source = temporary.join("source-repository");
        fs::create_dir(&source).unwrap();
        bounded_git(&source, &["init", "--initial-branch=main"]);
        bounded_git(&source, &["config", "user.name", "Cast Share Test"]);
        bounded_git(&source, &["config", "user.email", "cast-share@example.invalid"]);
        fs::write(source.join("source.txt"), b"descriptor-rooted Git source\n").unwrap();
        bounded_git(&source, &["add", "source.txt"]);
        bounded_git(&source, &["commit", "-m", "descriptor source"]);
        let commit = bounded_git(&source, &["rev-parse", "HEAD"]);
        let source_url = Url::from_directory_path(&source).unwrap();
        let mirror = temporary.join("mirror.git");
        let repo = gitwrap::Repository::clone_mirror(&mirror, &source_url).await.unwrap();
        let mut stored = StoredGit {
            name: "vendor-source".to_owned(),
            was_cached: false,
            resolved_hash: commit,
            original_index: 0,
            materialization_sha256: None,
            repo,
        };
        let export = temporary.join("digest-export");
        stored.materialization_sha256 = Some(
            stored
                .export_normalized(&export, source_date_epoch)
                .await
                .unwrap(),
        );
        stored
    }

    #[tokio::test]
    async fn retained_share_root_publishes_normalized_git_without_administration_state() {
        const EPOCH: i64 = 1_700_000_000;
        let temporary = tempfile::tempdir().unwrap();
        let stored = stored_git(temporary.path(), EPOCH).await;
        let visible = temporary.path().join("shared");
        let share = ShareRoot::prepare(&visible).unwrap();

        stored
            .share_into_root(share.directory(), share.descriptor_path(), EPOCH)
            .await
            .unwrap();
        share.normalize_and_verify(EPOCH).unwrap();
        let published = visible.join("vendor-source");
        assert_eq!(
            fs::read(published.join("source.txt")).unwrap(),
            b"descriptor-rooted Git source\n"
        );
        assert!(!published.join(".git").exists());
        for path in [&published, &published.join("source.txt")] {
            let metadata = fs::metadata(path).unwrap();
            assert_eq!(metadata.mtime(), EPOCH);
            assert_eq!(
                metadata.mode() & 0o7777,
                if metadata.is_dir() { 0o755 } else { 0o644 }
            );
        }

        assert!(
            stored
                .share_into_root(share.directory(), share.descriptor_path(), EPOCH)
                .await
                .is_err(),
            "a destination collision must fail rather than replace the verified tree"
        );
        assert_eq!(fs::read(published.join("source.txt")).unwrap(), b"descriptor-rooted Git source\n");
        assert_eq!(
            fs::read_dir(&visible)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect::<Vec<_>>(),
            ["vendor-source"]
        );
    }

    #[tokio::test]
    async fn retained_share_root_never_populates_a_replacement_public_parent() {
        const EPOCH: i64 = 1_700_000_000;
        let temporary = tempfile::tempdir().unwrap();
        let stored = stored_git(temporary.path(), EPOCH).await;
        let visible = temporary.path().join("shared");
        let retained = temporary.path().join("retained-shared");
        let share = ShareRoot::prepare(&visible).unwrap();
        fs::rename(&visible, &retained).unwrap();
        fs::create_dir(&visible).unwrap();
        fs::write(visible.join("attacker"), b"replacement parent\n").unwrap();

        stored
            .share_into_root(share.directory(), share.descriptor_path(), EPOCH)
            .await
            .unwrap();
        assert_eq!(
            fs::read(retained.join("vendor-source/source.txt")).unwrap(),
            b"descriptor-rooted Git source\n"
        );
        assert_eq!(fs::read(visible.join("attacker")).unwrap(), b"replacement parent\n");
        assert!(!visible.join("vendor-source").exists());
        assert!(matches!(
            share.normalize_and_verify(EPOCH),
            Err(crate::upstream::share_root::Error::Replaced(_))
        ));
    }
}
