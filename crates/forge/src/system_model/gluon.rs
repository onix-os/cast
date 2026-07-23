//! Versioned Gluon boundary for authored Cast system intent and snapshots.

use declarative_config::{
    DeclarationCodec, DeclarationEvaluationError, DeclarationEvaluator,
    Evaluation as DeclarationEvaluation, LanguageSpec, Limits, SourceRoot,
};
use gluon_config::{Diagnostic, EvaluationFingerprint, GluonEngine, ImportPolicy, Source};

use super::{SystemModel, spec};

mod encoding;

pub use self::encoding::GENERATED_GLUON_MARKER;

pub const SYSTEM_ABI_VERSION: u32 = 1;
pub const GLUON_SYSTEM_ABI: &str = include_str!("../../gluon/system.glu");

/// Owned authored source and its normalized generated system model.
#[derive(Debug, Clone)]
pub(crate) struct SystemIntentDeclaration {
    pub(crate) authored_source: String,
    pub(crate) model: SystemModel,
}

/// Stateful Gluon adapter for authored system intent.
#[derive(Debug, Clone)]
pub(crate) struct SystemIntentEvaluator {
    engine: GluonEngine,
}

impl Default for SystemIntentEvaluator {
    fn default() -> Self {
        Self::new(Limits::default())
    }
}

impl SystemIntentEvaluator {
    pub(crate) fn new(limits: Limits) -> Self {
        Self {
            engine: configured_engine(GluonEngine::new(limits))
                .expect("the embedded system ABI is valid and unique"),
        }
    }

}

/// Stateful Gluon codec for canonical generated system snapshots.
#[derive(Debug, Clone)]
pub(crate) struct SystemSnapshotCodec {
    engine: GluonEngine,
}

impl Default for SystemSnapshotCodec {
    fn default() -> Self {
        Self::new(Limits::default())
    }
}

impl SystemSnapshotCodec {
    pub(crate) fn new(limits: Limits) -> Self {
        Self {
            engine: configured_engine(GluonEngine::new(limits))
                .expect("the embedded system ABI is valid and unique"),
        }
    }

    pub(super) fn encode_normalized(value: &spec::SystemSpec) -> String {
        encoding::encode_generated(value)
    }
}

pub(super) fn is_generated_snapshot(source: &str) -> bool {
    encoding::is_generated(source)
}

pub(super) fn generated_source_fingerprint(source: &str) -> Option<String> {
    encoding::source_fingerprint(source)
}

pub(super) fn with_source_fingerprint(
    generated: &str,
    source_fingerprint: &str,
) -> String {
    encoding::with_source_fingerprint(generated, source_fingerprint)
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
enum GluonOptional<T> {
    None,
    Some(T),
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
enum GluonBool {
    False,
    True,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonSystemSpec {
    disable_warning: GluonBool,
    repositories: Vec<GluonRepositorySpec>,
    packages: Vec<String>,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonRepositorySpec {
    id: String,
    description: GluonOptional<String>,
    source: GluonRepositorySourceSpec,
    priority: GluonOptional<i64>,
    enabled: GluonOptional<GluonBool>,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
enum GluonRepositorySourceSpec {
    DirectIndex {
        uri: String,
    },
    RootIndex {
        base_uri: String,
        channel: GluonOptional<String>,
        version: String,
        arch: GluonOptional<String>,
    },
}

impl<T> From<GluonOptional<T>> for Option<T> {
    fn from(value: GluonOptional<T>) -> Self {
        match value {
            GluonOptional::None => None,
            GluonOptional::Some(value) => Some(value),
        }
    }
}

impl From<GluonBool> for bool {
    fn from(value: GluonBool) -> Self {
        match value {
            GluonBool::False => false,
            GluonBool::True => true,
        }
    }
}

impl From<GluonSystemSpec> for spec::SystemSpec {
    fn from(value: GluonSystemSpec) -> Self {
        Self {
            disable_warning: value.disable_warning.into(),
            repositories: value.repositories.into_iter().map(Into::into).collect(),
            packages: value.packages,
        }
    }
}

impl From<GluonRepositorySpec> for spec::RepositorySpec {
    fn from(value: GluonRepositorySpec) -> Self {
        Self {
            id: value.id,
            description: value.description.into(),
            source: value.source.into(),
            priority: value.priority.into(),
            enabled: Option::<GluonBool>::from(value.enabled).map(Into::into),
        }
    }
}

impl From<GluonRepositorySourceSpec> for spec::RepositorySourceSpec {
    fn from(value: GluonRepositorySourceSpec) -> Self {
        match value {
            GluonRepositorySourceSpec::DirectIndex { uri } => Self::DirectIndex { uri },
            GluonRepositorySourceSpec::RootIndex {
                base_uri,
                channel,
                version,
                arch,
            } => Self::RootIndex {
                base_uri,
                channel: channel.into(),
                version,
                arch: arch.into(),
            },
        }
    }
}

impl DeclarationEvaluator<SystemIntentDeclaration> for SystemIntentEvaluator {
    type Identity = EvaluationFingerprint;
    type Error = spec::ConversionError;

    fn language_spec(&self) -> &LanguageSpec {
        self.engine.language_spec()
    }

    fn limits(&self) -> Limits {
        self.engine.limits()
    }

    fn with_source_root(&self, source_root: SourceRoot) -> Self {
        Self {
            engine: self.engine.clone().with_source_root(source_root),
        }
    }

    fn evaluate(
        &self,
        source: &Source,
    ) -> Result<
        DeclarationEvaluation<SystemIntentDeclaration, Self::Identity>,
        DeclarationEvaluationError<Self::Error>,
    > {
        let authored_source = source.text().to_owned();
        let evaluated = evaluate_spec(&self.engine, source)?;
        let parts = spec::into_domain(evaluated.value)
            .map_err(DeclarationEvaluationError::Conversion)?;
        let model = SystemModel::regenerate(parts)?;

        Ok(DeclarationEvaluation {
            value: SystemIntentDeclaration {
                authored_source,
                model,
            },
            identity: evaluated.identity,
        })
    }
}

impl DeclarationEvaluator<SystemModel> for SystemSnapshotCodec {
    type Identity = EvaluationFingerprint;
    type Error = spec::ConversionError;

    fn language_spec(&self) -> &LanguageSpec {
        self.engine.language_spec()
    }

    fn limits(&self) -> Limits {
        self.engine.limits()
    }

    fn with_source_root(&self, source_root: SourceRoot) -> Self {
        Self {
            engine: self.engine.clone().with_source_root(source_root),
        }
    }

    fn evaluate(
        &self,
        source: &Source,
    ) -> Result<
        DeclarationEvaluation<SystemModel, Self::Identity>,
        DeclarationEvaluationError<Self::Error>,
    > {
        let source_text = source.text().to_owned();
        let evaluated = evaluate_spec(&self.engine, source)?;
        let parts = spec::into_domain(evaluated.value)
            .map_err(DeclarationEvaluationError::Conversion)?;
        let identity = evaluated.identity;
        let model = SystemModel::from_generated(
            parts,
            source_text,
            identity.clone(),
        );

        Ok(DeclarationEvaluation {
            value: model,
            identity,
        })
    }
}

impl DeclarationCodec<SystemModel> for SystemSnapshotCodec {
    fn encode(&self, value: &SystemModel) -> Result<String, Self::Error> {
        // The owned model already contains the exact canonical snapshot,
        // including any authored-source annotation. Re-emitting the semantic
        // value here would erase that provenance and break two-file metadata
        // publication byte identity.
        Ok(value.encoded().to_owned())
    }
}

pub fn import_policy() -> Result<ImportPolicy, Diagnostic> {
    ImportPolicy::new().with_embedded_module("cast.system.v1", GLUON_SYSTEM_ABI)
}

fn configured_engine(evaluator: GluonEngine) -> Result<GluonEngine, Diagnostic> {
    let mut policy = evaluator.import_policy().clone();
    policy.insert_embedded_module("cast.system.v1", GLUON_SYSTEM_ABI)?;
    Ok(evaluator.with_import_policy(policy))
}

fn evaluate_spec(
    evaluator: &GluonEngine,
    source: &Source,
) -> Result<
    DeclarationEvaluation<spec::SystemSpec, EvaluationFingerprint>,
    DeclarationEvaluationError<spec::ConversionError>,
> {
    let evaluation = evaluator
        .evaluate::<GluonSystemSpec>(source)
        .map_err(DeclarationEvaluationError::Evaluation)?;

    Ok(DeclarationEvaluation {
        value: spec::SystemSpec::from(evaluation.value),
        identity: evaluation.fingerprint,
    })
}

#[cfg(test)]
mod tests {
    use gluon_config::{DiagnosticCategory, Source};

    use super::*;
    use crate::{Provider, repository};

    #[derive(Debug)]
    struct EvaluatedSystem {
        model: SystemModel,
        fingerprint: EvaluationFingerprint,
    }

    fn evaluate(
        source: &Source,
    ) -> Result<
        EvaluatedSystem,
        DeclarationEvaluationError<spec::ConversionError>,
    > {
        let evaluated = <SystemIntentEvaluator as DeclarationEvaluator<
            SystemIntentDeclaration,
        >>::evaluate(&SystemIntentEvaluator::default(), source)?;
        Ok(EvaluatedSystem {
            model: evaluated.value.model,
            fingerprint: evaluated.identity,
        })
    }

    fn evaluate_generated_snapshot(
        source: &Source,
    ) -> Result<
        SystemModel,
        DeclarationEvaluationError<spec::ConversionError>,
    > {
        <SystemSnapshotCodec as DeclarationEvaluator<SystemModel>>::evaluate(
            &SystemSnapshotCodec::default(),
            source,
        )
        .map(|evaluation| evaluation.value)
    }

    fn authored(body: &str) -> Source {
        Source::new("system.glu", format!("let cast = import! cast.system.v1\n{body}"))
    }

    fn complete_normalized_system_value() -> spec::SystemSpec {
        spec::SystemSpec {
            disable_warning: true,
            repositories: vec![
                spec::RepositorySpec {
                    id: "a-direct".to_owned(),
                    description: Some("line \"quoted\"\npath\\leaf\t雪".to_owned()),
                    source: spec::RepositorySourceSpec::DirectIndex {
                        uri: "file:///var/cache/local.index".to_owned(),
                    },
                    priority: Some(5),
                    enabled: Some(false),
                },
                spec::RepositorySpec {
                    id: "z-root".to_owned(),
                    description: Some(String::new()),
                    source: spec::RepositorySourceSpec::RootIndex {
                        base_uri: "https://packages.example.test/".to_owned(),
                        channel: Some("main".to_owned()),
                        version: "stream/volatile".to_owned(),
                        arch: Some("x86_64".to_owned()),
                    },
                    priority: Some(0),
                    enabled: Some(true),
                },
            ],
            packages: ["alpha", "binary(tool)", "soname(libc.so.6)"].map(str::to_owned).to_vec(),
        }
    }

    #[test]
    fn empty_snapshot_bytes_are_exact_and_versioned() {
        assert_eq!(SYSTEM_ABI_VERSION, 1);
        assert_eq!(
            encoding::encode_generated(&spec::SystemSpec::default()),
            concat!(
                "// @generated by cast. DO NOT EDIT.\n",
                "// Canonical standalone SystemSpec snapshot (ABI 1).\n",
                "type Bool =\n",
                "    | False\n",
                "    | True\n",
                "\n",
                "type Option a =\n",
                "    | None\n",
                "    | Some a\n",
                "\n",
                "type RepositorySourceSpec =\n",
                "    | DirectIndex { uri : String }\n",
                "    | RootIndex {\n",
                "        base_uri : String,\n",
                "        channel : Option String,\n",
                "        version : String,\n",
                "        arch : Option String,\n",
                "    }\n",
                "\n",
                "type RepositorySpec = {\n",
                "    id : String,\n",
                "    description : Option String,\n",
                "    source : RepositorySourceSpec,\n",
                "    priority : Option Int,\n",
                "    enabled : Option Bool,\n",
                "}\n",
                "\n",
                "type SystemSpec = {\n",
                "    disable_warning : Bool,\n",
                "    repositories : Array RepositorySpec,\n",
                "    packages : Array String,\n",
                "}\n",
                "\n",
                "{\n",
                "    disable_warning = False,\n",
                "    repositories = [\n",
                "    ],\n",
                "    packages = [\n",
                "    ],\n",
                "}\n",
            )
        );
    }

    #[test]
    fn complete_snapshot_matches_the_frozen_golden() {
        assert_eq!(
            encoding::encode_generated(&complete_normalized_system_value()).as_bytes(),
            include_bytes!(
                "../../../../tests/fixtures/gluon/goldens/system-snapshot.glu"
            )
        );
    }

    #[test]
    fn generated_snapshot_is_marked_escaped_and_order_independent() {
        let mut value = complete_normalized_system_value();
        value.repositories.reverse();
        value.packages.reverse();
        let encoded = encoding::encode_generated(&value);

        assert_eq!(
            encoded,
            encoding::encode_generated(&complete_normalized_system_value())
        );
        assert!(encoding::is_generated(&encoded));
        assert!(!encoded.contains("import!"));
        assert!(encoded.contains("type RepositorySourceSpec ="));
        assert!(encoded.contains("type SystemSpec ="));
        assert!(encoded.contains("description = Some \"line \\\"quoted\\\"\\npath\\\\leaf\\t雪\","));
        assert!(
            encoded.find("id = \"a-direct\"").unwrap()
                < encoded.find("id = \"z-root\"").unwrap()
        );
        assert!(
            encoded.find("\"alpha\"").unwrap()
                < encoded.find("\"soname(libc.so.6)\"").unwrap()
        );
    }

    #[test]
    fn authored_source_fingerprint_annotation_round_trips_exactly() {
        let fingerprint =
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let generated = encoding::encode_generated(&spec::SystemSpec::default());
        let annotated = encoding::with_source_fingerprint(&generated, fingerprint);

        assert_eq!(
            encoding::source_fingerprint(&annotated).as_deref(),
            Some(fingerprint)
        );
        assert_eq!(
            annotated,
            format!(
                "{}// Authored source fingerprint: {fingerprint}\n{}",
                GENERATED_GLUON_MARKER,
                generated
                    .strip_prefix(GENERATED_GLUON_MARKER)
                    .unwrap()
            )
        );
    }

    #[test]
    fn documented_system_example_remains_loadable() {
        let source = Source::new(
            "docs/examples/gluon/system.glu",
            include_str!("../../../../docs/examples/gluon/system.glu"),
        );
        let evaluated = evaluate(&source).unwrap();

        assert!(
            evaluated
                .model
                .repositories
                .get(&repository::Id::new("local"))
                .is_some()
        );
        assert!(evaluated.model.packages.contains(&Provider::package_name("editor")));
    }

    #[test]
    fn evaluates_empty_and_populated_system_intent() {
        let empty = evaluate(&authored("cast.system")).unwrap();
        assert!(empty.model.repositories.iter().next().is_none());
        assert!(empty.model.packages.is_empty());
        assert_eq!(empty.fingerprint.imported_modules[0].logical_name, "cast.system.v1");

        let populated = evaluate(&authored(
            r#"
{
    disable_warning = cast.boolean.true,
    repositories = [
        cast.repository.direct_with {
            id = "local",
            description = cast.optional.some "local packages",
            uri = "file:///var/cache/local.index",
            priority = cast.optional.some 5,
            enabled = cast.optional.some cast.boolean.false,
        },
        cast.repository.root "volatile" "https://packages.example.test" "stream/volatile",
    ],
    packages = ["cast", "soname(libc.so.6)"],
}
"#,
        ))
        .unwrap();

        assert!(populated.model.disable_warning);
        let local = populated.model.repositories.get(&repository::Id::new("local")).unwrap();
        assert_eq!(local.description, "local packages");
        assert_eq!(u64::from(local.priority), 5);
        assert!(!local.active);
        assert!(populated.model.packages.contains(&Provider::package_name("cast")));
    }

    #[test]
    fn authored_intent_and_generated_snapshot_share_the_complete_normalized_value() {
        let authored = evaluate(&authored(
            r#"
{
    disable_warning = cast.boolean.true,
    repositories = [
        cast.repository.root "z-root" "https://packages.example.test" "stream/volatile",
        cast.repository.direct_with {
            id = "a-direct",
            description = cast.optional.some "line \"quoted\"\npath\\leaf\t雪",
            uri = "file:///var/cache/local.index",
            priority = cast.optional.some 5,
            enabled = cast.optional.some cast.boolean.false,
        },
    ],
    packages = ["soname(libc.so.6)", "alpha", "binary(tool)"],
}
"#,
        ))
        .unwrap();
        let authored_value = spec::SystemSpec::try_from(&authored.model).unwrap();

        let snapshot = evaluate_generated_snapshot(&Source::new(
            "system-model.glu",
            include_str!("../../../../tests/fixtures/gluon/goldens/system-snapshot.glu"),
        ))
        .unwrap();
        let snapshot_value = spec::SystemSpec::try_from(&snapshot).unwrap();
        let expected = complete_normalized_system_value();

        assert_eq!(authored_value, expected);
        assert_eq!(snapshot_value, expected);
    }

    #[test]
    fn generated_snapshot_evaluates_without_imports_and_round_trips() {
        let authored = evaluate(&authored(
            r#"
{
    repositories = [cast.repository.root "volatile" "https://packages.example.test" "stream/volatile"],
    packages = ["cast"],
    .. cast.system
}
"#,
        ))
        .unwrap();
        let normalized = spec::SystemSpec::try_from(&authored.model).unwrap();
        let generated = SystemSnapshotCodec::encode_normalized(&normalized);

        let evaluated = GluonEngine::default()
            .evaluate::<GluonSystemSpec>(&Source::new("system-model.glu", generated))
            .unwrap();
        let round_trip = SystemModel::try_from(spec::SystemSpec::from(evaluated.value)).unwrap();
        let round_trip = spec::SystemSpec::try_from(&round_trip).unwrap();

        assert_eq!(round_trip, normalized);
    }

    #[test]
    fn invalid_values_and_types_have_visible_paths() {
        let invalid_priority = evaluate(&authored(
            r#"
{
    repositories = [cast.repository.direct_with {
        id = "bad",
        description = cast.optional.none,
        uri = "https://example.test/index.stone",
        priority = cast.optional.some (-1),
        enabled = cast.optional.none,
    }],
    .. cast.system
}
"#,
        ))
        .unwrap_err();
        assert!(
            matches!(invalid_priority, DeclarationEvaluationError::Conversion(ref error) if error.path() == "repositories[0].priority")
        );

        let wrong_type = evaluate(&authored("{ packages = [1], .. cast.system }")).unwrap_err();
        assert!(
            matches!(wrong_type, DeclarationEvaluationError::Evaluation(ref error) if error.category == DiagnosticCategory::Type)
        );

        let unknown = evaluate(&authored("{ package = [], .. cast.system }")).unwrap_err();
        assert!(
            matches!(unknown, DeclarationEvaluationError::Evaluation(ref error) if error.category == DiagnosticCategory::Type)
        );
    }

    #[test]
    fn forbidden_effects_fail_and_fingerprints_are_deterministic() {
        let forbidden = evaluate(&authored("let _ = import! std.fs\ncast.system")).unwrap_err();
        assert!(
            matches!(forbidden, DeclarationEvaluationError::Evaluation(ref error) if error.category == DiagnosticCategory::Import)
        );

        let source = authored("cast.system");
        let first = evaluate(&source).unwrap();
        let repeated = evaluate(&source).unwrap();
        assert_eq!(first.fingerprint, repeated.fingerprint);
    }

    #[test]
    fn typed_adapters_preserve_v1_identity_and_canonical_snapshot_bytes() {
        let source = authored("cast.system");
        let intent = <SystemIntentEvaluator as DeclarationEvaluator<
            SystemIntentDeclaration,
        >>::evaluate(&SystemIntentEvaluator::default(), &source)
        .unwrap();

        assert_eq!(intent.value.authored_source, source.text());
        assert_eq!(intent.identity.gluon_version, "0.18.3");
        assert_eq!(intent.identity.configuration_abi_version, 1);
        assert_eq!(intent.identity.evaluator_policy_version, 1);
        intent.identity.validate().unwrap();

        let model = SystemModel::try_from(complete_normalized_system_value()).unwrap();
        let codec = SystemSnapshotCodec::default();
        let encoded = codec.encode(&model).unwrap();
        let golden = include_str!(
            "../../../../tests/fixtures/gluon/goldens/system-snapshot.glu"
        );

        assert_eq!(encoded, golden);
        let decoded = <SystemSnapshotCodec as DeclarationEvaluator<SystemModel>>::evaluate(
            &codec,
            &Source::new("system-model.glu", encoded.clone()),
        )
        .unwrap();
        assert_eq!(decoded.value.encoded(), encoded);
        assert_eq!(&decoded.identity, model.fingerprint());
        decoded.identity.validate().unwrap();
    }

    #[test]
    fn typed_intent_adapter_keeps_engine_and_conversion_failures_distinct() {
        let evaluator = SystemIntentEvaluator::default();
        let wrong_type = <SystemIntentEvaluator as DeclarationEvaluator<
            SystemIntentDeclaration,
        >>::evaluate(
            &evaluator,
            &authored("{ packages = [1], .. cast.system }"),
        )
        .unwrap_err();
        assert!(matches!(
            wrong_type,
            DeclarationEvaluationError::Evaluation(ref error)
                if error.category == DiagnosticCategory::Type
        ));

        let invalid_priority = <SystemIntentEvaluator as DeclarationEvaluator<
            SystemIntentDeclaration,
        >>::evaluate(
            &evaluator,
            &authored(
                r#"
{
    repositories = [cast.repository.direct_with {
        id = "bad",
        description = cast.optional.none,
        uri = "https://example.test/index.stone",
        priority = cast.optional.some (-1),
        enabled = cast.optional.none,
    }],
    .. cast.system
}
"#,
            ),
        )
        .unwrap_err();
        assert!(matches!(
            invalid_priority,
            DeclarationEvaluationError::Conversion(ref error)
                if error.path() == "repositories[0].priority"
        ));

        let bounded = SystemIntentEvaluator::new(Limits {
            max_source_bytes: 2,
            ..Limits::default()
        });
        let oversized = <SystemIntentEvaluator as DeclarationEvaluator<
            SystemIntentDeclaration,
        >>::evaluate(&bounded, &authored("cast.system"))
        .unwrap_err();
        assert!(matches!(
            oversized,
            DeclarationEvaluationError::Evaluation(ref error)
                if error.category == DiagnosticCategory::Limit
        ));
    }
}
