use std::{error::Error, fmt, io, sync::Arc};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticCategory {
    Parse,
    Type,
    Import,
    Io,
    Limit,
    Runtime,
    Internal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitKind {
    SourceSize,
    ExplicitInputSize,
    ImportedFileSize,
    ImportCount,
    ImportGraphSize,
    Memory,
    Stack,
    Time,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceSpan {
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub category: DiagnosticCategory,
    pub limit: Option<LimitKind>,
    pub source_name: Option<String>,
    pub span: Option<SourceSpan>,
    pub message: String,
    source: Option<Arc<dyn Error + Send + Sync + 'static>>,
}

impl Diagnostic {
    pub fn new(
        category: DiagnosticCategory,
        limit: Option<LimitKind>,
        source_name: Option<String>,
        span: Option<SourceSpan>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            category,
            limit,
            source_name,
            span,
            message: message.into(),
            source: None,
        }
    }

    pub fn with_source(mut self, source: impl Error + Send + Sync + 'static) -> Self {
        self.source = Some(Arc::new(source));
        self
    }

    pub fn limit(kind: LimitKind, source_name: Option<String>, message: impl Into<String>) -> Self {
        Self::new(DiagnosticCategory::Limit, Some(kind), source_name, None, message)
    }

    pub fn io(source_name: Option<String>, error: io::Error) -> Self {
        Self::new(
            DiagnosticCategory::Io,
            None,
            source_name,
            None,
            error.to_string(),
        )
        .with_source(error)
    }

    pub fn import(source_name: Option<String>, message: impl Into<String>) -> Self {
        Self::new(DiagnosticCategory::Import, None, source_name, None, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(DiagnosticCategory::Internal, None, None, None, message)
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for Diagnostic {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source
            .as_deref()
            .map(|source| source as &(dyn Error + 'static))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_error_sources_remain_visible_after_cloning() {
        let diagnostic = Diagnostic::io(
            Some("root.decl".to_owned()),
            io::Error::new(io::ErrorKind::NotFound, "missing declaration"),
        );
        let cloned = diagnostic.clone();

        assert_eq!(cloned.category, DiagnosticCategory::Io);
        assert_eq!(cloned.source_name.as_deref(), Some("root.decl"));
        assert_eq!(cloned.to_string(), "missing declaration");
        assert_eq!(
            Error::source(&cloned).map(ToString::to_string),
            Some("missing declaration".to_owned())
        );
    }
}
