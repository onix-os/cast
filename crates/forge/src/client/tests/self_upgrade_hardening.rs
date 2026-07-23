use super::*;
use crate::client::self_upgrade::{Error as SelfUpgradeError, self_upgrade as run_self_upgrade};

#[test]
fn ephemeral_self_upgrade_returns_a_typed_error_without_mutating_either_root() {
    let installation = tempfile::tempdir().unwrap();
    let destination = tempfile::tempdir().unwrap();
    let installation_sentinel = installation.path().join("installation-sentinel");
    fs::write(&installation_sentinel, b"installation remains").unwrap();

    let mut client = stateful_test_client(installation.path())
        .ephemeral(destination.path())
        .unwrap();
    let error = run_self_upgrade(&mut client, false).unwrap_err();

    assert!(matches!(error, SelfUpgradeError::EphemeralClient));
    assert_eq!(fs::read(installation_sentinel).unwrap(), b"installation remains");
    assert!(fs::read_dir(destination.path()).unwrap().next().is_none());
}

#[test]
fn stateful_self_upgrade_without_cast_returns_a_typed_error() {
    let installation = tempfile::tempdir().unwrap();
    let mut client = stateful_test_client(installation.path());

    let error = run_self_upgrade(&mut client, false).unwrap_err();

    assert!(matches!(error, SelfUpgradeError::CastNotInstalled));
    assert_eq!(client.state_db.all().unwrap(), Vec::new());
}
