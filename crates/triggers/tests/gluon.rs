use declarative_config::{DeclarationEvaluationError, DeclarationEvaluator};
use gluon_config::{DiagnosticCategory, Evaluator, Source};
use fnmatch::Pattern;
use triggers::{
    Collection, GluonTriggerConversionError, GluonTriggerEvaluator,
    TRIGGER_ABI_VERSION, TriggerEvaluationError, evaluate_gluon,
    evaluate_gluon_with_inputs,
    format::{Handler, PathKind, Trigger},
};

#[derive(Debug, PartialEq, Eq)]
struct NormalizedTriggerValue {
    name: String,
    description: String,
    before: Option<String>,
    after: Option<String>,
    inhibitors: Option<NormalizedInhibitorsValue>,
    paths: Vec<NormalizedPathValue>,
    handlers: Vec<NormalizedHandlerValue>,
}

#[derive(Debug, PartialEq, Eq)]
struct NormalizedInhibitorsValue {
    paths: Vec<String>,
    environment: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
struct NormalizedPathValue {
    pattern: Pattern,
    handlers: Vec<String>,
    kind: Option<NormalizedPathKind>,
}

#[derive(Debug, PartialEq, Eq)]
enum NormalizedPathKind {
    Directory,
    Symlink,
}

#[derive(Debug, PartialEq, Eq)]
struct NormalizedHandlerValue {
    name: String,
    action: Handler,
}

fn normalized_trigger_value(trigger: &Trigger) -> NormalizedTriggerValue {
    NormalizedTriggerValue {
        name: trigger.name.clone(),
        description: trigger.description.clone(),
        before: trigger.before.clone(),
        after: trigger.after.clone(),
        inhibitors: trigger.inhibitors.as_ref().map(|inhibitors| NormalizedInhibitorsValue {
            paths: inhibitors.paths.clone(),
            environment: inhibitors.environment.clone(),
        }),
        paths: trigger
            .paths
            .iter()
            .map(|(pattern, definition)| NormalizedPathValue {
                pattern: pattern.clone(),
                handlers: definition.handlers.clone(),
                kind: definition.kind.as_ref().map(|kind| match kind {
                    PathKind::Directory => NormalizedPathKind::Directory,
                    PathKind::Symlink => NormalizedPathKind::Symlink,
                }),
            })
            .collect(),
        handlers: trigger
            .handlers
            .iter()
            .map(|(name, action)| NormalizedHandlerValue {
                name: name.clone(),
                action: action.clone(),
            })
            .collect(),
    }
}

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

    assert_eq!(
        normalized_trigger_value(trigger),
        NormalizedTriggerValue {
            name: "kernel".to_owned(),
            description: "Maintain kernel metadata".to_owned(),
            before: Some("boot".to_owned()),
            after: Some("filesystem".to_owned()),
            inhibitors: Some(NormalizedInhibitorsValue {
                paths: vec!["/etc/inhibit".to_owned()],
                environment: ["chroot", "live"].map(str::to_owned).to_vec(),
            }),
            paths: vec![
                NormalizedPathValue {
                    pattern: "/usr/lib/modules/(version:*)/kernel".parse().unwrap(),
                    handlers: vec!["depmod".to_owned()],
                    kind: Some(NormalizedPathKind::Directory),
                },
                NormalizedPathValue {
                    pattern: "/var/lib/example-link".parse().unwrap(),
                    handlers: vec!["cleanup".to_owned()],
                    kind: Some(NormalizedPathKind::Symlink),
                },
            ],
            handlers: vec![
                NormalizedHandlerValue {
                    name: "cleanup".to_owned(),
                    action: Handler::Delete {
                        delete: ["/var/cache/example", "/var/lib/example.old"]
                            .map(str::to_owned)
                            .to_vec(),
                    },
                },
                NormalizedHandlerValue {
                    name: "depmod".to_owned(),
                    action: Handler::Run {
                        run: "/sbin/depmod".to_owned(),
                        args: ["-a", "$(version)"].map(str::to_owned).to_vec(),
                    },
                },
            ],
        }
    );

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
let base = cast.trigger "fingerprint" "Fingerprint"
{
    paths = [cast.path
        "/usr/share/typed-parity"
        ["refresh"]
        cast.optional.unset],
    handlers = [cast.handler.named "refresh" (cast.handler.run
        "/usr/bin/true"
        ["--typed-parity"])],
    .. base
}
"#,
    );
    let evaluator = Evaluator::default();

    let first = evaluate_gluon_with_inputs(&evaluator, &source, b"inputs-v1").unwrap();
    let repeated = evaluate_gluon_with_inputs(&evaluator, &source, b"inputs-v1").unwrap();
    let changed = evaluate_gluon_with_inputs(&evaluator, &source, b"inputs-v2").unwrap();
    let legacy = evaluate_gluon(&source).unwrap();
    let typed =
        <GluonTriggerEvaluator as DeclarationEvaluator<Trigger>>::evaluate(
            &GluonTriggerEvaluator::default(),
            &source,
        )
        .unwrap();

    assert_eq!(first.fingerprint, repeated.fingerprint);
    assert_ne!(first.fingerprint.sha256, changed.fingerprint.sha256);
    assert_eq!(typed.identity, legacy.fingerprint);
    assert_eq!(
        normalized_trigger_value(&typed.value),
        normalized_trigger_value(&legacy.trigger)
    );
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

    let typed_evaluator = GluonTriggerEvaluator::default();
    let engine_error =
        <GluonTriggerEvaluator as DeclarationEvaluator<Trigger>>::evaluate(
            &typed_evaluator,
            &authored(
                r#"
let base = cast.trigger "wrong-type" "Wrong type"
{ description = 42, .. base }
"#,
            ),
        )
        .unwrap_err();
    assert!(matches!(
        engine_error,
        DeclarationEvaluationError::Evaluation(ref diagnostic)
            if diagnostic.category == DiagnosticCategory::Type
    ));

    let conversion_error =
        <GluonTriggerEvaluator as DeclarationEvaluator<Trigger>>::evaluate(
            &typed_evaluator,
            &authored(
                r#"
let base = cast.trigger "broken-pattern" "Invalid pattern"
{
    paths = [cast.path "/usr/lib/(unterminated" [] cast.optional.unset],
    .. base
}
"#,
            ),
        )
        .unwrap_err();
    assert!(matches!(
        conversion_error,
        DeclarationEvaluationError::Conversion(
            GluonTriggerConversionError::Trigger(ref error)
        ) if error.field() == "paths[0].key"
    ));

    let missing_abi =
        <GluonTriggerEvaluator as DeclarationEvaluator<Trigger>>::evaluate(
            &typed_evaluator,
            &Source::new(
                "manual-trigger.glu",
                r#"
type Optional a =
    | Unset
    | Set a

{
    name = "manual",
    description = "Does not import the trigger ABI",
    before = Unset,
    after = Unset,
    inhibitors = Unset,
    paths = [],
    handlers = [],
}
"#,
            ),
        )
        .unwrap_err();
    assert!(matches!(
        missing_abi,
        DeclarationEvaluationError::Conversion(
            GluonTriggerConversionError::MissingAbiImport
        )
    ));
}
