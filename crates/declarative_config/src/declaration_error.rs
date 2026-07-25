use std::{error::Error, fmt};

use crate::Diagnostic;

/// Keeps engine diagnostics distinct from owned domain-conversion failures.
#[derive(Debug)]
pub enum DeclarationEvaluationError<E> {
    Evaluation(Diagnostic),
    Conversion(E),
}

impl<E> DeclarationEvaluationError<E> {
    pub fn conversion(error: E) -> Self {
        Self::Conversion(error)
    }
}

impl<E> From<Diagnostic> for DeclarationEvaluationError<E> {
    fn from(error: Diagnostic) -> Self {
        Self::Evaluation(error)
    }
}

impl<E: fmt::Display> fmt::Display for DeclarationEvaluationError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Evaluation(error) => error.fmt(formatter),
            Self::Conversion(error) => error.fmt(formatter),
        }
    }
}

impl<E> Error for DeclarationEvaluationError<E>
where
    E: Error + Send + Sync + 'static,
{
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Evaluation(error) => Some(error),
            Self::Conversion(error) => Some(error),
        }
    }
}
