use std::fmt;

use declarative_config::{Diagnostic, DiagnosticCategory, LimitKind, SourceSpan};
use gluon::base::error::InFile;

pub(crate) fn from_gluon(error: gluon::Error, timed_out: bool) -> Diagnostic {
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
            .map(|error| from_gluon(error.clone(), timed_out).category)
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

    Diagnostic::new(category, limit, source_name, span, message).with_source(error)
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
