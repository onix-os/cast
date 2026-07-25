use gluon_config::DiagnosticCategory;

use super::{
    super::ActiveReblitRootFilesystemIntentError,
    support::{Fixture, ROOT_LOCATOR, authored_root},
};

#[test]
fn authored_intent_exposes_one_revalidated_root_token_and_exact_provenance() {
    let fixture = Fixture::new();
    fixture.write_root(ROOT_LOCATOR);
    let prepared = fixture.prepare().unwrap();
    let revalidated = prepared.revalidate(&fixture.installation).unwrap();

    assert_eq!(revalidated.kernel_argument(), format!("root={ROOT_LOCATOR}"));
    assert_eq!(revalidated.kernel_argument().matches("root=").count(), 1);
    assert!(!revalidated.kernel_argument().contains(char::is_whitespace));
    let fingerprint = revalidated.fingerprint();
    fingerprint.validate().unwrap();
    assert_eq!(fingerprint.root_logical_name, "etc/cast/root-filesystem.glu");
    assert_eq!(fingerprint.modules.len(), 1);
    assert_eq!(fingerprint.modules[0].logical_name, "cast.root_filesystem.v1");
}

#[test]
fn root_locator_is_an_opaque_authored_scalar_not_device_or_filesystem_proof() {
    let fixture = Fixture::new();
    let opaque = "completely-authored:synthetic-value";
    fixture.write_root(opaque);
    let prepared = fixture.prepare().unwrap();
    let revalidated = prepared.revalidate(&fixture.installation).unwrap();

    assert_eq!(revalidated.kernel_argument(), format!("root={opaque}"));
    // Acceptance proves only the authenticated bytes and closed Gluon type.
    // This module intentionally performs no device, filesystem, or mount lookup.
}

#[test]
fn empty_whitespace_non_ascii_quoted_escaped_and_prefixed_values_are_rejected() {
    for invalid in [
        "",
        "has space",
        "has\ttab",
        "has\nnewline",
        "é",
        "has\"quote",
        "has'quote",
        "has\\backslash",
        "root=PARTUUID=already-prefixed",
    ] {
        let fixture = Fixture::new();
        fixture.write_root(invalid);
        assert!(matches!(
            fixture.prepare(),
            Err(ActiveReblitRootFilesystemIntentError::InvalidRoot { .. })
        ));
    }
}

#[test]
fn root_locator_byte_bound_is_inclusive_and_diagnostic_preview_is_bounded() {
    let exact = "a".repeat(4_095);
    let fixture = Fixture::new();
    fixture.write_root(&exact);
    let prepared = fixture.prepare().unwrap();
    assert_eq!(
        prepared
            .revalidate(&fixture.installation)
            .unwrap()
            .kernel_argument()
            .len(),
        4_100
    );

    let over = "a".repeat(4_096);
    let fixture = Fixture::new();
    fixture.write_root(&over);
    assert!(matches!(
        fixture.prepare(),
        Err(ActiveReblitRootFilesystemIntentError::RootBytesLimit {
            limit: 4_095,
            actual: 4_096,
        })
    ));

    let invalid = " ".repeat(129);
    let fixture = Fixture::new();
    fixture.write_root(&invalid);
    assert!(matches!(
        fixture.prepare(),
        Err(ActiveReblitRootFilesystemIntentError::InvalidRoot {
            value_preview,
            actual_bytes: 129,
            ..
        }) if value_preview.len() == 128
    ));
}

#[test]
fn relative_host_unknown_and_old_abi_imports_are_rejected() {
    for imported in ["\"other.glu\"", "std.fs", "cast.system.v1", "cast.root_filesystem.v0"] {
        let fixture = Fixture::new();
        fixture.write_source(format!(
            "let _ = import! {imported}\nlet cast = import! cast.root_filesystem.v1\ncast.root_filesystem {{ root = {ROOT_LOCATOR:?} }}\n"
        ));
        assert!(matches!(
            fixture.prepare(),
            Err(ActiveReblitRootFilesystemIntentError::Evaluation(ref diagnostic))
                if diagnostic.category == DiagnosticCategory::Import
        ));
    }
}

#[test]
fn v1_api_import_is_mandatory_and_the_output_record_is_closed() {
    let no_api = Fixture::new();
    no_api.write_source(format!("{{ root = {ROOT_LOCATOR:?} }}\n"));
    assert!(matches!(
        no_api.prepare(),
        Err(ActiveReblitRootFilesystemIntentError::EvaluationContract { .. })
    ));

    let missing = Fixture::new();
    missing.write_source("let cast = import! cast.root_filesystem.v1\ncast.root_filesystem { }\n");
    assert!(matches!(
        missing.prepare(),
        Err(ActiveReblitRootFilesystemIntentError::Evaluation(ref diagnostic))
            if diagnostic.category == DiagnosticCategory::Type
    ));

    let unknown = Fixture::new();
    unknown.write_source(format!(
        "let cast = import! cast.root_filesystem.v1\n{{ unexpected = \"input\", .. cast.root_filesystem {{ root = {ROOT_LOCATOR:?} }} }}\n"
    ));
    assert!(matches!(
        unknown.prepare(),
        Err(ActiveReblitRootFilesystemIntentError::Evaluation(ref diagnostic))
            if diagnostic.category == DiagnosticCategory::Type
    ));
}

#[test]
fn exact_source_and_embedded_abi_participate_in_a_deterministic_fingerprint() {
    let first = Fixture::new();
    first.write_root(ROOT_LOCATOR);
    let first_prepared = first.prepare().unwrap();
    let first_fingerprint = first_prepared
        .revalidate(&first.installation)
        .unwrap()
        .fingerprint()
        .clone();

    let repeated = Fixture::new();
    repeated.write_root(ROOT_LOCATOR);
    let repeated_prepared = repeated.prepare().unwrap();
    let repeated_fingerprint = repeated_prepared
        .revalidate(&repeated.installation)
        .unwrap()
        .fingerprint()
        .clone();
    assert_eq!(first_fingerprint, repeated_fingerprint);

    let changed = Fixture::new();
    changed.write_source(format!("{}\n", authored_root(ROOT_LOCATOR)));
    let changed_prepared = changed.prepare().unwrap();
    let changed_fingerprint = changed_prepared
        .revalidate(&changed.installation)
        .unwrap()
        .fingerprint()
        .clone();
    assert_ne!(first_fingerprint.sha256, changed_fingerprint.sha256);
    assert_eq!(
        first_fingerprint.modules[0].sha256,
        changed_fingerprint.modules[0].sha256
    );
}

#[test]
fn checked_documentation_example_uses_the_exact_restricted_loader() {
    let source = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/examples/gluon/root-filesystem.glu"
    ));
    let fixture = Fixture::new();
    fixture.write_source(source);
    let prepared = fixture.prepare().unwrap();
    let revalidated = prepared.revalidate(&fixture.installation).unwrap();

    assert_eq!(revalidated.kernel_argument(), format!("root={ROOT_LOCATOR}"));
}
