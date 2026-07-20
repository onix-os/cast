fn assert_packaging_permit_rejected_without_mutation(
    packager: &FrozenPackager,
    paths: &Paths,
    permit: &FrozenPackagingPermit<'_>,
) {
    let artifact_sentinel = paths.artefacts().host.join("must-survive");
    fs::write(&artifact_sentinel, b"artifact sentinel").unwrap();
    let install = paths.install().host;
    fs::create_dir_all(&install).unwrap();
    let install_sentinel = install.join("must-survive");
    fs::write(&install_sentinel, b"install sentinel").unwrap();
    let mut timing = Timing::default();

    let error = packager.package(permit, &mut timing).unwrap_err();

    assert!(matches!(error, Error::InvalidPackagingPermit(_)));
    assert_eq!(fs::read(&artifact_sentinel).unwrap(), b"artifact sentinel");
    assert_eq!(fs::read(&install_sentinel).unwrap(), b"install sentinel");
    assert_eq!(
        fs::read_dir(&paths.artefacts().host)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>(),
        [OsString::from("must-survive")]
    );
}

#[test]
fn frozen_packager_rejects_same_derivation_permit_from_another_workspace_before_mutation() {
    let (_root, plan, paths) = publication_fixture();
    let (_other_root, other_plan, other_paths) = publication_fixture();
    assert_eq!(plan.derivation_id(), other_plan.derivation_id());
    let packager = FrozenPackager::from_plan(&paths, &plan).unwrap();
    let wrong_lock = other_paths.acquire_execution_lock(&other_plan).unwrap();
    let wrong_permit = other_paths
        .issue_frozen_packaging_permit(&wrong_lock, &other_plan)
        .unwrap();

    assert_packaging_permit_rejected_without_mutation(&packager, &paths, &wrong_permit);
}

#[test]
fn frozen_packager_rejects_other_derivation_permit_in_same_workspace_before_mutation() {
    let (root, plan, paths) = publication_fixture();
    let mut other_plan = plan.clone();
    other_plan.source_date_epoch += 1;
    other_plan.validate().unwrap();
    assert_ne!(plan.derivation_id(), other_plan.derivation_id());
    let recipe =
        Recipe::load(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples/gluon/stone.glu")).unwrap();
    let other_output = root.path().join("other-output");
    fs::create_dir(&other_output).unwrap();
    let mut other_paths = Paths::new(&recipe, other_plan.layout.clone(), root.path(), other_output).unwrap();
    other_paths.bind_to_plan(&other_plan).unwrap();
    let wrong_lock = other_paths.acquire_execution_lock(&other_plan).unwrap();
    let wrong_permit = other_paths
        .issue_frozen_packaging_permit(&wrong_lock, &other_plan)
        .unwrap();
    let packager = FrozenPackager::from_plan(&paths, &plan).unwrap();

    assert_packaging_permit_rejected_without_mutation(&packager, &paths, &wrong_permit);
}

#[test]
fn frozen_packager_rejects_unbound_runtime_paths_during_construction() {
    let (_bound_root, plan, _bound_paths) = publication_fixture();
    let root = crate::private_tempdir();
    let output = root.path().join("output");
    fs::create_dir(&output).unwrap();
    let recipe =
        Recipe::load(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples/gluon/stone.glu")).unwrap();
    let unbound_paths = Paths::new(&recipe, plan.layout.clone(), root.path(), output).unwrap();

    assert!(matches!(
        FrozenPackager::from_plan(&unbound_paths, &plan),
        Err(Error::InvalidFrozenPaths(_))
    ));
}
