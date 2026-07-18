use gluon_config::DiagnosticCategory;

use super::{
    super::{ActiveReblitBootTopologyIntentError, BoundActiveReblitBootTopologyIntent},
    support::{ESP_PARTUUID, Fixture, XBOOTLDR_PARTUUID, authored_alias, authored_distinct},
};

#[test]
fn alias_intent_exposes_only_revalidated_typed_identity_and_exact_provenance() {
    let fixture = Fixture::new();
    fixture.write_alias();
    let prepared = fixture.prepare().unwrap();
    let revalidated = prepared.revalidate(&fixture.installation).unwrap();

    assert_eq!(
        revalidated.topology(),
        BoundActiveReblitBootTopologyIntent::BootAliasesEsp {
            esp_partuuid: ESP_PARTUUID,
        }
    );
    let fingerprint = revalidated.fingerprint();
    fingerprint.validate().unwrap();
    assert_eq!(fingerprint.root_logical_name, "etc/cast/boot-topology.glu");
    assert_eq!(fingerprint.imported_modules.len(), 1);
    assert_eq!(fingerprint.imported_modules[0].logical_name, "cast.boot_topology.v1");
}

#[test]
fn distinct_xbootldr_is_typed_declarative_intent_not_runtime_role_proof() {
    let fixture = Fixture::new();
    fixture.write_source(authored_distinct(ESP_PARTUUID, XBOOTLDR_PARTUUID));
    let prepared = fixture.prepare().unwrap();
    let revalidated = prepared.revalidate(&fixture.installation).unwrap();

    assert_eq!(
        revalidated.topology(),
        BoundActiveReblitBootTopologyIntent::DistinctXbootldr {
            esp_partuuid: ESP_PARTUUID,
            xbootldr_partuuid: XBOOTLDR_PARTUUID,
        }
    );
    // No mountpoint, device, GPT-role, or same-disk assertion is exposed by
    // this intent-only type. A later physical-topology aggregate must prove it.
}

#[test]
fn canonical_partuuid_policy_rejects_uppercase_malformed_and_nil_values() {
    for partuuid in [
        "AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE",
        "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeee",
        "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee/",
        "{aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee}",
        "00000000-0000-0000-0000-000000000000",
    ] {
        let fixture = Fixture::new();
        fixture.write_source(authored_alias(partuuid));
        assert!(matches!(
            fixture.prepare(),
            Err(ActiveReblitBootTopologyIntentError::InvalidPartUuid { .. })
        ));
    }
}

#[test]
fn invalid_partuuid_diagnostics_cap_the_preview_at_an_exact_byte_boundary() {
    for (value, expected_preview_bytes) in [("g".repeat(64), 64), ("g".repeat(65), 64)] {
        let fixture = Fixture::new();
        fixture.write_source(authored_alias(&value));
        let error = match fixture.prepare() {
            Err(error) => error,
            Ok(_) => panic!("invalid PARTUUID was accepted"),
        };
        assert!(matches!(
            error,
            ActiveReblitBootTopologyIntentError::InvalidPartUuid {
                value_preview,
                actual_bytes,
                ..
            } if value_preview.len() == expected_preview_bytes && actual_bytes == value.len()
        ));
    }

    let unicode = format!("{}é", "g".repeat(63));
    let fixture = Fixture::new();
    fixture.write_source(authored_alias(&unicode));
    assert!(matches!(
        fixture.prepare(),
        Err(ActiveReblitBootTopologyIntentError::InvalidPartUuid {
            value_preview,
            actual_bytes: 65,
            ..
        }) if value_preview.len() == 63
    ));
}

#[test]
fn distinct_form_rejects_duplicate_partition_identities() {
    let fixture = Fixture::new();
    fixture.write_source(authored_distinct(ESP_PARTUUID, ESP_PARTUUID));
    assert!(matches!(
        fixture.prepare(),
        Err(ActiveReblitBootTopologyIntentError::InvalidPartUuid {
            field: "xbootldr_partuuid",
            ..
        })
    ));
}

#[test]
fn relative_host_and_unknown_embedded_imports_are_all_rejected() {
    for imported in ["\"other.glu\"", "std.fs", "cast.system.v1"] {
        let fixture = Fixture::new();
        fixture.write_source(format!(
            "let _ = import! {imported}\nlet cast = import! cast.boot_topology.v1\ncast.boot_topology.aliases_esp \"{ESP_PARTUUID}\"\n"
        ));
        assert!(matches!(
            fixture.prepare(),
            Err(ActiveReblitBootTopologyIntentError::Evaluation(ref diagnostic))
                if diagnostic.category == DiagnosticCategory::Import
        ));
    }
}

#[test]
fn api_import_is_mandatory_and_unknown_output_fields_are_rejected() {
    let no_api = Fixture::new();
    no_api.write_source(format!(
        "type BootTarget = | AliasEsp\n{{ esp_partuuid = \"{ESP_PARTUUID}\", boot = AliasEsp }}\n"
    ));
    assert!(no_api.prepare().is_err());

    let unknown = Fixture::new();
    unknown.write_source(format!(
        "let cast = import! cast.boot_topology.v1\n{{ unexpected = \"input\", .. cast.boot_topology.aliases_esp \"{ESP_PARTUUID}\" }}\n"
    ));
    assert!(matches!(
        unknown.prepare(),
        Err(ActiveReblitBootTopologyIntentError::Evaluation(ref diagnostic))
            if diagnostic.category == DiagnosticCategory::Type
    ));
}

#[test]
fn exact_source_and_embedded_abi_participate_in_deterministic_fingerprint() {
    let first = Fixture::new();
    first.write_source(authored_alias(ESP_PARTUUID));
    let first_prepared = first.prepare().unwrap();
    let first_fingerprint = first_prepared
        .revalidate(&first.installation)
        .unwrap()
        .fingerprint()
        .clone();

    let repeated = Fixture::new();
    repeated.write_source(authored_alias(ESP_PARTUUID));
    let repeated_prepared = repeated.prepare().unwrap();
    let repeated_fingerprint = repeated_prepared
        .revalidate(&repeated.installation)
        .unwrap()
        .fingerprint()
        .clone();
    assert_eq!(first_fingerprint, repeated_fingerprint);

    let changed = Fixture::new();
    changed.write_source(format!("{}\n", authored_alias(ESP_PARTUUID)));
    let changed_prepared = changed.prepare().unwrap();
    let changed_fingerprint = changed_prepared
        .revalidate(&changed.installation)
        .unwrap()
        .fingerprint()
        .clone();
    assert_ne!(first_fingerprint.sha256, changed_fingerprint.sha256);
    assert_eq!(
        first_fingerprint.imported_modules[0].sha256,
        changed_fingerprint.imported_modules[0].sha256
    );
}

#[test]
fn checked_documentation_examples_use_the_exact_restricted_topology_loader() {
    for (source, expected) in [
        (
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../docs/examples/gluon/boot-topology-aliases-esp.glu"
            )),
            BoundActiveReblitBootTopologyIntent::BootAliasesEsp {
                esp_partuuid: ESP_PARTUUID,
            },
        ),
        (
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../docs/examples/gluon/boot-topology-distinct-xbootldr.glu"
            )),
            BoundActiveReblitBootTopologyIntent::DistinctXbootldr {
                esp_partuuid: ESP_PARTUUID,
                xbootldr_partuuid: XBOOTLDR_PARTUUID,
            },
        ),
    ] {
        let fixture = Fixture::new();
        fixture.write_source(source);
        let prepared = fixture.prepare().unwrap();
        assert_eq!(prepared.revalidate(&fixture.installation).unwrap().topology(), expected);
    }
}
