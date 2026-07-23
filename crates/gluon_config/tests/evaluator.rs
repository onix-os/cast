use std::{env, path::Path, process::Command, time::Duration};

use fs_err as fs;
use gluon_config::{
    Diagnostic, DiagnosticCategory, Evaluator, ImportPolicy, LimitKind, Limits, Source, SourceRoot, SourceSpan,
};

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
fn pure_builtin_overrides_same_named_embedded_source_in_either_policy_order() {
    let source = Source::new(
        "array-overlap.glu",
        r#"let array = import! std.array.prim
array.append ["one"] ["two"]"#,
    );
    let mut builtin_only = ImportPolicy::new();
    builtin_only.enable_array_primitives();
    let expected = Evaluator::default()
        .with_import_policy(builtin_only)
        .evaluate::<Vec<String>>(&source)
        .unwrap();

    let mut source_then_builtin = ImportPolicy::new();
    source_then_builtin
        .insert_embedded_module("std.array.prim", "41")
        .unwrap();
    source_then_builtin.enable_array_primitives();

    let mut builtin_then_source = ImportPolicy::new();
    builtin_then_source.enable_array_primitives();
    builtin_then_source
        .insert_embedded_module("std.array.prim", "41")
        .unwrap();

    for policy in [source_then_builtin, builtin_then_source] {
        let evaluated = Evaluator::default()
            .with_import_policy(policy)
            .evaluate::<Vec<String>>(&source)
            .unwrap();
        assert_eq!(evaluated.value, ["one", "two"]);
        assert_eq!(evaluated.fingerprint, expected.fingerprint);
    }
}

#[test]
fn repeated_pure_builtin_counts_as_one_reachable_module() {
    let source = Source::new(
        "array-dedup.glu",
        r#"let first = import! std.array.prim
let second = import! std.array.prim
second.append ["one"] ["two"]"#,
    );
    let mut policy = ImportPolicy::new();
    policy.enable_array_primitives();
    let evaluated = Evaluator::new(Limits {
        max_imports: 1,
        ..Limits::default()
    })
    .with_import_policy(policy)
    .evaluate::<Vec<String>>(&source)
    .unwrap();

    assert_eq!(evaluated.value, ["one", "two"]);
    assert_eq!(evaluated.fingerprint.imported_modules.len(), 1);
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

#[test]
fn graph_policy_rejections_precede_relative_path_normalization() {
    let missing_root = Evaluator::default()
        .evaluate::<i64>(&Source::new("root.glu", "import! \"../invalid.glu\""))
        .unwrap_err();
    assert!(missing_root.message.contains("explicit SourceRoot"));
    assert!(!missing_root.message.contains("parent traversal"));

    let directory = tempfile::tempdir().unwrap();
    let policy = ImportPolicy::new()
        .with_embedded_module("cast.parent", "import! \"../invalid.glu\"")
        .unwrap();
    let embedded = Evaluator::default()
        .with_source_root(SourceRoot::new(directory.path()).unwrap())
        .with_import_policy(policy)
        .evaluate::<i64>(&Source::new("root.glu", "import! cast.parent"))
        .unwrap_err();
    assert!(embedded
        .message
        .contains("embedded modules cannot import source-root files"));
    assert!(!embedded.message.contains("parent traversal"));
}

#[test]
fn earlier_import_limit_wins_over_a_later_invalid_import_shape() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("large.glu"), "123").unwrap();
    let evaluator = Evaluator::new(Limits {
        max_imported_file_bytes: 2,
        ..Limits::default()
    })
    .with_source_root(SourceRoot::new(directory.path()).unwrap());
    let source = Source::new(
        "root.glu",
        "let large = import! \"large.glu\"\nlet invalid = import! 1\nlarge",
    );

    let error = evaluator.evaluate::<i64>(&source).unwrap_err();
    assert_eq!(error.category, DiagnosticCategory::Limit);
    assert_eq!(error.limit, Some(LimitKind::ImportedFileSize));
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

#[test]
fn full_fingerprint_v1_fields_and_hashes_are_frozen() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("z.glu"), "1").unwrap();
    fs::write(directory.path().join("a.glu"), "41").unwrap();
    let evaluator = rooted_evaluator(directory.path());
    let source = Source::new(
        "root.glu",
        "let z = import! \"z.glu\"\nlet a = import! \"./a.glu\"\na",
    );
    let evaluation = evaluator
        .evaluate_with_inputs::<i64>(&source, b"catalog:v1")
        .unwrap();

    assert_eq!(evaluation.value, 41);
    let fingerprint = evaluation.fingerprint;
    assert_eq!(fingerprint.root_logical_name, "root.glu");
    assert_eq!(
        fingerprint.root_source_sha256,
        "5f50214cf87556218166a815eed07d3adddd95d7cef230041dbfc02d07b6f6c5"
    );
    assert_eq!(
        fingerprint
            .imported_modules
            .iter()
            .map(|module| (module.logical_name.as_str(), module.sha256.as_str()))
            .collect::<Vec<_>>(),
        [
            (
                "a.glu",
                "3d914f9348c9cc0ff8a79716700b9fcd4d2f3e711608004eb8f138bcba7f14d9"
            ),
            (
                "z.glu",
                "6b86b273ff34fce19d6b804eff5a3f5747ada4eaa22f1d49c01e52ddb7875b4b"
            ),
        ]
    );
    assert_eq!(fingerprint.gluon_version, "0.18.3");
    assert_eq!(fingerprint.configuration_abi_version, 1);
    assert_eq!(fingerprint.evaluator_policy_version, 1);
    assert_eq!(
        fingerprint.explicit_inputs_sha256,
        "10cb3e66d2e2d8b3473f106fa7d4c10e1d6628bf56523183d317d4a8d6fb9324"
    );
    assert_eq!(
        fingerprint.sha256,
        "9a12e007085f94b216112f6c0d32ce24ac96ad2eafc8c4526b8619af0e1b889d"
    );
    assert_eq!(fingerprint.validate(), Ok(()));
}

const PROCESS_FINGERPRINT_MARKER: &str = "cast-process-fingerprint:";

#[test]
fn fingerprint_v1_is_stable_across_fresh_processes() {
    let executable = env::current_exe().unwrap();
    let evaluate = || {
        let output = Command::new(&executable)
            .args([
                "--ignored",
                "--exact",
                "fingerprint_v1_process_probe",
                "--nocapture",
            ])
            .env("CAST_FINGERPRINT_PROCESS_PROBE", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "fingerprint child failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );

        String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .find_map(|line| {
                line.find(PROCESS_FINGERPRINT_MARKER)
                    .map(|offset| line[offset + PROCESS_FINGERPRINT_MARKER.len()..].to_owned())
            })
            .expect("fingerprint child did not emit its result")
    };

    assert_eq!(evaluate(), evaluate());
}

#[test]
#[ignore = "spawned by fingerprint_v1_is_stable_across_fresh_processes"]
fn fingerprint_v1_process_probe() {
    if env::var_os("CAST_FINGERPRINT_PROCESS_PROBE").is_none() {
        return;
    }

    let policy = ImportPolicy::new()
        .with_embedded_module("cast.answer", "41")
        .unwrap();
    let evaluation = Evaluator::default()
        .with_import_policy(policy)
        .evaluate_with_inputs::<i64>(
            &Source::new("process-root.glu", "import! cast.answer"),
            b"process-input:v1",
        )
        .unwrap();
    println!("{PROCESS_FINGERPRINT_MARKER}{}", evaluation.fingerprint.sha256);
}

#[test]
fn current_import_cycles_are_deduplicated_before_gluon_rejects_them() {
    let directory = tempfile::tempdir().unwrap();

    fs::write(
        directory.path().join("cycle_a.glu"),
        "let b = import! \"cycle_b.glu\"\n42",
    )
    .unwrap();
    fs::write(
        directory.path().join("cycle_b.glu"),
        "let a = import! \"cycle_a.glu\"\n41",
    )
    .unwrap();
    let limits = Limits {
        // There are exactly two unique imported modules. Traversing the
        // back-edge again would turn this into ImportCount instead of letting
        // the pinned Gluon compiler report its current cycle behavior.
        max_imports: 2,
        ..Limits::default()
    };
    let evaluator = Evaluator::new(limits).with_source_root(SourceRoot::new(directory.path()).unwrap());
    let error = evaluator
        .evaluate::<i64>(&Source::new("cycle_root.glu", "import! \"cycle_a.glu\""))
        .unwrap_err();

    assert_eq!(error.category, DiagnosticCategory::Runtime);
    assert_eq!(error.limit, None);
    assert_eq!(error.source_name.as_deref(), Some("cycle_root.glu"));
    assert_eq!(error.span, Some(SourceSpan { start: 0, end: 21 }));
    assert!(error.message.contains("cycle_a -> cycle_b -> cycle_a"));
}

#[test]
fn diagnostic_envelope_matrix_is_frozen() {
    let parse = Evaluator::default()
        .evaluate::<i64>(&Source::new("parse.glu", "let x = in x"))
        .unwrap_err();
    let type_error = Evaluator::default()
        .evaluate::<i64>(&Source::new("type.glu", "\"not an integer\""))
        .unwrap_err();
    let import = Evaluator::default()
        .evaluate::<i64>(&Source::new("import.glu", "import! local.missing"))
        .unwrap_err();

    let directory = tempfile::tempdir().unwrap();
    let io = rooted_evaluator(directory.path())
        .evaluate_file::<i64>("missing.glu")
        .unwrap_err();

    let source_limit = Evaluator::new(Limits {
        max_source_bytes: 2,
        ..Limits::default()
    })
    .evaluate::<i64>(&Source::new("limit.glu", "123"))
    .unwrap_err();
    let runtime = Evaluator::default()
        .evaluate::<i64>(&Source::new("runtime.glu", "1 #Int/ 0"))
        .unwrap_err();
    let internal = Evaluator::default()
        .evaluate_file::<i64>("missing-root.glu")
        .unwrap_err();

    let cases = [
        DiagnosticExpectation {
            label: "parse",
            diagnostic: parse,
            category: DiagnosticCategory::Parse,
            source_name: Some("parse.glu"),
            span: Some(SourceSpan { start: 8, end: 10 }),
            limit: None,
            exact_message: None,
            has_error_source: true,
        },
        DiagnosticExpectation {
            label: "type",
            diagnostic: type_error,
            category: DiagnosticCategory::Type,
            source_name: Some("type.glu"),
            span: Some(SourceSpan { start: 0, end: 16 }),
            limit: None,
            exact_message: None,
            has_error_source: true,
        },
        DiagnosticExpectation {
            label: "import",
            diagnostic: import,
            category: DiagnosticCategory::Import,
            source_name: Some("import.glu"),
            span: None,
            limit: None,
            exact_message: Some(
                "configuration import denied in root: local.missing (module is not explicitly embedded)",
            ),
            has_error_source: false,
        },
        DiagnosticExpectation {
            label: "I/O",
            diagnostic: io,
            category: DiagnosticCategory::Io,
            source_name: Some("missing.glu"),
            span: None,
            limit: None,
            exact_message: None,
            has_error_source: true,
        },
        DiagnosticExpectation {
            label: "limit",
            diagnostic: source_limit,
            category: DiagnosticCategory::Limit,
            source_name: Some("limit.glu"),
            span: None,
            limit: Some(LimitKind::SourceSize),
            exact_message: Some("source exceeds the 2-byte limit"),
            has_error_source: false,
        },
        DiagnosticExpectation {
            label: "runtime",
            diagnostic: runtime,
            category: DiagnosticCategory::Runtime,
            source_name: None,
            span: None,
            limit: None,
            exact_message: Some("Arithmetic overflow"),
            has_error_source: true,
        },
        DiagnosticExpectation {
            label: "internal",
            diagnostic: internal,
            category: DiagnosticCategory::Internal,
            source_name: None,
            span: None,
            limit: None,
            exact_message: Some("evaluate_file requires an explicit SourceRoot"),
            has_error_source: false,
        },
    ];

    for expected in cases {
        expected.assert();
    }
}

struct DiagnosticExpectation<'a> {
    label: &'a str,
    diagnostic: Diagnostic,
    category: DiagnosticCategory,
    source_name: Option<&'a str>,
    span: Option<SourceSpan>,
    limit: Option<LimitKind>,
    exact_message: Option<&'a str>,
    has_error_source: bool,
}

impl DiagnosticExpectation<'_> {
    fn assert(self) {
        assert_eq!(self.diagnostic.category, self.category, "{} category", self.label);
        assert_eq!(
            self.diagnostic.source_name.as_deref(),
            self.source_name,
            "{} source name",
            self.label
        );
        assert_eq!(self.diagnostic.span, self.span, "{} source span", self.label);
        assert_eq!(self.diagnostic.limit, self.limit, "{} limit", self.label);
        assert_eq!(
            std::error::Error::source(&self.diagnostic).is_some(),
            self.has_error_source,
            "{} backing source",
            self.label
        );
        assert!(!self.diagnostic.message.is_empty(), "{} empty message", self.label);
        if let Some(exact_message) = self.exact_message {
            assert_eq!(self.diagnostic.message, exact_message, "{} message", self.label);
        }
    }
}
