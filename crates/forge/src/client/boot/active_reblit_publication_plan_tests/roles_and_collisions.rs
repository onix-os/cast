#[test]
fn canonical_order_is_typed_phase_then_root_then_precomputed_folded_path() {
    let entry_bytes = b"entry";
    let loader_bytes = b"default @saved\n";
    let plan = prepare_alias([
        systemd_bootloader(9, 90, 50),
        payload("EFI/Aeryn/6.2/zeta.initrd", 2, 20, 20),
        loader_control(loader_bytes),
        payload("EFI/Aeryn/6.1/vmlinuz", 1, 10, 10),
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
                PathBuf::from("EFI/Aeryn/6.1/vmlinuz"),
            ),
            (
                ActiveReblitBootPublicationRole::Payload,
                ActiveReblitBootPublicationPhase::Payload,
                ActiveReblitBootDestinationRoot::Boot,
                PathBuf::from("EFI/Aeryn/6.2/zeta.initrd"),
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
    }
}

#[test]
fn generated_digest_is_derived_from_owned_bytes() {
    let bytes = b"title AerynOS\nlinux /EFI/Aeryn/6.1/vmlinuz\n";
    let plan = prepare_alias([entry("loader/entries/a.conf", bytes)]).unwrap();
    let source = plan.outputs()[0].source();
    assert_eq!(source.generated_bytes(), Some(bytes.as_slice()));
    assert_eq!(source.binding_index(), None);
    assert_eq!(source.digest(), xxhash_rust::xxh3::xxh3_128(bytes));

}

#[test]
fn raw_role_root_phase_source_and_path_mismatches_are_rejected() {
    let path = PathBuf::from("EFI/Aeryn/6.1/vmlinuz");
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
        PathBuf::from("EFI/Aeryn/6.1/vmlinuz"),
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
        payload("EFI/Aeryn/6.1/vmlinuz", 3, 7, 11),
        payload("EFI/Aeryn/6.1/vmlinuz", 3, 7, 11),
        entry("loader/entries/a.conf", b"same"),
        entry("loader/entries/a.conf", b"same"),
    ])
    .unwrap();
    assert_eq!(plan.outputs().len(), 2);
    assert_eq!(plan.logical_bytes(), 15);
    assert_eq!(plan.generated_bytes(), 4);

    let different_binding = prepare_alias([
        payload("EFI/Aeryn/6.1/vmlinuz", 3, 7, 11),
        payload("EFI/Aeryn/6.1/vmlinuz", 4, 7, 11),
    ]);
    assert!(matches!(
        different_binding,
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
}

#[test]
fn raw_cross_root_request_is_rejected_before_collision_planning() {
    let esp_alias = ActiveReblitBootPublicationRequest::raw(
        ActiveReblitBootPublicationRole::Payload,
        ActiveReblitBootDestinationRoot::Esp,
        ActiveReblitBootPublicationPhase::Payload,
        PathBuf::from("efi/aeryn/6.1/VMLINUZ"),
        sealed_source(2, 2, 2),
    );
    let error =
        prepare_alias([payload("EFI/Aeryn/6.1/vmlinuz", 1, 1, 1), esp_alias])
            .unwrap_err();
    assert!(matches!(
        error,
        ActiveReblitBootPublicationPlanError::RoleRootMismatch { .. }
    ));
}

#[test]
fn aliased_topology_rejects_cross_root_file_directory_hierarchy_in_both_orders() {
    let descendant = "EFI/Boot/bootx64.efi/vmlinuz";
    for requests in [
        [fallback_bootloader(0, 1, 1), payload(descendant, 1, 2, 2)],
        [payload(descendant, 1, 2, 2), fallback_bootloader(0, 1, 1)],
    ] {
        let error = prepare_alias(requests).unwrap_err();
        assert!(matches!(
            error,
            ActiveReblitBootPublicationPlanError::PublicationHierarchyCollision {
                ancestor_root: ActiveReblitBootDestinationRoot::Esp,
                descendant_root: ActiveReblitBootDestinationRoot::Boot,
                ref ancestor,
                ref descendant,
            } if ancestor == Path::new("EFI/Boot/BOOTX64.EFI")
                && descendant == Path::new("EFI/Boot/bootx64.efi/vmlinuz")
        ));
    }
}

#[test]
fn distinct_topology_keeps_esp_and_xbootldr_collision_domains_separate() {
    let descendant = "EFI/Boot/bootx64.efi/vmlinuz";
    for requests in [
        [fallback_bootloader(0, 1, 1), payload(descendant, 1, 2, 2)],
        [payload(descendant, 1, 2, 2), fallback_bootloader(0, 1, 1)],
    ] {
        let plan = prepare_distinct(requests).expect("distinct destinations must not falsely collide");
        assert_eq!(plan.outputs().len(), 2);
        assert_eq!(
            plan.outputs()
                .iter()
                .map(PlannedActiveReblitBootPublication::root)
                .collect::<Vec<_>>(),
            [
                ActiveReblitBootDestinationRoot::Boot,
                ActiveReblitBootDestinationRoot::Esp,
            ]
        );
    }
}

#[test]
fn plan_retains_the_topology_collision_layout_for_later_revalidation() {
    let plan = prepare_distinct([
        fallback_bootloader(0, 1, 1),
        payload("EFI/Boot/bootx64.efi/vmlinuz", 1, 2, 2),
    ])
    .unwrap();
    let distinct = distinct_topology();
    let alternate_distinct = alternate_distinct_topology();
    let alias = alias_topology();
    assert!(plan.collision_domains_match(distinct.bound()));
    assert!(
        plan.collision_domains_match(alternate_distinct.bound()),
        "layout matching deliberately proves only alias versus distinct, never destination identity"
    );
    assert!(!plan.collision_domains_match(alias.bound()));

    let alias_plan = prepare_alias([fallback_bootloader(0, 1, 1)]).unwrap();
    assert!(alias_plan.collision_domains_match(alias.bound()));
    assert!(!alias_plan.collision_domains_match(distinct.bound()));
}
