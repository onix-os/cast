// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use std::{fs, time::Duration};

use gluon_config::{DiagnosticCategory, Evaluator, LimitKind, Limits, Source, SourceRoot};

#[test]
fn evaluates_a_string_literal() {
    let source = Source::new("literal.glu", r#""declarative""#);
    let evaluation = Evaluator::default().evaluate::<String>(&source).unwrap();

    assert_eq!(evaluation.value, "declarative");
    assert_eq!(evaluation.fingerprint.gluon_version, "0.18.3");
    assert!(evaluation.fingerprint.imported_modules.is_empty());
}

#[test]
fn fingerprints_source_and_explicit_inputs() {
    let evaluator = Evaluator::default();
    let source = Source::new("literal.glu", "42");
    let first = evaluator.evaluate_with_inputs::<i64>(&source, b"first").unwrap();
    let repeated = evaluator.evaluate_with_inputs::<i64>(&source, b"first").unwrap();
    let changed = evaluator.evaluate_with_inputs::<i64>(&source, b"second").unwrap();

    assert_eq!(first.fingerprint, repeated.fingerprint);
    assert_ne!(first.fingerprint.sha256, changed.fingerprint.sha256);
}

#[test]
fn source_root_loads_only_contained_root_files() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("config.glu"), r#""rooted""#).unwrap();
    let root = SourceRoot::new(directory.path()).unwrap();
    let evaluator = Evaluator::default().with_source_root(root);

    assert_eq!(evaluator.evaluate_file::<String>("config.glu").unwrap().value, "rooted");
    assert!(evaluator.evaluate_file::<String>("../config.glu").is_err());
}

#[test]
fn host_and_nondeterministic_modules_are_denied() {
    for module in ["std.fs", "std.io", "std.process", "std.env", "std.random"] {
        let source = Source::new(format!("deny-{module}.glu"), format!("let _ = import! {module} in 0"));
        let error = Evaluator::default().evaluate::<i64>(&source).unwrap_err();
        assert_eq!(error.category, DiagnosticCategory::Import, "{module}: {error}");
        assert!(error.message.contains(module), "{module}: {error}");
    }
}

#[test]
fn arbitrary_and_ambient_imports_are_closed() {
    let source = Source::new("ambient.glu", "let _ = import! local.module in 0");
    let error = Evaluator::default().evaluate::<i64>(&source).unwrap_err();

    assert_eq!(error.category, DiagnosticCategory::Import);
    assert!(error.message.contains("local.module"));
}

#[test]
fn recursive_evaluation_is_interrupted_by_the_watchdog() {
    let limits = Limits {
        timeout: Duration::from_millis(50),
        ..Limits::default()
    };
    let source = Source::new("recursive.glu", "rec let loop value = loop value\nloop 0");
    let error = Evaluator::new(limits).evaluate::<i64>(&source).unwrap_err();

    assert_eq!(error.category, DiagnosticCategory::Limit);
    assert_eq!(error.limit, Some(LimitKind::Time));
}

#[test]
fn memory_exhaustion_is_a_structured_limit_error() {
    let limits = Limits {
        memory_bytes: 10,
        ..Limits::default()
    };
    let source = Source::new("memory.glu", "[1, 2, 3, 4]");
    let error = Evaluator::new(limits).evaluate::<Vec<i64>>(&source).unwrap_err();

    assert_eq!(error.category, DiagnosticCategory::Limit);
    assert_eq!(error.limit, Some(LimitKind::Memory));
}

#[test]
fn malformed_programs_have_a_source_span() {
    let source = Source::new("malformed.glu", "let x = in x");
    let error = Evaluator::default().evaluate::<i64>(&source).unwrap_err();

    assert_eq!(error.category, DiagnosticCategory::Parse);
    assert_eq!(error.source_name.as_deref(), Some("malformed.glu"));
    assert!(error.span.is_some());
}

#[test]
fn source_size_is_checked_before_evaluation() {
    let limits = Limits {
        max_source_bytes: 2,
        ..Limits::default()
    };
    let error = Evaluator::new(limits)
        .evaluate::<i64>(&Source::new("large.glu", "123"))
        .unwrap_err();

    assert_eq!(error.category, DiagnosticCategory::Limit);
    assert_eq!(error.limit, Some(LimitKind::SourceSize));
}
