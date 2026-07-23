//! Compile-time proof for the production declaration adapter boundary.

use std::{error::Error, fmt};

use config::GENERATED_GLUON_MARKER;
use declarative_config::{
    DeclarationCodec, DeclarationEvaluator, EngineId, Evaluation, LanguageId,
    LanguageSpec, Source,
};

#[derive(Debug, Clone, PartialEq, Eq)]
struct EvaluationIdentity {
    language: LanguageId,
    engine: EngineId,
    logical_source: String,
    source_hash: [u8; 32],
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
    type Identity = EvaluationIdentity;
    type Error = FixtureError;

    fn language_spec(&self) -> &LanguageSpec {
        &self.language
    }

    fn evaluate(
        &self,
        source: &Source,
    ) -> Result<Evaluation<TriggerDeclaration, Self::Identity>, Self::Error> {
        let (name, command) = source
            .text()
            .split_once(':')
            .ok_or(FixtureError("trigger shape"))?;
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
    type Identity = EvaluationIdentity;
    type Error = FixtureError;

    fn language_spec(&self) -> &LanguageSpec {
        &self.language
    }

    fn evaluate(
        &self,
        source: &Source,
    ) -> Result<Evaluation<ProfileFragment, Self::Identity>, Self::Error> {
        let (name, packages) = source
            .text()
            .split_once('=')
            .ok_or(FixtureError("fragment shape"))?;
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
        LanguageId::new("gluon").expect("fixture language is canonical"),
        EngineId::new("gluon-vm", "0.18.3").expect("fixture engine is canonical"),
        "glu",
        "declaration-v1",
        GENERATED_GLUON_MARKER,
    )
    .expect("fixture descriptors are canonical")
}

fn fixture_identity(spec: &LanguageSpec, source: &Source) -> EvaluationIdentity {
    let mut source_hash = [0; 32];
    for (index, byte) in source.text().bytes().enumerate() {
        source_hash[index % source_hash.len()] ^= byte;
    }
    EvaluationIdentity {
        language: spec.language().clone(),
        engine: spec.engine().clone(),
        logical_source: source.logical_name().to_owned(),
        source_hash,
    }
}

fn evaluate_read_only<T, E>(
    evaluator: &E,
    source: &Source,
) -> Result<Evaluation<T, E::Identity>, E::Error>
where
    E: DeclarationEvaluator<T>,
{
    evaluator.evaluate(source)
}

fn round_trip_writable<T, C>(
    codec: &C,
    source: &Source,
) -> Result<Evaluation<T, C::Identity>, C::Error>
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
    assert_eq!(language.language().as_str(), "gluon");
    assert_eq!(language.engine().implementation(), "gluon-vm");
    assert_eq!(language.engine().version(), "0.18.3");
    assert_eq!(language.extension(), "glu");
    assert_eq!(language.source_profile(), "declaration-v1");
    assert_eq!(language.generated_marker(), GENERATED_GLUON_MARKER);

    assert!(LanguageId::new("").is_err());
    assert!(LanguageId::new("Gluon").is_err());
    assert!(EngineId::new("gluon vm", "0.18.3").is_err());
    assert!(EngineId::new("gluon-vm", "").is_err());
    let valid_language = LanguageId::new("gluon").unwrap();
    let valid_engine = EngineId::new("gluon-vm", "0.18.3").unwrap();
    assert!(
        LanguageSpec::new(
            valid_language.clone(),
            valid_engine.clone(),
            ".glu",
            "v1",
            "generated\n",
        )
        .is_err()
    );
    assert!(
        LanguageSpec::new(
            valid_language.clone(),
            valid_engine.clone(),
            "glu",
            "v1",
            "missing newline",
        )
        .is_err()
    );
    assert!(
        LanguageSpec::new(
            valid_language,
            valid_engine,
            "glu",
            "v1",
            "line\nbreak\n",
        )
        .is_err()
    );
}

#[test]
fn read_only_trigger_needs_only_a_typed_evaluator() {
    let evaluation = evaluate_read_only(
        &TriggerEvaluator::new(),
        &Source::new("tx.d/rebuild.glu", "rebuild:/usr/bin/rebuild-cache"),
    )
    .expect("read-only trigger evaluates");

    assert_eq!(evaluation.value.name, "rebuild");
    assert_eq!(evaluation.value.command, "/usr/bin/rebuild-cache");
    assert_eq!(evaluation.identity.logical_source, "tx.d/rebuild.glu");
}

#[test]
fn writable_fragment_adds_a_codec_without_changing_evaluation() {
    let codec = ProfileCodec::new();
    let source = Source::new("profiles.d/workstation.glu", "workstation=base,desktop");
    let evaluation = round_trip_writable(&codec, &source).expect("writable fragment round trips");

    assert_eq!(evaluation.value.name, "workstation");
    assert_eq!(evaluation.value.packages, ["base", "desktop"]);
    assert_eq!(codec.encode(&evaluation.value).unwrap(), source.text());
}

#[test]
fn evaluation_owns_the_domain_value_and_identity() {
    let evaluation = {
        let evaluator = TriggerEvaluator::new();
        evaluate_read_only(
            &evaluator,
            &Source::new("sys.d/reindex.glu", "reindex:/usr/bin/reindex"),
        )
        .unwrap()
    };

    assert_eq!(evaluation.value.name, "reindex");
    assert_eq!(evaluation.identity.language.as_str(), "gluon");
}
