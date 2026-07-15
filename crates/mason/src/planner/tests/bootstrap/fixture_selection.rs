#[test]
fn execution_fixture_selector_accepts_all_and_defaults_to_all() {
    assert_eq!(
        parse_execution_fixture_selection(None),
        Ok(ExecutionFixtureSelection::All)
    );
    assert_eq!(
        parse_execution_fixture_selection(Some("all")),
        Ok(ExecutionFixtureSelection::All)
    );
}

#[test]
fn execution_fixture_selector_accepts_each_single_fixture_exactly() {
    for selected in REQUIRED_EXECUTION_FIXTURES {
        let selection = parse_execution_fixture_selection(Some(selected)).unwrap();
        assert_eq!(selection, ExecutionFixtureSelection::One(selected));
        assert_eq!(selection.expected_count(), 1);
        for fixture in REQUIRED_EXECUTION_FIXTURES {
            assert_eq!(selection.includes(fixture), fixture == selected);
        }
    }
}

#[test]
fn execution_fixture_selector_rejects_every_noncanonical_value() {
    for invalid in ["", "ALL", "cmake ", "not-a-fixture", "autotools,cargo"] {
        let error = parse_execution_fixture_selection(Some(invalid)).unwrap_err();
        assert!(error.contains(EXECUTION_FIXTURE_SELECTOR_ENV));
        assert!(error.contains(&format!("{invalid:?}")));
    }
}

#[test]
fn fixture_closure_coverage_is_exact_and_fail_closed() {
    let package_ids = vec!["00".repeat(32), "11".repeat(32)];
    let fixtures = REQUIRED_EXECUTION_FIXTURES
        .iter()
        .map(|name| FixtureClosure {
            name: (*name).to_owned(),
            package_ids: package_ids.clone(),
        })
        .collect::<Vec<_>>();
    assert_eq!(validate_fixture_closure_coverage(&fixtures, &package_ids), Ok(()));

    let mut missing = fixtures.clone();
    missing.pop();
    assert!(validate_fixture_closure_coverage(&missing, &package_ids).is_err());

    let mut duplicate = fixtures.clone();
    duplicate[0].package_ids.push(package_ids[1].clone());
    assert!(validate_fixture_closure_coverage(&duplicate, &package_ids).is_err());

    let mut unknown = fixtures;
    unknown[0].package_ids.push("ff".repeat(32));
    assert!(validate_fixture_closure_coverage(&unknown, &package_ids).is_err());
}
