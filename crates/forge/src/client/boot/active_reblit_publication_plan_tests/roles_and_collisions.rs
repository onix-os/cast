#[test]
fn canonical_order_is_typed_phase_then_root_then_precomputed_folded_path() {
    let entry_bytes = b"entry";
    let loader_bytes = b"default @saved\n";
    let plan = prepare_alias([
        systemd_bootloader(9, 90, 50),
        addressed_payload("Aeryn", "zeta.initrd", 2, 20, 20),
        loader_control(loader_bytes),
        addressed_payload("Aeryn", "vmlinuz", 1, 10, 10),
        entry("loader/entries/10-6.1.conf", entry_bytes),
        fallback_bootloader(9, 90, 50),
    ])
    .unwrap();

    let order = plan
        .outputs()
        .iter()
        .map(|output| {
            (
                output.role(),
                output.phase(),
                output.root(),
                output.relative_path().to_owned(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        order,
        [
            (
                ActiveReblitBootPublicationRole::Payload,
                ActiveReblitBootPublicationPhase::Payload,
                ActiveReblitBootDestinationRoot::Boot,
                checksum_payload_path("Aeryn", "vmlinuz", 10, 10),
            ),
            (
                ActiveReblitBootPublicationRole::Payload,
                ActiveReblitBootPublicationPhase::Payload,
                ActiveReblitBootDestinationRoot::Boot,
                checksum_payload_path("Aeryn", "zeta.initrd", 20, 20),
            ),
            (
                ActiveReblitBootPublicationRole::Entry,
                ActiveReblitBootPublicationPhase::Entry,
                ActiveReblitBootDestinationRoot::Boot,
                PathBuf::from("loader/entries/10-6.1.conf"),
            ),
            (
                ActiveReblitBootPublicationRole::LoaderControl,
                ActiveReblitBootPublicationPhase::LoaderControl,
                ActiveReblitBootDestinationRoot::Boot,
                PathBuf::from(ACTIVE_REBLIT_LOADER_CONTROL_PATH),
            ),
            (
                ActiveReblitBootPublicationRole::FallbackBootloader,
                ActiveReblitBootPublicationPhase::Bootloader,
                ActiveReblitBootDestinationRoot::Esp,
                PathBuf::from(ACTIVE_REBLIT_FALLBACK_BOOTLOADER_PATH),
            ),
            (
                ActiveReblitBootPublicationRole::SystemdBootloader,
                ActiveReblitBootPublicationPhase::Bootloader,
                ActiveReblitBootDestinationRoot::Esp,
                PathBuf::from(ACTIVE_REBLIT_SYSTEMD_BOOTLOADER_PATH),
            ),
        ]
    );
    assert!(plan.outputs().iter().all(|output| output.mode() == 0o644));
    assert_eq!(
        plan.logical_bytes(),
        10 + 20 + 50 + 50 + entry_bytes.len() as u64 + loader_bytes.len() as u64
    );
    assert_eq!(plan.generated_bytes(), entry_bytes.len() + loader_bytes.len());
    assert_eq!(
        plan.path_bytes(),
        plan.outputs()
            .iter()
            .map(|output| output.relative_path().as_os_str().as_bytes().len())
            .sum::<usize>()
    );
}

#[test]
fn both_bootloader_roles_bind_exact_esp_paths_and_carry_one_binding_coordinate() {
    let plan = prepare_alias([
        fallback_bootloader(7, 0xfeed, 42),
        systemd_bootloader(7, 0xfeed, 42),
    ])
    .unwrap();

    assert_eq!(plan.outputs().len(), 2);
    assert_eq!(plan.logical_bytes(), 84);
    assert_eq!(
        plan.outputs()
            .iter()
            .map(|output| output.relative_path())
            .collect::<Vec<_>>(),
        [
            Path::new(ACTIVE_REBLIT_FALLBACK_BOOTLOADER_PATH),
            Path::new(ACTIVE_REBLIT_SYSTEMD_BOOTLOADER_PATH),
        ]
    );
    for output in plan.outputs() {
        assert_eq!(output.root(), ActiveReblitBootDestinationRoot::Esp);
        assert_eq!(output.phase(), ActiveReblitBootPublicationPhase::Bootloader);
        assert_eq!(output.source().binding_index(), Some(7));
        assert_eq!(output.source().digest(), 0xfeed);
        assert_eq!(output.source().length(), 42);
        assert_eq!(
            output.source().content_identity(),
            fixture_content_identity(0xfeed, 42)
        );
    }
}

#[test]
fn generated_checksum_and_sha256_are_derived_from_owned_bytes() {
    let bytes = b"title AerynOS\noptions test.generated=1\n";
    let plan = prepare_alias([entry("loader/entries/a.conf", bytes)]).unwrap();
    let source = plan.outputs()[0].source();
    assert_eq!(source.generated_bytes(), Some(bytes.as_slice()));
    assert_eq!(source.binding_index(), None);
    assert_eq!(source.digest(), xxhash_rust::xxh3::xxh3_128(bytes));
    assert_eq!(source.content_identity(), BootContentIdentity::hash(bytes));
    assert_eq!(
        hex::encode(source.content_identity().as_bytes()),
        "59a5d781527833a3531f15528f39a5317e4a1916d69258d6b48e3a1e5940f1ed"
    );
}

#[test]
fn raw_role_root_phase_source_and_path_mismatches_are_rejected() {
    let path = checksum_payload_path("Aeryn", "vmlinuz", 1, 1);
    let root = ActiveReblitBootPublicationRequest::raw(
        ActiveReblitBootPublicationRole::Payload,
        ActiveReblitBootDestinationRoot::Esp,
        ActiveReblitBootPublicationPhase::Payload,
        path.clone(),
        sealed_source(0, 1, 1),
    );
    assert!(matches!(
        prepare_alias([root]),
        Err(ActiveReblitBootPublicationPlanError::RoleRootMismatch { .. })
    ));

    let phase = ActiveReblitBootPublicationRequest::raw(
        ActiveReblitBootPublicationRole::Payload,
        ActiveReblitBootDestinationRoot::Boot,
        ActiveReblitBootPublicationPhase::Entry,
        path.clone(),
        sealed_source(0, 1, 1),
    );
    assert!(matches!(
        prepare_alias([phase]),
        Err(ActiveReblitBootPublicationPlanError::RolePhaseMismatch { .. })
    ));

    let bytes = b"not sealed";
    let source = ActiveReblitBootPublicationRequest::raw(
        ActiveReblitBootPublicationRole::Payload,
        ActiveReblitBootDestinationRoot::Boot,
        ActiveReblitBootPublicationPhase::Payload,
        path,
        generated_source(bytes),
    );
    assert!(matches!(
        prepare_alias([source]),
        Err(ActiveReblitBootPublicationPlanError::RoleSourceMismatch { .. })
    ));

    let wrong_path = ActiveReblitBootPublicationRequest::raw(
        ActiveReblitBootPublicationRole::Entry,
        ActiveReblitBootDestinationRoot::Boot,
        ActiveReblitBootPublicationPhase::Entry,
        checksum_payload_path("Aeryn", "vmlinuz", 1, 1),
        generated_source(b"entry"),
    );
    assert!(matches!(
        prepare_alias([wrong_path]),
        Err(ActiveReblitBootPublicationPlanError::RolePathMismatch { .. })
    ));
}

#[test]
fn only_identical_typed_requests_including_binding_are_deduplicated() {
    let plan = prepare_alias([
        addressed_payload("Aeryn", "vmlinuz", 3, 7, 11),
        addressed_payload("Aeryn", "vmlinuz", 3, 7, 11),
        entry("loader/entries/a.conf", b"same"),
        entry("loader/entries/a.conf", b"same"),
    ])
    .unwrap();
    assert_eq!(plan.outputs().len(), 2);
    assert_eq!(plan.logical_bytes(), 15);
    assert_eq!(plan.generated_bytes(), 4);

    let different_binding = prepare_alias([
        addressed_payload("Aeryn", "vmlinuz", 3, 7, 11),
        addressed_payload("Aeryn", "vmlinuz", 4, 7, 11),
    ]);
    assert!(matches!(
        different_binding,
        Err(ActiveReblitBootPublicationPlanError::PublicationCollision { .. })
    ));

    let path = checksum_payload_path("Aeryn", "vmlinuz", 7, 11);
    let different_sha256 = prepare_alias([
        payload_with_content_identity(
            path.clone(),
            3,
            7,
            11,
            BootContentIdentity::hash(b"first-bytes"),
        ),
        payload_with_content_identity(
            path,
            3,
            7,
            11,
            BootContentIdentity::hash(b"other-bytes"),
        ),
    ]);
    assert!(matches!(
        different_sha256,
        Err(ActiveReblitBootPublicationPlanError::PublicationCollision { .. })
    ));
}

#[test]
fn same_destination_with_different_content_or_invalid_role_is_rejected() {
    let different_content = prepare_alias([
        entry("loader/entries/a.conf", b"first"),
        entry("loader/entries/a.conf", b"second"),
    ]);
    assert!(matches!(
        different_content,
        Err(ActiveReblitBootPublicationPlanError::PublicationCollision { .. })
    ));

    let role_conflict = ActiveReblitBootPublicationRequest::raw(
        ActiveReblitBootPublicationRole::LoaderControl,
        ActiveReblitBootDestinationRoot::Boot,
        ActiveReblitBootPublicationPhase::LoaderControl,
        PathBuf::from("loader/entries/a.conf"),
        generated_source(b"entry"),
    );
    assert!(matches!(
        prepare_alias([entry("loader/entries/a.conf", b"entry"), role_conflict,]),
        Err(ActiveReblitBootPublicationPlanError::RolePathMismatch { .. })
    ));

    let case_alias = prepare_alias([
        addressed_payload("Aeryn", "Shared.initrd", 3, 7, 11),
        addressed_payload("aeryn", "shared.initrd", 3, 7, 11),
    ]);
    assert!(matches!(
        case_alias,
        Err(ActiveReblitBootPublicationPlanError::CaseInsensitiveCollision { .. })
    ));
}

#[test]
fn raw_cross_root_request_is_rejected_before_collision_planning() {
    let esp_alias = ActiveReblitBootPublicationRequest::raw(
        ActiveReblitBootPublicationRole::Payload,
        ActiveReblitBootDestinationRoot::Esp,
        ActiveReblitBootPublicationPhase::Payload,
        checksum_payload_path("aeryn", "VMLINUZ", 2, 2),
        sealed_source(2, 2, 2),
    );
    let error = prepare_alias([addressed_payload("Aeryn", "vmlinuz", 1, 1, 1), esp_alias]).unwrap_err();
    assert!(matches!(
        error,
        ActiveReblitBootPublicationPlanError::RoleRootMismatch { .. }
    ));
}

#[test]
fn case_folded_hierarchy_helpers_detect_ancestors_and_descendants_in_both_orders() {
    let ancestor = case_folded_path(Path::new("EFI/Boot/BOOTX64.EFI"));
    let descendant = case_folded_path(Path::new("efi/boot/bootx64.efi/payload"));
    let mut policy = PublicationPlanPolicy::production();
    policy.max_work = 100;
    let mut budget = PublicationPlanBudget::new_until(policy, Instant::now() + Duration::from_secs(1)).unwrap();

    let ancestors = BTreeMap::from([(ancestor.clone(), 7)]);
    assert_eq!(
        existing_ancestor_index(&ancestors, &descendant, &mut budget).unwrap(),
        Some(7)
    );

    let descendants = BTreeMap::from([(descendant, 9)]);
    assert_eq!(
        existing_descendant_index(&descendants, &ancestor, &mut budget).unwrap(),
        Some(9)
    );
}

#[test]
fn alias_and_distinct_layouts_route_roots_to_expected_collision_domains() {
    let alias_topology = alias_topology();
    let alias = ActiveReblitBootDestinationCollisionDomains::from_topology(alias_topology.bound());
    assert_eq!(
        alias.for_root(ActiveReblitBootDestinationRoot::Esp),
        alias.for_root(ActiveReblitBootDestinationRoot::Boot)
    );

    let distinct_topology = distinct_topology();
    let distinct = ActiveReblitBootDestinationCollisionDomains::from_topology(distinct_topology.bound());
    assert_ne!(
        distinct.for_root(ActiveReblitBootDestinationRoot::Esp),
        distinct.for_root(ActiveReblitBootDestinationRoot::Boot)
    );
}

#[test]
fn plan_retains_the_topology_collision_layout_for_later_revalidation() {
    let plan = prepare_distinct([
        fallback_bootloader(0, 1, 1),
        addressed_payload("Boot", "vmlinuz", 1, 2, 2),
    ])
    .unwrap();
    let distinct = distinct_topology();
    let alternate_distinct = alternate_distinct_topology();
    let alias = alias_topology();
    assert_eq!(plan.destination_layout(), ActiveReblitBootDestinationLayout::DistinctXbootldr);
    assert!(plan.collision_domains_match(distinct.bound()));
    assert!(
        plan.collision_domains_match(alternate_distinct.bound()),
        "layout matching deliberately proves only alias versus distinct, never destination identity"
    );
    assert!(!plan.collision_domains_match(alias.bound()));

    let alias_plan = prepare_alias([fallback_bootloader(0, 1, 1)]).unwrap();
    assert_eq!(alias_plan.destination_layout(), ActiveReblitBootDestinationLayout::BootAliasesEsp);
    assert!(alias_plan.collision_domains_match(alias.bound()));
    assert!(!alias_plan.collision_domains_match(distinct.bound()));
}
