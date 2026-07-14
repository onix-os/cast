
//! Restricted Gluon boundary for ordered build-policy composition manifests.
//!
//! The manifest contains only data: Rust applies each operation and records
//! its evaluated module fingerprint. No Gluon closure crosses the evaluator
//! boundary.

use std::collections::BTreeSet;

use gluon_config::{Diagnostic, EvaluationFingerprint, Evaluator, Source};
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

/// A validated manifest and the complete provenance of its evaluation.
#[derive(Debug, Clone)]
pub struct EvaluatedBuildPolicyRoot {
    pub root: BuildPolicyRootSpec,
    pub fingerprint: EvaluationFingerprint,
}

/// Failure to evaluate or validate an ordered policy manifest.
#[derive(Debug, Error)]
pub enum BuildPolicyRootEvaluationError {
    #[error(transparent)]
    Evaluation(#[from] Diagnostic),
    #[error(transparent)]
    Conversion(#[from] BuildPolicyRootConversionError),
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

/// Evaluate a manifest with the restricted default evaluator.
pub fn evaluate_gluon(source: &Source) -> Result<EvaluatedBuildPolicyRoot, BuildPolicyRootEvaluationError> {
    evaluate_gluon_with(&Evaluator::default(), source)
}

/// Evaluate a manifest with caller-selected limits and source containment.
pub fn evaluate_gluon_with(
    evaluator: &Evaluator,
    source: &Source,
) -> Result<EvaluatedBuildPolicyRoot, BuildPolicyRootEvaluationError> {
    evaluate_gluon_with_inputs(evaluator, source, &[])
}

/// Evaluate a manifest while binding host-composed module identities into its
/// otherwise pure fingerprint.
pub fn evaluate_gluon_with_inputs(
    evaluator: &Evaluator,
    source: &Source,
    explicit_inputs: &[u8],
) -> Result<EvaluatedBuildPolicyRoot, BuildPolicyRootEvaluationError> {
    let mut import_policy = evaluator.import_policy().clone();
    import_policy.enable_array_primitives();
    import_policy.insert_embedded_module("cast.build_policy.layers.v1", GLUON_BUILD_POLICY_LAYERS_ABI)?;
    let evaluator = evaluator.clone().with_import_policy(import_policy);
    let evaluation = evaluator.evaluate_with_inputs::<GluonBuildPolicyRootSpec>(source, explicit_inputs)?;
    let root: BuildPolicyRootSpec = evaluation.value.into();
    root.validate()?;

    Ok(EvaluatedBuildPolicyRoot {
        root,
        fingerprint: evaluation.fingerprint,
    })
}
