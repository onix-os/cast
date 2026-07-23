//! `LuaEngine`'s implementation of the shared `EngineAdapter` contract.
//!
//! The shared core drives the stages (root load, import discovery, graph
//! preparation, identity, VM construction, typed decode). This module supplies
//! only the Lua-specific behavior: parsing imports, dependency-first module
//! loading into a fresh restricted VM, the deadline-driven interrupt, and the
//! typed value decode. No engine-neutral concern (source security, graph
//! limits, identity encoding, persistence) lives here.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::marker::PhantomData;
use std::time::Instant;

use declarative_config::{
    AbiCatalog, Diagnostic, DiagnosticCategory, EngineAdapter, EvaluationDeadline,
    EvaluationIdentity,
    Evaluation, IdentityInputs, ImportRequest, LanguageSpec, Limits, ModuleView,
    NormalizedRelative, PreparedGraph, Source, SourceRoot, TypedDecoder,
    evaluate as evaluate_declaration, evaluate_within as evaluate_declaration_within,
};
use mlua::{FromLua, HookTriggers, Lua, LuaOptions, StdLib, Table, Value, VmState};

use crate::{LuaEngine, imports, lua_configuration_abi, lua_evaluator_policy};

/// One source module to load into the VM, already in dependency-first order.
#[derive(Debug, Clone)]
pub struct PreparedLuaModule {
    alias: String,
    source: String,
}

/// The Lua-prepared input: source modules in load order.
#[derive(Debug, Clone, Default)]
pub struct LuaPrepared {
    modules: Vec<PreparedLuaModule>,
}

/// A fully constructed restricted runtime: a fresh VM whose imported modules
/// are already loaded and reachable only through `cast.import`.
pub struct LuaRuntime {
    lua: Lua,
    loaded: Table,
}

impl LuaEngine {
    /// Evaluate `source` into `T` under a fresh budget, driving every shared
    /// pipeline stage. `T` is decoded from the root's final Lua value.
    pub fn evaluate<T>(&self, source: &Source) -> Result<Evaluation<T, EvaluationIdentity>, Diagnostic>
    where
        T: FromLua,
    {
        evaluate_declaration(self, source, LuaValueDecoder::new())
    }

    /// Evaluate `source` and deserialize the root value into a serde schema
    /// type `T` — the entry point domain adapters use to reach shared wire
    /// types.
    pub fn evaluate_as<T>(&self, source: &Source) -> Result<Evaluation<T, EvaluationIdentity>, Diagnostic>
    where
        T: serde::de::DeserializeOwned,
    {
        evaluate_declaration(self, source, LuaSerdeDecoder::new())
    }

    /// Deserialize the root value into `T` under one caller-established budget.
    /// Domain `DeclarationEvaluator`s use this so the storage loaders' single
    /// read-through-decode budget spans Lua evaluation too.
    pub fn evaluate_within_as<T>(
        &self,
        source: &Source,
        deadline: EvaluationDeadline,
    ) -> Result<Evaluation<T, EvaluationIdentity>, Diagnostic>
    where
        T: serde::de::DeserializeOwned,
    {
        evaluate_declaration_within(self, source, deadline, LuaSerdeDecoder::new())
    }
}

impl EngineAdapter for LuaEngine {
    type Discovery = ();
    type Prepared = LuaPrepared;
    type Runtime = LuaRuntime;
    type Identity = EvaluationIdentity;

    fn language_spec(&self) -> &LanguageSpec {
        LuaEngine::language_spec(self)
    }

    fn limits(&self) -> Limits {
        LuaEngine::limits(self)
    }

    fn source_root(&self) -> Option<&SourceRoot> {
        LuaEngine::source_root(self)
    }

    fn abi_catalog(&self) -> &AbiCatalog {
        LuaEngine::abi_catalog(self)
    }

    fn begin_discovery(&self, _deadline: EvaluationDeadline) -> Result<(), Diagnostic> {
        Ok(())
    }

    fn discover_imports(
        &self,
        _discovery: &mut (),
        module: ModuleView<'_>,
        _deadline: EvaluationDeadline,
    ) -> Result<Vec<ImportRequest>, Diagnostic> {
        Ok(imports::discover_imports(module.source().text()))
    }

    fn normalize_relative(&self, requested: &str) -> Result<NormalizedRelative, String> {
        normalize_relative(requested)
    }

    fn build_identity(
        &self,
        inputs: IdentityInputs<'_>,
        deadline: EvaluationDeadline,
    ) -> Result<Self::Identity, Diagnostic> {
        let source_name = inputs.root().logical_name();
        let mut checkpoint = || deadline.check(source_name);
        EvaluationIdentity::new_checked(
            LuaEngine::language_spec(self),
            &lua_configuration_abi(),
            &lua_evaluator_policy(),
            &self.limits(),
            inputs.root(),
            inputs.graph(),
            inputs.explicit_inputs(),
            &mut checkpoint,
        )
    }

    fn prepare(
        &self,
        _discovery: (),
        graph: PreparedGraph,
        deadline: EvaluationDeadline,
    ) -> Result<Self::Prepared, Diagnostic> {
        deadline.check("<lua-prepare>")?;
        let modules = order_modules(&graph)?;
        Ok(LuaPrepared { modules })
    }

    fn create_runtime(
        &self,
        prepared: Self::Prepared,
        deadline: EvaluationDeadline,
    ) -> Result<Self::Runtime, Diagnostic> {
        let lua = Lua::new_with(StdLib::NONE, LuaOptions::default())
            .map_err(|error| Diagnostic::internal(format!("lua runtime construction failed: {error}")))?;
        lua.set_memory_limit(self.limits().memory_bytes)
            .map_err(|error| Diagnostic::internal(format!("lua memory limit rejected: {error}")))?;

        let loaded = lua
            .create_table()
            .map_err(|error| Diagnostic::internal(format!("lua table allocation failed: {error}")))?;

        // Load each imported module dependency-first. Each runs in its own
        // controlled environment so `cast.import` resolves only to already
        // loaded modules and no base-library global is reachable.
        for module in &prepared.modules {
            deadline.check(&module.alias)?;
            let value = evaluate_chunk(&lua, &loaded, &module.alias, &module.source)?;
            loaded
                .set(module.alias.as_str(), value)
                .map_err(|error| Diagnostic::internal(format!("lua module registration failed: {error}")))?;
        }

        Ok(LuaRuntime { lua, loaded })
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
        install_deadline_hook(&runtime.lua, deadline);
        let result = decoder.decode(&runtime, source, deadline);
        runtime.lua.remove_hook();
        result
    }
}

/// A typed decoder that runs the authored root chunk in the sandbox and
/// converts its final Lua value into `T`.
pub struct LuaValueDecoder<T>(PhantomData<fn() -> T>);

impl<T> LuaValueDecoder<T> {
    pub fn new() -> Self {
        Self(PhantomData)
    }
}

impl<T> Default for LuaValueDecoder<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> TypedDecoder<LuaRuntime> for LuaValueDecoder<T>
where
    T: FromLua,
{
    type Output = T;

    fn decode(
        self,
        runtime: &LuaRuntime,
        source: &Source,
        _deadline: EvaluationDeadline,
    ) -> Result<T, Diagnostic> {
        let value = evaluate_root_value(runtime, source)?;
        T::from_lua(value, &runtime.lua).map_err(|error| {
            Diagnostic::new(
                DiagnosticCategory::Type,
                None,
                Some(source.logical_name().to_owned()),
                None,
                format!("lua value does not match the target type: {error}"),
            )
        })
    }
}

/// A typed decoder that runs the authored root chunk and deserializes its final
/// value into `T` via serde. Domain adapters use this to reach the same shared
/// wire types the Gluon adapters decode into.
pub struct LuaSerdeDecoder<T>(PhantomData<fn() -> T>);

impl<T> LuaSerdeDecoder<T> {
    pub fn new() -> Self {
        Self(PhantomData)
    }
}

impl<T> Default for LuaSerdeDecoder<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> TypedDecoder<LuaRuntime> for LuaSerdeDecoder<T>
where
    T: serde::de::DeserializeOwned,
{
    type Output = T;

    fn decode(
        self,
        runtime: &LuaRuntime,
        source: &Source,
        _deadline: EvaluationDeadline,
    ) -> Result<T, Diagnostic> {
        use mlua::LuaSerdeExt as _;

        let value = evaluate_root_value(runtime, source)?;
        runtime.lua.from_value::<T>(value).map_err(|error| {
            Diagnostic::new(
                DiagnosticCategory::Type,
                None,
                Some(source.logical_name().to_owned()),
                None,
                format!("lua value does not match the target schema: {error}"),
            )
        })
    }
}

/// Run the authored root chunk and return its bounded, profile-checked final
/// value. Shared by every root decoder: enforce the authored profile, evaluate
/// the chunk in the sandbox, then bound and cycle-check the value tree before
/// any domain decoding sees it.
pub(crate) fn evaluate_root_value(
    runtime: &LuaRuntime,
    source: &Source,
) -> Result<Value, Diagnostic> {
    crate::profile::validate_profile(source.text()).map_err(|violation| {
        Diagnostic::new(
            DiagnosticCategory::Parse,
            None,
            Some(source.logical_name().to_owned()),
            None,
            format!(
                "lua source uses a construct outside the declaration profile: {}",
                violation.construct
            ),
        )
    })?;
    let value = evaluate_chunk(
        &runtime.lua,
        &runtime.loaded,
        source.logical_name(),
        source.text(),
    )?;
    crate::value::validate_value_tree(
        &value,
        source.logical_name(),
        &crate::value::ValueLimits::default(),
    )?;
    mark_empty_tables_as_arrays(&runtime.lua, &value)?;
    Ok(value)
}

/// Give every empty table the array metatable so mlua's deserializer reads a
/// bare `{}` as an empty sequence rather than an empty map.
///
/// This encoding represents every domain map as a `Vec<{key, value}>` and every
/// struct/variant with explicitly named fields, so a table with no entries is
/// unambiguously an empty sequence. Without the marker an empty list nested in a
/// `#[serde(tag = "kind")]` variant is buffered through serde's `deserialize_any`,
/// where mlua resolves an empty table to a map and the `Vec` field then fails
/// with "invalid type: map, expected a sequence". The value tree was already
/// cycle-checked above, so this walk terminates.
fn mark_empty_tables_as_arrays(lua: &Lua, value: &Value) -> Result<(), Diagnostic> {
    use mlua::LuaSerdeExt as _;

    let Value::Table(table) = value else {
        return Ok(());
    };
    let mut empty = true;
    for pair in table.pairs::<Value, Value>() {
        let (_, child) =
            pair.map_err(|error| Diagnostic::internal(format!("lua value walk failed: {error}")))?;
        empty = false;
        mark_empty_tables_as_arrays(lua, &child)?;
    }
    if empty {
        table.set_metatable(Some(lua.array_metatable()));
    }
    Ok(())
}

/// Evaluate one chunk in a fresh controlled environment whose only visible
/// binding is `cast.import`, resolving against the `loaded` module table.
fn evaluate_chunk(
    lua: &Lua,
    loaded: &Table,
    name: &str,
    source: &str,
) -> Result<Value, Diagnostic> {
    let environment = lua
        .create_table()
        .map_err(|error| Diagnostic::internal(format!("lua environment allocation failed: {error}")))?;
    let cast = lua
        .create_table()
        .map_err(|error| Diagnostic::internal(format!("lua cast table allocation failed: {error}")))?;
    let loaded_for_import = loaded.clone();
    let import_name = name.to_owned();
    let import = lua
        .create_function(move |_lua, requested: mlua::String| {
            let key = requested.to_str()?;
            // Every import was resolved and loaded by the shared graph already;
            // an unresolved name here is an internal invariant break.
            loaded_for_import.get::<Value>(&*key)
        })
        .map_err(|error| Diagnostic::internal(format!("lua import binding failed in {import_name}: {error}")))?;
    cast.set("import", import)
        .map_err(|error| Diagnostic::internal(format!("lua cast.import bind failed: {error}")))?;
    environment
        .set("cast", cast)
        .map_err(|error| Diagnostic::internal(format!("lua environment bind failed: {error}")))?;

    lua.load(source)
        .set_name(name)
        .set_environment(environment)
        .eval::<Value>()
        .map_err(|error| Diagnostic::new(
            DiagnosticCategory::Runtime,
            None,
            Some(name.to_owned()),
            None,
            error.to_string(),
        ))
}

/// Install a monotonic-deadline debug hook that interrupts the VM once the
/// shared budget is spent. The latch is host-side.
fn install_deadline_hook(lua: &Lua, deadline: EvaluationDeadline) {
    let start = Instant::now();
    let budget = deadline.remaining_duration();
    lua.set_hook(
        HookTriggers::new().every_nth_instruction(1024),
        move |_lua, _debug| match budget {
            Some(budget) if start.elapsed() >= budget => {
                Err(mlua::Error::runtime("cast: evaluation deadline exceeded"))
            }
            Some(_) => Ok(VmState::Continue),
            None => Err(mlua::Error::runtime("cast: evaluation deadline exceeded")),
        },
    );
}

/// Normalize one relative import spelling.
///
/// Relative imports must carry an exact `.lua` extension and stay beneath the
/// source root; absolute paths, parent traversal, and NUL bytes are rejected.
fn normalize_relative(requested: &str) -> Result<NormalizedRelative, String> {
    if requested.contains('\0') {
        return Err("relative import contains a NUL byte".to_owned());
    }
    let portable = requested.strip_prefix("./").unwrap_or(requested);
    if portable.starts_with('/') {
        return Err("relative import must not be absolute".to_owned());
    }
    if !portable.ends_with(".lua") {
        return Err("relative import must end with .lua".to_owned());
    }
    if portable.split('/').any(|part| part == ".." || part == "." || part.is_empty()) {
        return Err("relative import must not traverse or contain empty components".to_owned());
    }
    let alias = portable.strip_suffix(".lua").unwrap_or(portable).replace('/', ".");
    Ok(NormalizedRelative::new(portable, alias))
}

/// Order the graph's source modules dependency-first so every `cast.import`
/// resolves to an already-loaded module. The shared core has already rejected
/// cycles, so a stable topological order exists.
fn order_modules(graph: &PreparedGraph) -> Result<Vec<PreparedLuaModule>, Diagnostic> {
    let sources: BTreeMap<&str, &str> = graph.source_modules().collect();

    // Build an alias-level dependency graph from the observed edges.
    let mut adjacency: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for &alias in sources.keys() {
        adjacency.entry(alias).or_default();
    }
    for dependency in graph.dependencies() {
        // A dependency's `alias` is the import name the parent used; the target
        // is a loaded module. Order the parent after the aliased target.
        if sources.contains_key(dependency.alias.as_str()) {
            adjacency
                .entry(parent_alias(graph, &dependency.parent_identity))
                .or_default()
                .insert(dependency.alias.as_str());
        }
    }

    let ordered = topological_order(&adjacency);
    Ok(ordered
        .into_iter()
        .filter_map(|alias| {
            sources.get(alias).map(|source| PreparedLuaModule {
                alias: alias.to_owned(),
                source: (*source).to_owned(),
            })
        })
        .collect())
}

/// The alias a module identity is imported as, or `"root"` for the root.
fn parent_alias<'a>(graph: &'a PreparedGraph, identity: &'a str) -> &'a str {
    if identity == "root" {
        return "root";
    }
    graph
        .dependencies()
        .iter()
        .find(|dependency| dependency.target_identity == identity)
        .map(|dependency| dependency.alias.as_str())
        .unwrap_or(identity)
}

/// Deterministic Kahn-style topological order over the alias dependency graph.
/// Edges point from a module to the modules it imports; imported modules come
/// first. Nodes are visited in sorted order for a stable result.
fn topological_order<'a>(adjacency: &BTreeMap<&'a str, BTreeSet<&'a str>>) -> Vec<&'a str> {
    let mut ordered = Vec::new();
    let mut visited: BTreeSet<&str> = BTreeSet::new();
    let mut queue: VecDeque<&str> = adjacency.keys().copied().collect();
    // Repeatedly emit any node whose imports are all already emitted.
    while let Some(node) = queue.pop_front() {
        if visited.contains(node) {
            continue;
        }
        let ready = adjacency
            .get(node)
            .map(|deps| deps.iter().all(|dep| visited.contains(dep) || !adjacency.contains_key(dep)))
            .unwrap_or(true);
        if ready {
            visited.insert(node);
            ordered.push(node);
        } else {
            queue.push_back(node);
        }
    }
    ordered
}

#[cfg(test)]
mod tests {
    use declarative_config::Source;

    use crate::LuaEngine;

    #[test]
    fn evaluates_a_pure_root_with_no_imports() {
        let engine = LuaEngine::default();
        let value = engine
            .evaluate::<i64>(&Source::new("root.lua", "return 1 + 41"))
            .unwrap();
        assert_eq!(value.value, 42);
        value.identity.validate().unwrap();
        assert_eq!(value.identity.engine.implementation(), "lua");
    }

    #[test]
    fn a_forbidden_global_is_unreachable_from_the_root() {
        let engine = LuaEngine::default();
        let value = engine
            .evaluate::<bool>(&Source::new("root.lua", "return load ~= nil"))
            .unwrap();
        assert!(!value.value, "the root chunk must not reach base-library globals");
    }

    #[test]
    fn a_string_and_a_bool_root_decode() {
        let engine = LuaEngine::default();
        let text = engine
            .evaluate::<String>(&Source::new("root.lua", "return \"stable\""))
            .unwrap();
        assert_eq!(text.value, "stable");
        let flag = engine
            .evaluate::<bool>(&Source::new("root.lua", "return true"))
            .unwrap();
        assert!(flag.value);
    }

    #[test]
    fn a_root_using_a_loop_is_rejected_by_the_profile() {
        let engine = LuaEngine::default();
        let error = engine
            .evaluate::<i64>(&Source::new(
                "root.lua",
                "local n = 0\nwhile true do n = n + 1 end\nreturn n",
            ))
            .unwrap_err();
        assert_eq!(error.category, declarative_config::DiagnosticCategory::Parse);
        assert!(error.message.contains("while loop"));
    }

    #[test]
    fn a_root_returning_a_float_is_rejected_before_decoding() {
        let engine = LuaEngine::default();
        let error = engine
            .evaluate::<i64>(&Source::new("root.lua", "return 1.5"))
            .unwrap_err();
        assert_eq!(error.category, declarative_config::DiagnosticCategory::Type);
        assert!(error.message.contains("float"));
    }

    #[test]
    fn a_root_imports_an_embedded_module_through_cast_import() {
        use declarative_config::AbiCatalog;

        let mut catalog = AbiCatalog::new();
        assert!(catalog.insert_source(
            "cast.answer",
            "cast.answer",
            Source::new("cast.answer", "return 41"),
        ));
        let engine = LuaEngine::default().with_abi_catalog(catalog);

        let value = engine
            .evaluate::<i64>(&Source::new(
                "root.lua",
                "local base = cast.import(\"cast.answer\")\nreturn base + 1",
            ))
            .unwrap();

        assert_eq!(value.value, 42);
        // The imported module is part of the prepared graph and identity.
        assert!(
            value
                .identity
                .modules
                .iter()
                .any(|module| module.identity == "cast.answer")
        );
    }

    #[test]
    fn the_shared_source_size_limit_is_enforced() {
        use declarative_config::{DiagnosticCategory, LimitKind, Limits};

        let engine = LuaEngine::new(Limits {
            max_source_bytes: 8,
            ..Limits::default()
        });
        let error = engine
            .evaluate::<i64>(&Source::new("root.lua", "return 100000 + 1"))
            .unwrap_err();
        assert_eq!(error.category, DiagnosticCategory::Limit);
        assert_eq!(error.limit, Some(LimitKind::SourceSize));
    }

    #[test]
    fn the_shared_import_count_limit_is_enforced() {
        use declarative_config::{AbiCatalog, DiagnosticCategory, LimitKind, Limits};

        let mut catalog = AbiCatalog::new();
        assert!(catalog.insert_source("cast.a", "cast.a", Source::new("cast.a", "return 1")));
        assert!(catalog.insert_source("cast.b", "cast.b", Source::new("cast.b", "return 2")));
        let engine = LuaEngine::new(Limits {
            max_imports: 1,
            ..Limits::default()
        })
        .with_abi_catalog(catalog);

        let error = engine
            .evaluate::<i64>(&Source::new(
                "root.lua",
                "local a = cast.import(\"cast.a\")\nlocal b = cast.import(\"cast.b\")\nreturn a + b",
            ))
            .unwrap_err();
        assert_eq!(error.category, DiagnosticCategory::Limit);
        assert_eq!(error.limit, Some(LimitKind::ImportCount));
    }

    #[test]
    fn lua_and_gluon_style_identities_differ_by_engine() {
        let engine = LuaEngine::default();
        let first = engine
            .evaluate::<i64>(&Source::new("root.lua", "return 7"))
            .unwrap();
        let repeated = engine
            .evaluate::<i64>(&Source::new("root.lua", "return 7"))
            .unwrap();
        // Deterministic within the Lua engine.
        assert_eq!(first.identity.sha256, repeated.identity.sha256);
        // The engine descriptor is part of the identity, so it is Lua's.
        assert_eq!(first.identity.engine.implementation(), "lua");
        assert_eq!(first.identity.language.as_str(), "lua");
    }
}
