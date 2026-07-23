//! Gluon runtime and canonical serialization adapter for profiles.

use std::fmt::Write as _;

use config::declaration::ConfigDeclarationEvaluator;
use declarative_config::{
    DeclarationCodec, DeclarationEvaluationError, DeclarationEvaluator,
    Evaluation as DeclarationEvaluation, LanguageSpec, Limits, Source,
    SourceRoot,
};
use gluon_config::{EvaluationIdentity, GluonEngine, ImportPolicy};

use super::{
    Map, ProfileConversionError, ProfileSpec, RepositorySourceSpec,
    RepositorySpec, decode_specs, profile_to_spec,
};

/// Version of the embedded profile configuration API.
pub const PROFILE_ABI_VERSION: u32 = 1;

/// Pure definitions imported by authored fragments as `cast.profile.v1`.
pub const GLUON_PROFILE_ABI: &str = include_str!("../../gluon/profile.glu");

const STANDALONE_GLUON_TYPES: &str = r#"type Optional a =
    | None
    | Some a

type Boolean =
    | False
    | True

type RepositorySourceSpec =
    | DirectIndex { uri : String }
    | RootIndex {
        base_uri : String,
        channel : Optional String,
        version : String,
        arch : Optional String,
    }

type RepositorySpec = {
    id : String,
    description : Optional String,
    source : RepositorySourceSpec,
    priority : Optional Int,
    enabled : Optional Boolean,
}

type ProfileSpec = {
    id : String,
    repositories : Array RepositorySpec,
}

"#;

/// Stateful profile declaration adapter with its ABI fixed at construction.
#[derive(Debug, Clone)]
pub struct ProfileCodec {
    engine: GluonEngine,
}

impl Default for ProfileCodec {
    fn default() -> Self {
        Self::new(Limits::default())
    }
}

impl ProfileCodec {
    pub fn new(limits: Limits) -> Self {
        let mut import_policy = ImportPolicy::new();
        import_policy
            .insert_embedded_module("cast.profile.v1", GLUON_PROFILE_ABI)
            .expect("the embedded profile ABI is a valid, unique module");

        Self {
            engine: GluonEngine::new(limits).with_import_policy(import_policy),
        }
    }
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
struct GluonProfileSpec {
    id: String,
    repositories: Vec<GluonRepositorySpec>,
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

impl From<GluonProfileSpec> for ProfileSpec {
    fn from(value: GluonProfileSpec) -> Self {
        Self {
            id: value.id,
            repositories: value.repositories.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<GluonRepositorySpec> for RepositorySpec {
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

impl From<GluonRepositorySourceSpec> for RepositorySourceSpec {
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

impl DeclarationEvaluator<Map> for ProfileCodec {
    type Identity = EvaluationIdentity;
    type Error = ProfileConversionError;

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
        DeclarationEvaluation<Map, Self::Identity>,
        DeclarationEvaluationError<Self::Error>,
    > {
        let evaluation = self
            .engine
            .evaluate::<Vec<GluonProfileSpec>>(source)
            .map_err(DeclarationEvaluationError::Evaluation)?;
        let profiles = evaluation.value.into_iter().map(Into::into).collect();
        let value = decode_specs(profiles)
            .map_err(DeclarationEvaluationError::Conversion)?;

        Ok(DeclarationEvaluation {
            value,
            identity: evaluation.identity,
        })
    }
}

impl ConfigDeclarationEvaluator for ProfileCodec {
    type Config = Map;
}

impl DeclarationCodec<Map> for ProfileCodec {
    fn encode(&self, config: &Map) -> Result<String, Self::Error> {
        let specs = config
            .iter()
            .map(profile_to_spec)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(encode_specs(&specs))
    }
}

fn encode_specs(specs: &[ProfileSpec]) -> String {
    let mut profiles = specs.iter().collect::<Vec<_>>();
    profiles.sort_by(|left, right| left.id.cmp(&right.id));

    let mut output = format!("// Canonical standalone ProfileSpec snapshot (ABI {PROFILE_ABI_VERSION}).\n");
    output.push_str(STANDALONE_GLUON_TYPES);
    output.push_str("[\n");
    for profile in profiles {
        output.push_str("    {\n");
        writeln!(output, "        id = {},", gluon_string(&profile.id)).unwrap();
        output.push_str("        repositories = [\n");
        let mut repositories = profile.repositories.iter().collect::<Vec<_>>();
        repositories.sort_by(|left, right| left.id.cmp(&right.id));
        for repository in repositories {
            output.push_str("            {\n");
            writeln!(output, "                id = {},", gluon_string(&repository.id)).unwrap();
            writeln!(
                output,
                "                description = {},",
                gluon_optional_string(repository.description.as_deref())
            )
            .unwrap();
            encode_source(&mut output, &repository.source);
            writeln!(
                output,
                "                priority = {},",
                gluon_optional_integer(repository.priority)
            )
            .unwrap();
            writeln!(
                output,
                "                enabled = {},",
                gluon_optional_bool(repository.enabled)
            )
            .unwrap();
            output.push_str("            },\n");
        }
        output.push_str("        ],\n");
        output.push_str("    },\n");
    }
    output.push_str("]\n");
    output
}

fn encode_source(output: &mut String, source: &RepositorySourceSpec) {
    match source {
        RepositorySourceSpec::DirectIndex { uri } => {
            output.push_str("                source = DirectIndex {\n");
            writeln!(output, "                    uri = {},", gluon_string(uri)).unwrap();
            output.push_str("                },\n");
        }
        RepositorySourceSpec::RootIndex {
            base_uri,
            channel,
            version,
            arch,
        } => {
            output.push_str("                source = RootIndex {\n");
            writeln!(output, "                    base_uri = {},", gluon_string(base_uri)).unwrap();
            writeln!(
                output,
                "                    channel = {},",
                gluon_optional_string(channel.as_deref())
            )
            .unwrap();
            writeln!(output, "                    version = {},", gluon_string(version)).unwrap();
            writeln!(
                output,
                "                    arch = {},",
                gluon_optional_string(arch.as_deref())
            )
            .unwrap();
            output.push_str("                },\n");
        }
    }
}

fn gluon_optional_string(value: Option<&str>) -> String {
    value.map_or_else(|| "None".to_owned(), |value| format!("Some {}", gluon_string(value)))
}

fn gluon_optional_integer(value: Option<i64>) -> String {
    value.map_or_else(|| "None".to_owned(), |value| format!("Some {value}"))
}

fn gluon_optional_bool(value: Option<bool>) -> String {
    value.map_or_else(
        || "None".to_owned(),
        |value| format!("Some {}", if value { "True" } else { "False" }),
    )
}

fn gluon_string(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len() + 2);
    escaped.push('"');
    for character in value.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            character => escaped.push(character),
        }
    }
    escaped.push('"');
    escaped
}
