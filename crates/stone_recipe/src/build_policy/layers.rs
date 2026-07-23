//! Restricted Gluon boundary for ordered build-policy composition manifests.
//!
//! The manifest contains only data: Rust applies each operation and records
//! its evaluated module fingerprint. No Gluon closure crosses the evaluator
//! boundary.

use std::collections::BTreeSet;

use declarative_config::{
    DeclarationEvaluationError, DeclarationEvaluator,
    DeclarationInputEvaluator, Evaluation as DeclarationEvaluation,
    LanguageSpec, Limits, SourceRoot,
};
use gluon_config::{Diagnostic, EvaluationFingerprint, GluonEngine, Source};
use thiserror::Error;

/// Version of the ordered build-policy layer ABI.
pub const BUILD_POLICY_LAYERS_ABI_VERSION: u32 = 1;

/// Pure helpers imported as `cast.build_policy.layers.v1`.
pub const GLUON_BUILD_POLICY_LAYERS_ABI: &str = include_str!("../../gluon/build_policy_layers.glu");

/// One total state transition in an authored policy layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildPolicyOperation {
    Add,
    Replace,
    Modify,
}

/// One module declaration in an ordered policy layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildPolicyLayerEntrySpec {
    pub operation: BuildPolicyOperation,
    pub origin: String,
}

/// A named, ordered group of policy transitions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildPolicyLayerSpec {
    pub name: String,
    pub entries: Vec<BuildPolicyLayerEntrySpec>,
}

/// The single explicit repository policy root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildPolicyRootSpec {
    pub name: String,
    pub layers: Vec<BuildPolicyLayerSpec>,
}

impl BuildPolicyRootSpec {
    /// Validate manifest-only invariants before any declared module is loaded.
    pub fn validate(&self) -> Result<(), BuildPolicyRootConversionError> {
        require_nonempty("name", &self.name)?;
        let mut layer_names = BTreeSet::new();
        for (layer_index, layer) in self.layers.iter().enumerate() {
            let field = format!("layers[{layer_index}]");
            require_nonempty(&format!("{field}.name"), &layer.name)?;
            if !layer_names.insert(layer.name.as_str()) {
                return Err(BuildPolicyRootConversionError::DuplicateLayer {
                    name: layer.name.clone(),
                });
            }
            for (entry_index, entry) in layer.entries.iter().enumerate() {
                let origin_field = format!("{field}.entries[{entry_index}].origin");
                require_nonempty(&origin_field, &entry.origin)?;
                if entry.origin.starts_with('/')
                    || entry
                        .origin
                        .split('/')
                        .any(|component| component.is_empty() || component == "." || component == "..")
                {
                    return Err(BuildPolicyRootConversionError::InvalidOrigin {
                        field: origin_field,
                        value: entry.origin.clone(),
                    });
                }
            }
        }
        Ok(())
    }
}

fn require_nonempty(field: &str, value: &str) -> Result<(), BuildPolicyRootConversionError> {
    if value.trim().is_empty() {
        Err(BuildPolicyRootConversionError::Empty {
            field: field.to_owned(),
        })
    } else {
        Ok(())
    }
}

/// Semantic manifest error with a stable field path.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum BuildPolicyRootConversionError {
    #[error("{field}: value must not be empty")]
    Empty { field: String },
    #[error("layers: duplicate layer name `{name}`")]
    DuplicateLayer { name: String },
    #[error("{field}: module origin `{value}` must be a normalized relative path")]
    InvalidOrigin { field: String, value: String },
}

/// Stateful Gluon adapter for the ordered build-policy root manifest.
#[derive(Debug, Clone)]
pub struct GluonBuildPolicyRootEvaluator {
    engine: GluonEngine,
}

impl Default for GluonBuildPolicyRootEvaluator {
    fn default() -> Self {
        Self::new(Limits::default())
    }
}

impl GluonBuildPolicyRootEvaluator {
    pub fn new(limits: Limits) -> Self {
        Self::from_engine(GluonEngine::new(limits))
            .expect("the embedded build-policy layer ABI is valid and unique")
    }

    pub fn from_engine(engine: GluonEngine) -> Result<Self, Diagnostic> {
        let mut import_policy = engine.import_policy().clone();
        import_policy.enable_array_primitives();
        import_policy.insert_embedded_module(
            "cast.build_policy.layers.v1",
            GLUON_BUILD_POLICY_LAYERS_ABI,
        )?;
        Ok(Self {
            engine: engine.with_import_policy(import_policy),
        })
    }

    fn evaluate_root(
        &self,
        source: &Source,
        explicit_inputs: &[u8],
    ) -> Result<
        DeclarationEvaluation<BuildPolicyRootSpec, EvaluationFingerprint>,
        DeclarationEvaluationError<BuildPolicyRootConversionError>,
    > {
        let evaluation = self
            .engine
            .evaluate_with_inputs::<GluonBuildPolicyRootSpec>(source, explicit_inputs)
            .map_err(DeclarationEvaluationError::Evaluation)?;
        let root: BuildPolicyRootSpec = evaluation.value.into();
        root.validate()
            .map_err(DeclarationEvaluationError::Conversion)?;
        Ok(DeclarationEvaluation {
            value: root,
            identity: evaluation.fingerprint,
        })
    }
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
enum GluonBuildPolicyOperation {
    Add,
    Replace,
    Modify,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonBuildPolicyLayerEntrySpec {
    operation: GluonBuildPolicyOperation,
    origin: String,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonBuildPolicyLayerSpec {
    name: String,
    entries: Vec<GluonBuildPolicyLayerEntrySpec>,
}

#[derive(Debug, gluon_codegen::Getable, gluon_codegen::VmType)]
struct GluonBuildPolicyRootSpec {
    name: String,
    layers: Vec<GluonBuildPolicyLayerSpec>,
}

impl From<GluonBuildPolicyOperation> for BuildPolicyOperation {
    fn from(value: GluonBuildPolicyOperation) -> Self {
        match value {
            GluonBuildPolicyOperation::Add => Self::Add,
            GluonBuildPolicyOperation::Replace => Self::Replace,
            GluonBuildPolicyOperation::Modify => Self::Modify,
        }
    }
}

impl From<GluonBuildPolicyLayerEntrySpec> for BuildPolicyLayerEntrySpec {
    fn from(value: GluonBuildPolicyLayerEntrySpec) -> Self {
        Self {
            operation: value.operation.into(),
            origin: value.origin,
        }
    }
}

impl From<GluonBuildPolicyLayerSpec> for BuildPolicyLayerSpec {
    fn from(value: GluonBuildPolicyLayerSpec) -> Self {
        Self {
            name: value.name,
            entries: value.entries.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<GluonBuildPolicyRootSpec> for BuildPolicyRootSpec {
    fn from(value: GluonBuildPolicyRootSpec) -> Self {
        Self {
            name: value.name,
            layers: value.layers.into_iter().map(Into::into).collect(),
        }
    }
}

impl DeclarationEvaluator<BuildPolicyRootSpec>
    for GluonBuildPolicyRootEvaluator
{
    type Identity = EvaluationFingerprint;
    type Error = BuildPolicyRootConversionError;

    fn language_spec(&self) -> &LanguageSpec {
        self.engine.language_spec()
    }

    fn limits(&self) -> Limits {
        self.engine.limits()
    }

    fn with_source_root(&self, source_root: SourceRoot) -> Self {
        Self {
            engine: self.engine.clone().with_source_root(source_root),
        }
    }

    fn evaluate(
        &self,
        source: &Source,
    ) -> Result<
        DeclarationEvaluation<BuildPolicyRootSpec, Self::Identity>,
        DeclarationEvaluationError<Self::Error>,
    > {
        self.evaluate_root(source, &[])
    }
}

impl DeclarationInputEvaluator<BuildPolicyRootSpec>
    for GluonBuildPolicyRootEvaluator
{
    fn evaluate_with_inputs(
        &self,
        source: &Source,
        explicit_inputs: &[u8],
    ) -> Result<
        DeclarationEvaluation<BuildPolicyRootSpec, Self::Identity>,
        DeclarationEvaluationError<Self::Error>,
    > {
        self.evaluate_root(source, explicit_inputs)
    }
}
