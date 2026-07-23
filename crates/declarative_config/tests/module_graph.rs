use std::{collections::BTreeMap, path::PathBuf};

use declarative_config::{
    AbiCatalog, DiagnosticCategory, EvaluationDeadline, ImportRequest, LimitKind, Limits,
    ModuleClass, ModuleView, NormalizedRelative, Source, SourceRoot, prepare_module_graph,
};

fn deadline() -> EvaluationDeadline {
    EvaluationDeadline::start(Limits::default().timeout)
}

fn normalize_fixture(requested: &str) -> Result<NormalizedRelative, String> {
    let portable = requested.strip_prefix("./").unwrap_or(requested);
    let path = PathBuf::from(portable);
    let alias = path
        .with_extension("")
        .to_string_lossy()
        .replace('/', ".");
    Ok(NormalizedRelative::new(path, alias))
}

#[test]
fn resolves_only_reachable_catalog_and_rooted_modules_in_canonical_order() {
    let directory = tempfile::tempdir().unwrap();
    fs_err::create_dir(directory.path().join("nested")).unwrap();
    fs_err::write(directory.path().join("nested/branch.decl"), "branch").unwrap();
    fs_err::write(directory.path().join("nested/leaf.decl"), "leaf").unwrap();

    let mut catalog = AbiCatalog::new();
    assert!(catalog.insert_source(
        "abi.value",
        "embedded:abi.value",
        Source::new("abi.value", "abi")
    ));
    assert!(catalog.insert_source(
        "abi.unreachable",
        "embedded:abi.unreachable",
        Source::new("abi.unreachable", "unreachable")
    ));
    assert!(catalog.insert_external(
        "runtime.math",
        "external:runtime.math",
        "runtime.math",
        b"runtime-v1".to_vec()
    ));

    let source_root = SourceRoot::new(directory.path()).unwrap();
    let graph = prepare_module_graph(
        &catalog,
        Some(&source_root),
        Limits::default(),
        &Source::new("root.decl", "root"),
        deadline(),
        |module| {
            let requests = match module.source().text() {
                "root" => vec![
                    ImportRequest::relative("nested/branch.decl"),
                    ImportRequest::embedded("abi.value"),
                ],
                "branch" => vec![
                    ImportRequest::relative("leaf.decl"),
                    ImportRequest::relative("./leaf.decl"),
                ],
                "abi" => vec![ImportRequest::embedded("runtime.math")],
                "leaf" => Vec::new(),
                other => panic!("unexpected fixture source {other}"),
            };
            Ok(requests)
        },
        normalize_fixture,
    )
    .unwrap();

    assert_eq!(
        graph
            .modules()
            .iter()
            .map(|module| (module.identity(), module.class()))
            .collect::<Vec<_>>(),
        [
            ("embedded:abi.value", ModuleClass::Embedded),
            ("external:runtime.math", ModuleClass::External),
            ("relative:nested/branch.decl", ModuleClass::Relative),
            ("relative:nested/leaf.decl", ModuleClass::Relative),
        ]
    );
    assert_eq!(
        graph
            .fingerprints()
            .map(|fingerprint| fingerprint.logical_name.as_str())
            .collect::<Vec<_>>(),
        [
            "abi.value",
            "runtime.math",
            "nested/branch.decl",
            "nested/leaf.decl",
        ]
    );
    assert_eq!(graph.external_modules().collect::<Vec<_>>(), ["runtime.math"]);
    assert_eq!(
        graph
            .source_modules()
            .map(|(alias, source)| (alias, source))
            .collect::<Vec<_>>(),
        [
            ("abi.value", "abi"),
            ("leaf", "leaf"),
            ("nested.branch", "branch"),
        ]
    );
    assert!(!graph
        .modules()
        .iter()
        .any(|module| module.identity() == "embedded:abi.unreachable"));
    assert_eq!(graph.dependencies().len(), 4);
}

#[test]
fn observes_cycle_back_edges_without_reenqueueing_modules() {
    let directory = tempfile::tempdir().unwrap();
    fs_err::write(directory.path().join("a.decl"), "a").unwrap();
    fs_err::write(directory.path().join("b.decl"), "b").unwrap();
    let source_root = SourceRoot::new(directory.path()).unwrap();
    let limits = Limits {
        max_imports: 2,
        ..Limits::default()
    };

    let graph = prepare_module_graph(
        &AbiCatalog::new(),
        Some(&source_root),
        limits,
        &Source::new("root.decl", "root"),
        deadline(),
        |module| {
            Ok(match module.source().text() {
                "root" => vec![ImportRequest::relative("a.decl")],
                "a" => vec![ImportRequest::relative("b.decl")],
                "b" => vec![ImportRequest::relative("a.decl")],
                other => panic!("unexpected fixture source {other}"),
            })
        },
        normalize_fixture,
    )
    .unwrap();

    assert_eq!(graph.modules().len(), 2);
    assert!(graph.dependencies().iter().any(|edge| {
        edge.parent_identity == "relative:b.decl"
            && edge.target_identity == "relative:a.decl"
    }));
}

#[test]
fn enforces_catalog_count_file_and_aggregate_boundaries() {
    let mut catalog = AbiCatalog::new();
    assert!(catalog.insert_source(
        "abi.one",
        "embedded:abi.one",
        Source::new("abi.one", "12")
    ));
    assert!(catalog.insert_external(
        "runtime.one",
        "external:runtime.one",
        "runtime.one",
        b"external-v1".to_vec()
    ));
    let root = Source::new("root.decl", "root");
    let requests = |_: ModuleView<'_>| {
        Ok(vec![
            ImportRequest::embedded("abi.one"),
            ImportRequest::embedded("runtime.one"),
        ])
    };

    let count = prepare_module_graph(
        &catalog,
        None,
        Limits {
            max_imports: 1,
            ..Limits::default()
        },
        &root,
        deadline(),
        requests,
        normalize_fixture,
    )
    .unwrap_err();
    assert_eq!(count.limit, Some(LimitKind::ImportCount));

    let file = prepare_module_graph(
        &catalog,
        None,
        Limits {
            max_imported_file_bytes: 1,
            ..Limits::default()
        },
        &root,
        deadline(),
        |_| Ok(vec![ImportRequest::embedded("abi.one")]),
        normalize_fixture,
    )
    .unwrap_err();
    assert_eq!(file.limit, Some(LimitKind::ImportedFileSize));

    let exact = root.text().len() + 2;
    prepare_module_graph(
        &catalog,
        None,
        Limits {
            max_import_graph_bytes: exact,
            ..Limits::default()
        },
        &root,
        deadline(),
        |_| Ok(vec![ImportRequest::embedded("abi.one")]),
        normalize_fixture,
    )
    .unwrap();
    let aggregate = prepare_module_graph(
        &catalog,
        None,
        Limits {
            max_import_graph_bytes: exact - 1,
            ..Limits::default()
        },
        &root,
        deadline(),
        |_| Ok(vec![ImportRequest::embedded("abi.one")]),
        normalize_fixture,
    )
    .unwrap_err();
    assert_eq!(aggregate.limit, Some(LimitKind::ImportGraphSize));
}

#[test]
fn relative_policy_precedes_language_normalization_and_later_requests() {
    let root = Source::new("root.decl", "root");
    let missing_root = prepare_module_graph(
        &AbiCatalog::new(),
        None,
        Limits::default(),
        &root,
        deadline(),
        |_| Ok(vec![ImportRequest::relative("../invalid")]),
        |_| panic!("normalizer must not run without an admitted source root"),
    )
    .unwrap_err();
    assert!(missing_root.message.contains("explicit SourceRoot"));

    let directory = tempfile::tempdir().unwrap();
    let source_root = SourceRoot::new(directory.path()).unwrap();
    let mut catalog = AbiCatalog::new();
    assert!(catalog.insert_source(
        "abi.parent",
        "embedded:abi.parent",
        Source::new("abi.parent", "embedded")
    ));
    let embedded = prepare_module_graph(
        &catalog,
        Some(&source_root),
        Limits::default(),
        &root,
        deadline(),
        |module| {
            Ok(match module.class() {
                ModuleClass::Root => vec![ImportRequest::embedded("abi.parent")],
                ModuleClass::Embedded => vec![ImportRequest::relative("../invalid")],
                _ => Vec::new(),
            })
        },
        |_| panic!("normalizer must not run for an embedded parent"),
    )
    .unwrap_err();
    assert!(embedded
        .message
        .contains("embedded modules cannot import source-root files"));

    fs_err::write(directory.path().join("large.decl"), "123").unwrap();
    let first_request = prepare_module_graph(
        &AbiCatalog::new(),
        Some(&source_root),
        Limits {
            max_imported_file_bytes: 2,
            ..Limits::default()
        },
        &root,
        deadline(),
        |_| {
            Ok(vec![
                ImportRequest::relative("large.decl"),
                ImportRequest::invalid("later invalid import"),
            ])
        },
        normalize_fixture,
    )
    .unwrap_err();
    assert_eq!(first_request.limit, Some(LimitKind::ImportedFileSize));
}

#[test]
fn rejects_one_alias_resolving_to_distinct_rooted_sources() {
    let directory = tempfile::tempdir().unwrap();
    fs_err::create_dir(directory.path().join("a")).unwrap();
    fs_err::create_dir(directory.path().join("b")).unwrap();
    fs_err::write(directory.path().join("a/value.decl"), "one").unwrap();
    fs_err::write(directory.path().join("b/value.decl"), "two").unwrap();
    let source_root = SourceRoot::new(directory.path()).unwrap();
    let aliases = BTreeMap::from([
        ("a/value.decl", "value"),
        ("b/value.decl", "value"),
    ]);

    let error = prepare_module_graph(
        &AbiCatalog::new(),
        Some(&source_root),
        Limits::default(),
        &Source::new("root.decl", "root"),
        deadline(),
        |_| {
            Ok(vec![
                ImportRequest::relative("a/value.decl"),
                ImportRequest::relative("b/value.decl"),
            ])
        },
        |requested| {
            Ok(NormalizedRelative::new(
                requested,
                aliases.get(requested).unwrap().to_string(),
            ))
        },
    )
    .unwrap_err();

    assert_eq!(error.category, DiagnosticCategory::Import);
    assert_eq!(error.source_name.as_deref(), Some("root.decl"));
    assert!(error
        .message
        .contains("module alias resolves to more than one source"));
}
