use declarative_config::{
    DeclarationEvaluationError, DeclarationEvaluator,
    DeclarationInputEvaluator, Evaluation,
};
use gluon_config::{DiagnosticCategory, EvaluationIdentity, Source};
use fnmatch::Pattern;
use triggers::{
    Collection, GluonTriggerConversionError, GluonTriggerEvaluator,
    TRIGGER_ABI_VERSION,
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

fn evaluate_trigger(
    source: &Source,
) -> Result<
    Evaluation<Trigger, EvaluationIdentity>,
    DeclarationEvaluationError<GluonTriggerConversionError>,
> {
    <GluonTriggerEvaluator as DeclarationEvaluator<Trigger>>::evaluate(
        &GluonTriggerEvaluator::default(),
        source,
    )
}

fn evaluate_trigger_with_inputs(
    source: &Source,
    explicit_inputs: &[u8],
) -> Result<
    Evaluation<Trigger, EvaluationIdentity>,
    DeclarationEvaluationError<GluonTriggerConversionError>,
> {
    <GluonTriggerEvaluator as DeclarationInputEvaluator<Trigger>>::evaluate_with_inputs(
        &GluonTriggerEvaluator::default(),
        source,
        explicit_inputs,
    )
}

#[test]
fn retired_moss_trigger_abi_is_not_a_compatibility_alias() {
    let error = evaluate_trigger(&Source::new("retired-trigger.glu", "import! moss.trigger.v1")).unwrap_err();

    assert!(matches!(
        error,
        DeclarationEvaluationError::Evaluation(ref diagnostic)
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
    let evaluated = evaluate_trigger(&source).unwrap();

    assert_eq!(evaluated.value.name, "refresh-example");
    Collection::new([&evaluated.value]).unwrap();
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

    let evaluated = evaluate_trigger(&source).unwrap();
    let trigger = &evaluated.value;

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
    let evaluated = evaluate_trigger(&source).unwrap();

    let error = match Collection::new([&evaluated.value]) {
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

    let error = evaluate_trigger(&source).unwrap_err();
    assert!(matches!(
        error,
        DeclarationEvaluationError::Conversion(
            GluonTriggerConversionError::Trigger(ref error)
        )
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
        let error = evaluate_trigger(&authored(body)).unwrap_err();
        assert!(matches!(
            error,
            DeclarationEvaluationError::Evaluation(ref error)
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

    let error = evaluate_trigger(&source).unwrap_err();
    assert!(matches!(
        error,
        DeclarationEvaluationError::Evaluation(ref error)
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
    let first = evaluate_trigger_with_inputs(&source, b"inputs-v1").unwrap();
    let repeated = evaluate_trigger_with_inputs(&source, b"inputs-v1").unwrap();
    let changed = evaluate_trigger_with_inputs(&source, b"inputs-v2").unwrap();
    let typed = evaluate_trigger(&source).unwrap();

    assert_eq!(first.identity, repeated.identity);
    assert_ne!(first.identity.sha256, changed.identity.sha256);
    assert_eq!(
        normalized_trigger_value(&typed.value),
        normalized_trigger_value(&first.value)
    );
    assert_eq!(TRIGGER_ABI_VERSION, 1);
    assert_eq!(first.identity.configuration_abi.version(), "1");
    assert_eq!(
        first
            .identity
            .modules
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
