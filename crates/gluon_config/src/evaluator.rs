use std::path::Path;

use gluon::vm::api::{Getable, VmType};

use crate::{
    Diagnostic, Evaluation, GluonEngine, ImportPolicy, Limits, Source, SourceRoot,
};

/// Temporary compatibility facade while callers move to [`GluonEngine`].
#[derive(Debug, Clone)]
pub struct Evaluator {
    engine: GluonEngine,
}

impl Default for Evaluator {
    fn default() -> Self {
        Self::new(Limits::default())
    }
}

impl Evaluator {
    pub fn new(limits: Limits) -> Self {
        Self {
            engine: GluonEngine::new(limits),
        }
    }

    pub fn with_source_root(mut self, source_root: SourceRoot) -> Self {
        self.engine = self.engine.with_source_root(source_root);
        self
    }

    pub fn with_import_policy(mut self, import_policy: ImportPolicy) -> Self {
        self.engine = self.engine.with_import_policy(import_policy);
        self
    }

    pub fn import_policy(&self) -> &ImportPolicy {
        self.engine.import_policy()
    }

    pub fn limits(&self) -> Limits {
        self.engine.limits()
    }

    pub fn evaluate<T>(&self, source: &Source) -> Result<Evaluation<T>, Diagnostic>
    where
        T: VmType + Send,
        for<'vm, 'value> T: Getable<'vm, 'value>,
    {
        self.engine.evaluate(source)
    }

    pub fn evaluate_with_inputs<T>(
        &self,
        source: &Source,
        explicit_inputs: &[u8],
    ) -> Result<Evaluation<T>, Diagnostic>
    where
        T: VmType + Send,
        for<'vm, 'value> T: Getable<'vm, 'value>,
    {
        self.engine.evaluate_with_inputs(source, explicit_inputs)
    }

    pub fn evaluate_file<T>(
        &self,
        relative: impl AsRef<Path>,
    ) -> Result<Evaluation<T>, Diagnostic>
    where
        T: VmType + Send,
        for<'vm, 'value> T: Getable<'vm, 'value>,
    {
        self.engine.evaluate_file(relative)
    }
}
