//! Typed evaluation beneath a caller-retained declaration root.

use std::{collections::BTreeMap, os::fd::AsRawFd, path::Path};

use declarative_config::DeclarationEvaluationError;

use super::{
    ConfigDeclarationEvaluator, DeclarationEvaluatorSet,
    FragmentDeclarationLimits, LoadRootedDeclarationsError,
    LoadedDeclaration, RootedDeclarationRevalidationPhase,
    RootedFragmentDeclaration, RootedFragmentDeclarationSet,
};
use crate::Config;

const MAX_DECLARATION_FRAGMENTS: usize = 1024;
const MAX_DECLARATION_DIRECTORY_ENTRIES: usize = 4096;

/// Discover and evaluate one typed declaration domain beneath an already
/// retained directory descriptor.
///
/// `root_path` is used only in diagnostics. Discovery, source reads, relative
/// imports, and authority checks remain rooted at an owned duplicate of
/// `root`. The returned declarations are sorted by logical name.
pub fn load_rooted_declarations<E>(
    root_path: impl AsRef<Path>,
    root: &impl AsRawFd,
    evaluators: &DeclarationEvaluatorSet<E>,
) -> Result<
    Vec<LoadedDeclaration<E::Config, E::Identity>>,
    LoadRootedDeclarationsError<E::Error>,
>
where
    E: ConfigDeclarationEvaluator,
{
    let root_path = root_path.as_ref();
    let fragments = RootedFragmentDeclarationSet::discover(
        root_path,
        root,
        E::Config::domain(),
        evaluators.languages(),
        FragmentDeclarationLimits::new(
            MAX_DECLARATION_FRAGMENTS,
            MAX_DECLARATION_DIRECTORY_ENTRIES,
        ),
    )
    .map_err(|source| LoadRootedDeclarationsError::Discovery {
        root_path: root_path.to_owned(),
        source,
    })?;

    let mut loaded = BTreeMap::new();
    for fragment in fragments.iter() {
        let evaluator = evaluators.get(fragment.language()).ok_or_else(|| {
            LoadRootedDeclarationsError::UnregisteredLanguage {
                path: fragment.physical_path().to_owned(),
                extension: fragment.language().extension().to_owned(),
            }
        })?;

        revalidate(
            fragment,
            RootedDeclarationRevalidationPhase::BeforeRead,
            false,
        )?;
        let read = fragment.source_root().load(
            fragment.relative_path(),
            evaluator.limits().max_source_bytes,
        );
        revalidate(
            fragment,
            RootedDeclarationRevalidationPhase::AfterRead,
            true,
        )?;
        let source = read.map_err(|source| LoadRootedDeclarationsError::Read {
            path: fragment.physical_path().to_owned(),
            source,
        })?;

        revalidate(
            fragment,
            RootedDeclarationRevalidationPhase::BeforeEvaluation,
            false,
        )?;
        let rooted = evaluator.with_source_root(fragment.source_root().clone());
        let result = rooted.evaluate(&source);
        // This check deliberately runs before inspecting `result`, so an
        // evaluator error cannot bypass retained-directory revalidation.
        revalidate(
            fragment,
            RootedDeclarationRevalidationPhase::AfterEvaluation,
            true,
        )?;
        let evaluation = result.map_err(|error| match error {
            DeclarationEvaluationError::Evaluation(source) => {
                LoadRootedDeclarationsError::Evaluation {
                    path: fragment.physical_path().to_owned(),
                    source,
                }
            }
            DeclarationEvaluationError::Conversion(source) => {
                LoadRootedDeclarationsError::Conversion {
                    path: fragment.physical_path().to_owned(),
                    source,
                }
            }
        })?;

        loaded.insert(
            fragment.logical_name().to_owned(),
            LoadedDeclaration {
                logical_name: fragment.logical_name().to_owned(),
                path: fragment.physical_path().to_owned(),
                language: fragment.language().clone(),
                value: evaluation.value,
                identity: evaluation.identity,
            },
        );
    }

    fragments.revalidate().map_err(|source| {
        LoadRootedDeclarationsError::Revalidation {
            path: root_path.to_owned(),
            phase: RootedDeclarationRevalidationPhase::FinalSet,
            source,
        }
    })?;
    Ok(loaded.into_values().collect())
}

fn revalidate<E>(
    fragment: &RootedFragmentDeclaration,
    phase: RootedDeclarationRevalidationPhase,
    after: bool,
) -> Result<(), LoadRootedDeclarationsError<E>> {
    let result = if after {
        fragment.revalidate_after_evaluation()
    } else {
        fragment.revalidate_before_evaluation()
    };
    result.map_err(|source| LoadRootedDeclarationsError::Revalidation {
        path: fragment.physical_path().to_owned(),
        phase,
        source,
    })
}
