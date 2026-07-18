use super::*;

#[test]
fn head_os_info_is_bound_to_its_exact_stone_coordinate() {
    let bytes = valid_os_info("aerynos", "AerynOS", &[]);
    let fixture = Fixture::new(FixtureSchemaSource::OsInfo(bytes.clone()), Vec::new());
    let prepared = fixture.prepare().unwrap();
    let schema = prepared.schemas.schemas().first().unwrap();

    let ActiveReblitBootSchemaSourceBinding::StoneOsInfo {
        binding_index,
        digest,
        length,
    } = schema.source()
    else {
        panic!("head os-info must retain a Stone coordinate")
    };
    let asset = prepared.stone.asset_at(usize::from(binding_index)).unwrap();
    assert_eq!(asset.state_id(), fixture.head.id);
    assert!(matches!(asset.role(), BootAssetRole::OsInfo));
    assert_eq!(digest, xxhash_rust::xxh3::xxh3_128(&bytes));
    assert_eq!(digest, asset.digest());
    assert_eq!(length, bytes.len() as u64);
    assert_eq!(length, asset.length());
    assert_eq!(schema.schema().os_id(), "aerynos");
    assert_eq!(schema.schema().namespace(), "aerynos");
    assert_eq!(schema.schema().os_name(), "AerynOS");
    assert_eq!(schema.schema().display_name(), "AerynOS Stable");
    prepared.revalidate(&fixture).unwrap();
}

#[test]
fn generated_head_is_bound_beneath_the_revalidated_usr_descriptor() {
    let bytes = valid_os_release("aerynos", "AerynOS");
    let fixture = Fixture::new(FixtureSchemaSource::Generated(bytes.clone()), Vec::new());
    let prepared = fixture.prepare().unwrap();
    let schema = prepared.schemas.schemas().first().unwrap();

    assert_eq!(schema.state_id(), fixture.head.id);
    assert_eq!(
        schema.source(),
        ActiveReblitBootSchemaSourceBinding::GeneratedOsRelease {
            state_id: fixture.head.id,
            digest: xxhash_rust::xxh3::xxh3_128(&bytes),
            length: bytes.len() as u64,
        }
    );
    assert_eq!(schema.schema().os_id(), "aerynos");
    assert_eq!(prepared.schemas.total_source_bytes(), bytes.len());
    prepared.revalidate(&fixture).unwrap();
}

#[test]
fn os_info_preserves_bounded_unique_former_identities() {
    let bytes = valid_os_info(
        "current-os",
        "Current OS",
        &[("former-one", "Former One"), ("former-two", "Former Two")],
    );
    let fixture = Fixture::new(FixtureSchemaSource::OsInfo(bytes), Vec::new());
    let prepared = fixture.prepare().unwrap();
    let former = prepared.schemas.schemas()[0].schema().former_identities();

    assert_eq!(former.len(), 2);
    assert_eq!((former[0].id(), former[0].name()), ("former-one", "Former One"));
    assert_eq!((former[1].id(), former[1].name()), ("former-two", "Former Two"));
}

#[test]
fn schema_order_follows_the_stone_requirements_for_eligible_roots() {
    let fixture = Fixture::new(
        FixtureSchemaSource::Generated(valid_os_release("head-os", "Head OS")),
        vec![
            FixtureSchemaSource::Generated(valid_os_release("old-one", "Old One")),
            FixtureSchemaSource::OsInfo(valid_os_info("old-two", "Old Two", &[])),
        ],
    );
    let prepared = fixture.prepare().unwrap();
    let actual = prepared
        .schemas
        .schemas()
        .iter()
        .map(PreparedActiveReblitStateBootSchema::state_id)
        .collect::<Vec<_>>();
    let expected = prepared
        .stone
        .schema_requirements()
        .iter()
        .map(|requirement| requirement.state_id())
        .collect::<Vec<_>>();

    assert_eq!(actual, expected);
    assert_eq!(prepared.schemas.global_state(), fixture.head.id);
    prepared.revalidate(&fixture).unwrap();
}

#[test]
fn eligible_roots_from_a_different_projection_order_are_rejected() {
    let fixture = Fixture::new(
        FixtureSchemaSource::Generated(valid_os_release("head-os", "Head OS")),
        vec![
            FixtureSchemaSource::Generated(valid_os_release("old-one", "Old One")),
            FixtureSchemaSource::Generated(valid_os_release("old-two", "Old Two")),
        ],
    );
    let stone = ready_stone(
        PreparedActiveReblitStoneBootInputs::prepare(
            &fixture.installation,
            &fixture.state_db,
            &fixture.layout_db,
            &fixture.head,
        )
        .unwrap(),
    );
    let mut reordered = stone.state_ids().to_vec();
    assert_eq!(reordered.len(), 3);
    reordered.swap(1, 2);
    let roots = crate::transition_identity::PreparedActiveReblitBootStateRoots::prepare(
        &fixture.installation,
        &fixture.head_usr,
        fixture.head.id,
        &reordered,
    )
    .unwrap();
    let roots = roots.revalidate(&fixture.installation).unwrap();

    assert!(matches!(
        PreparedActiveReblitBootSchemas::prepare(&stone, &roots),
        Err(ActiveReblitBootSchemaInputsError::EligibleRootOrder { state })
            if state == i32::from(reordered[2])
    ));
}

#[test]
fn revalidation_rejects_foreign_stone_with_the_same_schema_requirements() {
    let bytes = valid_os_release("head-os", "Head OS");
    let fixture = Fixture::new(FixtureSchemaSource::Generated(bytes.clone()), Vec::new());
    let prepared = fixture.prepare().unwrap();
    let foreign = Fixture::new(
        FixtureSchemaSource::Generated(bytes),
        vec![FixtureSchemaSource::NoBootAssets],
    );
    let foreign_stone = ready_stone(
        PreparedActiveReblitStoneBootInputs::prepare(
            &foreign.installation,
            &foreign.state_db,
            &foreign.layout_db,
            &foreign.head,
        )
        .unwrap(),
    );
    assert_eq!(
        foreign_stone.schema_requirements(),
        prepared.stone.schema_requirements()
    );
    assert_ne!(foreign_stone.state_ids(), prepared.stone.state_ids());
    let roots = prepared.roots.revalidate(&fixture.installation).unwrap();

    assert!(matches!(
        prepared.schemas.revalidate_sources(&foreign_stone, &roots),
        Err(ActiveReblitBootSchemaInputsError::StateProjectionChanged)
    ));
}
