#[test]
fn unsafe_relative_paths_are_rejected_instead_of_normalized() {
    let error_for =
        |path: PathBuf| prepare_alias([payload(path, 0, 1, 1)]).unwrap_err();

    assert!(matches!(
        error_for(PathBuf::new()),
        ActiveReblitBootPublicationPlanError::EmptyPath
    ));
    assert!(matches!(
        error_for(PathBuf::from("/EFI/Aeryn/6.1/vmlinuz")),
        ActiveReblitBootPublicationPlanError::AbsolutePath { .. }
    ));
    assert!(matches!(
        error_for(PathBuf::from("EFI//Aeryn/6.1/vmlinuz")),
        ActiveReblitBootPublicationPlanError::EmptyPathComponent { .. }
    ));
    assert!(matches!(
        error_for(PathBuf::from("./EFI/Aeryn/6.1/vmlinuz")),
        ActiveReblitBootPublicationPlanError::DotPathComponent { .. }
    ));
    assert!(matches!(
        error_for(PathBuf::from("EFI/../Aeryn/6.1/vmlinuz")),
        ActiveReblitBootPublicationPlanError::ParentPathComponent { .. }
    ));
    assert!(matches!(
        error_for(PathBuf::from("EFI/Aeryn/6\n.1/vmlinuz")),
        ActiveReblitBootPublicationPlanError::ControlPathComponent { .. }
    ));
    assert!(matches!(
        error_for(PathBuf::from("EFI/Aeryn/6\0.1/vmlinuz")),
        ActiveReblitBootPublicationPlanError::NulPath { .. }
    ));
}

#[test]
fn non_utf8_and_non_ascii_paths_fail_closed() {
    let non_utf8 = PathBuf::from(OsString::from_vec(b"EFI/Aeryn/\xff/vmlinuz".to_vec()));
    assert!(matches!(
        prepare_alias([payload(non_utf8, 0, 1, 1)]),
        Err(ActiveReblitBootPublicationPlanError::NonUtf8Path { .. })
    ));
    assert!(matches!(
        prepare_alias([payload("EFI/Aeryn/Ä/vmlinuz", 0, 1, 1)]),
        Err(ActiveReblitBootPublicationPlanError::NonAsciiPathComponent { .. })
    ));
}

#[test]
fn fat_forbidden_trailing_reserved_and_short_name_components_are_rejected() {
    for character in ['<', '>', ':', '"', '\\', '|', '?', '*'] {
        let path = format!("EFI/Aeryn/6{character}1/vmlinuz");
        assert!(matches!(
            prepare_alias([payload(path, 0, 1, 1)]),
            Err(ActiveReblitBootPublicationPlanError::FatForbiddenCharacter { .. })
        ));
    }
    for component in ["version.", "version "] {
        let path = format!("EFI/Aeryn/{component}/vmlinuz");
        assert!(matches!(
            prepare_alias([payload(path, 0, 1, 1)]),
            Err(ActiveReblitBootPublicationPlanError::FatTrailingDotOrSpace { .. })
        ));
    }
    for component in ["CON", "CON .txt", "prn.txt", "AUX", "nul.bin", "COM1", "lpt9.log"] {
        let path = format!("EFI/Aeryn/{component}/vmlinuz");
        assert!(matches!(
            prepare_alias([payload(path, 0, 1, 1)]),
            Err(ActiveReblitBootPublicationPlanError::FatReservedName { .. })
        ));
    }
    assert!(matches!(
        prepare_alias([payload("EFI/Aeryn/kernel~1/vmlinuz", 0, 1, 1)]),
        Err(ActiveReblitBootPublicationPlanError::FatShortNameMarker { .. })
    ));
}

#[test]
fn fat_component_byte_bound_admits_n_and_rejects_n_plus_one() {
    let at_limit = "a".repeat(MAX_ACTIVE_REBLIT_BOOT_FAT_COMPONENT_BYTES);
    prepare_alias([payload(format!("EFI/{at_limit}/6.1/vmlinuz"), 0, 1, 1)])
        .unwrap();

    let above_limit = "a".repeat(MAX_ACTIVE_REBLIT_BOOT_FAT_COMPONENT_BYTES + 1);
    let error =
        prepare_alias([payload(format!("EFI/{above_limit}/6.1/vmlinuz"), 0, 1, 1)])
            .unwrap_err();
    assert!(matches!(
        error,
        ActiveReblitBootPublicationPlanError::FatComponentByteLimit {
            limit: MAX_ACTIVE_REBLIT_BOOT_FAT_COMPONENT_BYTES,
            actual,
            ..
        } if actual == MAX_ACTIVE_REBLIT_BOOT_FAT_COMPONENT_BYTES + 1
    ));
}

#[test]
fn role_specific_payload_and_entry_path_shapes_are_enforced() {
    for path in ["EFI/Aeryn/vmlinuz", "EFI/Aeryn/6.1/kernel", "efi/Aeryn/6.1/vmlinuz"] {
        assert!(matches!(
            prepare_alias([payload(path, 0, 1, 1)]),
            Err(ActiveReblitBootPublicationPlanError::RolePathMismatch { .. })
        ));
    }
    for path in ["loader/a.conf", "loader/entries/.conf", "loader/entries/a.txt"] {
        assert!(matches!(
            prepare_alias([entry(path, b"entry")]),
            Err(ActiveReblitBootPublicationPlanError::RolePathMismatch { .. })
        ));
    }
}
