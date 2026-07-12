// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use sha2::{Digest, Sha256};

use crate::{CONFIGURATION_ABI_VERSION, EVALUATOR_POLICY_VERSION, GLUON_VERSION, Source};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ModuleFingerprint {
    pub logical_name: String,
    pub sha256: String,
}

impl ModuleFingerprint {
    pub(crate) fn new(logical_name: impl Into<String>, source: &str) -> Self {
        Self {
            logical_name: logical_name.into(),
            sha256: sha256(source.as_bytes()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvaluationFingerprint {
    pub root_source_sha256: String,
    pub imported_modules: Vec<ModuleFingerprint>,
    pub gluon_version: &'static str,
    pub configuration_abi_version: u32,
    pub evaluator_policy_version: u32,
    pub explicit_inputs_sha256: String,
    pub sha256: String,
}

impl EvaluationFingerprint {
    pub(crate) fn new(source: &Source, mut imported_modules: Vec<ModuleFingerprint>, explicit_inputs: &[u8]) -> Self {
        let root_source_sha256 = sha256(source.text().as_bytes());
        let explicit_inputs_sha256 = sha256(explicit_inputs);
        imported_modules.sort();
        imported_modules.dedup_by(|left, right| left.logical_name == right.logical_name);
        let mut digest = Sha256::new();
        digest.update(b"os-tools-gluon-evaluation\0");
        update_field(&mut digest, source.logical_name().as_bytes());
        update_field(&mut digest, root_source_sha256.as_bytes());
        update_field(&mut digest, GLUON_VERSION.as_bytes());
        digest.update(CONFIGURATION_ABI_VERSION.to_le_bytes());
        digest.update(EVALUATOR_POLICY_VERSION.to_le_bytes());
        update_field(&mut digest, explicit_inputs_sha256.as_bytes());
        for module in &imported_modules {
            update_field(&mut digest, module.logical_name.as_bytes());
            update_field(&mut digest, module.sha256.as_bytes());
        }
        let sha256 = format!("{:x}", digest.finalize());

        Self {
            root_source_sha256,
            imported_modules,
            gluon_version: GLUON_VERSION,
            configuration_abi_version: CONFIGURATION_ABI_VERSION,
            evaluator_policy_version: EVALUATOR_POLICY_VERSION,
            explicit_inputs_sha256,
            sha256,
        }
    }
}

fn update_field(digest: &mut Sha256, value: &[u8]) {
    digest.update((value.len() as u64).to_le_bytes());
    digest.update(value);
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
