//! Engine-neutral evaluation identity.
//!
//! This is the language-agnostic successor to the Gluon-specific fingerprint.
//! It commits to the same evaluation inputs the shared pipeline already
//! prepares — validated language/engine descriptors, the caller-supplied
//! configuration ABI and evaluator-policy identities, the resource policy, the
//! root source, the canonical prepared-module graph, and explicit inputs — and
//! hashes them under a new neutral domain that shares no bytes with v1.
//!
//! The core invents no ABI or policy names. Adapters pass the typed descriptors
//! that identify their own configuration contract, so two engines that produce
//! the same domain value still receive distinct identities through their
//! distinct [`EngineId`] and [`LanguageId`], while an ABI semantic version never
//! changes merely because an engine adapter changed.

use std::{collections::BTreeMap, convert::Infallible, error::Error, fmt};

use sha2::{Digest, Sha256};

use crate::{
    AbiId, EngineId, EvaluatorPolicyId, LanguageId, LanguageSpec, Limits,
    ModuleClass, PreparedGraph, Source, content_hash::sha256_checked,
};

/// Hash-domain separator. Deliberately disjoint from the v1
/// `os-tools-gluon-evaluation\0` domain so no v1 aggregate can collide with a
/// v2 aggregate over otherwise-identical material.
const IDENTITY_HASH_DOMAIN: &[u8] = b"os-tools-declaration-evaluation\0";

/// Version of the neutral hash encoding itself. Bumping this deliberately
/// invalidates every derived v2 identity even when the semantic inputs are
/// unchanged; it is not the configuration ABI or evaluator-policy version.
const IDENTITY_ENCODING_VERSION: u32 = 1;

/// One reachable prepared module in canonical identity order.
///
/// `identity` is the graph/ABI identity assigned during preparation (for
/// example an embedded ABI name or a rooted relative identity); `logical_name`
/// and `sha256` are the exact source-fingerprint evidence.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct IdentityModule {
    pub identity: String,
    pub class: ModuleClass,
    pub logical_name: String,
    pub sha256: String,
}

/// One observed parent-to-target import edge in canonical order.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct IdentityDependency {
    pub parent_identity: String,
    pub target_identity: String,
    pub alias: String,
}

/// A complete, engine-neutral evaluation identity.
///
/// Fields are public because frozen plans and inspection tools must retain the
/// full provenance. Any consumer that receives an identity across such a
/// boundary must call [`EvaluationIdentity::validate`] rather than trusting
/// the aggregate digest alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvaluationIdentity {
    pub language: LanguageId,
    pub source_profile: String,
    pub engine: EngineId,
    pub configuration_abi: AbiId,
    pub evaluator_policy: EvaluatorPolicyId,
    pub resource_policy_sha256: String,
    pub root_logical_name: String,
    pub root_source_sha256: String,
    pub modules: Vec<IdentityModule>,
    pub dependencies: Vec<IdentityDependency>,
    pub explicit_inputs_sha256: String,
    pub encoding_version: u32,
    pub sha256: String,
}

impl EvaluationIdentity {
    #[cfg(test)]
    pub(crate) fn new(
        language_spec: &LanguageSpec,
        configuration_abi: &AbiId,
        evaluator_policy: &EvaluatorPolicyId,
        limits: &Limits,
        root: &Source,
        graph: &PreparedGraph,
        explicit_inputs: &[u8],
    ) -> Self {
        let mut checkpoint = || Ok::<(), Infallible>(());
        infallible(Self::new_checked(
            language_spec,
            configuration_abi,
            evaluator_policy,
            limits,
            root,
            graph,
            explicit_inputs,
            &mut checkpoint,
        ))
    }

    /// Build one canonical neutral identity from the exact material the shared
    /// pipeline prepares. `checkpoint` is invoked at bounded intervals so a
    /// deadline can interrupt hashing of a large graph.
    #[allow(clippy::too_many_arguments)]
    pub fn new_checked<E>(
        language_spec: &LanguageSpec,
        configuration_abi: &AbiId,
        evaluator_policy: &EvaluatorPolicyId,
        limits: &Limits,
        root: &Source,
        graph: &PreparedGraph,
        explicit_inputs: &[u8],
        checkpoint: &mut impl FnMut() -> Result<(), E>,
    ) -> Result<Self, E> {
        checkpoint()?;
        let mut modules = graph
            .modules()
            .iter()
            .map(|module| IdentityModule {
                identity: module.identity().to_owned(),
                class: module.class(),
                logical_name: module.fingerprint().logical_name.clone(),
                sha256: module.fingerprint().sha256.clone(),
            })
            .collect::<Vec<_>>();
        modules.sort();
        modules.dedup_by(|left, right| left.identity == right.identity);
        checkpoint()?;
        let mut dependencies = graph
            .dependencies()
            .iter()
            .map(|dependency| IdentityDependency {
                parent_identity: dependency.parent_identity.clone(),
                target_identity: dependency.target_identity.clone(),
                alias: dependency.alias.clone(),
            })
            .collect::<Vec<_>>();
        dependencies.sort();
        dependencies.dedup();

        let resource_policy_sha256 = resource_policy_sha256_checked(limits, checkpoint)?;
        let root_logical_name = root.logical_name().to_owned();
        let root_source_sha256 = sha256_checked(root.text().as_bytes(), checkpoint)?;
        let explicit_inputs_sha256 = sha256_checked(explicit_inputs, checkpoint)?;

        let sha256 = aggregate_sha256_checked(
            language_spec.language(),
            language_spec.source_profile(),
            language_spec.engine(),
            configuration_abi,
            evaluator_policy,
            &resource_policy_sha256,
            &root_logical_name,
            &root_source_sha256,
            &modules,
            &dependencies,
            &explicit_inputs_sha256,
            checkpoint,
        )?;

        checkpoint()?;
        Ok(Self {
            language: language_spec.language().clone(),
            source_profile: language_spec.source_profile().to_owned(),
            engine: language_spec.engine().clone(),
            configuration_abi: configuration_abi.clone(),
            evaluator_policy: evaluator_policy.clone(),
            resource_policy_sha256,
            root_logical_name,
            root_source_sha256,
            modules,
            dependencies,
            explicit_inputs_sha256,
            encoding_version: IDENTITY_ENCODING_VERSION,
            sha256,
        })
    }

    /// Validate that this is one canonical, internally consistent neutral
    /// identity. Typed descriptors cannot be constructed non-canonically, so
    /// this checks the public string/collection fields and the aggregate.
    pub fn validate(&self) -> Result<(), EvaluationIdentityValidationError> {
        require_nonempty("source_profile", &self.source_profile)?;
        validate_sha256("resource_policy_sha256", &self.resource_policy_sha256)?;
        require_nonempty("root_logical_name", &self.root_logical_name)?;
        validate_sha256("root_source_sha256", &self.root_source_sha256)?;
        validate_sha256("explicit_inputs_sha256", &self.explicit_inputs_sha256)?;
        validate_sha256("sha256", &self.sha256)?;

        let mut module_identities = BTreeMap::new();
        for (index, module) in self.modules.iter().enumerate() {
            require_nonempty(&format!("modules[{index}].identity"), &module.identity)?;
            require_nonempty(&format!("modules[{index}].logical_name"), &module.logical_name)?;
            validate_sha256(&format!("modules[{index}].sha256"), &module.sha256)?;
            if let Some(first_index) = module_identities.insert(module.identity.as_str(), index) {
                return Err(EvaluationIdentityValidationError::DuplicateModule {
                    identity: module.identity.clone(),
                    first_index,
                    duplicate_index: index,
                });
            }
        }
        for (index, pair) in self.modules.windows(2).enumerate() {
            if pair[0] > pair[1] {
                return Err(EvaluationIdentityValidationError::NonCanonicalModuleOrder {
                    previous_index: index,
                    previous_identity: pair[0].identity.clone(),
                    index: index + 1,
                    identity: pair[1].identity.clone(),
                });
            }
        }

        for (index, dependency) in self.dependencies.iter().enumerate() {
            require_nonempty(&format!("dependencies[{index}].parent_identity"), &dependency.parent_identity)?;
            require_nonempty(&format!("dependencies[{index}].target_identity"), &dependency.target_identity)?;
            require_nonempty(&format!("dependencies[{index}].alias"), &dependency.alias)?;
        }
        for (index, pair) in self.dependencies.windows(2).enumerate() {
            if pair[0] >= pair[1] {
                return Err(EvaluationIdentityValidationError::NonCanonicalDependencyOrder {
                    previous_index: index,
                    index: index + 1,
                });
            }
        }

        if self.encoding_version != IDENTITY_ENCODING_VERSION {
            return Err(EvaluationIdentityValidationError::UnknownEncodingVersion {
                found: self.encoding_version,
                expected: IDENTITY_ENCODING_VERSION,
            });
        }

        let expected = aggregate_sha256(
            &self.language,
            &self.source_profile,
            &self.engine,
            &self.configuration_abi,
            &self.evaluator_policy,
            &self.resource_policy_sha256,
            &self.root_logical_name,
            &self.root_source_sha256,
            &self.modules,
            &self.dependencies,
            &self.explicit_inputs_sha256,
        );
        if self.sha256 != expected {
            return Err(EvaluationIdentityValidationError::AggregateMismatch {
                expected,
                found: self.sha256.clone(),
            });
        }

        Ok(())
    }
}

/// Structural or aggregate inconsistency in an [`EvaluationIdentity`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvaluationIdentityValidationError {
    Empty {
        field: String,
    },
    InvalidSha256 {
        field: String,
        value: String,
    },
    DuplicateModule {
        identity: String,
        first_index: usize,
        duplicate_index: usize,
    },
    NonCanonicalModuleOrder {
        previous_index: usize,
        previous_identity: String,
        index: usize,
        identity: String,
    },
    NonCanonicalDependencyOrder {
        previous_index: usize,
        index: usize,
    },
    UnknownEncodingVersion {
        found: u32,
        expected: u32,
    },
    AggregateMismatch {
        expected: String,
        found: String,
    },
}

impl fmt::Display for EvaluationIdentityValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty { field } => write!(formatter, "{field}: value must not be empty"),
            Self::InvalidSha256 { field, value } => write!(
                formatter,
                "{field}: expected an exact lowercase 64-character SHA-256 value, found {value:?}"
            ),
            Self::DuplicateModule {
                identity,
                first_index,
                duplicate_index,
            } => write!(
                formatter,
                "modules[{duplicate_index}].identity: duplicate module {identity:?} first declared at modules[{first_index}]"
            ),
            Self::NonCanonicalModuleOrder {
                previous_index,
                previous_identity,
                index,
                identity,
            } => write!(
                formatter,
                "modules[{index}].identity {identity:?} must sort after modules[{previous_index}].identity {previous_identity:?}"
            ),
            Self::NonCanonicalDependencyOrder { previous_index, index } => write!(
                formatter,
                "dependencies[{index}] must sort strictly after dependencies[{previous_index}]"
            ),
            Self::UnknownEncodingVersion { found, expected } => write!(
                formatter,
                "encoding_version: expected {expected}, found {found}"
            ),
            Self::AggregateMismatch { expected, found } => write!(
                formatter,
                "sha256: aggregate identity mismatch: expected {expected:?}, found {found:?}"
            ),
        }
    }
}

impl Error for EvaluationIdentityValidationError {}

fn module_class_tag(class: ModuleClass) -> u8 {
    match class {
        ModuleClass::Root => 0,
        ModuleClass::Embedded => 1,
        ModuleClass::Relative => 2,
        ModuleClass::External => 3,
    }
}

fn resource_policy_sha256_checked<E>(
    limits: &Limits,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<String, E> {
    checkpoint()?;
    let mut bytes = Vec::with_capacity(8 * 8 + 16);
    for value in [
        limits.max_source_bytes,
        limits.max_explicit_input_bytes,
        limits.max_imported_file_bytes,
        limits.max_imports,
        limits.max_import_graph_bytes,
        limits.memory_bytes,
    ] {
        bytes.extend_from_slice(&(value as u64).to_le_bytes());
    }
    bytes.extend_from_slice(&u64::from(limits.max_stack_size).to_le_bytes());
    bytes.extend_from_slice(&limits.timeout.as_nanos().to_le_bytes());
    sha256_checked(&bytes, checkpoint)
}

#[allow(clippy::too_many_arguments)]
fn aggregate_sha256(
    language: &LanguageId,
    source_profile: &str,
    engine: &EngineId,
    configuration_abi: &AbiId,
    evaluator_policy: &EvaluatorPolicyId,
    resource_policy_sha256: &str,
    root_logical_name: &str,
    root_source_sha256: &str,
    modules: &[IdentityModule],
    dependencies: &[IdentityDependency],
    explicit_inputs_sha256: &str,
) -> String {
    let mut checkpoint = || Ok::<(), Infallible>(());
    infallible(aggregate_sha256_checked(
        language,
        source_profile,
        engine,
        configuration_abi,
        evaluator_policy,
        resource_policy_sha256,
        root_logical_name,
        root_source_sha256,
        modules,
        dependencies,
        explicit_inputs_sha256,
        &mut checkpoint,
    ))
}

#[allow(clippy::too_many_arguments)]
fn aggregate_sha256_checked<E>(
    language: &LanguageId,
    source_profile: &str,
    engine: &EngineId,
    configuration_abi: &AbiId,
    evaluator_policy: &EvaluatorPolicyId,
    resource_policy_sha256: &str,
    root_logical_name: &str,
    root_source_sha256: &str,
    modules: &[IdentityModule],
    dependencies: &[IdentityDependency],
    explicit_inputs_sha256: &str,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<String, E> {
    checkpoint()?;
    let mut digest = Sha256::new();
    digest.update(IDENTITY_HASH_DOMAIN);
    digest.update(IDENTITY_ENCODING_VERSION.to_le_bytes());
    update_field(&mut digest, language.as_str().as_bytes());
    update_field(&mut digest, source_profile.as_bytes());
    update_field(&mut digest, engine.implementation().as_bytes());
    update_field(&mut digest, engine.version().as_bytes());
    update_field(&mut digest, configuration_abi.name().as_bytes());
    update_field(&mut digest, configuration_abi.version().as_bytes());
    update_field(&mut digest, evaluator_policy.as_str().as_bytes());
    update_field(&mut digest, resource_policy_sha256.as_bytes());
    update_field(&mut digest, root_logical_name.as_bytes());
    update_field(&mut digest, root_source_sha256.as_bytes());
    update_field(&mut digest, explicit_inputs_sha256.as_bytes());

    digest.update((modules.len() as u64).to_le_bytes());
    for module in modules {
        update_field(&mut digest, module.identity.as_bytes());
        digest.update([module_class_tag(module.class)]);
        update_field(&mut digest, module.logical_name.as_bytes());
        update_field(&mut digest, module.sha256.as_bytes());
        checkpoint()?;
    }

    digest.update((dependencies.len() as u64).to_le_bytes());
    for dependency in dependencies {
        update_field(&mut digest, dependency.parent_identity.as_bytes());
        update_field(&mut digest, dependency.target_identity.as_bytes());
        update_field(&mut digest, dependency.alias.as_bytes());
        checkpoint()?;
    }

    let sha256 = format!("{:x}", digest.finalize());
    checkpoint()?;
    Ok(sha256)
}

fn update_field(digest: &mut Sha256, value: &[u8]) {
    digest.update((value.len() as u64).to_le_bytes());
    digest.update(value);
}

fn require_nonempty(field: &str, value: &str) -> Result<(), EvaluationIdentityValidationError> {
    if value.trim().is_empty() {
        Err(EvaluationIdentityValidationError::Empty {
            field: field.to_owned(),
        })
    } else {
        Ok(())
    }
}

fn validate_sha256(field: &str, value: &str) -> Result<(), EvaluationIdentityValidationError> {
    if value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(EvaluationIdentityValidationError::InvalidSha256 {
            field: field.to_owned(),
            value: value.to_owned(),
        })
    }
}

fn infallible<T>(result: Result<T, Infallible>) -> T {
    match result {
        Ok(value) => value,
        Err(never) => match never {},
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::{
        AbiCatalog, EvaluationDeadline, ImportRequest, prepare_module_graph,
    };

    type IdentityMutation = (&'static str, Box<dyn Fn(&mut EvaluationIdentity)>);

    fn spec() -> LanguageSpec {
        LanguageSpec::new(
            LanguageId::new("fixture").unwrap(),
            EngineId::new("fixture-engine", "1.2.3").unwrap(),
            "decl",
            "declaration-v1",
            "# generated fixture\n",
        )
        .unwrap()
    }

    fn abi() -> AbiId {
        AbiId::new("cast.configuration", "1").unwrap()
    }

    fn policy() -> EvaluatorPolicyId {
        EvaluatorPolicyId::new("evaluator-v1").unwrap()
    }

    /// Prepare a real two-module graph: `root` imports two embedded ABI
    /// modules, so identity has canonical modules and dependency edges.
    fn two_module_graph() -> (Source, PreparedGraph) {
        let mut catalog = AbiCatalog::new();
        assert!(catalog.insert_source(
            "z.dep",
            "abi.z.v1",
            Source::new("abi.z.v1", "z source"),
        ));
        assert!(catalog.insert_source(
            "a.dep",
            "abi.a.v1",
            Source::new("abi.a.v1", "a source"),
        ));
        let root = Source::new("root.decl", "root source");
        let deadline = EvaluationDeadline::start(Duration::from_secs(30));
        let graph = prepare_module_graph(
            &catalog,
            None,
            Limits::default(),
            &root,
            deadline,
            |view| {
                if view.class() == ModuleClass::Root {
                    Ok(vec![
                        ImportRequest::embedded("z.dep"),
                        ImportRequest::embedded("a.dep"),
                    ])
                } else {
                    Ok(Vec::new())
                }
            },
            |requested| Err(format!("no relative imports: {requested}")),
        )
        .unwrap();
        (root, graph)
    }

    fn identity_with(root: &Source, graph: &PreparedGraph) -> EvaluationIdentity {
        EvaluationIdentity::new(
            &spec(),
            &abi(),
            &policy(),
            &Limits::default(),
            root,
            graph,
            b"explicit inputs",
        )
    }

    fn valid_identity() -> EvaluationIdentity {
        let (root, graph) = two_module_graph();
        identity_with(&root, &graph)
    }

    #[test]
    fn identity_is_canonical_valid_and_captures_graph() {
        let identity = valid_identity();

        assert_eq!(
            identity
                .modules
                .iter()
                .map(|module| module.identity.as_str())
                .collect::<Vec<_>>(),
            ["abi.a.v1", "abi.z.v1"]
        );
        assert!(
            identity
                .dependencies
                .iter()
                .all(|dependency| dependency.parent_identity == "root")
        );
        assert_eq!(identity.dependencies.len(), 2);
        assert_eq!(identity.encoding_version, IDENTITY_ENCODING_VERSION);
        assert_eq!(identity.validate(), Ok(()));
    }

    #[test]
    fn checked_and_unchecked_identities_are_equal() {
        let (root, graph) = two_module_graph();
        let expected = identity_with(&root, &graph);
        let mut checkpoints = 0;
        let mut checkpoint = || {
            checkpoints += 1;
            Ok::<(), Infallible>(())
        };

        let checked = EvaluationIdentity::new_checked(
            &spec(),
            &abi(),
            &policy(),
            &Limits::default(),
            &root,
            &graph,
            b"explicit inputs",
            &mut checkpoint,
        )
        .unwrap();

        assert_eq!(checked, expected);
        assert!(checkpoints > 0);
    }

    #[test]
    fn identity_is_deterministic_across_rebuilds() {
        let first = valid_identity();
        let second = valid_identity();

        assert_eq!(first.sha256, second.sha256);
    }

    #[test]
    fn distinct_engine_yields_distinct_identity_for_equal_inputs() {
        let (root, graph) = two_module_graph();
        let baseline = identity_with(&root, &graph);
        let other_engine = LanguageSpec::new(
            LanguageId::new("fixture").unwrap(),
            EngineId::new("other-engine", "1.2.3").unwrap(),
            "decl",
            "declaration-v1",
            "# generated fixture\n",
        )
        .unwrap();

        let other = EvaluationIdentity::new(
            &other_engine,
            &abi(),
            &policy(),
            &Limits::default(),
            &root,
            &graph,
            b"explicit inputs",
        );

        assert_ne!(baseline.sha256, other.sha256);
        assert_eq!(baseline.configuration_abi, other.configuration_abi);
    }

    #[test]
    fn distinct_configuration_abi_yields_distinct_identity() {
        let (root, graph) = two_module_graph();
        let baseline = identity_with(&root, &graph);

        let other = EvaluationIdentity::new(
            &spec(),
            &AbiId::new("cast.configuration", "2").unwrap(),
            &policy(),
            &Limits::default(),
            &root,
            &graph,
            b"explicit inputs",
        );

        assert_ne!(baseline.sha256, other.sha256);
    }

    #[test]
    fn distinct_resource_policy_yields_distinct_identity() {
        let (root, graph) = two_module_graph();
        let baseline = identity_with(&root, &graph);
        let mut limits = Limits::default();
        limits.memory_bytes += 1;

        let other = EvaluationIdentity::new(
            &spec(),
            &abi(),
            &policy(),
            &limits,
            &root,
            &graph,
            b"explicit inputs",
        );

        assert_ne!(baseline.resource_policy_sha256, other.resource_policy_sha256);
        assert_ne!(baseline.sha256, other.sha256);
    }

    #[test]
    fn distinct_explicit_inputs_yield_distinct_identity() {
        let (root, graph) = two_module_graph();
        let baseline = identity_with(&root, &graph);

        let other = EvaluationIdentity::new(
            &spec(),
            &abi(),
            &policy(),
            &Limits::default(),
            &root,
            &graph,
            b"other inputs",
        );

        assert_ne!(baseline.sha256, other.sha256);
    }

    #[test]
    fn rejects_empty_and_noncanonical_string_fields() {
        let mutations: Vec<IdentityMutation> = vec![
            ("source_profile", Box::new(|identity| identity.source_profile = "  ".to_owned())),
            ("root_logical_name", Box::new(|identity| identity.root_logical_name.clear())),
            ("modules[0].identity", Box::new(|identity| identity.modules[0].identity.clear())),
            ("modules[0].logical_name", Box::new(|identity| identity.modules[0].logical_name.clear())),
            (
                "resource_policy_sha256",
                Box::new(|identity| identity.resource_policy_sha256 = "z".repeat(64)),
            ),
            ("sha256", Box::new(|identity| identity.sha256 = "nope".to_owned())),
        ];

        for (field, mutate) in mutations {
            let mut identity = valid_identity();
            mutate(&mut identity);
            assert!(
                matches!(
                    identity.validate(),
                    Err(EvaluationIdentityValidationError::Empty { field: ref bad } | EvaluationIdentityValidationError::InvalidSha256 { field: ref bad, .. })
                        if bad == field
                ),
                "mutation of {field} was not rejected with its field path"
            );
        }
    }

    #[test]
    fn rejects_duplicate_module_identity() {
        let mut identity = valid_identity();
        identity.modules[1].identity = identity.modules[0].identity.clone();

        assert!(matches!(
            identity.validate(),
            Err(EvaluationIdentityValidationError::DuplicateModule {
                identity: dup,
                first_index: 0,
                duplicate_index: 1,
            }) if dup == "abi.a.v1"
        ));
    }

    #[test]
    fn rejects_noncanonical_module_order() {
        let mut identity = valid_identity();
        identity.modules.reverse();

        assert!(matches!(
            identity.validate(),
            Err(EvaluationIdentityValidationError::NonCanonicalModuleOrder {
                previous_index: 0,
                index: 1,
                ..
            })
        ));
    }

    #[test]
    fn rejects_unknown_encoding_version() {
        let mut identity = valid_identity();
        identity.encoding_version = IDENTITY_ENCODING_VERSION + 1;

        assert!(matches!(
            identity.validate(),
            Err(EvaluationIdentityValidationError::UnknownEncodingVersion { .. })
        ));
    }

    #[test]
    fn every_aggregate_constituent_is_recomputed_and_verified() {
        let mutations: Vec<IdentityMutation> = vec![
            ("root_logical_name", Box::new(|identity| identity.root_logical_name = "renamed.decl".to_owned())),
            ("root_source_sha256", Box::new(|identity| identity.root_source_sha256 = "a".repeat(64))),
            ("modules.logical_name", Box::new(|identity| identity.modules[0].logical_name = "renamed".to_owned())),
            ("modules.sha256", Box::new(|identity| identity.modules[0].sha256 = "b".repeat(64))),
            ("modules.class", Box::new(|identity| identity.modules[0].class = ModuleClass::External)),
            (
                "modules.added",
                Box::new(|identity| {
                    identity.modules.push(IdentityModule {
                        identity: "zzz.added".to_owned(),
                        class: ModuleClass::Embedded,
                        logical_name: "zzz".to_owned(),
                        sha256: "c".repeat(64),
                    });
                }),
            ),
            ("dependencies.alias", Box::new(|identity| identity.dependencies[0].alias = "renamed".to_owned())),
            ("explicit_inputs_sha256", Box::new(|identity| identity.explicit_inputs_sha256 = "d".repeat(64))),
            ("resource_policy_sha256", Box::new(|identity| identity.resource_policy_sha256 = "e".repeat(64))),
        ];

        for (field, mutate) in mutations {
            let mut identity = valid_identity();
            mutate(&mut identity);
            assert!(
                matches!(
                    identity.validate(),
                    Err(EvaluationIdentityValidationError::AggregateMismatch { .. })
                ),
                "mutation of {field} was not detected as an aggregate mismatch"
            );
        }
    }
}
