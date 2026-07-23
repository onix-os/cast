use std::{collections::BTreeMap, error::Error, fmt};

use declarative_config::{DeclarationEvaluator, LanguageSpec};

use super::{LanguageRegistrationError, RegisteredLanguages};
use crate::Config;

/// Associates a typed declaration adapter with the configuration domain it
/// owns. Implementations remain concrete, so a consumer can use an enum to
/// combine several engines without runtime type erasure or a global registry.
pub trait ConfigDeclarationEvaluator:
    DeclarationEvaluator<Self::Config>
{
    type Config: Config;
}

/// Exact extension dispatch for one typed configuration domain.
///
/// `E` may be a single adapter or a consumer-owned enum whose variants wrap
/// heterogeneous engines. All variants produce the same owned domain value,
/// identity type, and conversion-error type through their trait implementation.
#[derive(Debug, Clone)]
pub struct DeclarationEvaluatorSet<E>
where
    E: ConfigDeclarationEvaluator,
{
    by_extension: BTreeMap<String, E>,
    languages: RegisteredLanguages,
}

impl<E> DeclarationEvaluatorSet<E>
where
    E: ConfigDeclarationEvaluator,
{
    pub fn new(
        evaluators: impl IntoIterator<Item = E>,
    ) -> Result<Self, DeclarationEvaluatorSetError> {
        let evaluators = evaluators.into_iter().collect::<Vec<_>>();
        let languages = RegisteredLanguages::new(
            evaluators
                .iter()
                .map(|evaluator| evaluator.language_spec().clone()),
        )
        .map_err(DeclarationEvaluatorSetError::LanguageRegistration)?;

        let by_extension = evaluators
            .into_iter()
            .map(|evaluator| {
                (evaluator.language_spec().extension().to_owned(), evaluator)
            })
            .collect();
        Ok(Self {
            by_extension,
            languages,
        })
    }

    pub fn len(&self) -> usize {
        self.by_extension.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_extension.is_empty()
    }

    pub fn languages(&self) -> &RegisteredLanguages {
        &self.languages
    }

    pub fn get(&self, language: &LanguageSpec) -> Option<&E> {
        self.by_extension
            .get(language.extension())
            .filter(|evaluator| evaluator.language_spec() == language)
    }
}

#[derive(Debug)]
pub enum DeclarationEvaluatorSetError {
    LanguageRegistration(LanguageRegistrationError),
}

impl fmt::Display for DeclarationEvaluatorSetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LanguageRegistration(error) => error.fmt(formatter),
        }
    }
}

impl Error for DeclarationEvaluatorSetError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::LanguageRegistration(error) => Some(error),
        }
    }
}
