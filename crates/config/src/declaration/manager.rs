use std::{collections::BTreeMap, fmt, path::PathBuf};

use declarative_config::{
    DeclarationCodec, DeclarationEvaluationError, EvaluationDeadline,
    LanguageSpec,
};

use super::{
    ConfigDeclarationEvaluator, DeclarationEvaluatorSet,
    DeclarationRevalidationPhase, DeleteManagedDeclarationError,
    DiscoveredFragmentDeclaration, FragmentDeclarationLimits,
    FragmentDeclarationSet, GeneratedDeclarationAuthority,
    GeneratedDeclarationSlot,
    GeneratedDeclarationSlotError, LoadedDeclaration,
    LoadManagedDeclarationError, SaveManagedDeclarationError,
};
use crate::{Config, Entry, Manager};

const MAX_DECLARATION_FRAGMENTS: usize = 1024;
const MAX_DECLARATION_DIRECTORY_ENTRIES: usize = 4096;
const MAX_GENERATED_DECLARATION_BYTES: usize = 1024 * 1024;

impl Manager {
    /// Evaluate every discovered declaration before applying whole-fragment
    /// precedence. The evaluator set determines the exact registered file
    /// extensions and dispatches each source to its typed adapter.
    pub fn load_declarations<E>(
        &self,
        evaluators: &DeclarationEvaluatorSet<E>,
    ) -> Result<
        Vec<LoadedDeclaration<E::Config, E::Identity>>,
        LoadManagedDeclarationError<E::Error>,
    >
    where
        E: ConfigDeclarationEvaluator,
    {
        let layers = self
            .scope
            .load_with()
            .into_iter()
            .filter_map(|(entry, location)| match entry {
                Entry::File => Some(location.config_dir()),
                Entry::Directory => None,
            })
            .collect();
        let fragments = FragmentDeclarationSet::discover(
            layers,
            E::Config::domain(),
            evaluators.languages(),
            FragmentDeclarationLimits::new(
                MAX_DECLARATION_FRAGMENTS,
                MAX_DECLARATION_DIRECTORY_ENTRIES,
            ),
        )
        .map_err(|source| LoadManagedDeclarationError::Discovery { source })?;

        let mut evaluated = Vec::with_capacity(fragments.len());
        for fragment in fragments.iter() {
            let evaluator = evaluators.get(fragment.language()).ok_or_else(|| {
                LoadManagedDeclarationError::UnregisteredLanguage {
                    path: fragment.physical_path().to_owned(),
                    extension: fragment.language().extension().to_owned(),
                }
            })?;

            revalidate(
                fragment,
                DeclarationRevalidationPhase::BeforeRead,
                true,
            )?;
            // Each fragment receives its own budget spanning read through decode.
            let deadline = EvaluationDeadline::start(evaluator.limits().timeout);
            let source = fragment
                .source_root()
                .load(
                    fragment.relative_path(),
                    evaluator.limits().max_source_bytes,
                )
                .map_err(|source| LoadManagedDeclarationError::Read {
                    path: fragment.physical_path().to_owned(),
                    source,
                })?;
            revalidate(
                fragment,
                DeclarationRevalidationPhase::AfterRead,
                false,
            )?;
            revalidate(
                fragment,
                DeclarationRevalidationPhase::BeforeEvaluation,
                true,
            )?;

            let rooted = evaluator.with_source_root(fragment.source_root().clone());
            let result = rooted.evaluate_within(&source, deadline);
            revalidate(
                fragment,
                DeclarationRevalidationPhase::AfterEvaluation,
                false,
            )?;
            let evaluation = result.map_err(|error| match error {
                DeclarationEvaluationError::Evaluation(source) => {
                    LoadManagedDeclarationError::Evaluation {
                        path: fragment.physical_path().to_owned(),
                        source,
                    }
                }
                DeclarationEvaluationError::Conversion(source) => {
                    LoadManagedDeclarationError::Conversion {
                        path: fragment.physical_path().to_owned(),
                        source,
                    }
                }
            })?;

            evaluated.push(LoadedDeclaration {
                logical_name: fragment.logical_name().to_owned(),
                path: fragment.physical_path().to_owned(),
                language: fragment.language().clone(),
                value: evaluation.value,
                identity: evaluation.identity,
            });
        }

        let mut selected = BTreeMap::new();
        for declaration in evaluated {
            selected.insert(declaration.logical_name.clone(), declaration);
        }
        Ok(selected.into_values().collect())
    }

    /// Canonically encode and atomically save one generated declaration.
    /// The complete evaluator set defines every extension that can own the
    /// logical output. `active_language` selects one exact registered codec;
    /// neither selection nor collision handling inspects declaration bytes.
    pub fn save_declaration<E>(
        &self,
        name: impl fmt::Display,
        value: &E::Config,
        evaluators: &DeclarationEvaluatorSet<E>,
        active_language: &LanguageSpec,
    ) -> Result<PathBuf, SaveManagedDeclarationError<E::Error>>
    where
        E: ConfigDeclarationEvaluator + DeclarationCodec<E::Config>,
    {
        let codec = active_evaluator(evaluators, active_language)
            .map_err(|source| SaveManagedDeclarationError::SlotPolicy { source })?;
        let literal = codec
            .encode(value)
            .map_err(|source| SaveManagedDeclarationError::Conversion { source })?;
        let marker = codec.language_spec().generated_marker();
        let generated_size = marker
            .len()
            .saturating_add(literal.len())
            .saturating_add(usize::from(!literal.ends_with('\n')));
        let mut generated = String::with_capacity(generated_size);
        generated.push_str(marker);
        generated.push_str(&literal);
        if !generated.ends_with('\n') {
            generated.push('\n');
        }

        let slot = generated_slot(
            self.scope.save_dir(&E::Config::domain()),
            name.to_string(),
            evaluators,
            active_language,
        )
        .map_err(|source| SaveManagedDeclarationError::SlotPolicy { source })?;
        slot.save(generated.as_bytes())
            .map_err(|source| SaveManagedDeclarationError::Storage { source })
    }

    /// Delete the sole generated declaration across the complete registered
    /// language set. The exact active descriptor supplies policy only; disk
    /// bytes never select an adapter.
    pub fn delete_declaration<E>(
        &self,
        name: impl fmt::Display,
        evaluators: &DeclarationEvaluatorSet<E>,
        active_language: &LanguageSpec,
    ) -> Result<(), DeleteManagedDeclarationError>
    where
        E: ConfigDeclarationEvaluator,
    {
        let slot = generated_slot(
            self.scope.save_dir(&E::Config::domain()),
            name.to_string(),
            evaluators,
            active_language,
        )
        .map_err(|source| DeleteManagedDeclarationError::SlotPolicy { source })?;
        slot.delete()
            .map_err(|source| DeleteManagedDeclarationError::Storage { source })
    }
}

fn revalidate<E>(
    fragment: &DiscoveredFragmentDeclaration,
    phase: DeclarationRevalidationPhase,
    before: bool,
) -> Result<(), LoadManagedDeclarationError<E>> {
    let result = if before {
        fragment.revalidate_before_read()
    } else {
        fragment.revalidate_after_read()
    };
    result.map_err(|source| LoadManagedDeclarationError::Revalidation {
        path: fragment.physical_path().to_owned(),
        phase,
        source,
    })
}

fn generated_slot<E>(
    directory: PathBuf,
    name: String,
    evaluators: &DeclarationEvaluatorSet<E>,
    active_language: &LanguageSpec,
) -> Result<GeneratedDeclarationSlot, GeneratedDeclarationSlotError>
where
    E: ConfigDeclarationEvaluator,
{
    active_evaluator(evaluators, active_language)?;
    let authorities = evaluators
        .languages()
        .iter()
        .map(|language| {
            GeneratedDeclarationAuthority::new(
                language.clone(),
                language.generated_marker(),
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    let active_authority = authorities
        .iter()
        .find(|authority| authority.language_spec() == active_language)
        .cloned()
        .ok_or_else(|| unregistered_active(active_language))?;
    let temporary_prefix = format!(
        ".{}-tmp-",
        active_language.language().as_str(),
    );
    GeneratedDeclarationSlot::with_registered_authorities(
        directory,
        name,
        authorities,
        active_authority,
        MAX_GENERATED_DECLARATION_BYTES,
        temporary_prefix,
    )
}

fn active_evaluator<'a, E>(
    evaluators: &'a DeclarationEvaluatorSet<E>,
    active_language: &LanguageSpec,
) -> Result<&'a E, GeneratedDeclarationSlotError>
where
    E: ConfigDeclarationEvaluator,
{
    evaluators
        .get(active_language)
        .ok_or_else(|| unregistered_active(active_language))
}

fn unregistered_active(
    active_language: &LanguageSpec,
) -> GeneratedDeclarationSlotError {
    GeneratedDeclarationSlotError::ActiveAuthorityNotRegistered {
        extension: active_language.extension().to_owned(),
    }
}
