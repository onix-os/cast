use super::*;

#[test]
fn interrupted_read_budget_admits_n_and_rejects_n_plus_one() {
    let path = Path::new("/retained/usr/.cast-tree-id");
    let mut interrupted = 0usize;
    for _ in 0..MAX_INTERRUPTED_READ_ATTEMPTS {
        charge_interrupted_read(&mut interrupted, path).unwrap();
    }
    let error = charge_interrupted_read(&mut interrupted, path).unwrap_err();
    assert!(matches!(
        error,
        TreeMarkerError::Io {
            source,
            ..
        } if source.kind() == io::ErrorKind::TimedOut
    ));
}
