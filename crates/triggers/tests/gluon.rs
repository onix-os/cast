
use gluon_config::{DiagnosticCategory, Evaluator, Source};
use triggers::{
    Collection, TRIGGER_ABI_VERSION, TriggerEvaluationError, evaluate_gluon, evaluate_gluon_with_inputs,
    format::{Handler, PathKind},
};

fn authored(body: &str) -> Source {
    Source::new("trigger.glu", format!("let cast = import! cast.trigger.v1\n{body}"))
}

#[test]
fn retired_moss_trigger_abi_is_not_a_compatibility_alias() {
    let error = evaluate_gluon(&Source::new("retired-trigger.glu", "import! moss.trigger.v1")).unwrap_err();

    assert!(matches!(
        error,
        TriggerEvaluationError::Evaluation(ref diagnostic)
            if diagnostic.category == DiagnosticCategory::Import
                && diagnostic.message.contains("moss.trigger.v1")
    ));
}

#[test]
fn documented_trigger_example_remains_loadable() {
    let source = Source::new(
        "docs/examples/gluon/trigger.glu",
        include_str!("../../../docs/examples/gluon/trigger.glu"),
    );
    let evaluated = evaluate_gluon(&source).unwrap();

    assert_eq!(evaluated.trigger.name, "refresh-example");
    Collection::new([&evaluated.trigger]).unwrap();
}

#[test]
fn constructors_cover_run_delete_inhibitors_patterns_and_path_kinds() {
    let source = authored(
        r#"
let base = cast.trigger "kernel" "Maintain kernel metadata"
{
    before = cast.optional.set "boot",
    after = cast.optional.set "filesystem",
    inhibitors = cast.optional.set (cast.inhibitors
        ["/etc/inhibit"]
        ["chroot", "live"]),
    paths = [
        cast.path
            "/usr/lib/modules/(version:*)/kernel"
            ["depmod"]
            (cast.optional.set cast.path_kind.directory),
        cast.path
            "/var/lib/example-link"
            ["cleanup"]
            (cast.optional.set cast.path_kind.symlink),
    ],
    handlers = [
        cast.handler.named "depmod" (cast.handler.run
            "/sbin/depmod"
            ["-a", "$(version)"]),
        cast.handler.named "cleanup" (cast.handler.delete
            ["/var/cache/example", "/var/lib/example.old"]),
    ],
    .. base
}
"#,
    );

    let evaluated = evaluate_gluon(&source).unwrap();
    let trigger = &evaluated.trigger;

    assert_eq!(trigger.name, "kernel");
    assert_eq!(trigger.before.as_deref(), Some("boot"));
    assert_eq!(trigger.after.as_deref(), Some("filesystem"));
    let inhibitors = trigger.inhibitors.as_ref().unwrap();
    assert_eq!(inhibitors.paths, ["/etc/inhibit"]);
    assert_eq!(inhibitors.environment, ["chroot", "live"]);

    let module_path = trigger
        .paths
        .iter()
        .find(|(pattern, _)| pattern.match_path("/usr/lib/modules/6.12.1/kernel").is_some())
        .map(|(_, definition)| definition)
        .unwrap();
    assert!(matches!(module_path.kind, Some(PathKind::Directory)));
    assert_eq!(module_path.handlers, ["depmod"]);
    let link_path = trigger
        .paths
        .iter()
        .find(|(pattern, _)| pattern.match_path("/var/lib/example-link").is_some())
        .map(|(_, definition)| definition)
        .unwrap();
    assert!(matches!(link_path.kind, Some(PathKind::Symlink)));

    assert!(matches!(
        trigger.handlers.get("depmod"),
        Some(Handler::Run { run, args })
            if run == "/sbin/depmod" && args == &["-a", "$(version)"]
    ));
    assert!(matches!(
        trigger.handlers.get("cleanup"),
        Some(Handler::Delete { delete })
            if delete == &["/var/cache/example", "/var/lib/example.old"]
    ));

    Collection::new([trigger]).unwrap();
}

#[test]
fn missing_handler_reference_is_reported_by_collection() {
    let source = authored(
        r#"
let base = cast.trigger "broken" "References an absent handler"
{
    paths = [cast.path "/usr/lib/broken" ["missing"] cast.optional.unset],
    .. base
}
"#,
    );
    let evaluated = evaluate_gluon(&source).unwrap();

    let error = match Collection::new([&evaluated.trigger]) {
        Ok(_) => panic!("missing handler reference should fail"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("missing handler reference"));
    assert!(error.to_string().contains("missing"));
}

#[test]
fn invalid_pattern_has_an_indexed_conversion_field() {
    let source = authored(
        r#"
let base = cast.trigger "broken-pattern" "Invalid pattern"
{
    paths = [cast.path "/usr/lib/(unterminated" [] cast.optional.unset],
    .. base
}
"#,
    );

    let error = evaluate_gluon(&source).unwrap_err();
    assert!(matches!(
        error,
        TriggerEvaluationError::Conversion(ref error)
            if error.field() == "paths[0].key"
    ));
}

#[test]
fn invalid_types_and_unknown_fields_are_type_errors() {
    for body in [
        r#"
let base = cast.trigger "wrong-type" "Wrong type"
{ description = 42, .. base }
"#,
        r#"
let base = cast.trigger "unknown-field" "Unknown field"
{ unexpected = "value", .. base }
"#,
    ] {
        let error = evaluate_gluon(&authored(body)).unwrap_err();
        assert!(matches!(
            error,
            TriggerEvaluationError::Evaluation(ref error)
                if error.category == DiagnosticCategory::Type
        ));
    }
}

#[test]
fn forbidden_host_effects_are_rejected() {
    let source = authored(
        r#"
let _ = import! std.fs
cast.trigger "forbidden" "Forbidden host effect"
"#,
    );

    let error = evaluate_gluon(&source).unwrap_err();
    assert!(matches!(
        error,
        TriggerEvaluationError::Evaluation(ref error)
            if error.category == DiagnosticCategory::Import
    ));
}

#[test]
fn fingerprint_is_deterministic_and_includes_the_versioned_abi() {
    let source = authored(
        r#"
let abi_version: Int = cast.abi_version
cast.trigger "fingerprint" "Fingerprint"
"#,
    );
    let evaluator = Evaluator::default();

    let first = evaluate_gluon_with_inputs(&evaluator, &source, b"inputs-v1").unwrap();
    let repeated = evaluate_gluon_with_inputs(&evaluator, &source, b"inputs-v1").unwrap();
    let changed = evaluate_gluon_with_inputs(&evaluator, &source, b"inputs-v2").unwrap();

    assert_eq!(first.fingerprint, repeated.fingerprint);
    assert_ne!(first.fingerprint.sha256, changed.fingerprint.sha256);
    assert_eq!(TRIGGER_ABI_VERSION, 1);
    assert_eq!(first.fingerprint.configuration_abi_version, TRIGGER_ABI_VERSION);
    assert_eq!(
        first
            .fingerprint
            .imported_modules
            .iter()
            .map(|module| module.logical_name.as_str())
            .collect::<Vec<_>>(),
        ["cast.trigger.v1"]
    );
}
