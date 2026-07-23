use std::{
    collections::BTreeSet,
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use declarative_config::{
    AbiCatalog, ImportRequest, ModuleView, NormalizedRelative, PreparedGraph,
};
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

use crate::{Diagnostic, GLUON_VERSION, Source, diagnostic::from_gluon};

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

/// Explicitly configured Gluon implementations of admitted ABI modules.
///
/// Sources are never looked up on disk. Only modules reachable from the
/// evaluated root enter the shared prepared graph and evaluation identity.
#[derive(Debug, Clone, Default)]
pub struct ImportPolicy {
    catalog: AbiCatalog,
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
        if self.catalog.contains_source(&logical_name) {
            return Err(Diagnostic::import(
                Some(logical_name),
                "duplicate embedded module name",
            ));
        }

        let identity = format!("embedded:{logical_name}");
        let inserted = self.catalog.insert_source(
            logical_name.clone(),
            identity,
            Source::new(logical_name, source.into()),
        );
        debug_assert!(inserted, "validated Gluon catalog source must be insertable");
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
        self.enable_pure_builtin("std.array.prim");
    }

    /// Allow Gluon's pure string inspection and concatenation primitives.
    pub fn enable_string_primitives(&mut self) {
        self.enable_pure_builtin("std.string.prim");
    }

    fn enable_pure_builtin(&mut self, module: &str) {
        let identity = format!("pure-builtin:{module}");
        let fingerprint_source = format!("gluon-{GLUON_VERSION}:pure-builtin:{module}").into_bytes();
        let _ = self
            .catalog
            .insert_external(module, identity, module, fingerprint_source);
    }

    pub(crate) fn catalog(&self) -> &AbiCatalog {
        &self.catalog
    }
}

#[derive(Debug)]
pub(crate) struct PreparedImports {
    graph: PreparedGraph,
}

impl PreparedImports {
    pub(crate) fn empty() -> Self {
        Self {
            graph: PreparedGraph::empty(),
        }
    }

    pub(crate) fn from_graph(graph: PreparedGraph) -> Self {
        Self { graph }
    }

    pub(crate) fn allowed_modules(&self) -> BTreeSet<String> {
        self.graph.allowed_modules().map(str::to_owned).collect()
    }

    pub(crate) fn module_sources(&self) -> impl Iterator<Item = (&str, &str)> {
        self.graph.source_modules()
    }

}

pub(crate) fn discover_imports(
    parser_vm: &RootedThread,
    module: ModuleView<'_>,
) -> Result<Vec<ImportRequest>, Diagnostic> {
    collect_imports(parser_vm, module.source())
        .map(|requests| {
            requests
                .into_iter()
                .map(normalize_request)
                .collect()
        })
}

fn normalize_request(request: RawImportRequest) -> ImportRequest {
    match request {
        RawImportRequest::Embedded(module) => {
            if RestrictedImporter::is_forbidden(&module) {
                ImportRequest::denied(module, "forbidden host-capability module")
            } else {
                ImportRequest::embedded(module)
            }
        }
        RawImportRequest::Relative(raw_path) => ImportRequest::relative(raw_path),
        RawImportRequest::Invalid(message) => ImportRequest::invalid(message),
    }
}

pub(crate) fn normalize_relative_import(
    policy: &ImportPolicy,
    raw_path: &str,
) -> Result<NormalizedRelative, String> {
    let (requested_path, alias) = normalize_import_path(raw_path)?;
    if RestrictedImporter::is_forbidden(&alias) {
        return Err("forbidden host-capability module".to_owned());
    }
    if policy.catalog.contains_source(&alias) {
        return Err("relative import aliases an embedded module name".to_owned());
    }
    if policy.catalog.contains_external(&alias) {
        return Err("relative import aliases a pure builtin module name".to_owned());
    }
    Ok(NormalizedRelative::new(requested_path, alias))
}

fn collect_imports(parser_vm: &RootedThread, source: &Source) -> Result<Vec<RawImportRequest>, Diagnostic> {
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

#[derive(Debug)]
enum RawImportRequest {
    Embedded(String),
    Relative(String),
    Invalid(String),
}

#[derive(Default)]
struct ImportCollector {
    imports: Vec<RawImportRequest>,
}

impl<'a, 'ast> Visitor<'a, 'ast> for ImportCollector {
    type Ident = Symbol;

    fn visit_expr(&mut self, expression: &'a SpannedExpr<'ast, Symbol>) {
        if let Expr::App { func, args, .. } = &expression.value
            && matches!(&func.value, Expr::Ident(identifier) if identifier.name.declared_name() == "import!")
        {
            let request = match &**args {
                [argument] => match &argument.value {
                    Expr::Literal(Literal::String(path)) => RawImportRequest::Relative(path.clone()),
                    Expr::Ident(_) | Expr::Projection(..) => {
                        let mut module = String::new();
                        match expr_to_path(argument, &mut module) {
                            Ok(()) => RawImportRequest::Embedded(module),
                            Err(message) => RawImportRequest::Invalid(message.to_owned()),
                        }
                    }
                    _ => RawImportRequest::Invalid(
                        "configuration import expects a string literal or embedded module path".to_owned(),
                    ),
                },
                _ => RawImportRequest::Invalid(
                    "configuration import expects exactly one argument".to_owned(),
                ),
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
            Component::ParentDir => {
                return Err("parent traversal is not permitted in imports".to_owned());
            }
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
            || !characters.all(|character| {
                character == '_' || character == '\'' || character.is_ascii_alphanumeric()
            })
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
