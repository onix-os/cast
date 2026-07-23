fn validate_hooks_patch_source_contract(sources: &[UpstreamSpec], lock: &SourceLock) -> Result<(), String> {
    lock.validate_against(sources)
        .map_err(|error| format!("source lock does not match the authored source order: {error}"))?;
    let [
        UpstreamSpec::Archive {
            url: archive_url,
            rename: archive_rename,
            strip_dirs: archive_strip_dirs,
            unpack: archive_unpack,
            unpack_dir: archive_unpack_dir,
            ..
        },
        UpstreamSpec::Archive {
            url: patch_url,
            rename: patch_rename,
            strip_dirs: patch_strip_dirs,
            unpack: patch_unpack,
            unpack_dir: patch_unpack_dir,
            ..
        },
    ] = sources
    else {
        return Err("hooks-patch must declare exactly two archive-kind locked sources".to_owned());
    };
    let [SourceResolution::Archive(archive_lock), SourceResolution::Archive(patch_lock)] = lock.sources.as_slice()
    else {
        return Err("hooks-patch lock must contain exactly two archive resolutions".to_owned());
    };

    if archive_url != HOOKS_ARCHIVE_URL
        || archive_rename.as_deref() != Some("cast-hooks-fixture.tar.xz")
        || *archive_strip_dirs != Some(1)
        || !*archive_unpack
        || archive_unpack_dir.as_deref() != Some("cast-hooks-fixture")
    {
        return Err("hooks-patch primary source must remain the sole extracted XZ USTAR archive".to_owned());
    }
    if patch_url != HOOKS_PATCH_URL
        || patch_rename.as_deref() != Some(HOOKS_PATCH_MATERIALIZATION)
        || patch_strip_dirs.is_some()
        || *patch_unpack
        || patch_unpack_dir.is_some()
    {
        return Err("hooks-patch patch source must remain a non-unpacked raw source".to_owned());
    }
    if archive_lock.order != 0
        || archive_lock.url.as_str() != archive_url.as_str()
        || patch_lock.order != 1
        || patch_lock.url.as_str() != patch_url.as_str()
    {
        return Err("hooks-patch source lock order must bind the archive before the raw patch".to_owned());
    }
    Ok(())
}

#[test]
fn hooks_patch_external_source_contract_fails_closed() {
    let package = execution_fixture_package_directory("hooks-patch");
    let recipe = crate::Recipe::load_authored(&package.join("stone.glu")).unwrap();
    let lock_bytes = fs::read(package.join(SOURCE_LOCK_FILE_NAME)).unwrap();
    let lock = evaluate_source_lock(SOURCE_LOCK_FILE_NAME, &lock_bytes).unwrap();
    validate_hooks_patch_source_contract(&recipe.declaration.sources, &lock).unwrap();

    let mut missing = lock.clone();
    missing.sources.pop();
    assert!(
        validate_hooks_patch_source_contract(&recipe.declaration.sources, &missing).is_err(),
        "missing raw patch lock entry must fail closed"
    );

    let mut reordered = lock.clone();
    reordered.sources.swap(0, 1);
    assert!(
        validate_hooks_patch_source_contract(&recipe.declaration.sources, &reordered).is_err(),
        "reordered raw patch lock entry must fail closed"
    );

    let mut wrong_unpack = recipe.declaration.sources.clone();
    let UpstreamSpec::Archive { unpack, .. } = &mut wrong_unpack[1] else {
        panic!("hooks-patch second source stopped being archive-kind");
    };
    *unpack = true;
    assert!(
        validate_hooks_patch_source_contract(&wrong_unpack, &lock).is_err(),
        "an unpacked raw patch declaration must fail closed"
    );

    let SourceResolution::Archive(patch) = &lock.sources[1] else {
        panic!("hooks-patch raw patch lock stopped being archive-kind");
    };
    let materialization_name = recipe.declaration.sources[1].materialization_name().unwrap();
    let locked_patch = stone_recipe::derivation::LockedSource::Archive {
        order: patch.order,
        url: patch.url.clone(),
        sha256: patch.sha256.clone(),
        filename: materialization_name,
    };
    let temporary = crate::private_tempdir();
    let missing_cache = temporary.path().join("missing-cache");
    assert!(
        crate::upstream::import_locked_archive_fixture(
            &locked_patch,
            &missing_cache,
            &temporary.path().join("missing.patch"),
        )
        .is_err(),
        "missing raw patch bytes must fail closed"
    );
    assert!(
        !execution_source_cache_path(&missing_cache, &patch.url, &patch.sha256).exists(),
        "missing raw patch must not publish a cache entry"
    );

    let tampered = temporary.path().join("tampered.patch");
    fs::write(&tampered, b"tampered patch bytes\n").unwrap();
    let tampered_cache = temporary.path().join("tampered-cache");
    assert!(
        crate::upstream::import_locked_archive_fixture(&locked_patch, &tampered_cache, &tampered).is_err(),
        "tampered raw patch bytes must fail closed"
    );
    assert!(
        !execution_source_cache_path(&tampered_cache, &patch.url, &patch.sha256).exists(),
        "tampered raw patch must not publish a cache entry"
    );
}
