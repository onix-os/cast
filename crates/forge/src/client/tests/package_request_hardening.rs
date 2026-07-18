use super::*;

#[test]
fn malformed_package_requests_return_typed_errors_without_persistent_side_effects() {
    let installation = tempfile::tempdir().unwrap();
    let output_parent = tempfile::tempdir().unwrap();
    let output = output_parent.path().join("must-stay-absent");
    let mut client = stateful_test_client(installation.path());

    let install_error = match client.install(&["binary("], true, false) {
        Ok(_) => panic!("malformed install request unexpectedly succeeded"),
        Err(error) => error,
    };
    assert!(matches!(
        install_error,
        Error::Install(source) if matches!(*source, install::Error::Provider(_))
    ));

    let remove_error = match client.remove(&["binary("], true, false) {
        Ok(_) => panic!("malformed remove request unexpectedly succeeded"),
        Err(error) => error,
    };
    assert!(matches!(
        remove_error,
        Error::Remove(source) if matches!(*source, remove::Error::Provider(_))
    ));

    let fetch_error = match client.fetch(&["binary("], &output, false) {
        Ok(_) => panic!("malformed fetch request unexpectedly succeeded"),
        Err(error) => error,
    };
    assert!(matches!(
        fetch_error,
        Error::Fetch(source) if matches!(*source, fetch::Error::Provider(_))
    ));

    assert!(client.state_db.all().unwrap().is_empty());
    assert!(!client.installation.cache_path("downloads").exists());
    assert!(!output.exists());
}
