
//! Versioned Gluon boundary for repository configuration fragments.
//!
//! Gluon arrays retain authored order at the language boundary. Conversion
//! then validates each [`RepositorySpec`] and builds the canonical
//! [`repository::Map`] keyed by normalized repository identifiers.

use std::{error::Error, fmt, fmt::Write as _};

use config::{DecodedGluon, GluonCodec, GluonCodecError};
use gluon_config::{Evaluator, Source as GluonSource};

use super::{Map, Repository, Source};
use crate::{
    repository,
    system_model::spec::{RepositorySourceSpec, RepositorySpec},
};

/// Version of the embedded repository configuration API.
pub const REPOSITORY_ABI_VERSION: u32 = 1;

/// Pure definitions imported by authored fragments as `cast.repository.v1`.
pub const GLUON_REPOSITORY_ABI: &str = include_str!("../../gluon/repository.glu");

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

"#;

/// Stateless repository configuration codec used by [`config::Manager`].
#[derive(Debug, Clone, Copy, Default)]
pub struct RepositoryCodec;

/// Semantic repository conversion failure with a stable field path.
#[derive(Debug)]
pub struct RepositoryConversionError {
    path: String,
    message: String,
    source: Option<crate::system_model::spec::ConversionError>,
}

impl RepositoryConversionError {
    fn from_spec(index: usize, error: crate::system_model::spec::ConversionError) -> Self {
        let path = if error.path().is_empty() {
            format!("repositories[{index}]")
        } else {
            format!("repositories[{index}].{}", error.path())
        };
        Self {
            path,
            message: error.message().to_owned(),
            source: Some(error),
        }
    }

    fn duplicate(index: usize, id: &repository::Id) -> Self {
        Self {
            path: format!("repositories[{index}].id"),
            message: format!("duplicate repository identifier `{id}`"),
            source: None,
        }
    }

    fn encode(path: String, error: impl fmt::Display) -> Self {
        Self {
            path,
            message: error.to_string(),
            source: None,
        }
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for RepositoryConversionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "invalid repository configuration at `{}`: {}",
            self.path, self.message
        )
    }
}

impl Error for RepositoryConversionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source.as_ref().map(|error| error as &(dyn Error + 'static))
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

impl GluonCodec for RepositoryCodec {
    type Config = Map;

    fn decode(
        &self,
        evaluator: &Evaluator,
        source: &GluonSource,
    ) -> Result<DecodedGluon<Self::Config>, GluonCodecError> {
        let mut policy = evaluator.import_policy().clone();
        policy.insert_embedded_module("cast.repository.v1", GLUON_REPOSITORY_ABI)?;
        let evaluator = evaluator.clone().with_import_policy(policy);
        let evaluation = evaluator.evaluate::<Vec<GluonRepositorySpec>>(source)?;
        let fingerprint = evaluation.fingerprint;
        let value = decode_specs(evaluation.value.into_iter().map(Into::into).collect())
            .map_err(GluonCodecError::conversion)?;

        Ok(DecodedGluon { value, fingerprint })
    }

    fn encode(&self, config: &Self::Config) -> Result<String, GluonCodecError> {
        let specs = config
            .iter()
            .map(repository_to_spec)
            .collect::<Result<Vec<_>, _>>()
            .map_err(GluonCodecError::conversion)?;
        Ok(encode_specs(&specs))
    }
}

fn decode_specs(specs: Vec<RepositorySpec>) -> Result<Map, RepositoryConversionError> {
    let mut repositories = Map::default();
    for (index, spec) in specs.into_iter().enumerate() {
        let (id, repository) = <(repository::Id, Repository)>::try_from(spec)
            .map_err(|error| RepositoryConversionError::from_spec(index, error))?;
        if repositories.contains_id(&id) {
            return Err(RepositoryConversionError::duplicate(index, &id));
        }
        repositories.add(id, repository);
    }
    Ok(repositories)
}

fn repository_to_spec(
    (id, value): (&repository::Id, &Repository),
) -> Result<RepositorySpec, RepositoryConversionError> {
    let priority = i64::try_from(u64::from(value.priority))
        .map_err(|error| RepositoryConversionError::encode(format!("repositories[\"{id}\"].priority"), error))?;
    let source = match &value.source {
        Source::DirectIndex(uri) => RepositorySourceSpec::DirectIndex { uri: uri.to_string() },
        Source::RootIndex(source) => RepositorySourceSpec::RootIndex {
            base_uri: source.base_uri.to_string(),
            channel: Some(source.channel.to_string()),
            version: source.version.to_string(),
            arch: Some(source.arch.clone()),
        },
    };

    Ok(RepositorySpec {
        id: id.to_string(),
        description: Some(value.description.clone()),
        source,
        priority: Some(priority),
        enabled: Some(value.active),
    })
}

fn encode_specs(specs: &[RepositorySpec]) -> String {
    let mut specs = specs.iter().collect::<Vec<_>>();
    specs.sort_by(|left, right| left.id.cmp(&right.id));

    let mut output = String::from(STANDALONE_GLUON_TYPES);
    output.push_str("[\n");
    for spec in specs {
        output.push_str("    {\n");
        writeln!(output, "        id = {},", gluon_string(&spec.id)).unwrap();
        writeln!(
            output,
            "        description = {},",
            gluon_optional_string(spec.description.as_deref())
        )
        .unwrap();
        encode_source(&mut output, &spec.source);
        writeln!(output, "        priority = {},", gluon_optional_integer(spec.priority)).unwrap();
        writeln!(output, "        enabled = {},", gluon_optional_bool(spec.enabled)).unwrap();
        output.push_str("    },\n");
    }
    output.push_str("]\n");
    output
}

fn encode_source(output: &mut String, source: &RepositorySourceSpec) {
    match source {
        RepositorySourceSpec::DirectIndex { uri } => {
            output.push_str("        source = DirectIndex {\n");
            writeln!(output, "            uri = {},", gluon_string(uri)).unwrap();
            output.push_str("        },\n");
        }
        RepositorySourceSpec::RootIndex {
            base_uri,
            channel,
            version,
            arch,
        } => {
            output.push_str("        source = RootIndex {\n");
            writeln!(output, "            base_uri = {},", gluon_string(base_uri)).unwrap();
            writeln!(
                output,
                "            channel = {},",
                gluon_optional_string(channel.as_deref())
            )
            .unwrap();
            writeln!(output, "            version = {},", gluon_string(version)).unwrap();
            writeln!(output, "            arch = {},", gluon_optional_string(arch.as_deref())).unwrap();
            output.push_str("        },\n");
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

#[cfg(test)]
mod tests {
    use std::path::Path;

    use config::{LoadGluonError, Manager};
    use fs_err as fs;
    use gluon_config::{DiagnosticCategory, Evaluator};

    use super::*;

    fn write(path: &Path, source: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, source).unwrap();
    }

    fn authored(body: &str) -> String {
        format!("let cast = import! cast.repository.v1\n{body}")
    }

    #[test]
    fn documented_repository_example_remains_loadable() {
        let source = GluonSource::new(
            "docs/examples/gluon/repositories.glu",
            include_str!("../../../../docs/examples/gluon/repositories.glu"),
        );
        let decoded = RepositoryCodec.decode(&Evaluator::default(), &source).unwrap();

        assert!(decoded.value.contains_id(&repository::Id::new("local")));
        assert!(decoded.value.contains_id(&repository::Id::new("volatile")));
    }

    #[test]
    fn manager_loads_direct_root_and_repository_defaults() {
        let temporary = tempfile::tempdir().unwrap();
        let manager = Manager::custom(temporary.path());
        write(
            &temporary.path().join("repo.d/authored.glu"),
            &authored(
                r#"cast.repositories [
    cast.repository.direct "local" "file:///var/cache/local.index",
    cast.repository.root_index "volatile" "https://packages.example.test" "stream/volatile",
]"#,
            ),
        );

        let loaded = manager.load_gluon(&Evaluator::default(), &RepositoryCodec).unwrap();
        assert_eq!(loaded.len(), 1);
        let repositories = &loaded[0].value;

        let local = repositories.get(&repository::Id::new("local")).unwrap();
        assert_eq!(local.description, "");
        assert_eq!(u64::from(local.priority), 0);
        assert!(local.active);
        assert!(matches!(&local.source, Source::DirectIndex(_)));

        let volatile = repositories.get(&repository::Id::new("volatile")).unwrap();
        let Source::RootIndex(root) = &volatile.source else {
            panic!("expected root-index repository");
        };
        assert_eq!(root.channel.as_ref(), repository::DEFAULT_CHANNEL);
        assert_eq!(root.arch, repository::DEFAULT_ARCH);
        assert_eq!(root.version.to_string(), "stream/volatile");
        assert!(
            loaded[0]
                .fingerprint
                .imported_modules
                .iter()
                .any(|module| module.logical_name == "cast.repository.v1")
        );
    }

    #[test]
    fn generated_save_is_deterministic_and_loadable() {
        let source = authored(
            r#"cast.repositories [
    cast.repository.root_index "z-root" "https://packages.example.test" "stream/volatile",
    cast.repository.direct "a-direct" "file:///var/cache/local.index",
]"#,
        );
        let evaluator = Evaluator::default();
        let decoded = RepositoryCodec
            .decode(&evaluator, &GluonSource::new("authored.glu", source))
            .unwrap();
        let first = RepositoryCodec.encode(&decoded.value).unwrap();
        let repeated = RepositoryCodec.encode(&decoded.value).unwrap();
        assert_eq!(first, repeated);
        assert!(first.find("id = \"a-direct\"").unwrap() < first.find("id = \"z-root\"").unwrap());

        let temporary = tempfile::tempdir().unwrap();
        let manager = Manager::custom(temporary.path());
        let path = manager
            .save_gluon("generated", &decoded.value, &RepositoryCodec)
            .unwrap();
        let generated = fs::read_to_string(&path).unwrap();
        assert!(generated.starts_with(config::GENERATED_GLUON_MARKER));
        assert!(generated.contains("type RepositorySpec ="));
        assert!(!generated.contains("import!"));

        let loaded = manager.load_gluon(&Evaluator::default(), &RepositoryCodec).unwrap();
        assert_eq!(loaded.len(), 1);
        assert!(loaded[0].value.contains_id(&repository::Id::new("a-direct")));
        assert!(loaded[0].value.contains_id(&repository::Id::new("z-root")));
    }

    #[test]
    fn malformed_and_conversion_errors_include_fragment_and_field_paths() {
        let temporary = tempfile::tempdir().unwrap();
        let manager = Manager::custom(temporary.path());
        let malformed = temporary.path().join("repo.d/malformed.glu");
        write(&malformed, "let value = in value");

        let error = manager.load_gluon(&Evaluator::default(), &RepositoryCodec).unwrap_err();
        let LoadGluonError::Evaluation { path, source } = error else {
            panic!("expected evaluation error");
        };
        assert_eq!(path, malformed);
        assert_eq!(source.source_name.as_deref(), Some("repo.d/malformed.glu"));
        assert!(source.span.is_some());

        fs::remove_file(&path).unwrap();
        let invalid = temporary.path().join("repo.d/invalid.glu");
        write(
            &invalid,
            &authored(
                r#"cast.repositories [
    cast.repository.direct "broken" "not a url",
]"#,
            ),
        );
        let error = manager.load_gluon(&Evaluator::default(), &RepositoryCodec).unwrap_err();
        let LoadGluonError::Conversion { path, source } = error else {
            panic!("expected conversion error");
        };
        assert_eq!(path, invalid);
        assert!(source.to_string().contains("repositories[0].source.uri"));
    }

    #[test]
    fn forbidden_effects_are_rejected() {
        let temporary = tempfile::tempdir().unwrap();
        let manager = Manager::custom(temporary.path());
        write(
            &temporary.path().join("repo.d/forbidden.glu"),
            &authored("let _ = import! std.fs\ncast.repositories []"),
        );

        let error = manager.load_gluon(&Evaluator::default(), &RepositoryCodec).unwrap_err();
        let LoadGluonError::Evaluation { source, .. } = error else {
            panic!("expected evaluation error");
        };
        assert_eq!(source.category, DiagnosticCategory::Import);
    }

    #[test]
    fn duplicate_ids_are_rejected_at_the_second_path() {
        let temporary = tempfile::tempdir().unwrap();
        let manager = Manager::custom(temporary.path());
        write(
            &temporary.path().join("repo.d/duplicate.glu"),
            &authored(
                r#"cast.repositories [
    cast.repository.direct "same/id" "file:///one.index",
    cast.repository.direct "same_id" "file:///two.index",
]"#,
            ),
        );

        let error = manager.load_gluon(&Evaluator::default(), &RepositoryCodec).unwrap_err();
        let LoadGluonError::Conversion { source, .. } = error else {
            panic!("expected conversion error");
        };
        assert!(source.to_string().contains("repositories[1].id"));
        assert!(source.to_string().contains("same_id"));
    }

    #[test]
    fn repeated_manager_loads_have_the_same_fingerprint() {
        let temporary = tempfile::tempdir().unwrap();
        let manager = Manager::custom(temporary.path());
        write(
            &temporary.path().join("repo.d/fingerprint.glu"),
            &authored(r#"cast.repositories [cast.repository.direct "local" "file:///local.index"]"#),
        );

        let first = manager.load_gluon(&Evaluator::default(), &RepositoryCodec).unwrap();
        let repeated = manager.load_gluon(&Evaluator::default(), &RepositoryCodec).unwrap();
        assert_eq!(first[0].fingerprint, repeated[0].fingerprint);
        assert!(!first[0].fingerprint.imported_modules.is_empty());
    }
}
