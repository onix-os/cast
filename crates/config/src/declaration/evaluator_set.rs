use std::{
    collections::BTreeMap,
    error::Error,
    fmt,
    marker::PhantomData,
};

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

/// Exact extension dispatch for one typed declaration value.
///
/// `E` may be a single adapter or a consumer-owned enum whose variants wrap
/// heterogeneous engines. All variants produce the same owned domain value,
/// identity type, and conversion-error type through their trait implementation.
pub struct TypedDeclarationEvaluatorSet<T, E>
where
    E: DeclarationEvaluator<T>,
{
    by_extension: BTreeMap<String, E>,
    languages: RegisteredLanguages,
    value: PhantomData<fn() -> T>,
}

impl<T, E> TypedDeclarationEvaluatorSet<T, E>
where
    E: DeclarationEvaluator<T>,
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
            value: PhantomData,
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

impl<T, E> Clone for TypedDeclarationEvaluatorSet<T, E>
where
    E: DeclarationEvaluator<T> + Clone,
{
    fn clone(&self) -> Self {
        Self {
            by_extension: self.by_extension.clone(),
            languages: self.languages.clone(),
            value: PhantomData,
        }
    }
}

impl<T, E> fmt::Debug for TypedDeclarationEvaluatorSet<T, E>
where
    E: DeclarationEvaluator<T> + fmt::Debug,
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TypedDeclarationEvaluatorSet")
            .field("by_extension", &self.by_extension)
            .field("languages", &self.languages)
            .finish()
    }
}

/// Configuration-domain wrapper retaining the original inferred API.
pub struct DeclarationEvaluatorSet<E>
where
    E: ConfigDeclarationEvaluator,
{
    typed: TypedDeclarationEvaluatorSet<E::Config, E>,
}

impl<E> DeclarationEvaluatorSet<E>
where
    E: ConfigDeclarationEvaluator,
{
    pub fn new(
        evaluators: impl IntoIterator<Item = E>,
    ) -> Result<Self, DeclarationEvaluatorSetError> {
        Ok(Self {
            typed: TypedDeclarationEvaluatorSet::new(evaluators)?,
        })
    }

    pub fn len(&self) -> usize {
        self.typed.len()
    }

    pub fn is_empty(&self) -> bool {
        self.typed.is_empty()
    }

    pub fn languages(&self) -> &RegisteredLanguages {
        self.typed.languages()
    }

    pub fn get(&self, language: &LanguageSpec) -> Option<&E> {
        self.typed.get(language)
    }
}

impl<E> Clone for DeclarationEvaluatorSet<E>
where
    E: ConfigDeclarationEvaluator + Clone,
{
    fn clone(&self) -> Self {
        Self {
            typed: self.typed.clone(),
        }
    }
}

impl<E> fmt::Debug for DeclarationEvaluatorSet<E>
where
    E: ConfigDeclarationEvaluator + fmt::Debug,
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("DeclarationEvaluatorSet")
            .field(&self.typed)
            .finish()
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
