use std::path::Path;

use crate::{
    AbiCatalog, Diagnostic, EvaluationDeadline, ImportRequest, LanguageSpec, LimitKind, Limits,
    ModuleView, NormalizedRelative, PreparedGraph, Source, SourceRoot, prepare_module_graph,
};

/// An owned domain value paired with the identity produced by its engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evaluation<T, I> {
    pub value: T,
    pub identity: I,
}

/// Exact, borrowed identity material prepared by the shared pipeline.
#[derive(Debug, Clone, Copy)]
pub struct IdentityInputs<'a> {
    root: &'a Source,
    graph: &'a PreparedGraph,
    explicit_inputs: &'a [u8],
}

impl<'a> IdentityInputs<'a> {
    pub fn root(self) -> &'a Source {
        self.root
    }

    pub fn graph(self) -> &'a PreparedGraph {
        self.graph
    }

    pub fn explicit_inputs(self) -> &'a [u8] {
        self.explicit_inputs
    }
}

/// Typed conversion selected by an engine adapter without a universal value
/// tree, runtime type erasure, or a global registry.
pub trait TypedDecoder<R> {
    type Output;

    fn decode(
        self,
        runtime: &R,
        source: &Source,
        deadline: EvaluationDeadline,
    ) -> Result<Self::Output, Diagnostic>;
}

/// One concrete declaration engine behind the shared evaluation sequence.
///
/// This contract is deliberately generic and non-object-safe. Each adapter
/// retains its native parser, prepared state, runtime, identity, and typed
/// decoder without leaking any of those representations into this crate.
pub trait EngineAdapter: Sized {
    type Discovery;
    type Prepared;
    type Runtime;
    type Identity;

    fn language_spec(&self) -> &LanguageSpec;

    fn limits(&self) -> Limits;

    fn source_root(&self) -> Option<&SourceRoot>;

    fn abi_catalog(&self) -> &AbiCatalog;

    fn begin_discovery(
        &self,
        deadline: EvaluationDeadline,
    ) -> Result<Self::Discovery, Diagnostic>;

    fn discover_imports(
        &self,
        discovery: &mut Self::Discovery,
        module: ModuleView<'_>,
        deadline: EvaluationDeadline,
    ) -> Result<Vec<ImportRequest>, Diagnostic>;

    fn normalize_relative(&self, requested: &str) -> Result<NormalizedRelative, String>;

    fn build_identity(
        &self,
        inputs: IdentityInputs<'_>,
        deadline: EvaluationDeadline,
    ) -> Result<Self::Identity, Diagnostic>;

    fn prepare(
        &self,
        discovery: Self::Discovery,
        graph: PreparedGraph,
        deadline: EvaluationDeadline,
    ) -> Result<Self::Prepared, Diagnostic>;

    fn create_runtime(
        &self,
        prepared: Self::Prepared,
        deadline: EvaluationDeadline,
    ) -> Result<Self::Runtime, Diagnostic>;

    fn execute<D>(
        &self,
        runtime: Self::Runtime,
        source: &Source,
        deadline: EvaluationDeadline,
        decoder: D,
    ) -> Result<D::Output, Diagnostic>
    where
        D: TypedDecoder<Self::Runtime>;
}

pub fn evaluate<E, D>(
    engine: &E,
    source: &Source,
    decoder: D,
) -> Result<Evaluation<D::Output, E::Identity>, Diagnostic>
where
    E: EngineAdapter,
    D: TypedDecoder<E::Runtime>,
{
    let deadline = EvaluationDeadline::start(engine.limits().timeout);
    evaluate_with_inputs_until(engine, source, &[], deadline, decoder)
}

pub fn evaluate_with_inputs<E, D>(
    engine: &E,
    source: &Source,
    explicit_inputs: &[u8],
    decoder: D,
) -> Result<Evaluation<D::Output, E::Identity>, Diagnostic>
where
    E: EngineAdapter,
    D: TypedDecoder<E::Runtime>,
{
    let deadline = EvaluationDeadline::start(engine.limits().timeout);
    evaluate_with_inputs_until(engine, source, explicit_inputs, deadline, decoder)
}

pub fn evaluate_file<E, D>(
    engine: &E,
    relative: impl AsRef<Path>,
    decoder: D,
) -> Result<Evaluation<D::Output, E::Identity>, Diagnostic>
where
    E: EngineAdapter,
    D: TypedDecoder<E::Runtime>,
{
    let limits = engine.limits();
    let deadline = EvaluationDeadline::start(limits.timeout);
    let relative = relative.as_ref();
    let requested_name = relative.to_string_lossy();
    deadline.check(&requested_name)?;
    let source_root = engine
        .source_root()
        .ok_or_else(|| Diagnostic::internal("evaluate_file requires an explicit SourceRoot"))?;
    let source = source_root.load(relative, limits.max_source_bytes);
    deadline.check(&requested_name)?;
    let source = source?;
    evaluate_with_inputs_until(engine, &source, &[], deadline, decoder)
}

fn evaluate_with_inputs_until<E, D>(
    engine: &E,
    source: &Source,
    explicit_inputs: &[u8],
    deadline: EvaluationDeadline,
    decoder: D,
) -> Result<Evaluation<D::Output, E::Identity>, Diagnostic>
where
    E: EngineAdapter,
    D: TypedDecoder<E::Runtime>,
{
    let limits = engine.limits();
    let source_name = source.logical_name();
    deadline.check(source_name)?;
    if source.text().len() > limits.max_source_bytes {
        return Err(Diagnostic::limit(
            LimitKind::SourceSize,
            Some(source_name.to_owned()),
            format!("source exceeds the {}-byte limit", limits.max_source_bytes),
        ));
    }
    if explicit_inputs.len() > limits.max_explicit_input_bytes {
        return Err(Diagnostic::limit(
            LimitKind::ExplicitInputSize,
            Some(source_name.to_owned()),
            format!(
                "explicit evaluation inputs exceed the {}-byte limit",
                limits.max_explicit_input_bytes
            ),
        ));
    }

    let discovery = engine.begin_discovery(deadline);
    deadline.check(source_name)?;
    let mut discovery = discovery?;
    let graph = prepare_module_graph(
        engine.abi_catalog(),
        engine.source_root(),
        limits,
        source,
        deadline,
        |module| engine.discover_imports(&mut discovery, module, deadline),
        |requested| engine.normalize_relative(requested),
    );
    deadline.check(source_name)?;
    let graph = graph?;

    let identity = engine.build_identity(
        IdentityInputs {
            root: source,
            graph: &graph,
            explicit_inputs,
        },
        deadline,
    );
    deadline.check(source_name)?;
    let identity = identity?;

    let prepared = engine.prepare(discovery, graph, deadline);
    deadline.check(source_name)?;
    let prepared = prepared?;

    let runtime = engine.create_runtime(prepared, deadline);
    deadline.check(source_name)?;
    let runtime = runtime?;

    // Runtime adapters own the terminal completion-versus-timeout race. A
    // shared check after they latch completion would let cleanup latency
    // relabel an in-budget result.
    let value = engine.execute(runtime, source, deadline, decoder)?;
    Ok(Evaluation { value, identity })
}
