use super::*;

#[test]
fn absent_historical_os_release_selects_the_authenticated_head_schema() {
    let fixture = Fixture::new(
        FixtureSchemaSource::OsInfo(valid_os_info("head-os", "Head OS", &[])),
        vec![FixtureSchemaSource::MissingGenerated],
    );
    let history = fixture.histories[0].id;
    let prepared = fixture.prepare().unwrap();
    let global = prepared.schemas.schema_for_state(fixture.head.id).unwrap();
    let fallback = prepared.schemas.schema_for_state(history).unwrap();

    assert_eq!(fallback.schema(), global.schema());
    assert_eq!(
        fallback.source(),
        ActiveReblitBootSchemaSourceBinding::GlobalFallback {
            failed_local: ActiveReblitBootSchemaFallbackReason::Structural(
                ActiveReblitBootSchemaStructuralReason::MissingLib,
            ),
            global_state: fixture.head.id,
        }
    );
    prepared.revalidate(&fixture).unwrap();
}

#[test]
fn malformed_historical_os_release_falls_back_only_as_semantic_invalidity() {
    let fixture = Fixture::new(
        FixtureSchemaSource::Generated(valid_os_release("head-os", "Head OS")),
        vec![FixtureSchemaSource::Generated(b"NAME=History Without ID\n".to_vec())],
    );
    let history = fixture.histories[0].id;
    let prepared = fixture.prepare().unwrap();
    let fallback = prepared.schemas.schema_for_state(history).unwrap();

    assert_eq!(
        fallback.source(),
        ActiveReblitBootSchemaSourceBinding::GlobalFallback {
            failed_local: ActiveReblitBootSchemaFallbackReason::Semantic(
                ActiveReblitBootSchemaSemanticReason::MissingIdentity,
            ),
            global_state: fixture.head.id,
        }
    );
}

#[test]
fn malformed_historical_os_info_falls_back_to_the_same_global_schema() {
    let fixture = Fixture::new(
        FixtureSchemaSource::OsInfo(valid_os_info("head-os", "Head OS", &[])),
        vec![FixtureSchemaSource::OsInfo(br#"{"metadata":{}}"#.to_vec())],
    );
    let history = fixture.histories[0].id;
    let prepared = fixture.prepare().unwrap();

    assert_eq!(
        prepared.schemas.schema_for_state(history).unwrap().source(),
        ActiveReblitBootSchemaSourceBinding::GlobalFallback {
            failed_local: ActiveReblitBootSchemaFallbackReason::Semantic(
                ActiveReblitBootSchemaSemanticReason::InvalidDocument,
            ),
            global_state: fixture.head.id,
        }
    );
}

#[test]
fn required_head_never_downgrades_structural_or_semantic_failure() {
    for source in [
        FixtureSchemaSource::MissingGenerated,
        FixtureSchemaSource::Generated(b"NAME=Head Without ID\n".to_vec()),
        FixtureSchemaSource::OsInfo(b"not json".to_vec()),
    ] {
        let fixture = Fixture::new(source, Vec::new());
        assert!(matches!(
            fixture.prepare(),
            Err(ActiveReblitBootSchemaInputsError::RequiredSchemaUnavailable { state, .. })
                if state == i32::from(fixture.head.id)
        ));
    }
}

#[test]
fn sticky_fallback_is_not_promoted_when_metadata_appears_later() {
    let fixture = Fixture::new(
        FixtureSchemaSource::Generated(valid_os_release("head-os", "Head OS")),
        vec![FixtureSchemaSource::MissingGenerated],
    );
    let history = fixture.histories[0].id;
    let prepared = fixture.prepare().unwrap();
    let selected = prepared.schemas.schema_for_state(history).unwrap().source();

    fixture.write_generated(history, &valid_os_release("late-history", "Late History"));
    prepared.revalidate(&fixture).unwrap();
    assert_eq!(prepared.schemas.schema_for_state(history).unwrap().source(), selected);
    assert!(matches!(
        selected,
        ActiveReblitBootSchemaSourceBinding::GlobalFallback { .. }
    ));
}

#[test]
fn state_root_exclusion_prevents_a_projected_history_schema_from_rendering() {
    let fixture = Fixture::new(
        FixtureSchemaSource::Generated(valid_os_release("head-os", "Head OS")),
        vec![FixtureSchemaSource::Generated(valid_os_release("history", "History"))],
    );
    let history = fixture.histories[0].id;
    fixture.exclude_history(history);
    let prepared = fixture.prepare().unwrap();

    assert!(
        prepared
            .stone
            .schema_requirements()
            .iter()
            .any(|entry| entry.state_id() == history)
    );
    assert!(prepared.schemas.schema_for_state(history).is_none());
    assert_eq!(prepared.schemas.schemas().len(), 1);
}
