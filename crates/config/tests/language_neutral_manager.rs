use std::{
    convert::Infallible,
    error::Error,
    fmt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use config::{
    Config, Manager,
    declaration::{
        ConfigDeclarationEvaluator, DeclarationEvaluatorSet,
        DeleteManagedDeclarationError, FragmentDeclarationSetError,
        GeneratedDeclarationSlotError, LoadManagedDeclarationError,
        SaveDeclarationError,
        SaveManagedDeclarationError, TypedDeclarationEvaluatorSet,
    },
};
use declarative_config::{
    DeclarationCodec, DeclarationEvaluationError, DeclarationEvaluator,
    Diagnostic, DiagnosticCategory, EngineId, Evaluation, LanguageId,
    LanguageSpec, LimitKind, Limits, Source, SourceRoot,
};
use fs_err as fs;
use tempfile::tempdir;

#[derive(Debug, Clone, PartialEq, Eq)]
struct FixtureConfig {
    value: String,
}

impl Config for FixtureConfig {
    fn domain() -> String {
        "bundle".to_owned()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FixtureIdentity {
    language: String,
    logical_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FixtureError(&'static str);

impl fmt::Display for FixtureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0)
    }
}

impl Error for FixtureError {}

#[derive(Clone)]
struct AdapterState {
    language: LanguageSpec,
    expected_prefix: &'static str,
    source_limit: usize,
    root: Option<SourceRoot>,
    evaluations: Arc<Mutex<Vec<String>>>,
    replace_collection: Option<(PathBuf, PathBuf)>,
}

#[derive(Clone)]
enum FixtureAdapter {
    First(AdapterState),
    Second(AdapterState),
}

impl FixtureAdapter {
    fn first(evaluations: Arc<Mutex<Vec<String>>>) -> Self {
        Self::First(AdapterState {
            language: language("first", "one"),
            expected_prefix: "one:",
            source_limit: 64,
            root: None,
            evaluations,
            replace_collection: None,
        })
    }

    fn second(evaluations: Arc<Mutex<Vec<String>>>) -> Self {
        Self::Second(AdapterState {
            language: language("second", "two"),
            expected_prefix: "two:",
            source_limit: 12,
            root: None,
            evaluations,
            replace_collection: None,
        })
    }

    fn replacing_collection(
        evaluations: Arc<Mutex<Vec<String>>>,
        collection: PathBuf,
        held: PathBuf,
    ) -> Self {
        let mut adapter = Self::first(evaluations);
        adapter.state_mut().replace_collection = Some((collection, held));
        adapter
    }

    fn state(&self) -> &AdapterState {
        match self {
            Self::First(state) | Self::Second(state) => state,
        }
    }

    fn state_mut(&mut self) -> &mut AdapterState {
        match self {
            Self::First(state) | Self::Second(state) => state,
        }
    }
}

impl DeclarationEvaluator<FixtureConfig> for FixtureAdapter {
    type Identity = FixtureIdentity;
    type Error = FixtureError;

    fn language_spec(&self) -> &LanguageSpec {
        &self.state().language
    }

    fn limits(&self) -> Limits {
        Limits {
            max_source_bytes: self.state().source_limit,
            ..Limits::default()
        }
    }

    fn with_source_root(&self, source_root: SourceRoot) -> Self {
        let mut rooted = self.clone();
        rooted.state_mut().root = Some(source_root);
        rooted
    }

    fn evaluate(
        &self,
        source: &Source,
    ) -> Result<
        Evaluation<FixtureConfig, Self::Identity>,
        DeclarationEvaluationError<Self::Error>,
    > {
        if self.state().root.is_none() {
            return Err(Diagnostic::internal("fixture adapter was not rooted").into());
        }
        self.state()
            .evaluations
            .lock()
            .unwrap()
            .push(format!(
                "{}:{}",
                self.language_spec().language().as_str(),
                source.text()
            ));
        if source.text() == "diagnostic" {
            return Err(Diagnostic::internal("fixture diagnostic").into());
        }
        if source.text() == "conversion" {
            return Err(DeclarationEvaluationError::conversion(FixtureError(
                "fixture conversion",
            )));
        }
        let value = source
            .text()
            .strip_prefix(self.state().expected_prefix)
            .ok_or_else(|| {
                DeclarationEvaluationError::conversion(FixtureError(
                    "wrong adapter dispatch",
                ))
            })?;
        if let Some((collection, held)) = &self.state().replace_collection {
            fs::rename(collection, held).unwrap();
            fs::create_dir(collection).unwrap();
        }
        Ok(Evaluation {
            value: FixtureConfig {
                value: value.to_owned(),
            },
            identity: FixtureIdentity {
                language: self.language_spec().language().as_str().to_owned(),
                logical_name: source.logical_name().to_owned(),
            },
        })
    }
}

impl ConfigDeclarationEvaluator for FixtureAdapter {
    type Config = FixtureConfig;
}

impl DeclarationCodec<FixtureConfig> for FixtureAdapter {
    fn encode(&self, value: &FixtureConfig) -> Result<String, Self::Error> {
        if value.value == "encode-error" {
            return Err(FixtureError("fixture encode"));
        }
        Ok(format!("{}{}", self.state().expected_prefix, value.value))
    }
}

fn language(name: &str, extension: &str) -> LanguageSpec {
    LanguageSpec::new(
        LanguageId::new(name).unwrap(),
        EngineId::new(format!("{name}-engine"), "1").unwrap(),
        extension,
        "declaration-v1",
        format!("# generated by {name}\n"),
    )
    .unwrap()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DetachedValue(u8);

#[derive(Debug, Clone)]
enum DetachedAdapter {
    First(LanguageSpec),
    Second(LanguageSpec),
}

impl DetachedAdapter {
    fn descriptor(&self) -> &LanguageSpec {
        match self {
            Self::First(language) | Self::Second(language) => language,
        }
    }
}

impl DeclarationEvaluator<DetachedValue> for DetachedAdapter {
    type Identity = &'static str;
    type Error = Infallible;

    fn language_spec(&self) -> &LanguageSpec {
        self.descriptor()
    }

    fn limits(&self) -> Limits {
        Limits::default()
    }

    fn with_source_root(&self, _source_root: SourceRoot) -> Self {
        self.clone()
    }

    fn evaluate(
        &self,
        _source: &Source,
    ) -> Result<
        Evaluation<DetachedValue, Self::Identity>,
        DeclarationEvaluationError<Self::Error>,
    > {
        let (value, identity) = match self {
            Self::First(_) => (1, "first"),
            Self::Second(_) => (2, "second"),
        };
        Ok(Evaluation {
            value: DetachedValue(value),
            identity,
        })
    }
}

fn evaluators(log: Arc<Mutex<Vec<String>>>) -> DeclarationEvaluatorSet<FixtureAdapter> {
    DeclarationEvaluatorSet::new([
        FixtureAdapter::first(Arc::clone(&log)),
        FixtureAdapter::second(log),
    ])
    .unwrap()
}

#[test]
fn typed_registry_dispatches_non_config_values_by_exact_descriptor() {
    let first = language("detached-first", "alpha");
    let second = language("detached-second", "beta");
    let set: TypedDeclarationEvaluatorSet<DetachedValue, DetachedAdapter> =
        TypedDeclarationEvaluatorSet::new([
            DetachedAdapter::First(first.clone()),
            DetachedAdapter::Second(second.clone()),
        ])
        .unwrap();

    assert_eq!(set.len(), 2);
    assert_eq!(set.languages().len(), 2);
    let first_value = set
        .get(&first)
        .unwrap()
        .evaluate(&Source::new("fixture.alpha", "ignored"))
        .unwrap();
    let second_value = set
        .get(&second)
        .unwrap()
        .evaluate(&Source::new("fixture.beta", "ignored"))
        .unwrap();
    assert_eq!(first_value.value, DetachedValue(1));
    assert_eq!(first_value.identity, "first");
    assert_eq!(second_value.value, DetachedValue(2));
    assert_eq!(second_value.identity, "second");

    let wrong_descriptor = language("detached-other", "alpha");
    assert!(set.get(&wrong_descriptor).is_none());
}

fn write(path: impl AsRef<Path>, contents: &str) {
    let path = path.as_ref();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

#[test]
fn exact_dispatch_evaluates_every_layer_before_precedence_and_ignores_unknowns() {
    let temporary = tempdir().unwrap();
    let lower = temporary.path().join("usr/share/fixture-program");
    let upper = temporary.path().join("etc/fixture-program");
    write(lower.join("bundle.d/shared.one"), "one:lower");
    write(lower.join("bundle.unknown"), "ignored");
    fs::create_dir_all(lower.join("bundle.d/ignored.unknown")).unwrap();
    write(upper.join("bundle.d/shared.two"), "two:upper");
    write(upper.join("bundle.d/distinct.one"), "one:distinct");
    let log = Arc::new(Mutex::new(Vec::new()));
    let set = evaluators(Arc::clone(&log));
    let manager = Manager::system(temporary.path(), "fixture-program");

    let loaded = manager.load_declarations(&set).unwrap();

    assert_eq!(loaded.len(), 2);
    assert_eq!(loaded[0].logical_name, "distinct");
    assert_eq!(loaded[0].value.value, "distinct");
    assert_eq!(loaded[0].identity.language, "first");
    assert_eq!(loaded[1].logical_name, "shared");
    assert_eq!(loaded[1].value.value, "upper");
    assert_eq!(loaded[1].language.extension(), "two");
    assert_eq!(
        *log.lock().unwrap(),
        ["first:one:lower", "first:one:distinct", "second:two:upper"]
    );
}

#[test]
fn shadowed_failures_and_engine_diagnostics_keep_distinct_error_variants() {
    let temporary = tempdir().unwrap();
    let lower = temporary.path().join("usr/share/fixture-program");
    let upper = temporary.path().join("etc/fixture-program");
    write(lower.join("bundle.d/shared.one"), "conversion");
    write(upper.join("bundle.d/shared.two"), "two:valid-override");
    let log = Arc::new(Mutex::new(Vec::new()));
    let set = evaluators(Arc::clone(&log));
    let manager = Manager::system(temporary.path(), "fixture-program");

    let error = manager.load_declarations(&set).unwrap_err();
    assert!(matches!(
        error,
        LoadManagedDeclarationError::Conversion {
            source: FixtureError("fixture conversion"),
            ..
        }
    ));
    assert_eq!(*log.lock().unwrap(), ["first:conversion"]);

    fs::remove_file(lower.join("bundle.d/shared.one")).unwrap();
    write(lower.join("bundle.d/failure.one"), "diagnostic");
    let error = manager.load_declarations(&set).unwrap_err();
    assert!(matches!(
        error,
        LoadManagedDeclarationError::Evaluation { source, .. }
            if source.category == DiagnosticCategory::Internal
    ));
}

#[test]
fn each_adapter_source_limit_is_applied_before_evaluation() {
    let temporary = tempdir().unwrap();
    write(
        temporary.path().join("bundle.d/oversized.two"),
        "two:this-value-exceeds-the-second-adapter-limit",
    );
    let log = Arc::new(Mutex::new(Vec::new()));
    let set = evaluators(Arc::clone(&log));

    let error = Manager::custom(temporary.path())
        .load_declarations(&set)
        .unwrap_err();

    assert!(matches!(
        error,
        LoadManagedDeclarationError::Read { source, .. }
            if source.limit == Some(LimitKind::SourceSize)
    ));
    assert!(log.lock().unwrap().is_empty());
}

#[test]
fn generated_save_switches_exact_active_authority_and_delete_is_logical() {
    let temporary = tempdir().unwrap();
    let manager = Manager::custom(temporary.path());
    let log = Arc::new(Mutex::new(Vec::new()));
    let first = FixtureAdapter::first(Arc::clone(&log));
    let second = FixtureAdapter::second(log);
    let first_language = first.language_spec().clone();
    let second_language = second.language_spec().clone();
    let set = DeclarationEvaluatorSet::new([first, second]).unwrap();
    let value = FixtureConfig {
        value: "saved".to_owned(),
    };
    let same_extension_wrong_identity = language("other", "one");

    assert!(matches!(
        manager.save_declaration(
            "profile",
            &value,
            &set,
            &same_extension_wrong_identity,
        ),
        Err(SaveManagedDeclarationError::SlotPolicy {
            source: GeneratedDeclarationSlotError::ActiveAuthorityNotRegistered { .. },
        })
    ));

    let path = manager
        .save_declaration("profile", &value, &set, &first_language)
        .unwrap();

    assert_eq!(path, temporary.path().join("bundle.d/profile.one"));
    assert_eq!(fs::read_to_string(&path).unwrap(), "# generated by first\none:saved\n");
    assert!(matches!(
        manager.delete_declaration(
            "profile",
            &set,
            &same_extension_wrong_identity,
        ),
        Err(DeleteManagedDeclarationError::SlotPolicy {
            source: GeneratedDeclarationSlotError::ActiveAuthorityNotRegistered { .. },
        })
    ));
    assert!(path.exists());
    let switched = manager
        .save_declaration("profile", &value, &set, &second_language)
        .unwrap();
    assert_eq!(switched, temporary.path().join("bundle.d/profile.two"));
    assert!(!path.exists());
    assert_eq!(
        fs::read_to_string(&switched).unwrap(),
        "# generated by second\ntwo:saved\n",
    );
    manager
        .delete_declaration("profile", &set, &first_language)
        .unwrap();
    assert!(!switched.exists());
}

#[test]
fn collisions_and_save_failures_remain_structured() {
    let temporary = tempdir().unwrap();
    write(temporary.path().join("bundle.d/shared.one"), "one:first");
    write(temporary.path().join("bundle.d/shared.two"), "two:second");
    let log = Arc::new(Mutex::new(Vec::new()));
    let set = evaluators(Arc::clone(&log));
    let manager = Manager::custom(temporary.path());

    let error = manager.load_declarations(&set).unwrap_err();
    assert!(matches!(
        error,
        LoadManagedDeclarationError::Discovery {
            source: FragmentDeclarationSetError::Collision { .. }
        }
    ));
    assert!(log.lock().unwrap().is_empty());

    let first_language = language("first", "one");
    let error = manager
        .save_declaration(
            "profile",
            &FixtureConfig {
                value: "encode-error".to_owned(),
            },
            &set,
            &first_language,
        )
        .unwrap_err();
    assert!(matches!(
        error,
        SaveManagedDeclarationError::Conversion {
            source: FixtureError("fixture encode")
        }
    ));

    let authored = temporary.path().join("bundle.d/profile.one");
    write(&authored, "authored");
    let error = manager
        .save_declaration(
            "profile",
            &FixtureConfig {
                value: "replacement".to_owned(),
            },
            &set,
            &first_language,
        )
        .unwrap_err();
    assert!(matches!(
        error,
        SaveManagedDeclarationError::Storage {
            source: SaveDeclarationError::AuthoredDeclaration { .. }
        }
    ));
    assert!(matches!(
        manager.delete_declaration("profile", &set, &first_language),
        Err(DeleteManagedDeclarationError::Storage { .. })
    ));
}

#[test]
fn collection_substitution_during_evaluation_is_rejected() {
    let temporary = tempdir().unwrap();
    let collection = temporary.path().join("bundle.d");
    let held = temporary.path().join("held");
    write(collection.join("race.one"), "one:value");
    let log = Arc::new(Mutex::new(Vec::new()));
    let replacing = FixtureAdapter::replacing_collection(
        log,
        collection,
        held,
    );
    let set = DeclarationEvaluatorSet::new([replacing]).unwrap();

    let error = Manager::custom(temporary.path())
        .load_declarations(&set)
        .unwrap_err();

    assert!(matches!(
        error,
        LoadManagedDeclarationError::Revalidation { .. }
    ));
}
