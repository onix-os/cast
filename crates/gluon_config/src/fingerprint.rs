
use std::{collections::BTreeMap, convert::Infallible, error::Error as StdError, fmt};

use sha2::{Digest, Sha256};

use crate::{CONFIGURATION_ABI_VERSION, EVALUATOR_POLICY_VERSION, GLUON_VERSION, Source};

const HASH_CHECKPOINT_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ModuleFingerprint {
    pub logical_name: String,
    pub sha256: String,
}

impl ModuleFingerprint {
    #[cfg(test)]
    pub(crate) fn new(logical_name: impl Into<String>, source: &str) -> Self {
        let mut checkpoint = || Ok::<(), Infallible>(());
        infallible(Self::new_checked(logical_name, source, &mut checkpoint))
    }

    pub(crate) fn new_checked<E>(
        logical_name: impl Into<String>,
        source: &str,
        checkpoint: &mut impl FnMut() -> Result<(), E>,
    ) -> Result<Self, E> {
        checkpoint()?;
        let logical_name = logical_name.into();
        checkpoint()?;
        let sha256 = sha256_checked(source.as_bytes(), checkpoint)?;
        Ok(Self { logical_name, sha256 })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvaluationFingerprint {
    pub root_logical_name: String,
    pub root_source_sha256: String,
    pub imported_modules: Vec<ModuleFingerprint>,
    pub gluon_version: &'static str,
    pub configuration_abi_version: u32,
    pub evaluator_policy_version: u32,
    pub explicit_inputs_sha256: String,
    pub sha256: String,
}

impl EvaluationFingerprint {
    #[cfg(test)]
    pub(crate) fn new(source: &Source, imported_modules: Vec<ModuleFingerprint>, explicit_inputs: &[u8]) -> Self {
        let mut checkpoint = || Ok::<(), Infallible>(());
        infallible(Self::new_checked(
            source,
            imported_modules,
            explicit_inputs,
            &mut checkpoint,
        ))
    }

    pub(crate) fn new_checked<E>(
        source: &Source,
        mut imported_modules: Vec<ModuleFingerprint>,
        explicit_inputs: &[u8],
        checkpoint: &mut impl FnMut() -> Result<(), E>,
    ) -> Result<Self, E> {
        checkpoint()?;
        let root_logical_name = source.logical_name().to_owned();
        checkpoint()?;
        let root_source_sha256 = sha256_checked(source.text().as_bytes(), checkpoint)?;
        let explicit_inputs_sha256 = sha256_checked(explicit_inputs, checkpoint)?;
        imported_modules.sort();
        imported_modules.dedup_by(|left, right| left.logical_name == right.logical_name);
        checkpoint()?;
        let sha256 = aggregate_sha256_checked(
            &root_logical_name,
            &root_source_sha256,
            &imported_modules,
            GLUON_VERSION,
            CONFIGURATION_ABI_VERSION,
            EVALUATOR_POLICY_VERSION,
            &explicit_inputs_sha256,
            checkpoint,
        )?;

        checkpoint()?;
        Ok(Self {
            root_logical_name,
            root_source_sha256,
            imported_modules,
            gluon_version: GLUON_VERSION,
            configuration_abi_version: CONFIGURATION_ABI_VERSION,
            evaluator_policy_version: EVALUATOR_POLICY_VERSION,
            explicit_inputs_sha256,
            sha256,
        })
    }

    /// Validate that this is one canonical, internally consistent evaluation
    /// identity.
    ///
    /// Fingerprint fields are public because frozen plans and inspection tools
    /// must retain their complete provenance. Consumers which receive a
    /// fingerprint across such a boundary must call this method rather than
    /// trusting the aggregate digest alone.
    pub fn validate(&self) -> Result<(), EvaluationFingerprintValidationError> {
        require_nonempty("root_logical_name", &self.root_logical_name)?;
        validate_sha256("root_source_sha256", &self.root_source_sha256)?;
        require_nonempty("gluon_version", self.gluon_version)?;
        validate_sha256("explicit_inputs_sha256", &self.explicit_inputs_sha256)?;
        validate_sha256("sha256", &self.sha256)?;

        let mut module_names = BTreeMap::new();
        for (index, module) in self.imported_modules.iter().enumerate() {
            let logical_name_field = format!("imported_modules[{index}].logical_name");
            require_nonempty(&logical_name_field, &module.logical_name)?;
            validate_sha256(&format!("imported_modules[{index}].sha256"), &module.sha256)?;
            if let Some(first_index) = module_names.insert(module.logical_name.as_str(), index) {
                return Err(EvaluationFingerprintValidationError::DuplicateImportedModule {
                    logical_name: module.logical_name.clone(),
                    first_index,
                    duplicate_index: index,
                });
            }
        }
        for (index, modules) in self.imported_modules.windows(2).enumerate() {
            if modules[0] > modules[1] {
                return Err(EvaluationFingerprintValidationError::NonCanonicalImportedModuleOrder {
                    previous_index: index,
                    previous_logical_name: modules[0].logical_name.clone(),
                    index: index + 1,
                    logical_name: modules[1].logical_name.clone(),
                });
            }
        }

        let expected = aggregate_sha256(
            &self.root_logical_name,
            &self.root_source_sha256,
            &self.imported_modules,
            self.gluon_version,
            self.configuration_abi_version,
            self.evaluator_policy_version,
            &self.explicit_inputs_sha256,
        );
        if self.sha256 != expected {
            return Err(EvaluationFingerprintValidationError::AggregateMismatch {
                expected,
                found: self.sha256.clone(),
            });
        }

        Ok(())
    }
}

/// Structural or aggregate inconsistency in an [`EvaluationFingerprint`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvaluationFingerprintValidationError {
    Empty {
        field: String,
    },
    InvalidSha256 {
        field: String,
        value: String,
    },
    DuplicateImportedModule {
        logical_name: String,
        first_index: usize,
        duplicate_index: usize,
    },
    NonCanonicalImportedModuleOrder {
        previous_index: usize,
        previous_logical_name: String,
        index: usize,
        logical_name: String,
    },
    AggregateMismatch {
        expected: String,
        found: String,
    },
}

impl fmt::Display for EvaluationFingerprintValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty { field } => write!(formatter, "{field}: value must not be empty"),
            Self::InvalidSha256 { field, value } => write!(
                formatter,
                "{field}: expected an exact lowercase 64-character SHA-256 value, found {value:?}"
            ),
            Self::DuplicateImportedModule {
                logical_name,
                first_index,
                duplicate_index,
            } => write!(
                formatter,
                "imported_modules[{duplicate_index}].logical_name: duplicate module {logical_name:?} first declared at imported_modules[{first_index}]"
            ),
            Self::NonCanonicalImportedModuleOrder {
                previous_index,
                previous_logical_name,
                index,
                logical_name,
            } => write!(
                formatter,
                "imported_modules[{index}].logical_name {logical_name:?} must sort after imported_modules[{previous_index}].logical_name {previous_logical_name:?}"
            ),
            Self::AggregateMismatch { expected, found } => write!(
                formatter,
                "sha256: aggregate fingerprint mismatch: expected {expected:?}, found {found:?}"
            ),
        }
    }
}

impl StdError for EvaluationFingerprintValidationError {}

fn require_nonempty(field: &str, value: &str) -> Result<(), EvaluationFingerprintValidationError> {
    if value.trim().is_empty() {
        Err(EvaluationFingerprintValidationError::Empty {
            field: field.to_owned(),
        })
    } else {
        Ok(())
    }
}

fn validate_sha256(field: &str, value: &str) -> Result<(), EvaluationFingerprintValidationError> {
    if value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(EvaluationFingerprintValidationError::InvalidSha256 {
            field: field.to_owned(),
            value: value.to_owned(),
        })
    }
}

fn aggregate_sha256(
    root_logical_name: &str,
    root_source_sha256: &str,
    imported_modules: &[ModuleFingerprint],
    gluon_version: &str,
    configuration_abi_version: u32,
    evaluator_policy_version: u32,
    explicit_inputs_sha256: &str,
) -> String {
    let mut checkpoint = || Ok::<(), Infallible>(());
    infallible(aggregate_sha256_checked(
        root_logical_name,
        root_source_sha256,
        imported_modules,
        gluon_version,
        configuration_abi_version,
        evaluator_policy_version,
        explicit_inputs_sha256,
        &mut checkpoint,
    ))
}

#[allow(clippy::too_many_arguments)]
fn aggregate_sha256_checked<E>(
    root_logical_name: &str,
    root_source_sha256: &str,
    imported_modules: &[ModuleFingerprint],
    gluon_version: &str,
    configuration_abi_version: u32,
    evaluator_policy_version: u32,
    explicit_inputs_sha256: &str,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<String, E> {
    checkpoint()?;
    let mut digest = Sha256::new();
    digest.update(b"os-tools-gluon-evaluation\0");
    update_field(&mut digest, root_logical_name.as_bytes());
    update_field(&mut digest, root_source_sha256.as_bytes());
    update_field(&mut digest, gluon_version.as_bytes());
    digest.update(configuration_abi_version.to_le_bytes());
    digest.update(evaluator_policy_version.to_le_bytes());
    update_field(&mut digest, explicit_inputs_sha256.as_bytes());
    for module in imported_modules {
        update_field(&mut digest, module.logical_name.as_bytes());
        update_field(&mut digest, module.sha256.as_bytes());
        checkpoint()?;
    }
    let sha256 = format!("{:x}", digest.finalize());
    checkpoint()?;
    Ok(sha256)
}

fn update_field(digest: &mut Sha256, value: &[u8]) {
    digest.update((value.len() as u64).to_le_bytes());
    digest.update(value);
}

#[cfg(test)]
fn sha256(bytes: &[u8]) -> String {
    let mut checkpoint = || Ok::<(), Infallible>(());
    infallible(sha256_checked(bytes, &mut checkpoint))
}

fn sha256_checked<E>(bytes: &[u8], checkpoint: &mut impl FnMut() -> Result<(), E>) -> Result<String, E> {
    checkpoint()?;
    let mut digest = Sha256::new();
    for chunk in bytes.chunks(HASH_CHECKPOINT_BYTES) {
        digest.update(chunk);
        checkpoint()?;
    }
    let sha256 = format!("{:x}", digest.finalize());
    checkpoint()?;
    Ok(sha256)
}

fn infallible<T>(result: Result<T, Infallible>) -> T {
    match result {
        Ok(value) => value,
        Err(never) => match never {},
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type FingerprintMutation = (&'static str, Box<dyn Fn(&mut EvaluationFingerprint)>);

    fn valid_fingerprint() -> EvaluationFingerprint {
        EvaluationFingerprint::new(
            &Source::new("root.glu", "42"),
            vec![
                ModuleFingerprint::new("z.module", "z source"),
                ModuleFingerprint::new("a.module", "a source"),
            ],
            b"explicit inputs",
        )
    }

    #[test]
    fn generated_fingerprint_is_canonical_and_valid() {
        let fingerprint = valid_fingerprint();

        assert_eq!(
            fingerprint
                .imported_modules
                .iter()
                .map(|module| module.logical_name.as_str())
                .collect::<Vec<_>>(),
            ["a.module", "z.module"]
        );
        assert_eq!(fingerprint.validate(), Ok(()));
    }

    #[test]
    fn checked_hashing_yields_at_bounded_intervals() {
        let bytes = vec![0_u8; HASH_CHECKPOINT_BYTES * 2];
        let mut checkpoints = 0;
        let mut checkpoint = || {
            checkpoints += 1;
            if checkpoints == 3 { Err("deadline") } else { Ok(()) }
        };

        assert_eq!(sha256_checked(&bytes, &mut checkpoint), Err("deadline"));
        // One checkpoint before hashing and one after each bounded chunk.
        assert_eq!(checkpoints, 3);
    }

    #[test]
    fn checked_and_unchecked_fingerprints_have_the_same_identity() {
        let source = Source::new("root.glu", "42");
        let modules = vec![ModuleFingerprint::new("cast.answer", "42")];
        let expected = EvaluationFingerprint::new(&source, modules.clone(), b"inputs");
        let mut checkpoints = 0;
        let mut checkpoint = || {
            checkpoints += 1;
            Ok::<(), Infallible>(())
        };

        let checked = EvaluationFingerprint::new_checked(&source, modules, b"inputs", &mut checkpoint).unwrap();

        assert_eq!(checked, expected);
        assert!(checkpoints > 0);
    }

    #[test]
    fn rejects_empty_logical_names_and_gluon_version_with_field_paths() {
        let mutations: Vec<FingerprintMutation> = vec![
            (
                "root_logical_name",
                Box::new(|fingerprint| fingerprint.root_logical_name = "   ".to_owned()),
            ),
            (
                "imported_modules[0].logical_name",
                Box::new(|fingerprint| fingerprint.imported_modules[0].logical_name.clear()),
            ),
            ("gluon_version", Box::new(|fingerprint| fingerprint.gluon_version = "")),
        ];

        for (field, mutate) in mutations {
            let mut fingerprint = valid_fingerprint();
            mutate(&mut fingerprint);
            assert_eq!(
                fingerprint.validate(),
                Err(EvaluationFingerprintValidationError::Empty {
                    field: field.to_owned(),
                }),
                "mutation of {field}"
            );
        }
    }

    #[test]
    fn rejects_every_noncanonical_sha256_field_with_its_path() {
        let mutations: Vec<FingerprintMutation> = vec![
            (
                "root_source_sha256",
                Box::new(|fingerprint| fingerprint.root_source_sha256 = "A".repeat(64)),
            ),
            (
                "imported_modules[0].sha256",
                Box::new(|fingerprint| fingerprint.imported_modules[0].sha256 = "0".repeat(63)),
            ),
            (
                "explicit_inputs_sha256",
                Box::new(|fingerprint| fingerprint.explicit_inputs_sha256 = "g".repeat(64)),
            ),
            ("sha256", Box::new(|fingerprint| fingerprint.sha256 = "ABC".to_owned())),
        ];

        for (field, mutate) in mutations {
            let mut fingerprint = valid_fingerprint();
            mutate(&mut fingerprint);
            assert!(
                matches!(
                    fingerprint.validate(),
                    Err(EvaluationFingerprintValidationError::InvalidSha256 {
                        field: error_field,
                        ..
                    }) if error_field == field
                ),
                "mutation of {field}"
            );
        }
    }

    #[test]
    fn rejects_duplicate_imported_module_logical_names() {
        let mut fingerprint = valid_fingerprint();
        fingerprint.imported_modules[1].logical_name = fingerprint.imported_modules[0].logical_name.clone();

        assert!(matches!(
            fingerprint.validate(),
            Err(EvaluationFingerprintValidationError::DuplicateImportedModule {
                logical_name,
                first_index: 0,
                duplicate_index: 1,
            }) if logical_name == "a.module"
        ));
    }

    #[test]
    fn rejects_noncanonical_imported_module_order() {
        let mut fingerprint = valid_fingerprint();
        fingerprint.imported_modules.reverse();

        assert!(matches!(
            fingerprint.validate(),
            Err(EvaluationFingerprintValidationError::NonCanonicalImportedModuleOrder {
                previous_index: 0,
                previous_logical_name,
                index: 1,
                logical_name,
            }) if previous_logical_name == "z.module" && logical_name == "a.module"
        ));
    }

    #[test]
    fn every_aggregate_constituent_is_recomputed_and_verified() {
        let replacement_sha256 = sha256(b"replacement");
        let mutations: Vec<FingerprintMutation> = vec![
            (
                "root_logical_name",
                Box::new(|fingerprint| fingerprint.root_logical_name = "renamed.glu".to_owned()),
            ),
            (
                "root_source_sha256",
                Box::new({
                    let replacement_sha256 = replacement_sha256.clone();
                    move |fingerprint| fingerprint.root_source_sha256 = replacement_sha256.clone()
                }),
            ),
            (
                "imported_modules.logical_name",
                Box::new(|fingerprint| fingerprint.imported_modules[0].logical_name = "b.module".to_owned()),
            ),
            (
                "imported_modules.sha256",
                Box::new({
                    let replacement_sha256 = replacement_sha256.clone();
                    move |fingerprint| fingerprint.imported_modules[0].sha256 = replacement_sha256.clone()
                }),
            ),
            (
                "imported_modules.added",
                Box::new(|fingerprint| {
                    fingerprint
                        .imported_modules
                        .push(ModuleFingerprint::new("zz.module", "added source"));
                }),
            ),
            (
                "imported_modules.removed",
                Box::new(|fingerprint| {
                    fingerprint.imported_modules.pop();
                }),
            ),
            (
                "gluon_version",
                Box::new(|fingerprint| fingerprint.gluon_version = "test-gluon"),
            ),
            (
                "configuration_abi_version",
                Box::new(|fingerprint| fingerprint.configuration_abi_version += 1),
            ),
            (
                "evaluator_policy_version",
                Box::new(|fingerprint| fingerprint.evaluator_policy_version += 1),
            ),
            (
                "explicit_inputs_sha256",
                Box::new({
                    let replacement_sha256 = replacement_sha256.clone();
                    move |fingerprint| fingerprint.explicit_inputs_sha256 = replacement_sha256.clone()
                }),
            ),
            ("sha256", Box::new(|fingerprint| fingerprint.sha256 = "f".repeat(64))),
        ];

        for (field, mutate) in mutations {
            let mut fingerprint = valid_fingerprint();
            mutate(&mut fingerprint);
            assert!(
                matches!(
                    fingerprint.validate(),
                    Err(EvaluationFingerprintValidationError::AggregateMismatch { .. })
                ),
                "mutation of {field} was not detected as an aggregate mismatch"
            );
        }
    }
}
