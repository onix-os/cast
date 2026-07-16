#[derive(Debug)]
struct PackageImage {
    records: Vec<StonePayloadMetaRecord>,
    meta: Meta,
    layouts: BTreeMap<String, StonePayloadLayoutRecord>,
    content: BTreeMap<u128, Vec<u8>>,
}

/// Decode and verify the complete emitted bundle for one contentful execution
/// fixture. This deliberately does more than prove that Stone can parse its
/// own output: it ties metadata, layouts, indices, content, manifests, the
/// frozen plan, and the checked-in source fixture together.
pub(super) fn assert_fixture_bundle(
    name: &str,
    planned: &super::super::Planned,
    root: &Path,
    role: BundleRootRole,
) -> BTreeMap<String, Vec<u8>> {
    assert!(
        matches!(
            name,
            "autotools"
                | "autotools-options"
                | "cargo"
                | "cargo-features"
                | "cargo-vendored"
                | "cmake"
                | "custom"
                | "daemon-generated"
                | "factory-override"
                | "generated-config"
                | "hooks-patch"
                | "meson"
                | "split"
        ),
        "unknown contentful execution fixture {name:?}"
    );
    planned
        .plan
        .validate()
        .unwrap_or_else(|error| panic!("{name}: validate the frozen plan before inspecting its bundle: {error}"));

    let package_name = match name {
        "daemon-generated" => "cast-daemon-fixture".to_owned(),
        "hooks-patch" => "cast-hooks-fixture".to_owned(),
        _ => format!("cast-{name}-fixture"),
    };
    assert_eq!(planned.plan.package.name, package_name);
    assert_eq!(planned.plan.package.version, "1.0.0");
    assert_eq!(planned.plan.package.source_release, 1);
    assert_eq!(planned.plan.package.build_release, 1);
    assert_eq!(planned.plan.package.architecture, "x86_64");
    assert_eq!(
        planned.plan.package.homepage,
        format!("https://fixtures.invalid/{package_name}"),
        "{name}: fixture homepage is part of the package metadata golden"
    );
    assert_eq!(
        planned
            .plan
            .package
            .licenses
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["MPL-2.0"]
    );
    assert!(planned.plan.analysis.debug, "{name}: fixtures exercise debug splitting");
    assert!(
        planned.plan.analysis.strip,
        "{name}: fixtures exercise deterministic stripping"
    );
    assert!(
        !planned.plan.analysis.compress_man,
        "{name}: the tracked manual-page bytes require compression to be explicitly disabled"
    );

    let expected_names = planned
        .plan
        .outputs
        .iter()
        .map(|output| package_filename(planned, output))
        .chain([
            format!("manifest.{}.bin", planned.plan.package.architecture),
            format!("manifest.{}.jsonc", planned.plan.package.architecture),
        ])
        .collect::<BTreeSet<_>>();
    let artefacts = BundleDirectory::open(name, root, role).snapshot(name, &expected_names);

    let output_names = planned
        .plan
        .outputs
        .iter()
        .map(|output| output.name.as_str())
        .collect::<BTreeSet<_>>();
    if name == "generated-config" {
        assert_eq!(output_names, BTreeSet::from(["out"]));
    } else if name == "split" {
        assert_eq!(
            output_names,
            BTreeSet::from(["out", "libs", "devel", "docs", "dbginfo"])
        );
    } else if name == "daemon-generated" {
        assert_eq!(output_names, BTreeSet::from(["out", "docs", "dbginfo"]));
    } else {
        assert_eq!(
            output_names,
            BTreeSet::from([
                "out",
                "docs",
                "devel",
                "dbginfo",
                "libs",
                "32bit",
                "32bit-devel",
                "32bit-dbginfo",
                "demos",
            ]),
            "{name}: the versioned package factory output ABI drifted"
        );
    }

    let mut packages = BTreeMap::new();
    for output in &planned.plan.outputs {
        let filename = package_filename(planned, output);
        let image = decode_package(name, planned, output, &artefacts[&filename]);
        assert!(
            packages.insert(output.package_name.clone(), image).is_none(),
            "{name}: duplicate emitted package name {}",
            output.package_name
        );
    }

    assert_global_layout_integrity(name, &packages);
    assert_manifests(name, planned, &artefacts, &packages);
    if name == "generated-config" {
        assert_generated_config_fixture(planned, &packages);
    } else if name == "split" {
        assert_split_fixture(planned, &packages);
    } else if name == "daemon-generated" {
        assert_daemon_fixture(planned, &packages);
    } else if name == "cargo-features" {
        assert_cargo_features_fixture(planned, &packages);
    } else {
        assert_simple_fixture(name, planned, &packages);
    }

    artefacts
}

fn fixture_limits() -> StoneDecodeLimits {
    StoneDecodeLimits {
        max_payloads: 16,
        max_records_per_payload: 4_096,
        max_record_bytes: 64 * 1024,
        max_stored_payload_bytes: MAX_FIXTURE_CONTENT_BYTES,
        max_plain_payload_bytes: MAX_FIXTURE_CONTENT_BYTES,
        max_total_records: 16_384,
        max_total_record_bytes: 8 * 1024 * 1024,
        max_total_stored_bytes: MAX_FIXTURE_ARTEFACT_BYTES,
        max_total_plain_bytes: MAX_FIXTURE_ARTEFACT_BYTES,
        max_zstd_window_log: 24,
    }
}

fn package_filename(planned: &super::super::Planned, output: &OutputPlan) -> String {
    format!(
        "{}-{}-{}-{}-{}.stone",
        output.package_name,
        planned.plan.package.version,
        planned.plan.package.source_release,
        planned.plan.package.build_release,
        planned.plan.package.architecture,
    )
}

fn decode_package(fixture: &str, planned: &super::super::Planned, output: &OutputPlan, bytes: &[u8]) -> PackageImage {
    let mut reader = stone::read_bytes_with_limits(bytes, fixture_limits())
        .unwrap_or_else(|error| panic!("{fixture}: decode emitted package {}: {error}", output.package_name));
    let StoneHeader::V1(container_header) = reader.header;
    assert_eq!(
        container_header.file_type,
        StoneHeaderV1FileType::Binary,
        "{fixture}: {} has the wrong Stone file type",
        output.package_name
    );
    let payloads = reader
        .payloads()
        .unwrap_or_else(|error| panic!("{fixture}: seek package payloads for {}: {error}", output.package_name))
        .collect::<Result<Vec<_>, _>>()
        .unwrap_or_else(|error| {
            panic!(
                "{fixture}: decode package payloads for {}: {error}",
                output.package_name
            )
        });
    assert_eq!(
        usize::from(container_header.num_payloads),
        payloads.len(),
        "{fixture}: {} Stone header payload cardinality drift",
        output.package_name
    );
    assert_canonical_payload_headers(fixture, &output.package_name, &payloads);

    let payload_names = payloads.iter().map(StoneDecodedPayload::name).collect::<Vec<_>>();
    let metadata = payloads
        .iter()
        .filter_map(StoneDecodedPayload::meta)
        .collect::<Vec<_>>();
    assert_eq!(
        metadata.len(),
        1,
        "{fixture}: {} must have exactly one Meta payload",
        output.package_name
    );
    let records = metadata[0].body.clone();
    validate_meta_records(fixture, &output.package_name, &records, false);
    assert_frozen_provenance(fixture, planned, &output.package_name, &records);
    let meta = Meta::from_stone_payload(&records)
        .unwrap_or_else(|error| panic!("{fixture}: decode metadata for {}: {error}", output.package_name));
    assert_package_meta(fixture, planned, output, &records, &meta);

    let layout_payloads = payloads
        .iter()
        .filter_map(StoneDecodedPayload::layout)
        .collect::<Vec<_>>();
    assert!(
        layout_payloads.len() <= 1,
        "{fixture}: {} repeats its Layout payload",
        output.package_name
    );
    let layout_records = layout_payloads
        .first()
        .map(|payload| {
            assert_eq!(payload.header.num_records, payload.body.len());
            payload.body.clone()
        })
        .unwrap_or_default();
    assert_eq!(
        layout_payloads.is_empty(),
        layout_records.is_empty(),
        "{fixture}: {} encoded an empty Layout payload",
        output.package_name
    );

    assert!(
        layout_records
            .windows(2)
            .all(|pair| pair[0].file.target() < pair[1].file.target()),
        "{fixture}: {} Layout targets are not in strict canonical order",
        output.package_name
    );

    let mut layouts = BTreeMap::new();
    let mut regular_digests = BTreeSet::new();
    for layout in layout_records {
        validate_layout_record(fixture, &output.package_name, &layout);
        let target = layout.file.target().to_owned();
        if let StonePayloadLayoutFile::Regular(digest, _) = &layout.file {
            regular_digests.insert(*digest);
        }
        assert!(
            layouts.insert(target.clone(), layout).is_none(),
            "{fixture}: {} repeats layout target /usr/{target}",
            output.package_name
        );
    }

    let index_payloads = payloads
        .iter()
        .filter_map(StoneDecodedPayload::index)
        .collect::<Vec<_>>();
    let content_payloads = payloads
        .iter()
        .filter_map(StoneDecodedPayload::content)
        .collect::<Vec<_>>();
    assert!(
        index_payloads.len() <= 1,
        "{fixture}: {} repeats its Index payload",
        output.package_name
    );
    assert!(
        content_payloads.len() <= 1,
        "{fixture}: {} repeats its Content payload",
        output.package_name
    );

    if regular_digests.is_empty() {
        assert!(
            index_payloads.is_empty(),
            "{fixture}: {} has an orphan Index payload",
            output.package_name
        );
        assert!(
            content_payloads.is_empty(),
            "{fixture}: {} has an orphan Content payload",
            output.package_name
        );
        assert_eq!(
            payload_names,
            if layouts.is_empty() {
                vec!["Meta"]
            } else {
                vec!["Meta", "Layout"]
            },
            "{fixture}: {} has a non-canonical payload topology",
            output.package_name
        );
        return PackageImage {
            records,
            meta,
            layouts,
            content: BTreeMap::new(),
        };
    }

    assert_eq!(
        payload_names,
        ["Meta", "Layout", "Index", "Content"],
        "{fixture}: {} has a non-canonical content payload topology",
        output.package_name
    );
    let indices = &index_payloads[0].body;
    assert_eq!(index_payloads[0].header.num_records, indices.len());
    assert_eq!(
        indices.len(),
        regular_digests.len(),
        "{fixture}: {} must index each unique regular-file digest exactly once",
        output.package_name
    );

    let content_payload = content_payloads[0].clone();
    assert!(
        content_payload.header.plain_size <= MAX_FIXTURE_CONTENT_BYTES,
        "{fixture}: {} content exceeds the fixture boundary",
        output.package_name
    );
    let mut unpacked = Vec::new();
    unpacked
        .try_reserve_exact(usize::try_from(content_payload.header.plain_size).unwrap())
        .unwrap_or_else(|error| panic!("{fixture}: reserve content for {}: {error}", output.package_name));
    reader
        .unpack_content(&content_payload, &mut unpacked)
        .unwrap_or_else(|error| panic!("{fixture}: unpack content for {}: {error}", output.package_name));
    assert_eq!(
        u64::try_from(unpacked.len()).unwrap(),
        content_payload.header.plain_size
    );

    let mut content = BTreeMap::new();
    let mut cursor = 0u64;
    let mut previous_index_key = None;
    for index in indices {
        assert_eq!(
            index.start, cursor,
            "{fixture}: {} index ranges must be gapless and ordered",
            output.package_name
        );
        assert!(
            index.end >= index.start,
            "{fixture}: {} has a reversed index range",
            output.package_name
        );
        let size = index.end - index.start;
        if let Some((previous_size, previous_digest)) = previous_index_key {
            assert!(
                previous_size > size || (previous_size == size && previous_digest < index.digest),
                "{fixture}: {} Index records are not ordered by size descending then digest ascending",
                output.package_name
            );
        }
        previous_index_key = Some((size, index.digest));
        let start = usize::try_from(index.start).unwrap();
        let end = usize::try_from(index.end).unwrap();
        let blob = unpacked
            .get(start..end)
            .unwrap_or_else(|| panic!("{fixture}: {} index range escapes Content", output.package_name));
        let digest = content_digest(blob);
        assert_eq!(
            digest, index.digest,
            "{fixture}: {} index XXH3 digest does not authenticate its content range",
            output.package_name
        );
        assert!(
            content.insert(index.digest, blob.to_vec()).is_none(),
            "{fixture}: {} repeats an Index digest",
            output.package_name
        );
        cursor = index.end;
    }
    assert_eq!(
        cursor,
        u64::try_from(unpacked.len()).unwrap(),
        "{fixture}: {} Content has unindexed trailing bytes",
        output.package_name
    );
    assert_eq!(
        content.keys().copied().collect::<BTreeSet<_>>(),
        regular_digests,
        "{fixture}: {} Layout and Index digest sets disagree",
        output.package_name
    );

    PackageImage {
        records,
        meta,
        layouts,
        content,
    }
}

fn content_digest(bytes: &[u8]) -> u128 {
    let mut hasher = StoneDigestWriterHasher::new();
    hasher.update(bytes);
    hasher.digest128()
}

fn assert_canonical_payload_headers(fixture: &str, role: &str, payloads: &[StoneDecodedPayload]) {
    for payload in payloads {
        let header = payload.header();
        let expected_kind = match payload {
            StoneDecodedPayload::Meta(_) => StonePayloadKind::Meta,
            StoneDecodedPayload::Attributes(_) => StonePayloadKind::Attributes,
            StoneDecodedPayload::Layout(_) => StonePayloadKind::Layout,
            StoneDecodedPayload::Index(_) => StonePayloadKind::Index,
            StoneDecodedPayload::Content(_) => StonePayloadKind::Content,
            StoneDecodedPayload::Unknown(_) | StoneDecodedPayload::UnknownCompression(_) => {
                panic!("{fixture}: {role} contains an unknown Stone payload")
            }
        };
        assert_eq!(header.version, 1, "{fixture}: {role} contains a non-v1 Stone payload");
        assert_eq!(
            header.compression,
            StonePayloadCompression::Zstd,
            "{fixture}: {role} payload {:?} is not canonically zstd-compressed",
            header.kind
        );
        assert_eq!(
            header.kind, expected_kind,
            "{fixture}: {role} decoded payload variant disagrees with its header"
        );
        assert!(
            header.stored_size > 0,
            "{fixture}: {role} contains an empty stored payload"
        );
        assert!(
            header.plain_size > 0,
            "{fixture}: {role} contains an empty plain payload"
        );
        if expected_kind == StonePayloadKind::Content {
            assert_eq!(
                header.num_records, 0,
                "{fixture}: {role} Content must use the canonical zero record count"
            );
        } else {
            assert!(
                header.num_records > 0,
                "{fixture}: {role} contains a canonically forbidden empty record payload"
            );
        }
    }
}

fn validate_meta_records(fixture: &str, package: &str, records: &[StonePayloadMetaRecord], manifest: bool) {
    let fixed_tags = [
        StonePayloadMetaTag::Name,
        StonePayloadMetaTag::Version,
        StonePayloadMetaTag::Release,
        StonePayloadMetaTag::BuildRelease,
        StonePayloadMetaTag::Architecture,
        StonePayloadMetaTag::Summary,
        StonePayloadMetaTag::Description,
        StonePayloadMetaTag::SourceID,
        StonePayloadMetaTag::Homepage,
    ];
    assert!(
        records.len() >= fixed_tags.len() + 3,
        "{fixture}: {package} metadata is shorter than the canonical fixed fields, license, and provenance"
    );
    assert_eq!(
        records[..fixed_tags.len()]
            .iter()
            .map(|record| record.tag)
            .collect::<Vec<_>>(),
        fixed_tags,
        "{fixture}: {package} fixed metadata record order drift"
    );

    let mut unique = BTreeSet::new();
    for record in records {
        assert!(
            unique.insert(format!("{:?}:{:?}", record.tag, record.primitive)),
            "{fixture}: {package} repeats metadata record {:?}",
            record.tag
        );
        match record.tag {
            StonePayloadMetaTag::Name
            | StonePayloadMetaTag::Architecture
            | StonePayloadMetaTag::Version
            | StonePayloadMetaTag::Summary
            | StonePayloadMetaTag::Description
            | StonePayloadMetaTag::Homepage
            | StonePayloadMetaTag::SourceID
            | StonePayloadMetaTag::License
            | StonePayloadMetaTag::SourceRef => {
                let StonePayloadMetaPrimitive::String(value) = &record.primitive else {
                    panic!(
                        "{fixture}: {package} metadata tag {:?} has the wrong primitive",
                        record.tag
                    );
                };
                assert!(!value.contains('\0'), "{fixture}: {package} metadata contains NUL");
            }
            StonePayloadMetaTag::Release | StonePayloadMetaTag::BuildRelease => assert!(
                matches!(&record.primitive, StonePayloadMetaPrimitive::Uint64(_)),
                "{fixture}: {package} metadata tag {:?} has the wrong primitive",
                record.tag
            ),
            StonePayloadMetaTag::Depends | StonePayloadMetaTag::BuildDepends => {
                assert!(
                    record.tag != StonePayloadMetaTag::BuildDepends || manifest,
                    "{fixture}: binary package {package} contains manifest-only BuildDepends"
                );
                let StonePayloadMetaPrimitive::Dependency(kind, value) = &record.primitive else {
                    panic!(
                        "{fixture}: {package} metadata tag {:?} has the wrong primitive",
                        record.tag
                    );
                };
                let kind = Kind::from_stone_dependency(*kind)
                    .unwrap_or_else(|| panic!("{fixture}: {package} uses an unknown dependency kind"));
                Dependency::new(kind, value.clone())
                    .unwrap_or_else(|error| panic!("{fixture}: {package} has an invalid dependency: {error}"));
            }
            StonePayloadMetaTag::Provides | StonePayloadMetaTag::Conflicts => {
                let StonePayloadMetaPrimitive::Provider(kind, value) = &record.primitive else {
                    panic!(
                        "{fixture}: {package} metadata tag {:?} has the wrong primitive",
                        record.tag
                    );
                };
                let kind = Kind::from_stone_dependency(*kind)
                    .unwrap_or_else(|| panic!("{fixture}: {package} uses an unknown provider kind"));
                Provider::new(kind, value.clone())
                    .unwrap_or_else(|error| panic!("{fixture}: {package} has an invalid provider: {error}"));
            }
            StonePayloadMetaTag::PackageURI
            | StonePayloadMetaTag::PackageHash
            | StonePayloadMetaTag::PackageSize
            | StonePayloadMetaTag::SourceURI
            | StonePayloadMetaTag::SourcePath
            | StonePayloadMetaTag::Unknown => {
                panic!("{fixture}: {package} contains forbidden metadata tag {:?}", record.tag)
            }
        }
        assert!(
            !matches!(&record.primitive, StonePayloadMetaPrimitive::Unknown(_)),
            "{fixture}: {package} contains an unknown metadata primitive"
        );
    }

    for tag in [
        StonePayloadMetaTag::Name,
        StonePayloadMetaTag::Architecture,
        StonePayloadMetaTag::Version,
        StonePayloadMetaTag::Summary,
        StonePayloadMetaTag::Description,
        StonePayloadMetaTag::Homepage,
        StonePayloadMetaTag::SourceID,
        StonePayloadMetaTag::Release,
        StonePayloadMetaTag::BuildRelease,
    ] {
        assert_eq!(
            records.iter().filter(|record| record.tag == tag).count(),
            1,
            "{fixture}: {package} must contain exactly one {tag:?} metadata record"
        );
    }
    assert_eq!(
        records
            .iter()
            .filter(|record| record.tag == StonePayloadMetaTag::SourceRef)
            .count(),
        2,
        "{fixture}: {package} must contain exactly two frozen provenance references"
    );

    let variable_ranks = records[fixed_tags.len()..]
        .iter()
        .map(|record| match record.tag {
            StonePayloadMetaTag::License => 0,
            StonePayloadMetaTag::Depends => 1,
            StonePayloadMetaTag::Provides => 2,
            StonePayloadMetaTag::Conflicts => 3,
            StonePayloadMetaTag::SourceRef => 4,
            StonePayloadMetaTag::BuildDepends if manifest => 5,
            tag => panic!("{fixture}: {package} metadata tag {tag:?} is outside canonical record order"),
        })
        .collect::<Vec<_>>();
    assert!(
        variable_ranks.windows(2).all(|pair| pair[0] <= pair[1]),
        "{fixture}: {package} variable metadata record groups are not canonical"
    );
    for tag in [
        StonePayloadMetaTag::License,
        StonePayloadMetaTag::Depends,
        StonePayloadMetaTag::Provides,
        StonePayloadMetaTag::Conflicts,
        StonePayloadMetaTag::BuildDepends,
    ] {
        let values = records
            .iter()
            .filter(|record| record.tag == tag)
            .map(|record| format!("{:?}", record.primitive))
            .collect::<Vec<_>>();
        assert!(
            values.windows(2).all(|pair| pair[0] < pair[1]),
            "{fixture}: {package} {tag:?} records are not strictly canonical"
        );
    }
}
