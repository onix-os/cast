// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use std::{error::Error, fmt, io, sync::Arc};

use gluon::base::error::InFile;

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
enum DiagnosticSource {
    Gluon(Arc<gluon::Error>),
    Io(Arc<io::Error>),
}

#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub category: DiagnosticCategory,
    pub limit: Option<LimitKind>,
    pub source_name: Option<String>,
    pub span: Option<SourceSpan>,
    pub message: String,
    source: Option<DiagnosticSource>,
}

impl Diagnostic {
    pub(crate) fn limit(kind: LimitKind, source_name: Option<String>, message: impl Into<String>) -> Self {
        Self {
            category: DiagnosticCategory::Limit,
            limit: Some(kind),
            source_name,
            span: None,
            message: message.into(),
            source: None,
        }
    }

    pub(crate) fn io(source_name: Option<String>, error: io::Error) -> Self {
        Self {
            category: DiagnosticCategory::Io,
            limit: None,
            source_name,
            span: None,
            message: error.to_string(),
            source: Some(DiagnosticSource::Io(Arc::new(error))),
        }
    }

    pub(crate) fn import(source_name: Option<String>, message: impl Into<String>) -> Self {
        Self {
            category: DiagnosticCategory::Import,
            limit: None,
            source_name,
            span: None,
            message: message.into(),
            source: None,
        }
    }

    pub(crate) fn internal(message: impl Into<String>) -> Self {
        Self {
            category: DiagnosticCategory::Internal,
            limit: None,
            source_name: None,
            span: None,
            message: message.into(),
            source: None,
        }
    }

    pub(crate) fn from_gluon(error: gluon::Error, timed_out: bool) -> Self {
        let category = match &error {
            gluon::Error::Parse(_) => DiagnosticCategory::Parse,
            gluon::Error::Typecheck(_) => DiagnosticCategory::Type,
            gluon::Error::Macro(_) | gluon::Error::Other(_)
                if error.to_string().contains("configuration import denied") =>
            {
                DiagnosticCategory::Import
            }
            gluon::Error::IO(_) => DiagnosticCategory::Io,
            gluon::Error::VM(gluon::vm::Error::OutOfMemory { .. })
            | gluon::Error::VM(gluon::vm::Error::StackOverflow(_))
            | gluon::Error::VM(gluon::vm::Error::Interrupted) => DiagnosticCategory::Limit,
            gluon::Error::Multiple(errors) => errors
                .iter()
                .map(|error| Self::from_gluon(error.clone(), timed_out).category)
                .next()
                .unwrap_or(DiagnosticCategory::Runtime),
            _ => DiagnosticCategory::Runtime,
        };

        let limit = match &error {
            gluon::Error::VM(gluon::vm::Error::OutOfMemory { .. }) => Some(LimitKind::Memory),
            gluon::Error::VM(gluon::vm::Error::StackOverflow(_)) => Some(LimitKind::Stack),
            gluon::Error::VM(gluon::vm::Error::Interrupted) if timed_out => Some(LimitKind::Time),
            _ => None,
        };
        let (source_name, span) = error_location(&error).unwrap_or((None, None));
        let message = error.emit_string().unwrap_or_else(|_| error.to_string());

        Self {
            category,
            limit,
            source_name,
            span,
            message,
            source: Some(DiagnosticSource::Gluon(Arc::new(error))),
        }
    }
}

fn in_file_location<E: fmt::Display>(error: &InFile<E>) -> (Option<String>, Option<SourceSpan>) {
    let span = error.errors().iter().next().and_then(|error_item| {
        error_item.span.to_range(error.source()).map(|range| SourceSpan {
            start: range.start,
            end: range.end,
        })
    });
    (Some(error.source_name().to_owned()), span)
}

fn error_location(error: &gluon::Error) -> Option<(Option<String>, Option<SourceSpan>)> {
    match error {
        gluon::Error::Parse(error) => Some(in_file_location(error)),
        gluon::Error::Typecheck(error) => Some(in_file_location(error)),
        gluon::Error::Macro(error) => Some(in_file_location(error)),
        gluon::Error::Multiple(errors) => errors.iter().find_map(error_location),
        _ => None,
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for Diagnostic {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self.source.as_ref()? {
            DiagnosticSource::Gluon(error) => Some(error.as_ref()),
            DiagnosticSource::Io(error) => Some(error.as_ref()),
        }
    }
}
