use std::{
    error::Error,
    fmt,
    hint::spin_loop,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use declarative_config::{
    AbiCatalog, Diagnostic, DiagnosticCategory, EngineAdapter, EngineId, EvaluationDeadline,
    IdentityInputs, ImportRequest, LanguageId, LanguageSpec, LimitKind, Limits, ModuleView,
    NormalizedRelative, PreparedGraph, Source, SourceRoot, TypedDecoder, evaluate,
    evaluate_file, evaluate_with_inputs,
};

#[derive(Debug, Clone, PartialEq, Eq)]
struct SyntheticIdentity {
    root: String,
    modules: Vec<(String, String)>,
    explicit_inputs: Vec<u8>,
}

#[derive(Debug)]
struct SyntheticDiscovery {
    logical_sources: Vec<String>,
}

#[derive(Debug)]
struct SyntheticPrepared {
    discovery: SyntheticDiscovery,
    graph: PreparedGraph,
}

#[derive(Debug)]
struct SyntheticRuntime {
    prepared: SyntheticPrepared,
    events: Arc<Mutex<Vec<&'static str>>>,
}

#[derive(Debug, Clone)]
struct SyntheticEngine {
    spec: LanguageSpec,
    limits: Limits,
    source_root: Option<SourceRoot>,
    catalog: AbiCatalog,
    events: Arc<Mutex<Vec<&'static str>>>,
}

impl SyntheticEngine {
    fn new(limits: Limits) -> Self {
        let spec = LanguageSpec::new(
            LanguageId::new("synthetic").unwrap(),
            EngineId::new("synthetic-engine", "7").unwrap(),
            "decl",
            "fixture-v1",
            "# generated synthetic fixture\n",
        )
        .unwrap();
        Self {
            spec,
            limits,
            source_root: None,
            catalog: AbiCatalog::new(),
            events: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn with_source_root(mut self, source_root: SourceRoot) -> Self {
        self.source_root = Some(source_root);
        self
    }

    fn with_embedded(mut self, name: &str, text: &str) -> Self {
        assert!(self.catalog.insert_source(
            name,
            format!("synthetic:{name}"),
            Source::new(name, text),
        ));
        self
    }

    fn record(&self, event: &'static str) {
        self.events.lock().unwrap().push(event);
    }

    fn events(&self) -> Vec<&'static str> {
        self.events.lock().unwrap().clone()
    }
}

impl EngineAdapter for SyntheticEngine {
    type Discovery = SyntheticDiscovery;
    type Prepared = SyntheticPrepared;
    type Runtime = SyntheticRuntime;
    type Identity = SyntheticIdentity;

    fn language_spec(&self) -> &LanguageSpec {
        &self.spec
    }

    fn limits(&self) -> Limits {
        self.limits
    }

    fn source_root(&self) -> Option<&SourceRoot> {
        self.source_root.as_ref()
    }

    fn abi_catalog(&self) -> &AbiCatalog {
        &self.catalog
    }

    fn begin_discovery(
        &self,
        _deadline: EvaluationDeadline,
    ) -> Result<Self::Discovery, Diagnostic> {
        self.record("begin-discovery");
        Ok(SyntheticDiscovery {
            logical_sources: Vec::new(),
        })
    }

    fn discover_imports(
        &self,
        discovery: &mut Self::Discovery,
        module: ModuleView<'_>,
        _deadline: EvaluationDeadline,
    ) -> Result<Vec<ImportRequest>, Diagnostic> {
        self.record("discover");
        discovery
            .logical_sources
            .push(module.source().logical_name().to_owned());
        let mut requests = Vec::new();
        for line in module.source().text().lines() {
            if let Some(name) = line.strip_prefix("embed ") {
                requests.push(ImportRequest::embedded(name));
            } else if let Some(path) = line.strip_prefix("relative ") {
                requests.push(ImportRequest::relative(path));
            }
        }
        Ok(requests)
    }

    fn normalize_relative(&self, requested: &str) -> Result<NormalizedRelative, String> {
        let path = Path::new(requested);
        if path.is_absolute() || requested.split('/').any(|segment| segment == "..") {
            return Err("synthetic imports must remain relative".to_owned());
        }
        let alias = path
            .with_extension("")
            .to_string_lossy()
            .replace('/', ".");
        Ok(NormalizedRelative::new(path, alias))
    }

    fn build_identity(
        &self,
        inputs: IdentityInputs<'_>,
        _deadline: EvaluationDeadline,
    ) -> Result<Self::Identity, Diagnostic> {
        self.record("identity");
        Ok(SyntheticIdentity {
            root: inputs.root().logical_name().to_owned(),
            modules: inputs
                .graph()
                .fingerprints()
                .map(|module| (module.logical_name.clone(), module.sha256.clone()))
                .collect(),
            explicit_inputs: inputs.explicit_inputs().to_vec(),
        })
    }

    fn prepare(
        &self,
        discovery: Self::Discovery,
        graph: PreparedGraph,
        _deadline: EvaluationDeadline,
    ) -> Result<Self::Prepared, Diagnostic> {
        self.record("prepare");
        Ok(SyntheticPrepared { discovery, graph })
    }

    fn create_runtime(
        &self,
        prepared: Self::Prepared,
        _deadline: EvaluationDeadline,
    ) -> Result<Self::Runtime, Diagnostic> {
        self.record("runtime");
        Ok(SyntheticRuntime {
            prepared,
            events: Arc::clone(&self.events),
        })
    }

    fn execute<D>(
        &self,
        runtime: Self::Runtime,
        source: &Source,
        deadline: EvaluationDeadline,
        decoder: D,
    ) -> Result<D::Output, Diagnostic>
    where
        D: TypedDecoder<Self::Runtime>,
    {
        if source.text() == "spin" {
            self.record("interrupt");
            while !deadline.expired() {
                spin_loop();
            }
            return Err(deadline.exceeded(source.logical_name()));
        }
        decoder.decode(&runtime, source, deadline)
    }
}

struct SumDecoder;

impl TypedDecoder<SyntheticRuntime> for SumDecoder {
    type Output = u64;

    fn decode(
        self,
        runtime: &SyntheticRuntime,
        source: &Source,
        _deadline: EvaluationDeadline,
    ) -> Result<Self::Output, Diagnostic> {
        runtime.events.lock().unwrap().push("decode-sum");
        let sources = std::iter::once(source.text()).chain(
            runtime
                .prepared
                .graph
                .source_modules()
                .map(|(_, text)| text),
        );
        Ok(sources
            .flat_map(str::lines)
            .filter_map(|line| line.strip_prefix("value="))
            .map(|value| value.parse::<u64>().unwrap())
            .sum())
    }
}

struct SourceNamesDecoder;

impl TypedDecoder<SyntheticRuntime> for SourceNamesDecoder {
    type Output = Vec<String>;

    fn decode(
        self,
        runtime: &SyntheticRuntime,
        _source: &Source,
        _deadline: EvaluationDeadline,
    ) -> Result<Self::Output, Diagnostic> {
        runtime.events.lock().unwrap().push("decode-names");
        Ok(runtime.prepared.discovery.logical_sources.clone())
    }
}

#[derive(Debug)]
struct SyntheticNativeError;

impl fmt::Display for SyntheticNativeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("synthetic native decoder failure")
    }
}

impl Error for SyntheticNativeError {}

struct NativeErrorDecoder;

impl TypedDecoder<SyntheticRuntime> for NativeErrorDecoder {
    type Output = ();

    fn decode(
        self,
        _runtime: &SyntheticRuntime,
        source: &Source,
        _deadline: EvaluationDeadline,
    ) -> Result<Self::Output, Diagnostic> {
        Err(Diagnostic::new(
            DiagnosticCategory::Runtime,
            None,
            Some(source.logical_name().to_owned()),
            None,
            "synthetic decode failed",
        )
        .with_source(SyntheticNativeError))
    }
}

struct UnitDecoder;

impl TypedDecoder<SyntheticRuntime> for UnitDecoder {
    type Output = ();

    fn decode(
        self,
        _runtime: &SyntheticRuntime,
        _source: &Source,
        _deadline: EvaluationDeadline,
    ) -> Result<Self::Output, Diagnostic> {
        Ok(())
    }
}

fn fixture_engine() -> (tempfile::TempDir, SyntheticEngine, Source) {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("child.decl"), "value=40").unwrap();
    let engine = SyntheticEngine::new(Limits::default())
        .with_source_root(SourceRoot::new(directory.path()).unwrap())
        .with_embedded("fixture.base", "value=1");
    let source = Source::new(
        "root.decl",
        "relative child.decl\nembed fixture.base\nvalue=1",
    );
    (directory, engine, source)
}

#[test]
fn shared_pipeline_uses_distinct_prepared_state_identity_and_typed_decoders() {
    let (_directory, engine, source) = fixture_engine();
    let evaluation = evaluate_with_inputs(&engine, &source, b"synthetic-input-v1", SumDecoder).unwrap();

    assert_eq!(evaluation.value, 42);
    assert_eq!(evaluation.identity.root, "root.decl");
    assert_eq!(evaluation.identity.explicit_inputs, b"synthetic-input-v1");
    assert_eq!(
        evaluation
            .identity
            .modules
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        ["child.decl", "fixture.base"]
    );
    assert_eq!(engine.language_spec().language().as_str(), "synthetic");

    let names = evaluate(&engine, &source, SourceNamesDecoder).unwrap().value;
    assert_eq!(names, ["root.decl", "child.decl", "fixture.base"]);

    let events = engine.events();
    let first_identity = events.iter().position(|event| *event == "identity").unwrap();
    let first_prepare = events.iter().position(|event| *event == "prepare").unwrap();
    let first_runtime = events.iter().position(|event| *event == "runtime").unwrap();
    let first_decode = events.iter().position(|event| *event == "decode-sum").unwrap();
    assert!(first_identity < first_prepare);
    assert!(first_prepare < first_runtime);
    assert!(first_runtime < first_decode);
}

#[test]
fn native_errors_remain_in_the_shared_diagnostic_chain() {
    let engine = SyntheticEngine::new(Limits::default());
    let error = evaluate(
        &engine,
        &Source::new("native-error.decl", "native-error"),
        NativeErrorDecoder,
    )
    .unwrap_err();

    assert_eq!(error.category, DiagnosticCategory::Runtime);
    assert_eq!(
        error.source().map(ToString::to_string),
        Some("synthetic native decoder failure".to_owned())
    );
}

#[test]
fn synthetic_execution_has_a_deterministic_deadline_interrupt() {
    let engine = SyntheticEngine::new(Limits {
        timeout: Duration::from_millis(5),
        ..Limits::default()
    });
    let error = evaluate(&engine, &Source::new("spin.decl", "spin"), UnitDecoder).unwrap_err();

    assert_eq!(error.category, DiagnosticCategory::Limit);
    assert_eq!(error.limit, Some(LimitKind::Time));
    assert!(engine.events().contains(&"interrupt"));
}

#[test]
fn zero_time_file_evaluation_expires_before_root_loading() {
    let directory = tempfile::tempdir().unwrap();
    let path = PathBuf::from("root.decl");
    std::fs::write(directory.path().join(&path), "value=1").unwrap();
    let engine = SyntheticEngine::new(Limits {
        timeout: Duration::ZERO,
        ..Limits::default()
    })
    .with_source_root(SourceRoot::new(directory.path()).unwrap());

    let error = evaluate_file(&engine, path, UnitDecoder).unwrap_err();
    assert_eq!(error.limit, Some(LimitKind::Time));
    assert!(engine.events().is_empty());
}
