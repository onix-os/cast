#[test]
#[ignore = "explicit network preparation for the offline bootstrap package store"]
fn fetch_pinned_bootstrap_package_files() {
    let (closure, indexed) = validated_bootstrap();
    let store = package_store();
    std::fs::create_dir_all(&store).unwrap();

    for (position, hash) in closure.packages.sha256.iter().enumerate() {
        let meta = indexed
            .get(hash)
            .unwrap_or_else(|| panic!("bootstrap package {hash} is absent from the pinned index"));
        let size = meta.download_size.expect("bootstrap package has no declared size");
        let destination = store.join(format!("{hash}.stone"));
        if package_file_matches(&destination, hash, size) {
            eprintln!(
                "bootstrap package {}/{} is already verified",
                position + 1,
                closure.packages.sha256.len()
            );
            continue;
        }
        let url = package_url(
            &closure.repository,
            meta.uri.as_deref().expect("bootstrap package has no URI"),
        );
        eprintln!(
            "fetching bootstrap package {}/{}: {} ({size} bytes)",
            position + 1,
            closure.packages.sha256.len(),
            meta.name
        );
        forge::runtime::block_on(forge::request::download_with_progress_and_expected_sha256_and_limits(
            url,
            &destination,
            hash,
            forge::request::DownloadLimits::new(size, Duration::from_secs(5 * 60)),
            |_| {},
        ))
        .unwrap_or_else(|error| panic!("fetch bootstrap package {}: {error:#}", meta.name));
        assert!(
            package_file_matches(&destination, hash, size),
            "downloaded bootstrap package {} did not survive exact re-verification",
            meta.name
        );
    }
}

#[test]
fn pinned_bootstrap_manifest_is_bounded_and_index_authoritative() {
    let _ = validated_bootstrap();
}
