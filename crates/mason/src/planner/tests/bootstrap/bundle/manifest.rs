fn assert_manifests(
    fixture: &str,
    planned: &super::super::Planned,
    artefacts: &BTreeMap<String, Vec<u8>>,
    packages: &BTreeMap<String, PackageImage>,
) {
    let included = planned
        .plan
        .outputs
        .iter()
        .filter(|output| output.include_in_manifest)
        .map(|output| output.package_name.as_str())
        .collect::<BTreeSet<_>>();
    let expected_build_dependencies = planned
        .plan
        .manifest_build_inputs
        .iter()
        .map(|relation| relation.canonical_name())
        .collect::<BTreeSet<_>>();

    let binary_name = format!("manifest.{}.bin", planned.plan.package.architecture);
    let binary = &artefacts[&binary_name];
    let mut reader = stone::read_bytes_with_limits(binary, fixture_limits())
        .unwrap_or_else(|error| panic!("{fixture}: decode binary build manifest: {error}"));
    let StoneHeader::V1(container_header) = reader.header;
    assert_eq!(
        container_header.file_type,
        StoneHeaderV1FileType::BuildManifest,
        "{fixture}: binary build manifest has the wrong Stone file type"
    );
    let payloads = reader
        .payloads()
        .unwrap_or_else(|error| panic!("{fixture}: seek binary manifest payloads: {error}"))
        .collect::<Result<Vec<_>, _>>()
        .unwrap_or_else(|error| panic!("{fixture}: decode binary manifest payloads: {error}"));
    assert_eq!(payloads.len(), included.len());
    assert_eq!(usize::from(container_header.num_payloads), included.len());
    assert_canonical_payload_headers(fixture, "binary build manifest", &payloads);
    assert!(
        payloads
            .iter()
            .all(|payload| matches!(payload, StoneDecodedPayload::Meta(_))),
        "{fixture}: binary build manifest may contain only Meta payloads"
    );
    let mut manifested = BTreeSet::new();
    let mut manifest_order = Vec::new();
    for payload in &payloads {
        let records = &payload.meta().expect("manifest payload was checked").body;
        let decoded = Meta::from_stone_payload(records)
            .unwrap_or_else(|error| panic!("{fixture}: decode binary manifest package metadata: {error}"));
        let package = decoded.name.to_string();
        validate_meta_records(fixture, &package, records, true);
        assert_frozen_provenance(fixture, planned, &package, records);
        assert!(
            manifested.insert(package.clone()),
            "{fixture}: binary manifest repeats {package}"
        );
        manifest_order.push(package.clone());
        let image = packages
            .get(&package)
            .unwrap_or_else(|| panic!("{fixture}: binary manifest names unknown output {package}"));
        let mut expected = image.meta.clone();
        expected.build_release = 0;
        // Forge's legacy Meta view intentionally flattens both Depends and
        // BuildDepends primitives. Keep checking the raw tags separately
        // below, then model that lossy view for the whole-struct comparison.
        expected.dependencies.extend(
            planned
                .plan
                .manifest_build_inputs
                .iter()
                .map(|relation| relation.to_dependency()),
        );
        assert_eq!(
            decoded, expected,
            "{fixture}: binary manifest metadata drift for {package}"
        );
        assert_eq!(
            raw_dependencies(records, StonePayloadMetaTag::BuildDepends),
            expected_build_dependencies,
            "{fixture}: binary manifest build closure drift for {package}"
        );
    }
    assert_eq!(manifested.iter().map(String::as_str).collect::<BTreeSet<_>>(), included);
    assert_eq!(
        manifest_order,
        included.iter().map(|package| (*package).to_owned()).collect::<Vec<_>>(),
        "{fixture}: binary manifest package payload order is not canonical"
    );

    let json_name = format!("manifest.{}.jsonc", planned.plan.package.architecture);
    let jsonc = std::str::from_utf8(&artefacts[&json_name])
        .unwrap_or_else(|error| panic!("{fixture}: JSONC build report is not UTF-8: {error}"));
    let (comment, json) = jsonc
        .split_once('\n')
        .unwrap_or_else(|| panic!("{fixture}: JSONC report has no comment boundary"));
    assert_eq!(comment, "/** Human readable report. This is not consumed by Cast */");
    let report: serde_json::Value =
        serde_json::from_str(json).unwrap_or_else(|error| panic!("{fixture}: decode JSONC build report: {error}"));
    let report_object = report.as_object().expect("JSONC report must be an object");
    assert_eq!(
        report_object.keys().map(String::as_str).collect::<BTreeSet<_>>(),
        BTreeSet::from([
            "manifest-version",
            "packages",
            "derivation-id",
            "recipe-fingerprint",
            "source-name",
            "source-release",
            "source-version",
        ]),
        "{fixture}: JSONC report schema drift"
    );
    assert_eq!(report["manifest-version"], "0.2");
    assert_eq!(report["derivation-id"], planned.plan.derivation_id().as_str());
    assert_eq!(report["recipe-fingerprint"], planned.plan.provenance.recipe.sha256);
    assert_eq!(report["source-name"], planned.plan.package.name);
    assert_eq!(
        report["source-release"],
        planned.plan.package.source_release.to_string()
    );
    assert_eq!(report["source-version"], planned.plan.package.version);

    let report_packages = report["packages"]
        .as_object()
        .expect("JSONC packages must be an object");
    assert_eq!(
        report_packages.keys().map(String::as_str).collect::<BTreeSet<_>>(),
        included,
        "{fixture}: binary and JSONC manifest membership disagree"
    );
    for package in included {
        let image = &packages[package];
        let report_package = report_packages
            .get(package)
            .expect("manifest membership was checked")
            .as_object()
            .unwrap_or_else(|| panic!("{fixture}: JSONC package {package} is not an object"));
        assert!(
            report_package.keys().all(|key| matches!(
                key.as_str(),
                "name" | "files" | "depends" | "provides" | "build-depends"
            )),
            "{fixture}: JSONC package {package} contains an unknown field"
        );
        assert_eq!(
            report_package.get("name").and_then(serde_json::Value::as_str),
            Some(package)
        );

        let files = image
            .layouts
            .keys()
            .map(|target| format!("/usr/{target}"))
            .collect::<Vec<_>>();
        let dependencies = raw_dependencies(&image.records, StonePayloadMetaTag::Depends)
            .into_iter()
            .collect::<Vec<_>>();
        let providers = raw_providers(&image.records, StonePayloadMetaTag::Provides)
            .into_iter()
            .collect::<Vec<_>>();
        assert_json_string_array(fixture, package, report_package, "files", &files);
        assert_json_string_array(fixture, package, report_package, "depends", &dependencies);
        assert_json_string_array(fixture, package, report_package, "provides", &providers);
        assert_json_string_array(
            fixture,
            package,
            report_package,
            "build-depends",
            &expected_build_dependencies.iter().cloned().collect::<Vec<_>>(),
        );
    }
}

fn assert_json_string_array(
    fixture: &str,
    package: &str,
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
    expected: &[String],
) {
    let actual = object
        .get(field)
        .map(|value| {
            value
                .as_array()
                .unwrap_or_else(|| panic!("{fixture}: JSONC {package}.{field} is not an array"))
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .unwrap_or_else(|| panic!("{fixture}: JSONC {package}.{field} contains a non-string"))
                        .to_owned()
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    assert_eq!(actual, expected, "{fixture}: JSONC {package}.{field} drift");
    assert_eq!(
        object.contains_key(field),
        !expected.is_empty(),
        "{fixture}: JSONC {package}.{field} must be omitted exactly when empty"
    );
}

fn output<'a>(
    planned: &'a super::super::Planned,
    packages: &'a BTreeMap<String, PackageImage>,
    logical_name: &str,
) -> (&'a OutputPlan, &'a PackageImage) {
    let output = planned
        .plan
        .outputs
        .iter()
        .find(|output| output.name == logical_name)
        .unwrap_or_else(|| panic!("missing frozen output {logical_name}"));
    (output, &packages[&output.package_name])
}

fn planned_output_dependencies(planned: &super::super::Planned, output: &OutputPlan) -> BTreeSet<String> {
    output
        .runtime_inputs
        .iter()
        .map(|relation| match relation {
            OutputRelation::Locked { relation, .. } => relation.canonical_name(),
            OutputRelation::Planned { output } => planned
                .plan
                .outputs
                .iter()
                .find(|candidate| candidate.name == *output)
                .unwrap_or_else(|| panic!("missing planned runtime output {output}"))
                .package_name
                .clone(),
        })
        .collect()
}

fn assert_exact_relations(
    fixture: &str,
    image: &PackageImage,
    dependencies: BTreeSet<String>,
    providers: BTreeSet<String>,
) {
    assert_eq!(
        image
            .meta
            .dependencies
            .iter()
            .map(Dependency::to_name)
            .collect::<BTreeSet<_>>(),
        dependencies,
        "{fixture}: {} dependency set is not the exact allowed fixture set",
        image.meta.name
    );
    assert_eq!(
        image
            .meta
            .providers
            .iter()
            .map(Provider::to_name)
            .collect::<BTreeSet<_>>(),
        providers,
        "{fixture}: {} provider set is not the exact allowed fixture set",
        image.meta.name
    );
}
