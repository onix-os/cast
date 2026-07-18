use std::time::Duration;

use super::*;

#[test]
fn per_source_byte_policy_admits_n_and_rejects_n_plus_one() {
    let bytes = valid_os_info("head-os", "Head OS", &[]);
    let fixture = Fixture::new(FixtureSchemaSource::OsInfo(bytes.clone()), Vec::new());
    let mut policy = SCHEMA_POLICY;
    policy.max_source_bytes = bytes.len();
    fixture.prepare_with_schema_policy(policy).unwrap();

    policy.max_source_bytes -= 1;
    assert!(matches!(
        fixture.prepare_with_schema_policy(policy),
        Err(ActiveReblitBootSchemaInputsError::SourceByteLimit { limit, actual })
            if limit + 1 == actual && actual == bytes.len()
    ));
}

#[test]
fn aggregate_byte_policy_accounts_for_each_authenticated_local_source() {
    let head = valid_os_info("head-os", "Head OS", &[]);
    let history = valid_os_info("history", "History", &[]);
    let fixture = Fixture::new(
        FixtureSchemaSource::OsInfo(head.clone()),
        vec![FixtureSchemaSource::OsInfo(history.clone())],
    );
    let total = head.len() + history.len();
    let mut policy = SCHEMA_POLICY;
    policy.max_total_bytes = total;
    fixture.prepare_with_schema_policy(policy).unwrap();

    policy.max_total_bytes -= 1;
    assert!(matches!(
        fixture.prepare_with_schema_policy(policy),
        Err(ActiveReblitBootSchemaInputsError::TotalByteLimit { limit, actual })
            if limit + 1 == actual && actual == total
    ));
}

#[test]
fn work_policy_admits_observed_n_and_rejects_n_minus_one() {
    let fixture = Fixture::new(
        FixtureSchemaSource::Generated(valid_os_release("head-os", "Head OS")),
        Vec::new(),
    );
    let observed = fixture.prepare().unwrap().schemas.preparation_work();
    let mut policy = SCHEMA_POLICY;
    policy.max_work = observed;
    fixture.prepare_with_schema_policy(policy).unwrap();

    policy.max_work -= 1;
    assert!(matches!(
        fixture.prepare_with_schema_policy(policy),
        Err(ActiveReblitBootSchemaInputsError::WorkLimit { limit, actual })
            if actual == limit + 1
    ));
}

#[test]
fn unrepresentable_deadline_is_rejected_before_source_work() {
    let fixture = Fixture::new(
        FixtureSchemaSource::OsInfo(valid_os_info("head-os", "Head OS", &[])),
        Vec::new(),
    );
    let mut policy = SCHEMA_POLICY;
    policy.timeout = Duration::MAX;
    assert!(matches!(
        fixture.prepare_with_schema_policy(policy),
        Err(ActiveReblitBootSchemaInputsError::InvalidDeadline { timeout })
            if timeout == Duration::MAX
    ));
}

#[test]
fn os_release_parser_rejects_duplicate_keys_and_fat_unsafe_ids() {
    assert_eq!(
        parse_os_release(b"NAME=One\nNAME=Two\nID=test\n").unwrap_err(),
        ActiveReblitBootSchemaSemanticReason::InvalidDocument
    );
    assert_eq!(
        parse_os_release(b"NAME=Test\nID=CON\n").unwrap_err(),
        ActiveReblitBootSchemaSemanticReason::UnsafeIdentifier
    );
    assert_eq!(
        parse_os_release(b"NAME=Test\nID=bad/path\n").unwrap_err(),
        ActiveReblitBootSchemaSemanticReason::UnsafeIdentifier
    );
}

#[test]
fn os_info_parser_rejects_duplicate_or_current_former_identity() {
    let bytes = valid_os_info("current", "Current", &[("current", "Former Current")]);
    assert_eq!(
        parse_os_info(std::str::from_utf8(&bytes).unwrap()).unwrap_err(),
        ActiveReblitBootSchemaSemanticReason::DuplicateFormerIdentity
    );
}
