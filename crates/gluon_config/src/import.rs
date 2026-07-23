use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use gluon::{
    Error, ModuleCompiler, RootedThread, Thread, ThreadExt,
    base::{
        ast::{self, Expr, Literal, SpannedExpr, Visitor, expr_to_path},
        filename_to_module,
        symbol::Symbol,
        types::ArcType,
    },
    compiler_pipeline::SalvageResult,
    import::{DefaultImporter, Importer},
};

use crate::{
    Diagnostic, LimitKind, Limits, ModuleFingerprint, Source, SourceRoot,
    deadline::EvaluationDeadline,
    diagnostic::from_gluon,
};

const FORBIDDEN_MODULE_PREFIXES: &[&str] = &[
    "std.fs",
    "std.io",
    "std.process",
    "std.env",
    "std.random",
    "std.http",
    "std.thread",
    "std.channel",
    "std.reference",
    "std.st.reference",
    "std.effect",
    "std.debug",
    "std.path",
];

/// Explicitly configured modules which are compiled from in-memory source.
///
/// Embedded modules are never looked up on disk. Only modules reachable from
/// the evaluated root are installed in the VM and included in its fingerprint.
#[derive(Debug, Clone, Default)]
pub struct ImportPolicy {
    embedded_modules: BTreeMap<String, String>,
    pure_builtin_modules: BTreeSet<String>,
}

impl ImportPolicy {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert_embedded_module(
        &mut self,
        logical_name: impl Into<String>,
        source: impl Into<String>,
    ) -> Result<(), Diagnostic> {
        let logical_name = logical_name.into();
        validate_module_name(&logical_name)
            .map_err(|message| Diagnostic::import(Some(logical_name.clone()), message))?;
        if RestrictedImporter::is_forbidden(&logical_name) {
            return Err(Diagnostic::import(
                Some(logical_name),
                "embedded modules cannot use a forbidden host-capability namespace",
            ));
        }
        if self.embedded_modules.contains_key(&logical_name) {
            return Err(Diagnostic::import(Some(logical_name), "duplicate embedded module name"));
        }
        self.embedded_modules.insert(logical_name, source.into());
        Ok(())
    }

    pub fn with_embedded_module(
        mut self,
        logical_name: impl Into<String>,
        source: impl Into<String>,
    ) -> Result<Self, Diagnostic> {
        self.insert_embedded_module(logical_name, source)?;
        Ok(self)
    }

    /// Allow Gluon's pure array primitives for an explicitly embedded ABI.
    ///
    /// These primitives only inspect and construct VM arrays. They do not
    /// grant filesystem, process, network, environment, time, or random access.
    pub fn enable_array_primitives(&mut self) {
        self.pure_builtin_modules.insert("std.array.prim".to_owned());
    }

    /// Allow Gluon's pure string inspection and concatenation primitives.
    pub fn enable_string_primitives(&mut self) {
        self.pure_builtin_modules.insert("std.string.prim".to_owned());
    }
}

#[derive(Debug)]
pub(crate) struct PreparedImports {
    module_sources: BTreeMap<String, String>,
    pure_builtin_modules: BTreeSet<String>,
    fingerprints: Vec<ModuleFingerprint>,
}

impl PreparedImports {
    pub(crate) fn empty() -> Self {
        Self {
            module_sources: BTreeMap::new(),
            pure_builtin_modules: BTreeSet::new(),
            fingerprints: Vec::new(),
        }
    }

    pub(crate) fn allowed_modules(&self) -> BTreeSet<String> {
        self.module_sources
            .keys()
            .cloned()
            .chain(self.pure_builtin_modules.iter().cloned())
            .collect()
    }

    pub(crate) fn module_sources(&self) -> impl Iterator<Item = (&str, &str)> {
        self.module_sources
            .iter()
            .map(|(logical_name, source)| (logical_name.as_str(), source.as_str()))
    }

    pub(crate) fn fingerprints(&self) -> Vec<ModuleFingerprint> {
        self.fingerprints.clone()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SourceClass {
    Root,
    Embedded,
    Relative,
}

#[derive(Debug)]
struct PendingSource {
    identity: String,
    class: SourceClass,
    source: Source,
}

#[derive(Debug)]
enum ImportRequest {
    Embedded(String),
    Relative(String),
    Invalid(String),
}

pub(crate) fn prepare_imports(
    parser_vm: &RootedThread,
    policy: &ImportPolicy,
    source_root: Option<&SourceRoot>,
    limits: Limits,
    root_source: &Source,
    deadline: EvaluationDeadline,
) -> Result<PreparedImports, Diagnostic> {
    let evaluation_source_name = root_source.logical_name();
    deadline.check(evaluation_source_name)?;
    let mut graph = ImportGraph {
        policy,
        source_root,
        limits,
        deadline,
        evaluation_source_name: evaluation_source_name.to_owned(),
        total_bytes: root_source.text().len(),
        identities: BTreeSet::new(),
        aliases: BTreeMap::new(),
        fingerprints: BTreeMap::new(),
        pure_builtin_modules: BTreeSet::new(),
        pending: VecDeque::from([PendingSource {
            identity: "root".to_owned(),
            class: SourceClass::Root,
            source: root_source.clone(),
        }]),
    };

    graph.check_graph_bytes(root_source.logical_name())?;
    while let Some(pending) = graph.pending.pop_front() {
        graph.checkpoint()?;
        let requests = collect_imports(parser_vm, &pending.source);
        graph.checkpoint()?;
        for request in requests? {
            graph.checkpoint()?;
            match request {
                ImportRequest::Embedded(module) => graph.add_embedded(&pending, &module)?,
                ImportRequest::Relative(path) => graph.add_relative(&pending, &path)?,
                ImportRequest::Invalid(message) => {
                    return Err(Diagnostic::import(
                        Some(pending.source.logical_name().to_owned()),
                        message,
                    ));
                }
            }
        }
    }

    let prepared = PreparedImports {
        module_sources: graph
            .aliases
            .into_iter()
            .map(|(alias, (_, source))| (alias, source))
            .collect(),
        pure_builtin_modules: graph.pure_builtin_modules,
        fingerprints: graph.fingerprints.into_values().collect(),
    };
    deadline.check(evaluation_source_name)?;
    Ok(prepared)
}

struct ImportGraph<'a> {
    policy: &'a ImportPolicy,
    source_root: Option<&'a SourceRoot>,
    limits: Limits,
    deadline: EvaluationDeadline,
    evaluation_source_name: String,
    total_bytes: usize,
    identities: BTreeSet<String>,
    aliases: BTreeMap<String, (String, String)>,
    fingerprints: BTreeMap<String, ModuleFingerprint>,
    pure_builtin_modules: BTreeSet<String>,
    pending: VecDeque<PendingSource>,
}

impl ImportGraph<'_> {
    fn add_embedded(&mut self, parent: &PendingSource, module: &str) -> Result<(), Diagnostic> {
        self.checkpoint()?;
        if RestrictedImporter::is_forbidden(module) {
            return Err(self.denied(parent, module, "forbidden host-capability module"));
        }
        if self.policy.pure_builtin_modules.contains(module) {
            return self.register_pure_builtin(module);
        }
        let source = self.policy.embedded_modules.get(module);
        self.checkpoint()?;
        let source = source.ok_or_else(|| self.denied(parent, module, "module is not explicitly embedded"))?;
        let identity = format!("embedded:{module}");
        if self.identities.contains(&identity) {
            return self.checkpoint();
        }

        // Embedded ABI text is caller-owned and may be much larger than the
        // configured evaluator budget. Reject it while it is still borrowed;
        // cloning first would briefly admit unbounded memory even though
        // register_import eventually returned a size diagnostic.
        self.check_new_import_limits(module, source.len())?;
        let source = source.clone();
        self.checkpoint()?;
        self.register_alias(parent, module, &identity, &source)?;
        self.register_import(
            identity,
            SourceClass::Embedded,
            Source::new(module, source),
            module.to_owned(),
        )
    }

    fn add_relative(&mut self, parent: &PendingSource, raw_path: &str) -> Result<(), Diagnostic> {
        self.checkpoint()?;
        if parent.class == SourceClass::Embedded {
            return Err(self.denied(parent, raw_path, "embedded modules cannot import source-root files"));
        }
        let source_root = self
            .source_root
            .ok_or_else(|| self.denied(parent, raw_path, "relative imports require an explicit SourceRoot"))?;
        let (requested_path, alias) =
            normalize_import_path(raw_path).map_err(|message| self.denied(parent, raw_path, &message))?;
        if RestrictedImporter::is_forbidden(&alias) {
            return Err(self.denied(parent, raw_path, "forbidden host-capability module"));
        }
        if self.policy.embedded_modules.contains_key(&alias) {
            return Err(self.denied(parent, raw_path, "relative import aliases an embedded module name"));
        }
        if self.policy.pure_builtin_modules.contains(&alias) {
            return Err(self.denied(parent, raw_path, "relative import aliases a pure builtin module name"));
        }

        let base = Path::new(parent.source.logical_name())
            .parent()
            .unwrap_or_else(|| Path::new(""));
        let relative_path = base.join(requested_path);
        let source = declarative_config::source_access::load_import(
            source_root,
            &relative_path,
            self.limits.max_imported_file_bytes,
        );
        self.checkpoint()?;
        let source = source?;
        let identity = format!("relative:{}", source.logical_name());
        self.register_alias(parent, &alias, &identity, source.text())?;
        let fingerprint_name = source.logical_name().to_owned();
        self.register_import(identity, SourceClass::Relative, source, fingerprint_name)
    }

    fn register_alias(
        &mut self,
        parent: &PendingSource,
        alias: &str,
        identity: &str,
        source: &str,
    ) -> Result<(), Diagnostic> {
        self.checkpoint()?;
        match self.aliases.get(alias) {
            Some((registered_identity, _)) if registered_identity != identity => {
                Err(self.denied(parent, alias, "module alias resolves to more than one source"))
            }
            Some(_) => Ok(()),
            None => {
                self.aliases
                    .insert(alias.to_owned(), (identity.to_owned(), source.to_owned()));
                self.checkpoint()
            }
        }
    }

    fn register_import(
        &mut self,
        identity: String,
        class: SourceClass,
        source: Source,
        fingerprint_name: String,
    ) -> Result<(), Diagnostic> {
        self.checkpoint()?;
        if !self.identities.insert(identity.clone()) {
            return self.checkpoint();
        }
        if self.identities.len() > self.limits.max_imports {
            return Err(Diagnostic::limit(
                LimitKind::ImportCount,
                Some(source.logical_name().to_owned()),
                format!("import graph exceeds the {}-module limit", self.limits.max_imports),
            ));
        }
        if source.text().len() > self.limits.max_imported_file_bytes {
            return Err(Diagnostic::limit(
                LimitKind::ImportedFileSize,
                Some(source.logical_name().to_owned()),
                format!(
                    "imported module exceeds the {}-byte limit",
                    self.limits.max_imported_file_bytes
                ),
            ));
        }
        self.total_bytes = self.total_bytes.checked_add(source.text().len()).ok_or_else(|| {
            Diagnostic::limit(
                LimitKind::ImportGraphSize,
                Some(source.logical_name().to_owned()),
                "import graph byte count overflowed",
            )
        })?;
        self.check_graph_bytes(source.logical_name())?;
        let deadline = self.deadline;
        let evaluation_source_name = self.evaluation_source_name.as_str();
        let mut checkpoint = || deadline.check(evaluation_source_name);
        let fingerprint = ModuleFingerprint::new_checked(fingerprint_name, source.text(), &mut checkpoint)?;
        self.fingerprints.insert(identity.clone(), fingerprint);
        self.pending.push_back(PendingSource {
            identity,
            class,
            source,
        });
        self.checkpoint()
    }

    fn check_new_import_limits(&self, source_name: &str, source_bytes: usize) -> Result<(), Diagnostic> {
        if self.identities.len() >= self.limits.max_imports {
            return Err(Diagnostic::limit(
                LimitKind::ImportCount,
                Some(source_name.to_owned()),
                format!("import graph exceeds the {}-module limit", self.limits.max_imports),
            ));
        }
        if source_bytes > self.limits.max_imported_file_bytes {
            return Err(Diagnostic::limit(
                LimitKind::ImportedFileSize,
                Some(source_name.to_owned()),
                format!(
                    "imported module exceeds the {}-byte limit",
                    self.limits.max_imported_file_bytes
                ),
            ));
        }
        let total_bytes = self.total_bytes.checked_add(source_bytes).ok_or_else(|| {
            Diagnostic::limit(
                LimitKind::ImportGraphSize,
                Some(source_name.to_owned()),
                "import graph byte count overflowed",
            )
        })?;
        if total_bytes > self.limits.max_import_graph_bytes {
            return Err(Diagnostic::limit(
                LimitKind::ImportGraphSize,
                Some(source_name.to_owned()),
                format!(
                    "source and import graph exceeds the {}-byte limit",
                    self.limits.max_import_graph_bytes
                ),
            ));
        }
        self.checkpoint()
    }

    fn register_pure_builtin(&mut self, module: &str) -> Result<(), Diagnostic> {
        self.checkpoint()?;
        let identity = format!("pure-builtin:{module}");
        if !self.identities.insert(identity.clone()) {
            return self.checkpoint();
        }
        if self.identities.len() > self.limits.max_imports {
            return Err(Diagnostic::limit(
                LimitKind::ImportCount,
                Some(module.to_owned()),
                format!("import graph exceeds the {}-module limit", self.limits.max_imports),
            ));
        }
        self.pure_builtin_modules.insert(module.to_owned());
        let fingerprint_source = format!("gluon-0.18.3:pure-builtin:{module}");
        let deadline = self.deadline;
        let evaluation_source_name = self.evaluation_source_name.as_str();
        let mut checkpoint = || deadline.check(evaluation_source_name);
        let fingerprint = ModuleFingerprint::new_checked(module, &fingerprint_source, &mut checkpoint)?;
        self.fingerprints.insert(identity, fingerprint);
        self.checkpoint()
    }

    fn check_graph_bytes(&self, source_name: &str) -> Result<(), Diagnostic> {
        self.checkpoint()?;
        if self.total_bytes > self.limits.max_import_graph_bytes {
            return Err(Diagnostic::limit(
                LimitKind::ImportGraphSize,
                Some(source_name.to_owned()),
                format!(
                    "source and import graph exceeds the {}-byte limit",
                    self.limits.max_import_graph_bytes
                ),
            ));
        }
        Ok(())
    }

    fn checkpoint(&self) -> Result<(), Diagnostic> {
        self.deadline.check(&self.evaluation_source_name)
    }

    fn denied(&self, parent: &PendingSource, requested: &str, reason: &str) -> Diagnostic {
        Diagnostic::import(
            Some(parent.source.logical_name().to_owned()),
            format!(
                "configuration import denied in {}: {requested} ({reason})",
                parent.identity
            ),
        )
    }
}

fn collect_imports(parser_vm: &RootedThread, source: &Source) -> Result<Vec<ImportRequest>, Diagnostic> {
    let expression = parser_vm
        .parse_expr(
            parser_vm.global_env().type_cache(),
            source.logical_name(),
            source.text(),
        )
        .map_err(|error| from_gluon(Error::Parse(error), false))?;
    let mut collector = ImportCollector::default();
    collector.visit_expr(expression.expr());
    Ok(collector.imports)
}

#[derive(Default)]
struct ImportCollector {
    imports: Vec<ImportRequest>,
}

impl<'a, 'ast> Visitor<'a, 'ast> for ImportCollector {
    type Ident = Symbol;

    fn visit_expr(&mut self, expression: &'a SpannedExpr<'ast, Symbol>) {
        if let Expr::App { func, args, .. } = &expression.value
            && matches!(&func.value, Expr::Ident(identifier) if identifier.name.declared_name() == "import!")
        {
            let request = match &**args {
                [argument] => match &argument.value {
                    Expr::Literal(Literal::String(path)) => ImportRequest::Relative(path.clone()),
                    Expr::Ident(_) | Expr::Projection(..) => {
                        let mut module = String::new();
                        match expr_to_path(argument, &mut module) {
                            Ok(()) => ImportRequest::Embedded(module),
                            Err(message) => ImportRequest::Invalid(message.to_owned()),
                        }
                    }
                    _ => ImportRequest::Invalid(
                        "configuration import expects a string literal or embedded module path".to_owned(),
                    ),
                },
                _ => ImportRequest::Invalid("configuration import expects exactly one argument".to_owned()),
            };
            self.imports.push(request);
        }
        ast::walk_expr(self, expression);
    }
}

fn normalize_import_path(raw_path: &str) -> Result<(PathBuf, String), String> {
    if raw_path.is_empty() || raw_path.contains('\0') {
        return Err("relative import path is empty or contains a NUL byte".to_owned());
    }
    let portable_path = raw_path.replace('\\', "/");
    if portable_path.starts_with('/')
        || portable_path.starts_with("//")
        || portable_path
            .as_bytes()
            .get(1)
            .is_some_and(|separator| *separator == b':')
    {
        return Err("absolute import paths are not permitted".to_owned());
    }
    for component in Path::new(&portable_path).components() {
        match component {
            Component::ParentDir => return Err("parent traversal is not permitted in imports".to_owned()),
            Component::RootDir | Component::Prefix(_) => {
                return Err("absolute import paths are not permitted".to_owned());
            }
            Component::CurDir | Component::Normal(_) => {}
        }
    }

    let alias = filename_to_module(&portable_path);
    validate_module_name(&alias)?;
    let mut relative_path = PathBuf::new();
    for segment in alias.split('.') {
        relative_path.push(segment);
    }
    relative_path.set_extension("glu");
    Ok((relative_path, alias))
}

fn validate_module_name(module: &str) -> Result<(), String> {
    if module.is_empty() {
        return Err("module name is empty".to_owned());
    }
    for segment in module.split('.') {
        let mut characters = segment.chars();
        if !characters
            .next()
            .is_some_and(|character| character == '_' || character.is_ascii_alphabetic())
            || !characters.all(|character| character == '_' || character == '\'' || character.is_ascii_alphanumeric())
        {
            return Err(format!("invalid configuration module name: {module}"));
        }
    }
    Ok(())
}

#[derive(Clone, Default)]
pub(crate) struct RestrictedImporter {
    allowed_modules: Arc<BTreeSet<String>>,
}

impl RestrictedImporter {
    pub(crate) fn allowing(allowed_modules: BTreeSet<String>) -> Self {
        Self {
            allowed_modules: Arc::new(allowed_modules),
        }
    }

    fn is_forbidden(module: &str) -> bool {
        FORBIDDEN_MODULE_PREFIXES
            .iter()
            .any(|prefix| module == *prefix || module.starts_with(&format!("{prefix}.")))
    }
}

#[async_trait]
impl Importer for RestrictedImporter {
    async fn import(
        &self,
        compiler: &mut ModuleCompiler<'_, '_>,
        vm: &Thread,
        module: &str,
    ) -> SalvageResult<ArcType, Error> {
        if Self::is_forbidden(module) || !self.allowed_modules.contains(module) {
            return Err(Error::from(format!("configuration import denied: {module}")).into());
        }

        DefaultImporter.import(compiler, vm, module).await
    }
}
