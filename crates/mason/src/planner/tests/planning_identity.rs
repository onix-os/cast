#[test]
fn identical_explicit_inputs_produce_identical_plans_and_locks() {
    let fixture = Fixture::new();

    let first = plan(fixture.env(), fixture.request()).unwrap();
    let first_plan = first.plan.canonical_bytes();
    let first_id = first.plan.derivation_id();
    let first_lock = fs::read(&first.lock_path).unwrap();

    assert_eq!(first.lock_outcome, Some(WriteOutcome::Written));
    assert_eq!(first.plan.execution.executor.name, super::EXECUTOR_ABI);
    assert_eq!(first.plan.build_lock.builder.name, "custom");
    assert_ne!(
        first.plan.build_lock.builder.name, first.plan.execution.executor.name,
        "authored structural builder and executor identities must remain separate"
    );
    assert_eq!(first.plan.execution.network, NetworkMode::Disabled);
    assert_eq!(first.plan.execution.filesystems, FilesystemPolicy::default());
    assert_eq!(
        first.plan.environment.get("SOURCE_DATE_EPOCH").map(String::as_str),
        Some("1700000000")
    );
    assert!(!first.plan.build_lock.requests.is_empty());
    let runtime_request = first
        .plan
        .build_lock
        .requests
        .iter()
        .find(|request| request.request == RUNTIME_REQUEST)
        .expect("the external output runtime input must be resolved");
    assert_eq!(
        runtime_request.origins,
        [InputOrigin::OutputRuntime {
            output: "out".to_owned(),
            index: 0,
        }]
    );
    assert!(
        first
            .plan
            .build_lock
            .repositories
            .iter()
            .all(|repository| { Url::parse(&repository.index_uri).is_ok_and(|uri| uri.scheme() == "file") })
    );

    let repeated = plan(fixture.env(), fixture.request()).unwrap();

    assert_eq!(repeated.lock_outcome, Some(WriteOutcome::Unchanged));
    assert_eq!(
        repeated.plan.build_lock.request_fingerprint,
        first.plan.build_lock.request_fingerprint
    );
    assert_eq!(repeated.plan.build_lock.requests, first.plan.build_lock.requests);
    assert_eq!(repeated.plan.provenance, first.plan.provenance);
    assert_eq!(repeated.plan.canonical_bytes(), first_plan);
    assert_eq!(repeated.plan.derivation_id(), first_id);
    assert_eq!(fs::read(&repeated.lock_path).unwrap(), first_lock);
}

#[test]
fn selected_profile_name_participates_in_the_request_fingerprint() {
    let fixture = Fixture::new();
    let first = plan(fixture.env(), fixture.request()).unwrap();
    let mut alternate_request = fixture.request();
    alternate_request.profile = profile::Id::new(ALTERNATE_PROFILE);
    let alternate = plan(fixture.env(), alternate_request).unwrap();

    assert_eq!(
        first.plan.build_lock.profile.fingerprint, alternate.plan.build_lock.profile.fingerprint,
        "both selections intentionally share the same ordered fragment aggregate"
    );
    assert_ne!(
        first.plan.build_lock.profile.name,
        alternate.plan.build_lock.profile.name
    );
    assert_ne!(
        first.plan.build_lock.request_fingerprint,
        alternate.plan.build_lock.request_fingerprint
    );
}
