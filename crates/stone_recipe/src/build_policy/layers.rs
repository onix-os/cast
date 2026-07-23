//! Language-neutral ordered build-policy composition manifests.
//!
//! The manifest contains only data: Rust applies each operation and records
//! the declaration identity supplied by its language adapter. No evaluator
//! closure crosses the declaration boundary.

use std::collections::BTreeSet;

use thiserror::Error;

mod gluon;
mod lua;

pub use self::gluon::{
    BUILD_POLICY_LAYERS_ABI_VERSION, GLUON_BUILD_POLICY_LAYERS_ABI,
    GluonBuildPolicyRootEvaluator,
};

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
