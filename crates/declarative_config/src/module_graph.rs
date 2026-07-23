use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    path::{Path, PathBuf},
};

use crate::{
    Diagnostic, EvaluationDeadline, LimitKind, Limits, Source, SourceRoot,
    content_hash::sha256_checked,
};

/// The source authority of a module currently being inspected by an adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ModuleClass {
    Root,
    Embedded,
    Relative,
    External,
}

/// A borrowed module presented to language-specific import discovery.
#[derive(Debug, Clone, Copy)]
pub struct ModuleView<'a> {
    identity: &'a str,
    class: ModuleClass,
    source: &'a Source,
}

impl<'a> ModuleView<'a> {
    pub fn identity(self) -> &'a str {
        self.identity
    }

    pub fn class(self) -> ModuleClass {
        self.class
    }

    pub fn source(self) -> &'a Source {
        self.source
    }
}

/// One syntax-neutral import request emitted by a language adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportRequest {
    Embedded {
        name: String,
    },
    Relative {
        requested: String,
    },
    /// A language/policy rejection which still needs graph-parent context.
    Denied {
        requested: String,
        reason: String,
    },
    /// An invalid import expression whose diagnostic is attached to its source.
    Invalid {
        message: String,
    },
}

impl ImportRequest {
    pub fn embedded(name: impl Into<String>) -> Self {
        Self::Embedded { name: name.into() }
    }

    pub fn relative(requested: impl Into<String>) -> Self {
        Self::Relative {
            requested: requested.into(),
        }
    }

    pub fn denied(requested: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::Denied {
            requested: requested.into(),
            reason: reason.into(),
        }
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::Invalid {
            message: message.into(),
        }
    }
}

/// A language adapter's canonical interpretation of one authored relative
/// import spelling. Resolution and filesystem access still remain in core.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedRelative {
    path: PathBuf,
    alias: String,
}

impl NormalizedRelative {
    pub fn new(path: impl Into<PathBuf>, alias: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            alias: alias.into(),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn alias(&self) -> &str {
        &self.alias
    }
}

#[derive(Debug, Clone)]
struct CatalogSource {
    identity: String,
    source: Source,
}

#[derive(Debug, Clone)]
struct CatalogExternal {
    identity: String,
    fingerprint_name: String,
    fingerprint_source: Vec<u8>,
}

/// Adapter/domain-supplied implementations of explicitly admitted ABI modules.
///
/// Source and external entries are kept in separate maps because an adapter may
/// deliberately let an external primitive override an embedded implementation.
#[derive(Debug, Clone, Default)]
pub struct AbiCatalog {
    sources: BTreeMap<String, CatalogSource>,
    externals: BTreeMap<String, CatalogExternal>,
}

impl AbiCatalog {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert one immutable embedded implementation. Returns `false` without
    /// replacing the original when the request name already exists.
    pub fn insert_source(
        &mut self,
        request_name: impl Into<String>,
        identity: impl Into<String>,
        source: Source,
    ) -> bool {
        let request_name = request_name.into();
        if request_name.is_empty() || self.sources.contains_key(&request_name) {
            return false;
        }
        let identity = identity.into();
        if identity.is_empty() {
            return false;
        }
        self.sources.insert(request_name, CatalogSource { identity, source });
        true
    }

    /// Insert one runtime-supplied module and its immutable identity evidence.
    /// Repeated insertion is idempotent and never replaces the first entry.
    pub fn insert_external(
        &mut self,
        request_name: impl Into<String>,
        identity: impl Into<String>,
        fingerprint_name: impl Into<String>,
        fingerprint_source: impl Into<Vec<u8>>,
    ) -> bool {
        let request_name = request_name.into();
        if request_name.is_empty() || self.externals.contains_key(&request_name) {
            return false;
        }
        let identity = identity.into();
        let fingerprint_name = fingerprint_name.into();
        if identity.is_empty() || fingerprint_name.is_empty() {
            return false;
        }
        self.externals.insert(
            request_name,
            CatalogExternal {
                identity,
                fingerprint_name,
                fingerprint_source: fingerprint_source.into(),
            },
        );
        true
    }

    pub fn contains_source(&self, request_name: &str) -> bool {
        self.sources.contains_key(request_name)
    }

    pub fn contains_external(&self, request_name: &str) -> bool {
        self.externals.contains_key(request_name)
    }
}

/// Canonical source evidence for one reachable prepared module.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct PreparedModuleFingerprint {
    pub logical_name: String,
    pub sha256: String,
}

impl PreparedModuleFingerprint {
    fn new_checked(
        logical_name: impl Into<String>,
        bytes: &[u8],
        checkpoint: &mut impl FnMut() -> Result<(), Diagnostic>,
    ) -> Result<Self, Diagnostic> {
        checkpoint()?;
        let logical_name = logical_name.into();
        checkpoint()?;
        let sha256 = sha256_checked(bytes, checkpoint)?;
        Ok(Self { logical_name, sha256 })
    }
}

/// One unique reachable module in canonical identity order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedModule {
    identity: String,
    class: ModuleClass,
    fingerprint: PreparedModuleFingerprint,
}

impl PreparedModule {
    pub fn identity(&self) -> &str {
        &self.identity
    }

    pub fn class(&self) -> ModuleClass {
        self.class
    }

    pub fn fingerprint(&self) -> &PreparedModuleFingerprint {
        &self.fingerprint
    }
}

/// One observed parent-to-target import edge in canonical order.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct PreparedDependency {
    pub parent_identity: String,
    pub target_identity: String,
    pub alias: String,
}

/// Fully bounded, rooted, deterministic input graph for an engine adapter.
#[derive(Debug, Clone, Default)]
pub struct PreparedGraph {
    source_aliases: BTreeMap<String, (String, String)>,
    external_modules: BTreeSet<String>,
    modules: Vec<PreparedModule>,
    dependencies: Vec<PreparedDependency>,
}

impl PreparedGraph {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn source_modules(&self) -> impl Iterator<Item = (&str, &str)> {
        self.source_aliases
            .iter()
            .map(|(alias, (_, source))| (alias.as_str(), source.as_str()))
    }

    pub fn external_modules(&self) -> impl Iterator<Item = &str> {
        self.external_modules.iter().map(String::as_str)
    }

    pub fn allowed_modules(&self) -> impl Iterator<Item = &str> {
        self.source_aliases
            .keys()
            .map(String::as_str)
            .chain(self.external_modules())
    }

    pub fn modules(&self) -> &[PreparedModule] {
        &self.modules
    }

    pub fn fingerprints(&self) -> impl Iterator<Item = &PreparedModuleFingerprint> {
        self.modules.iter().map(PreparedModule::fingerprint)
    }

    pub fn dependencies(&self) -> &[PreparedDependency] {
        &self.dependencies
    }
}

#[derive(Debug)]
struct PendingSource {
    identity: String,
    class: ModuleClass,
    source: Source,
}

/// Resolve adapter-discovered imports without parsing any declaration syntax.
pub fn prepare_module_graph<F, N>(
    catalog: &AbiCatalog,
    source_root: Option<&SourceRoot>,
    limits: Limits,
    root_source: &Source,
    deadline: EvaluationDeadline,
    mut discover: F,
    mut normalize_relative: N,
) -> Result<PreparedGraph, Diagnostic>
where
    F: for<'a> FnMut(ModuleView<'a>) -> Result<Vec<ImportRequest>, Diagnostic>,
    N: FnMut(&str) -> Result<NormalizedRelative, String>,
{
    let evaluation_source_name = root_source.logical_name();
    deadline.check(evaluation_source_name)?;
    let mut graph = ModuleGraph {
        catalog,
        source_root,
        limits,
        deadline,
        evaluation_source_name: evaluation_source_name.to_owned(),
        total_bytes: root_source.text().len(),
        identities: BTreeSet::new(),
        aliases: BTreeMap::new(),
        modules: BTreeMap::new(),
        external_modules: BTreeSet::new(),
        dependencies: BTreeSet::new(),
        pending: VecDeque::from([PendingSource {
            identity: "root".to_owned(),
            class: ModuleClass::Root,
            source: root_source.clone(),
        }]),
    };

    graph.check_graph_bytes(root_source.logical_name())?;
    while let Some(pending) = graph.pending.pop_front() {
        graph.checkpoint()?;
        let requests = discover(ModuleView {
            identity: &pending.identity,
            class: pending.class,
            source: &pending.source,
        });
        graph.checkpoint()?;
        for request in requests? {
            graph.checkpoint()?;
            match request {
                ImportRequest::Embedded { name } => graph.add_embedded(&pending, &name)?,
                ImportRequest::Relative { requested } => {
                    graph.add_relative(&pending, &requested, &mut normalize_relative)?
                }
                ImportRequest::Denied { requested, reason } => {
                    return Err(graph.denied(&pending, &requested, &reason));
                }
                ImportRequest::Invalid { message } => {
                    return Err(Diagnostic::import(
                        Some(pending.source.logical_name().to_owned()),
                        message,
                    ));
                }
            }
        }
    }

    let prepared = PreparedGraph {
        source_aliases: graph.aliases,
        external_modules: graph.external_modules,
        modules: graph.modules.into_values().collect(),
        dependencies: graph.dependencies.into_iter().collect(),
    };
    deadline.check(evaluation_source_name)?;
    Ok(prepared)
}

struct ModuleGraph<'a> {
    catalog: &'a AbiCatalog,
    source_root: Option<&'a SourceRoot>,
    limits: Limits,
    deadline: EvaluationDeadline,
    evaluation_source_name: String,
    total_bytes: usize,
    identities: BTreeSet<String>,
    aliases: BTreeMap<String, (String, String)>,
    modules: BTreeMap<String, PreparedModule>,
    external_modules: BTreeSet<String>,
    dependencies: BTreeSet<PreparedDependency>,
    pending: VecDeque<PendingSource>,
}

impl ModuleGraph<'_> {
    fn add_embedded(&mut self, parent: &PendingSource, name: &str) -> Result<(), Diagnostic> {
        self.checkpoint()?;
        if let Some(external) = self.catalog.externals.get(name) {
            return self.register_external(parent, name, external);
        }
        let source = self.catalog.sources.get(name);
        self.checkpoint()?;
        let source = source.ok_or_else(|| self.denied(parent, name, "module is not explicitly embedded"))?;
        if self.identities.contains(&source.identity) {
            self.register_dependency(parent, &source.identity, name);
            return self.checkpoint();
        }

        // Enforce all graph limits while catalog text is still borrowed. This
        // prevents an oversized adapter-owned ABI from being cloned first.
        self.check_new_import_limits(name, source.source.text().len())?;
        let identity = source.identity.clone();
        let imported_source = source.source.clone();
        self.checkpoint()?;
        self.register_alias(parent, name, &identity, imported_source.text())?;
        self.register_dependency(parent, &identity, name);
        let fingerprint_name = imported_source.logical_name().to_owned();
        self.register_import(
            identity,
            ModuleClass::Embedded,
            imported_source,
            fingerprint_name,
        )
    }

    fn add_relative<N>(
        &mut self,
        parent: &PendingSource,
        requested: &str,
        normalize_relative: &mut N,
    ) -> Result<(), Diagnostic>
    where
        N: FnMut(&str) -> Result<NormalizedRelative, String>,
    {
        self.checkpoint()?;
        if parent.class == ModuleClass::Embedded {
            return Err(self.denied(
                parent,
                requested,
                "embedded modules cannot import source-root files",
            ));
        }
        let source_root = self.source_root.ok_or_else(|| {
            self.denied(
                parent,
                requested,
                "relative imports require an explicit SourceRoot",
            )
        })?;
        let normalized = normalize_relative(requested)
            .map_err(|reason| self.denied(parent, requested, &reason))?;
        let requested_path = normalized.path();
        let alias = normalized.alias();
        if self.catalog.sources.contains_key(alias) {
            return Err(self.denied(
                parent,
                requested,
                "relative import aliases a catalog source name",
            ));
        }
        if self.catalog.externals.contains_key(alias) {
            return Err(self.denied(
                parent,
                requested,
                "relative import aliases an external module name",
            ));
        }

        let base = Path::new(parent.source.logical_name())
            .parent()
            .unwrap_or_else(|| Path::new(""));
        let relative_path = base.join(requested_path);
        let source = source_root.load_import(&relative_path, self.limits.max_imported_file_bytes);
        self.checkpoint()?;
        let source = source?;
        let identity = format!("relative:{}", source.logical_name());
        self.register_alias(parent, alias, &identity, source.text())?;
        self.register_dependency(parent, &identity, alias);
        let fingerprint_name = source.logical_name().to_owned();
        self.register_import(identity, ModuleClass::Relative, source, fingerprint_name)
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
        class: ModuleClass,
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
        let fingerprint =
            PreparedModuleFingerprint::new_checked(fingerprint_name, source.text().as_bytes(), &mut checkpoint)?;
        self.modules.insert(
            identity.clone(),
            PreparedModule {
                identity: identity.clone(),
                class,
                fingerprint,
            },
        );
        self.pending.push_back(PendingSource {
            identity,
            class,
            source,
        });
        self.checkpoint()
    }

    fn register_external(
        &mut self,
        parent: &PendingSource,
        request_name: &str,
        external: &CatalogExternal,
    ) -> Result<(), Diagnostic> {
        self.checkpoint()?;
        self.register_dependency(parent, &external.identity, request_name);
        if !self.identities.insert(external.identity.clone()) {
            return self.checkpoint();
        }
        if self.identities.len() > self.limits.max_imports {
            return Err(Diagnostic::limit(
                LimitKind::ImportCount,
                Some(request_name.to_owned()),
                format!("import graph exceeds the {}-module limit", self.limits.max_imports),
            ));
        }
        self.external_modules.insert(request_name.to_owned());
        let deadline = self.deadline;
        let evaluation_source_name = self.evaluation_source_name.as_str();
        let mut checkpoint = || deadline.check(evaluation_source_name);
        let fingerprint = PreparedModuleFingerprint::new_checked(
            external.fingerprint_name.clone(),
            &external.fingerprint_source,
            &mut checkpoint,
        )?;
        self.modules.insert(
            external.identity.clone(),
            PreparedModule {
                identity: external.identity.clone(),
                class: ModuleClass::External,
                fingerprint,
            },
        );
        self.checkpoint()
    }

    fn register_dependency(&mut self, parent: &PendingSource, target_identity: &str, alias: &str) {
        self.dependencies.insert(PreparedDependency {
            parent_identity: parent.identity.clone(),
            target_identity: target_identity.to_owned(),
            alias: alias.to_owned(),
        });
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
