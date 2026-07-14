
use std::{env, path::Path, time::Duration};

use fs_err as fs;
use gluon_config::{DiagnosticCategory, Evaluator, ImportPolicy, LimitKind, Limits, Source, SourceRoot};

#[derive(Debug, PartialEq, Eq, gluon_codegen::Getable, gluon_codegen::VmType)]
struct LiteralRecord {
    name: String,
    generation: i64,
}

#[test]
fn evaluates_a_typed_record_literal() {
    let source = Source::new("literal.glu", r#"{ name = "declarative", generation = 1 }"#);
    let evaluation = Evaluator::default().evaluate::<LiteralRecord>(&source).unwrap();

    assert_eq!(
        evaluation.value,
        LiteralRecord {
            name: "declarative".to_owned(),
            generation: 1,
        }
    );
    assert_eq!(evaluation.fingerprint.gluon_version, "0.18.3");
    assert_eq!(evaluation.fingerprint.root_logical_name, "literal.glu");
    assert!(evaluation.fingerprint.imported_modules.is_empty());
    assert_eq!(evaluation.fingerprint.validate(), Ok(()));
}

#[test]
fn root_logical_name_participates_in_the_evaluation_identity() {
    let evaluator = Evaluator::default();
    let first = evaluator.evaluate::<i64>(&Source::new("first.glu", "42")).unwrap();
    let renamed = evaluator.evaluate::<i64>(&Source::new("renamed.glu", "42")).unwrap();

    assert_eq!(first.value, renamed.value);
    assert_eq!(first.fingerprint.root_logical_name, "first.glu");
    assert_eq!(renamed.fingerprint.root_logical_name, "renamed.glu");
    assert_eq!(
        first.fingerprint.root_source_sha256,
        renamed.fingerprint.root_source_sha256
    );
    assert_ne!(first.fingerprint.sha256, renamed.fingerprint.sha256);
}

#[test]
fn pure_array_primitives_are_explicit_and_fingerprinted() {
    let source = Source::new(
        "array-policy.glu",
        r#"let array = import! std.array.prim
array.append ["one"] ["two"]"#,
    );
    let denied = Evaluator::default().evaluate::<Vec<String>>(&source).unwrap_err();
    assert_eq!(denied.category, DiagnosticCategory::Import);

    let mut policy = ImportPolicy::new();
    policy.enable_array_primitives();
    let evaluated = Evaluator::default()
        .with_import_policy(policy)
        .evaluate::<Vec<String>>(&source)
        .unwrap();

    assert_eq!(evaluated.value, ["one", "two"]);
    assert!(
        evaluated
            .fingerprint
            .imported_modules
            .iter()
            .any(|module| module.logical_name == "std.array.prim")
    );
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
fn explicit_evaluation_inputs_are_bounded_before_fingerprinting() {
    let limits = Limits {
        max_explicit_input_bytes: 2,
        ..Limits::default()
    };
    let evaluator = Evaluator::new(limits);
    let source = Source::new("explicit-inputs.glu", "42");

    assert_eq!(evaluator.evaluate_with_inputs::<i64>(&source, b"12").unwrap().value, 42);
    let error = evaluator.evaluate_with_inputs::<i64>(&source, b"123").unwrap_err();

    assert_eq!(error.category, DiagnosticCategory::Limit);
    assert_eq!(error.limit, Some(LimitKind::ExplicitInputSize));
    assert_eq!(error.source_name.as_deref(), Some("explicit-inputs.glu"));
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
    for module in [
        "std.fs",
        "std.io",
        "std.process",
        "std.env",
        "std.random",
        "std.http",
        "std.time",
    ] {
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
fn zero_timeout_wins_before_import_discovery_and_parsing() {
    let limits = Limits {
        timeout: Duration::ZERO,
        ..Limits::default()
    };
    // Without the total deadline this would be reported as a parse error
    // before the old run_expr-only watchdog was even started.
    let source = Source::new("malformed.glu", "let x = in import! \"missing.glu\"");

    let error = Evaluator::new(limits).evaluate::<i64>(&source).unwrap_err();

    assert_eq!(error.category, DiagnosticCategory::Limit);
    assert_eq!(error.limit, Some(LimitKind::Time));
    assert_eq!(error.source_name.as_deref(), Some("malformed.glu"));
}

#[test]
fn evaluate_file_uses_one_deadline_for_loading_and_evaluation() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("config.glu"), "42").unwrap();
    let limits = Limits {
        timeout: Duration::ZERO,
        ..Limits::default()
    };
    let evaluator = Evaluator::new(limits).with_source_root(SourceRoot::new(directory.path()).unwrap());

    let error = evaluator.evaluate_file::<i64>("config.glu").unwrap_err();

    assert_eq!(error.category, DiagnosticCategory::Limit);
    assert_eq!(error.limit, Some(LimitKind::Time));
    assert_eq!(error.source_name.as_deref(), Some("config.glu"));
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
fn ill_typed_programs_have_a_source_span() {
    let source = Source::new("ill-typed.glu", r#""not an integer""#);
    let error = Evaluator::default().evaluate::<i64>(&source).unwrap_err();

    assert_eq!(error.category, DiagnosticCategory::Type);
    assert_eq!(error.source_name.as_deref(), Some("ill-typed.glu"));
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

#[test]
fn evaluates_a_contained_relative_import() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("answer.glu"), "42").unwrap();
    let evaluator = rooted_evaluator(directory.path());
    let source = Source::new("root.glu", "let answer = import! \"./answer.glu\"\nanswer");

    let evaluation = evaluator.evaluate::<i64>(&source).unwrap();

    assert_eq!(evaluation.value, 42);
    assert_eq!(evaluation.fingerprint.imported_modules.len(), 1);
    assert_eq!(evaluation.fingerprint.imported_modules[0].logical_name, "answer.glu");
}

#[test]
fn nested_relative_imports_resolve_from_the_importing_module() {
    let directory = tempfile::tempdir().unwrap();
    let nested = directory.path().join("nested");
    fs::create_dir(&nested).unwrap();
    fs::write(nested.join("answer.glu"), "42").unwrap();
    fs::write(nested.join("module.glu"), "import! \"answer.glu\"").unwrap();
    let evaluator = rooted_evaluator(directory.path());

    let evaluation = evaluator
        .evaluate::<i64>(&Source::new("root.glu", "import! \"nested/module.glu\""))
        .unwrap();

    assert_eq!(evaluation.value, 42);
    assert_eq!(
        evaluation
            .fingerprint
            .imported_modules
            .iter()
            .map(|module| module.logical_name.as_str())
            .collect::<Vec<_>>(),
        ["nested/answer.glu", "nested/module.glu"]
    );
}

#[test]
fn evaluates_an_explicit_embedded_module() {
    let policy = ImportPolicy::new().with_embedded_module("cast.answer", "42").unwrap();
    let source = Source::new("root.glu", "let answer = import! cast.answer\nanswer");

    let evaluation = Evaluator::default()
        .with_import_policy(policy)
        .evaluate::<i64>(&source)
        .unwrap();

    assert_eq!(evaluation.value, 42);
    assert_eq!(evaluation.fingerprint.imported_modules.len(), 1);
    assert_eq!(evaluation.fingerprint.imported_modules[0].logical_name, "cast.answer");
}

#[test]
fn embedded_module_size_is_bounded_before_evaluation() {
    let limits = Limits {
        max_imported_file_bytes: 2,
        ..Limits::default()
    };
    let source = Source::new("root.glu", "import! cast.answer");

    let exact = ImportPolicy::new().with_embedded_module("cast.answer", "42").unwrap();
    assert_eq!(
        Evaluator::new(limits)
            .with_import_policy(exact)
            .evaluate::<i64>(&source)
            .unwrap()
            .value,
        42
    );

    let oversized = ImportPolicy::new().with_embedded_module("cast.answer", "420").unwrap();
    let error = Evaluator::new(limits)
        .with_import_policy(oversized)
        .evaluate::<i64>(&source)
        .unwrap_err();
    assert_eq!(error.category, DiagnosticCategory::Limit);
    assert_eq!(error.limit, Some(LimitKind::ImportedFileSize));
    assert_eq!(error.source_name.as_deref(), Some("cast.answer"));
}

#[test]
fn duplicate_embedded_module_is_rejected_without_replacing_the_first() {
    let mut policy = ImportPolicy::new();
    policy.insert_embedded_module("cast.value", "41").unwrap();
    let error = policy.insert_embedded_module("cast.value", "42").unwrap_err();
    let evaluation = Evaluator::default()
        .with_import_policy(policy)
        .evaluate::<i64>(&Source::new("root.glu", "import! cast.value"))
        .unwrap();

    assert_eq!(error.category, DiagnosticCategory::Import);
    assert_eq!(evaluation.value, 41);
}

#[test]
fn parent_traversal_and_absolute_imports_are_rejected() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("root");
    fs::create_dir(&root).unwrap();
    fs::write(directory.path().join("outside.glu"), "42").unwrap();
    let evaluator = rooted_evaluator(&root);

    let traversal = evaluator
        .evaluate::<i64>(&Source::new("root.glu", "import! \"../outside.glu\""))
        .unwrap_err();
    assert_eq!(traversal.category, DiagnosticCategory::Import);
    assert!(traversal.message.contains("parent traversal"));

    let absolute_path = directory.path().join("outside.glu");
    let absolute = evaluator
        .evaluate::<i64>(&Source::new(
            "root.glu",
            format!("import! {:?}", absolute_path.display().to_string()),
        ))
        .unwrap_err();
    assert_eq!(absolute.category, DiagnosticCategory::Import);
    assert!(absolute.message.contains("absolute"));
}

#[cfg(unix)]
#[test]
fn symlink_escape_is_rejected_without_following_the_target() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("root");
    fs::create_dir(&root).unwrap();
    fs::write(directory.path().join("outside.glu"), "42").unwrap();
    symlink(directory.path().join("outside.glu"), root.join("escape.glu")).unwrap();

    let error = rooted_evaluator(&root)
        .evaluate::<i64>(&Source::new("root.glu", "import! \"escape.glu\""))
        .unwrap_err();

    assert_eq!(error.category, DiagnosticCategory::Import);
    assert_eq!(error.source_name.as_deref(), Some("escape.glu"));
    assert!(error.message.contains("cannot be loaded"));
}

#[test]
fn ambient_current_directory_is_not_an_import_root() {
    let ambient = tempfile::tempdir_in(env::current_dir().unwrap()).unwrap();
    fs::write(ambient.path().join("answer.glu"), "42").unwrap();
    let relative = ambient
        .path()
        .strip_prefix(env::current_dir().unwrap())
        .unwrap()
        .join("answer.glu");
    let source = Source::new("root.glu", format!("import! {:?}", portable_path(&relative)));

    let error = Evaluator::default().evaluate::<i64>(&source).unwrap_err();

    assert_eq!(error.category, DiagnosticCategory::Import);
    assert!(error.message.contains("explicit SourceRoot"));
}

#[test]
fn gluon_path_does_not_affect_import_resolution() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("ambient.glu"), "42").unwrap();
    let previous = env::var_os("GLUON_PATH");
    // SAFETY: This test restores the variable before returning. The evaluator
    // never reads it, so concurrent evaluator tests are unaffected.
    unsafe { env::set_var("GLUON_PATH", directory.path()) };

    let result = Evaluator::default().evaluate::<i64>(&Source::new("root.glu", "import! \"ambient.glu\""));

    // SAFETY: Restore the process environment to its pre-test value.
    unsafe {
        match previous {
            Some(value) => env::set_var("GLUON_PATH", value),
            None => env::remove_var("GLUON_PATH"),
        }
    }
    let error = result.unwrap_err();
    assert_eq!(error.category, DiagnosticCategory::Import);
    assert!(error.message.contains("explicit SourceRoot"));
}

#[test]
fn imported_file_size_is_bounded() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("large.glu"), "123").unwrap();
    let limits = Limits {
        max_imported_file_bytes: 2,
        ..Limits::default()
    };
    let evaluator = Evaluator::new(limits).with_source_root(SourceRoot::new(directory.path()).unwrap());

    let error = evaluator
        .evaluate::<i64>(&Source::new("root.glu", "import! \"large.glu\""))
        .unwrap_err();

    assert_eq!(error.category, DiagnosticCategory::Limit);
    assert_eq!(error.limit, Some(LimitKind::ImportedFileSize));
}

#[test]
fn import_count_is_bounded() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("one.glu"), "1").unwrap();
    fs::write(directory.path().join("two.glu"), "2").unwrap();
    let limits = Limits {
        max_imports: 1,
        ..Limits::default()
    };
    let evaluator = Evaluator::new(limits).with_source_root(SourceRoot::new(directory.path()).unwrap());
    let source = Source::new(
        "root.glu",
        "let one = import! \"one.glu\"\nlet two = import! \"two.glu\"\none + two",
    );

    let error = evaluator.evaluate::<i64>(&source).unwrap_err();

    assert_eq!(error.category, DiagnosticCategory::Limit);
    assert_eq!(error.limit, Some(LimitKind::ImportCount));
}

#[test]
fn total_import_graph_size_is_bounded() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("answer.glu"), "42").unwrap();
    let root_text = "import! \"answer.glu\"";
    let limits = Limits {
        max_import_graph_bytes: root_text.len() + 1,
        ..Limits::default()
    };
    let evaluator = Evaluator::new(limits).with_source_root(SourceRoot::new(directory.path()).unwrap());

    let error = evaluator
        .evaluate::<i64>(&Source::new("root.glu", root_text))
        .unwrap_err();

    assert_eq!(error.category, DiagnosticCategory::Limit);
    assert_eq!(error.limit, Some(LimitKind::ImportGraphSize));
}

#[test]
fn imported_content_changes_the_evaluation_fingerprint() {
    let directory = tempfile::tempdir().unwrap();
    let imported = directory.path().join("answer.glu");
    fs::write(&imported, "41").unwrap();
    let evaluator = rooted_evaluator(directory.path());
    let source = Source::new("root.glu", "import! \"answer.glu\"");

    let first = evaluator.evaluate::<i64>(&source).unwrap();
    fs::write(&imported, "42").unwrap();
    let changed = evaluator.evaluate::<i64>(&source).unwrap();

    assert_eq!(first.value, 41);
    assert_eq!(changed.value, 42);
    assert_ne!(
        first.fingerprint.imported_modules[0].sha256,
        changed.fingerprint.imported_modules[0].sha256
    );
    assert_ne!(first.fingerprint.sha256, changed.fingerprint.sha256);
}

#[test]
fn import_aliases_share_a_stable_logical_identity() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("answer.glu"), "42").unwrap();
    let evaluator = rooted_evaluator(directory.path());
    let source = Source::new(
        "root.glu",
        "let first = import! \"answer.glu\"\nlet second = import! \"./answer.glu\"\nsecond",
    );

    let evaluation = evaluator.evaluate::<i64>(&source).unwrap();

    assert_eq!(evaluation.value, 42);
    assert_eq!(evaluation.fingerprint.imported_modules.len(), 1);
    assert_eq!(evaluation.fingerprint.imported_modules[0].logical_name, "answer.glu");
}

#[cfg(unix)]
#[test]
fn symlink_imports_are_rejected_instead_of_becoming_aliases() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("answer.glu"), "42").unwrap();
    symlink("answer.glu", directory.path().join("alias.glu")).unwrap();
    let evaluator = rooted_evaluator(directory.path());
    let source = Source::new("root.glu", "import! \"alias.glu\"");

    let error = evaluator.evaluate::<i64>(&source).unwrap_err();

    assert_eq!(error.category, DiagnosticCategory::Import);
    assert_eq!(error.source_name.as_deref(), Some("alias.glu"));
    assert!(error.message.contains("cannot be loaded"));
}

fn rooted_evaluator(path: &Path) -> Evaluator {
    Evaluator::default().with_source_root(SourceRoot::new(path).unwrap())
}

fn portable_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}
