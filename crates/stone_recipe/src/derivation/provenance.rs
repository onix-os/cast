use std::collections::BTreeMap;

use gluon_config::EvaluationFingerprint;

use crate::build_policy::layers::BuildPolicyOperation;

use super::{BuildLock, CanonicalEncoder, DerivationValidationError, LockedIdentity, require_nonblank, sha256};

const PROFILE_AGGREGATE_DOMAIN: &[u8] = b"cast-profile-fragments-v2\0";
const POLICY_COMPOSITION_IDENTITY_DOMAIN: &str = "cast-build-policy-composition-v2";

/// Complete authored and repository-policy provenance frozen into a plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerivationProvenance {
    pub recipe: EvaluationFingerprint,
    /// Profile fragments retain configuration precedence order.
    pub profiles: Vec<ProfileFragmentProvenance>,
    pub policy: PolicyProvenance,
}

/// One ordered profile fragment and its complete evaluation identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileFragmentProvenance {
    /// Stable loader merge key; distinct from the evaluation root's logical
    /// source name so both identities remain explicit.
    pub logical_name: String,
    pub evaluation: EvaluationFingerprint,
}

/// The explicit repository policy root and its ordered composition graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyProvenance {
    pub name: String,
    /// Final root evaluation whose explicit input is
    /// [`policy_composition_identity`].
    pub root: EvaluationFingerprint,
    /// Manifest order is semantic, including named layers with no transitions.
    pub layers: Vec<PolicyLayerProvenance>,
}

/// One named policy layer in manifest order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyLayerProvenance {
    pub name: String,
    pub transitions: Vec<PolicyTransitionProvenance>,
}

/// One successfully evaluated policy state transition in layer order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyTransitionProvenance {
    pub operation: BuildPolicyOperation,
    pub origin: String,
    pub evaluation: EvaluationFingerprint,
}

impl DerivationProvenance {
    pub(super) fn validate(
        &self,
        source_lock_digest: &str,
        build_lock: &BuildLock,
    ) -> Result<(), DerivationValidationError> {
        validate_evaluation_fingerprint("provenance.recipe", &self.recipe)?;
        if self.recipe.explicit_inputs_sha256 != source_lock_digest {
            return Err(DerivationValidationError::RecipeSourceLockDigestMismatch {
                recipe: self.recipe.explicit_inputs_sha256.clone(),
                source_lock: source_lock_digest.to_owned(),
            });
        }

        let mut profile_keys = BTreeMap::new();
        for (index, fragment) in self.profiles.iter().enumerate() {
            let field = format!("provenance.profiles[{index}]");
            validate_logical_name(&format!("{field}.logical_name"), &fragment.logical_name)?;
            if let Some(first_index) = profile_keys.insert(fragment.logical_name.as_str(), index) {
                return Err(DerivationValidationError::DuplicateProfileLogicalName {
                    logical_name: fragment.logical_name.clone(),
                    first_index,
                    duplicate_index: index,
                });
            }
            validate_evaluation_fingerprint(&format!("{field}.evaluation"), &fragment.evaluation)?;
        }
        let profile_fingerprint = profile_aggregate_fingerprint(&self.profiles);
        if profile_fingerprint != build_lock.profile.fingerprint {
            return Err(DerivationValidationError::ProfileAggregateMismatch {
                expected: profile_fingerprint,
                found: build_lock.profile.fingerprint.clone(),
            });
        }

        self.policy.validate(&build_lock.policy)
    }

    pub(super) fn encode(&self, encoder: &mut CanonicalEncoder) {
        encode_evaluation_fingerprint(encoder, &self.recipe);
        encoder.sequence(&self.profiles, |encoder, fragment| fragment.encode(encoder));
        self.policy.encode(encoder);
    }
}

impl ProfileFragmentProvenance {
    fn encode(&self, encoder: &mut CanonicalEncoder) {
        encoder.string(&self.logical_name);
        encode_evaluation_fingerprint(encoder, &self.evaluation);
    }
}

impl PolicyProvenance {
    fn validate(&self, locked: &LockedIdentity) -> Result<(), DerivationValidationError> {
        require_nonblank("provenance.policy.name", &self.name)?;
        validate_evaluation_fingerprint("provenance.policy.root", &self.root)?;
        if self.name != locked.name {
            return Err(DerivationValidationError::PolicyNameMismatch {
                expected: self.name.clone(),
                found: locked.name.clone(),
            });
        }
        if self.root.sha256 != locked.fingerprint {
            return Err(DerivationValidationError::PolicyAggregateMismatch {
                expected: self.root.sha256.clone(),
                found: locked.fingerprint.clone(),
            });
        }

        let mut layer_names = BTreeMap::new();
        let mut has_policy = false;
        for (layer_index, layer) in self.layers.iter().enumerate() {
            let field = format!("provenance.policy.layers[{layer_index}]");
            require_nonblank(&format!("{field}.name"), &layer.name)?;
            if let Some(first_index) = layer_names.insert(layer.name.as_str(), layer_index) {
                return Err(DerivationValidationError::DuplicatePolicyLayer {
                    name: layer.name.clone(),
                    first_index,
                    duplicate_index: layer_index,
                });
            }
            for (transition_index, transition) in layer.transitions.iter().enumerate() {
                let transition_field = format!("{field}.transitions[{transition_index}]");
                validate_policy_origin(&format!("{transition_field}.origin"), &transition.origin)?;
                validate_evaluation_fingerprint(&format!("{transition_field}.evaluation"), &transition.evaluation)?;
                match transition.operation {
                    BuildPolicyOperation::Add if has_policy => {
                        return Err(DerivationValidationError::InvalidPolicyTransition {
                            field: format!("{transition_field}.operation"),
                            operation: transition.operation,
                            reason: "add requires an absent policy",
                        });
                    }
                    BuildPolicyOperation::Replace | BuildPolicyOperation::Modify if !has_policy => {
                        return Err(DerivationValidationError::InvalidPolicyTransition {
                            field: format!("{transition_field}.operation"),
                            operation: transition.operation,
                            reason: "replace and modify require an existing policy",
                        });
                    }
                    BuildPolicyOperation::Add => has_policy = true,
                    BuildPolicyOperation::Replace | BuildPolicyOperation::Modify => {}
                }
            }
        }
        if !has_policy {
            return Err(DerivationValidationError::MissingPolicyState);
        }

        let expected_explicit_inputs = sha256(&policy_composition_identity(&self.name, &self.layers));
        if self.root.explicit_inputs_sha256 != expected_explicit_inputs {
            return Err(DerivationValidationError::PolicyCompositionDigestMismatch {
                expected: expected_explicit_inputs,
                found: self.root.explicit_inputs_sha256.clone(),
            });
        }
        Ok(())
    }

    fn encode(&self, encoder: &mut CanonicalEncoder) {
        encoder.string(&self.name);
        encode_evaluation_fingerprint(encoder, &self.root);
        encoder.sequence(&self.layers, |encoder, layer| layer.encode(encoder));
    }
}

impl PolicyLayerProvenance {
    fn encode(&self, encoder: &mut CanonicalEncoder) {
        encoder.string(&self.name);
        encoder.sequence(&self.transitions, |encoder, transition| transition.encode(encoder));
    }
}

impl PolicyTransitionProvenance {
    fn encode(&self, encoder: &mut CanonicalEncoder) {
        encode_policy_operation(encoder, self.operation);
        encoder.string(&self.origin);
        encode_evaluation_fingerprint(encoder, &self.evaluation);
    }
}

/// Compute the v2 aggregate identity of profile fragments in precedence order.
pub fn profile_aggregate_fingerprint(fragments: &[ProfileFragmentProvenance]) -> String {
    let mut encoder = CanonicalEncoder::new(PROFILE_AGGREGATE_DOMAIN);
    encoder.sequence(fragments, |encoder, fragment| fragment.encode(encoder));
    sha256(&encoder.finish())
}

/// Encode the ordered nested input used to finalize a repository policy root.
///
/// The root evaluation itself is deliberately excluded to avoid a recursive
/// identity. Its `explicit_inputs_sha256` must equal the SHA-256 of these bytes.
pub fn policy_composition_identity(policy: &str, layers: &[PolicyLayerProvenance]) -> Vec<u8> {
    let mut encoder = CanonicalEncoder::new(&[]);
    encoder.string(POLICY_COMPOSITION_IDENTITY_DOMAIN);
    encoder.string(policy);
    encoder.u64(layers.len() as u64);
    for (layer_index, layer) in layers.iter().enumerate() {
        encoder.u64(layer_index as u64);
        encoder.string(&layer.name);
        encoder.u64(layer.transitions.len() as u64);
        for (transition_index, transition) in layer.transitions.iter().enumerate() {
            encoder.u64(transition_index as u64);
            transition.encode(&mut encoder);
        }
    }
    encoder.finish()
}

fn encode_evaluation_fingerprint(encoder: &mut CanonicalEncoder, fingerprint: &EvaluationFingerprint) {
    encoder.string(&fingerprint.root_logical_name);
    encoder.string(&fingerprint.root_source_sha256);
    encoder.sequence(&fingerprint.imported_modules, |encoder, module| {
        encoder.string(&module.logical_name);
        encoder.string(&module.sha256);
    });
    encoder.string(fingerprint.gluon_version);
    encoder.u32(fingerprint.configuration_abi_version);
    encoder.u32(fingerprint.evaluator_policy_version);
    encoder.string(&fingerprint.explicit_inputs_sha256);
    encoder.string(&fingerprint.sha256);
}

fn encode_policy_operation(encoder: &mut CanonicalEncoder, operation: BuildPolicyOperation) {
    encoder.variant(match operation {
        BuildPolicyOperation::Add => 0,
        BuildPolicyOperation::Replace => 1,
        BuildPolicyOperation::Modify => 2,
    });
}

fn validate_evaluation_fingerprint(
    field: &str,
    fingerprint: &EvaluationFingerprint,
) -> Result<(), DerivationValidationError> {
    validate_logical_name(&format!("{field}.root_logical_name"), &fingerprint.root_logical_name)?;
    for (index, module) in fingerprint.imported_modules.iter().enumerate() {
        validate_logical_name(
            &format!("{field}.imported_modules[{index}].logical_name"),
            &module.logical_name,
        )?;
    }
    fingerprint
        .validate()
        .map_err(|source| DerivationValidationError::InvalidEvaluationFingerprint {
            field: field.to_owned(),
            source,
        })
}

fn validate_logical_name(field: &str, value: &str) -> Result<(), DerivationValidationError> {
    require_nonblank(field, value)?;
    if is_normalized_logical_name(value) {
        Ok(())
    } else {
        Err(DerivationValidationError::InvalidLogicalName {
            field: field.to_owned(),
            value: value.to_owned(),
        })
    }
}

fn is_normalized_logical_name(value: &str) -> bool {
    !value.starts_with('/')
        && !value.contains('\\')
        && !value.contains(':')
        && !value
            .split('/')
            .any(|component| component.is_empty() || component == "." || component == "..")
}

fn validate_policy_origin(field: &str, origin: &str) -> Result<(), DerivationValidationError> {
    require_nonblank(field, origin)?;
    if !is_normalized_logical_name(origin) {
        Err(DerivationValidationError::InvalidPolicyOrigin {
            field: field.to_owned(),
            value: origin.to_owned(),
        })
    } else {
        Ok(())
    }
}
