//! Compile-time prototype for the declaration adapter boundary.
//!
//! This module is deliberately test-only. It proves the generic shape before
//! production code moves out of the Gluon-specific implementation.

use std::{error::Error, fmt};

use config::GENERATED_GLUON_MARKER;

#[derive(Debug, Clone, PartialEq, Eq)]
struct DescriptorError {
    field: &'static str,
    value: String,
}

impl fmt::Display for DescriptorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "invalid {} descriptor `{}`", self.field, self.value)
    }
}

impl Error for DescriptorError {}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CanonicalName(String);

impl CanonicalName {
    fn parse(field: &'static str, value: impl Into<String>) -> Result<Self, DescriptorError> {
        let value = value.into();
        let valid = !value.is_empty()
            && value.bytes().enumerate().all(|(index, byte)| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || (index > 0 && matches!(byte, b'-' | b'_' | b'.'))
            });
        if !valid {
            return Err(DescriptorError { field, value });
        }
        Ok(Self(value))
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LanguageId(CanonicalName);

impl LanguageId {
    fn parse(value: impl Into<String>) -> Result<Self, DescriptorError> {
        CanonicalName::parse("language", value).map(Self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EngineId {
    implementation: CanonicalName,
    version: CanonicalName,
}

impl EngineId {
    fn parse(implementation: impl Into<String>, version: impl Into<String>) -> Result<Self, DescriptorError> {
        Ok(Self {
            implementation: CanonicalName::parse("engine implementation", implementation)?,
            version: CanonicalName::parse("engine version", version)?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LanguageSpec {
    language: LanguageId,
    engine: EngineId,
    extension: CanonicalName,
    source_profile: CanonicalName,
    generated_marker: String,
}

impl LanguageSpec {
    fn new(
        language: impl Into<String>,
        engine: impl Into<String>,
        version: impl Into<String>,
        extension: impl Into<String>,
        source_profile: impl Into<String>,
        generated_marker: impl Into<String>,
    ) -> Result<Self, DescriptorError> {
        let generated_marker = generated_marker.into();
        if generated_marker.is_empty()
            || generated_marker.contains('\r')
            || !generated_marker.ends_with('\n')
            || generated_marker[..generated_marker.len() - 1].contains('\n')
        {
            return Err(DescriptorError {
                field: "generated marker",
                value: generated_marker,
            });
        }

        Ok(Self {
            language: LanguageId::parse(language)?,
            engine: EngineId::parse(engine, version)?,
            extension: CanonicalName::parse("extension", extension)?,
            source_profile: CanonicalName::parse("source profile", source_profile)?,
            generated_marker,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EvaluationIdentity {
    language: LanguageId,
    engine: EngineId,
    logical_source: String,
    source_hash: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Evaluation<T> {
    value: T,
    identity: EvaluationIdentity,
}

#[derive(Debug, Clone, Copy)]
struct DeclarationSource<'a> {
    logical_name: &'a str,
    text: &'a str,
}

trait DeclarationEvaluator<T> {
    type Error: Error + Send + Sync + 'static;

    fn language_spec(&self) -> &LanguageSpec;
    fn evaluate(&self, source: DeclarationSource<'_>) -> Result<Evaluation<T>, Self::Error>;
}

trait DeclarationCodec<T>: DeclarationEvaluator<T> {
    fn encode(&self, value: &T) -> Result<String, Self::Error>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TriggerDeclaration {
    name: String,
    command: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProfileFragment {
    name: String,
    packages: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FixtureError(&'static str);

impl fmt::Display for FixtureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0)
    }
}

impl Error for FixtureError {}

struct TriggerEvaluator {
    language: LanguageSpec,
}

impl TriggerEvaluator {
    fn new() -> Self {
        Self {
            language: gluon_language(),
        }
    }
}

impl DeclarationEvaluator<TriggerDeclaration> for TriggerEvaluator {
    type Error = FixtureError;

    fn language_spec(&self) -> &LanguageSpec {
        &self.language
    }

    fn evaluate(&self, source: DeclarationSource<'_>) -> Result<Evaluation<TriggerDeclaration>, Self::Error> {
        let (name, command) = source.text.split_once(':').ok_or(FixtureError("trigger shape"))?;
        Ok(Evaluation {
            value: TriggerDeclaration {
                name: name.to_owned(),
                command: command.to_owned(),
            },
            identity: fixture_identity(self.language_spec(), source),
        })
    }
}

struct ProfileCodec {
    language: LanguageSpec,
}

impl ProfileCodec {
    fn new() -> Self {
        Self {
            language: gluon_language(),
        }
    }
}

impl DeclarationEvaluator<ProfileFragment> for ProfileCodec {
    type Error = FixtureError;

    fn language_spec(&self) -> &LanguageSpec {
        &self.language
    }

    fn evaluate(&self, source: DeclarationSource<'_>) -> Result<Evaluation<ProfileFragment>, Self::Error> {
        let (name, packages) = source.text.split_once('=').ok_or(FixtureError("fragment shape"))?;
        Ok(Evaluation {
            value: ProfileFragment {
                name: name.to_owned(),
                packages: packages.split(',').map(str::to_owned).collect(),
            },
            identity: fixture_identity(self.language_spec(), source),
        })
    }
}

impl DeclarationCodec<ProfileFragment> for ProfileCodec {
    fn encode(&self, value: &ProfileFragment) -> Result<String, Self::Error> {
        Ok(format!("{}={}", value.name, value.packages.join(",")))
    }
}

fn gluon_language() -> LanguageSpec {
    LanguageSpec::new(
        "gluon",
        "gluon-vm",
        "0.18.3",
        "glu",
        "declaration-v1",
        GENERATED_GLUON_MARKER,
    )
    .expect("fixture descriptors are canonical")
}

fn fixture_identity(spec: &LanguageSpec, source: DeclarationSource<'_>) -> EvaluationIdentity {
    let mut source_hash = [0; 32];
    for (index, byte) in source.text.bytes().enumerate() {
        source_hash[index % source_hash.len()] ^= byte;
    }
    EvaluationIdentity {
        language: spec.language.clone(),
        engine: spec.engine.clone(),
        logical_source: source.logical_name.to_owned(),
        source_hash,
    }
}

fn evaluate_read_only<T, E>(evaluator: &E, source: DeclarationSource<'_>) -> Result<Evaluation<T>, E::Error>
where
    E: DeclarationEvaluator<T>,
{
    evaluator.evaluate(source)
}

fn round_trip_writable<T, C>(codec: &C, source: DeclarationSource<'_>) -> Result<Evaluation<T>, C::Error>
where
    C: DeclarationCodec<T>,
{
    let evaluation = codec.evaluate(source)?;
    let _canonical_source = codec.encode(&evaluation.value)?;
    Ok(evaluation)
}

#[test]
fn language_and_engine_descriptors_reject_noncanonical_values() {
    let language = gluon_language();
    assert_eq!(language.language.0.as_str(), "gluon");
    assert_eq!(language.engine.implementation.as_str(), "gluon-vm");
    assert_eq!(language.engine.version.as_str(), "0.18.3");
    assert_eq!(language.extension.as_str(), "glu");
    assert_eq!(language.source_profile.as_str(), "declaration-v1");
    assert_eq!(language.generated_marker, GENERATED_GLUON_MARKER);

    assert!(LanguageId::parse("").is_err());
    assert!(LanguageId::parse("Gluon").is_err());
    assert!(EngineId::parse("gluon vm", "0.18.3").is_err());
    assert!(EngineId::parse("gluon-vm", "").is_err());
    assert!(LanguageSpec::new("gluon", "gluon-vm", "0.18.3", ".glu", "v1", "generated\n").is_err());
    assert!(LanguageSpec::new("gluon", "gluon-vm", "0.18.3", "glu", "v1", "missing newline").is_err());
    assert!(LanguageSpec::new("gluon", "gluon-vm", "0.18.3", "glu", "v1", "line\nbreak\n").is_err());
}

#[test]
fn read_only_trigger_needs_only_a_typed_evaluator() {
    let evaluation = evaluate_read_only(
        &TriggerEvaluator::new(),
        DeclarationSource {
            logical_name: "tx.d/rebuild.glu",
            text: "rebuild:/usr/bin/rebuild-cache",
        },
    )
    .expect("read-only trigger evaluates");

    assert_eq!(evaluation.value.name, "rebuild");
    assert_eq!(evaluation.value.command, "/usr/bin/rebuild-cache");
    assert_eq!(evaluation.identity.logical_source, "tx.d/rebuild.glu");
}

#[test]
fn writable_fragment_adds_a_codec_without_changing_evaluation() {
    let codec = ProfileCodec::new();
    let source = DeclarationSource {
        logical_name: "profiles.d/workstation.glu",
        text: "workstation=base,desktop",
    };
    let evaluation = round_trip_writable(&codec, source).expect("writable fragment round trips");

    assert_eq!(evaluation.value.name, "workstation");
    assert_eq!(evaluation.value.packages, ["base", "desktop"]);
    assert_eq!(codec.encode(&evaluation.value).unwrap(), source.text);
}

#[test]
fn evaluation_owns_the_domain_value_and_identity() {
    let evaluation = {
        let evaluator = TriggerEvaluator::new();
        evaluate_read_only(
            &evaluator,
            DeclarationSource {
                logical_name: "sys.d/reindex.glu",
                text: "reindex:/usr/bin/reindex",
            },
        )
        .unwrap()
    };

    assert_eq!(evaluation.value.name, "reindex");
    assert_eq!(evaluation.identity.language.0.as_str(), "gluon");
}
