use std::os::unix::fs::{symlink, MetadataExt as _};

use super::*;

fn bounded_git(repository: &Path, arguments: &[&str]) -> String {
    let output = Command::new("timeout")
        .arg("10s")
        .arg("git")
        .arg("-C")
        .arg(repository)
        .args(arguments)
        .env_clear()
        .env("PATH", env::var_os("PATH").unwrap_or_default())
        .env("HOME", "/nonexistent")
        .env("XDG_CONFIG_HOME", "/nonexistent")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "bounded git {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn create_bundle(root: &Path) -> (PathBuf, String) {
    let repository = root.join("authored-repository");
    fs::create_dir(&repository).unwrap();
    bounded_git(&repository, &["init", "--initial-branch=main"]);
    bounded_git(&repository, &["config", "user.name", "Gitwrap Fixture"]);
    bounded_git(
        &repository,
        &["config", "user.email", "gitwrap-fixture@example.invalid"],
    );
    fs::write(repository.join("source.txt"), b"fixture bundle source\n").unwrap();
    bounded_git(&repository, &["add", "source.txt"]);
    bounded_git(&repository, &["commit", "-m", "fixture source"]);
    let commit = bounded_git(&repository, &["rev-parse", "HEAD"]);
    let bundle = root.join("source.bundle");
    bounded_git(
        &repository,
        &["bundle", "create", bundle.to_str().unwrap(), "refs/heads/main"],
    );
    (bundle, commit)
}

fn assert_no_fixture_staging(parent: &Path) {
    assert!(
        fs::read_dir(parent)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .all(|name| !name.to_string_lossy().starts_with(".gitwrap-fixture-mirror-")),
        "failed bundle import left reusable private staging behind"
    );
}

#[tokio::test]
async fn direct_bundle_clone_is_canonical_reopenable_and_exact() {
    assert!(FIXTURE_TEST_SUPPORT_ENABLED);
    let temporary = tempfile::tempdir().unwrap();
    let (bundle, commit) = create_bundle(temporary.path());
    let destination = temporary.path().join("mirror.git");
    let origin = Url::parse("https://fixtures.invalid/source.git").unwrap();

    let repository = Repository::clone_fixture_bundle_mirror_with_limits(&destination, &bundle, &origin, test_limits())
        .await
        .unwrap();
    assert_eq!(repository.get_remote_url("origin").await.unwrap(), origin.as_str());
    assert_eq!(repository.peel_commit(&commit).await.unwrap(), commit);
    assert!(repository
        .peel_commit("0123456789abcdef0123456789abcdef01234567")
        .await
        .is_err());
    drop(repository);

    let reopened = Repository::open_private_mirror_with_limits(&destination, &origin, test_limits())
        .await
        .unwrap();
    assert_eq!(reopened.peel_commit(&commit).await.unwrap(), commit);
    let wrong_origin = Url::parse("https://fixtures.invalid/other.git").unwrap();
    assert!(
        Repository::open_private_mirror_with_limits(&destination, &wrong_origin, test_limits())
            .await
            .unwrap_err()
            .mirror_origin_mismatch()
    );
    assert_no_fixture_staging(temporary.path());
}

#[tokio::test]
async fn unsafe_or_invalid_bundle_inputs_never_publish_a_destination() {
    let temporary = tempfile::tempdir().unwrap();
    let (bundle, _) = create_bundle(temporary.path());
    let origin = Url::parse("https://fixtures.invalid/source.git").unwrap();
    let missing = temporary.path().join("missing.bundle");
    let directory = temporary.path().join("directory.bundle");
    fs::create_dir(&directory).unwrap();
    let empty = temporary.path().join("empty.bundle");
    fs::write(&empty, []).unwrap();
    let corrupt = temporary.path().join("corrupt.bundle");
    fs::write(&corrupt, b"not a Git bundle\n").unwrap();
    let linked = temporary.path().join("linked.bundle");
    fs::hard_link(&bundle, &linked).unwrap();
    assert_eq!(fs::metadata(&bundle).unwrap().nlink(), 2);
    let symlinked = temporary.path().join("symlink.bundle");
    symlink(&corrupt, &symlinked).unwrap();

    for (index, source) in [missing, directory, empty, corrupt, linked, symlinked]
        .into_iter()
        .enumerate()
    {
        let destination = temporary.path().join(format!("rejected-{index}.git"));
        assert!(
            Repository::clone_fixture_bundle_mirror_with_limits(&destination, &source, &origin, test_limits(),)
                .await
                .is_err(),
            "unsafe fixture bundle {source:?} was accepted"
        );
        assert!(!destination.exists(), "failed fixture bundle published a mirror");
        assert_no_fixture_staging(temporary.path());
    }
}

#[tokio::test]
async fn bundle_byte_ceiling_origin_policy_and_existing_destination_fail_closed() {
    let temporary = tempfile::tempdir().unwrap();
    let (bundle, _) = create_bundle(temporary.path());
    let https_origin = Url::parse("https://fixtures.invalid/source.git").unwrap();

    let mut tiny_limits = test_limits();
    tiny_limits.repository_bytes = 1;
    let oversized_destination = temporary.path().join("oversized.git");
    assert!(Repository::clone_fixture_bundle_mirror_with_limits(
        &oversized_destination,
        &bundle,
        &https_origin,
        tiny_limits,
    )
    .await
    .is_err());
    assert!(!oversized_destination.exists());

    let ssh_destination = temporary.path().join("ssh.git");
    let ssh_origin = Url::parse("ssh://fixtures.invalid/source.git").unwrap();
    assert!(
        Repository::clone_fixture_bundle_mirror_with_limits(&ssh_destination, &bundle, &ssh_origin, test_limits(),)
            .await
            .is_err()
    );
    assert!(!ssh_destination.exists());

    let occupied = temporary.path().join("occupied.git");
    fs::create_dir(&occupied).unwrap();
    fs::write(occupied.join("sentinel"), b"do not replace\n").unwrap();
    assert!(
        Repository::clone_fixture_bundle_mirror_with_limits(&occupied, &bundle, &https_origin, test_limits(),)
            .await
            .is_err()
    );
    assert_eq!(fs::read(occupied.join("sentinel")).unwrap(), b"do not replace\n");
    assert_no_fixture_staging(temporary.path());
}
