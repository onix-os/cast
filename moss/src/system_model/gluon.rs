// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

//! Versioned Gluon boundary for authored Moss system intent and snapshots.

use gluon_config::{Diagnostic, EvaluationFingerprint, Evaluator, ImportPolicy, Source};
use thiserror::Error;

use super::{SystemModel, spec};

pub const SYSTEM_ABI_VERSION: u32 = 1;
pub const GLUON_SYSTEM_ABI: &str = include_str!("../../gluon/system.glu");

#[derive(Debug, Clone)]
pub struct EvaluatedSystem {
    pub model: SystemModel,
    pub fingerprint: EvaluationFingerprint,
}

struct EvaluatedSpec {
    spec: spec::SystemSpec,
    fingerprint: EvaluationFingerprint,
}

#[derive(Debug, Error)]
pub enum EvaluationError {
    #[error(transparent)]
    Evaluation(#[from] Diagnostic),
    #[error(transparent)]
    Conversion(#[from] spec::ConversionError),
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

pub fn import_policy() -> Result<ImportPolicy, Diagnostic> {
    ImportPolicy::new().with_embedded_module("moss.system.v1", GLUON_SYSTEM_ABI)
}

pub fn evaluate(source: &Source) -> Result<EvaluatedSystem, EvaluationError> {
    evaluate_with(&Evaluator::default(), source)
}

pub fn evaluate_with(evaluator: &Evaluator, source: &Source) -> Result<EvaluatedSystem, EvaluationError> {
    let evaluated = evaluate_spec_with(evaluator, source)?;
    let parts = spec::into_domain(evaluated.spec)?;
    let model = SystemModel::regenerate(parts)?;

    Ok(EvaluatedSystem {
        model,
        fingerprint: evaluated.fingerprint,
    })
}

/// Evaluate a canonical generated snapshot without rewriting it.
pub fn evaluate_generated_snapshot(source: &Source) -> Result<SystemModel, EvaluationError> {
    evaluate_generated_snapshot_with(&Evaluator::default(), source)
}

/// Evaluate a canonical generated snapshot with caller-selected limits/root.
pub fn evaluate_generated_snapshot_with(
    evaluator: &Evaluator,
    source: &Source,
) -> Result<SystemModel, EvaluationError> {
    let source_text = source.text().to_owned();
    let evaluated = evaluate_spec_with(evaluator, source)?;
    let parts = spec::into_domain(evaluated.spec)?;

    Ok(SystemModel::from_generated(parts, source_text, evaluated.fingerprint))
}

fn evaluate_spec_with(evaluator: &Evaluator, source: &Source) -> Result<EvaluatedSpec, EvaluationError> {
    let mut policy = evaluator.import_policy().clone();
    policy.insert_embedded_module("moss.system.v1", GLUON_SYSTEM_ABI)?;
    let evaluator = evaluator.clone().with_import_policy(policy);
    let evaluation = evaluator.evaluate::<GluonSystemSpec>(source)?;

    Ok(EvaluatedSpec {
        spec: spec::SystemSpec::from(evaluation.value),
        fingerprint: evaluation.fingerprint,
    })
}

#[cfg(test)]
mod tests {
    use gluon_config::{DiagnosticCategory, Source};

    use super::*;
    use crate::{Provider, repository};

    fn authored(body: &str) -> Source {
        Source::new("system.glu", format!("let moss = import! moss.system.v1\n{body}"))
    }

    #[test]
    fn documented_system_example_remains_loadable() {
        let source = Source::new(
            "docs/examples/gluon/system.glu",
            include_str!("../../../docs/examples/gluon/system.glu"),
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
        let empty = evaluate(&authored("moss.system")).unwrap();
        assert!(empty.model.repositories.iter().next().is_none());
        assert!(empty.model.packages.is_empty());
        assert_eq!(empty.fingerprint.imported_modules[0].logical_name, "moss.system.v1");

        let populated = evaluate(&authored(
            r#"
{
    disable_warning = moss.boolean.true,
    repositories = [
        moss.repository.direct_with {
            id = "local",
            description = moss.optional.some "local packages",
            uri = "file:///var/cache/local.index",
            priority = moss.optional.some 5,
            enabled = moss.optional.some moss.boolean.false,
        },
        moss.repository.root "volatile" "https://packages.example.test" "stream/volatile",
    ],
    packages = ["moss", "soname(libc.so.6)"],
}
"#,
        ))
        .unwrap();

        assert!(populated.model.disable_warning);
        let local = populated.model.repositories.get(&repository::Id::new("local")).unwrap();
        assert_eq!(local.description, "local packages");
        assert_eq!(u64::from(local.priority), 5);
        assert!(!local.active);
        assert!(populated.model.packages.contains(&Provider::package_name("moss")));
    }

    #[test]
    fn generated_snapshot_evaluates_without_imports_and_round_trips() {
        let authored = evaluate(&authored(
            r#"
{
    repositories = [moss.repository.root "volatile" "https://packages.example.test" "stream/volatile"],
    packages = ["moss"],
    .. moss.system
}
"#,
        ))
        .unwrap();
        let normalized = spec::SystemSpec::try_from(&authored.model).unwrap();
        let generated = spec::encode_generated_gluon(&normalized);

        let evaluated = Evaluator::default()
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
    repositories = [moss.repository.direct_with {
        id = "bad",
        description = moss.optional.none,
        uri = "https://example.test/index.stone",
        priority = moss.optional.some (-1),
        enabled = moss.optional.none,
    }],
    .. moss.system
}
"#,
        ))
        .unwrap_err();
        assert!(
            matches!(invalid_priority, EvaluationError::Conversion(ref error) if error.path() == "repositories[0].priority")
        );

        let wrong_type = evaluate(&authored("{ packages = [1], .. moss.system }")).unwrap_err();
        assert!(
            matches!(wrong_type, EvaluationError::Evaluation(ref error) if error.category == DiagnosticCategory::Type)
        );

        let unknown = evaluate(&authored("{ package = [], .. moss.system }")).unwrap_err();
        assert!(
            matches!(unknown, EvaluationError::Evaluation(ref error) if error.category == DiagnosticCategory::Type)
        );
    }

    #[test]
    fn forbidden_effects_fail_and_fingerprints_are_deterministic() {
        let forbidden = evaluate(&authored("let _ = import! std.fs\nmoss.system")).unwrap_err();
        assert!(
            matches!(forbidden, EvaluationError::Evaluation(ref error) if error.category == DiagnosticCategory::Import)
        );

        let source = authored("moss.system");
        let first = evaluate(&source).unwrap();
        let repeated = evaluate(&source).unwrap();
        assert_eq!(first.fingerprint, repeated.fingerprint);
    }
}
