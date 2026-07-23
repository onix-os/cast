//! Canonical generated dependency and policy resolution data.

use std::collections::{BTreeMap, BTreeSet};

use sha2::{Digest, Sha256};

use super::CanonicalEncoder;

use self::closure_validation::{detect_dependency_cycles, require_nonempty, require_reachable_packages};
pub use self::{
    gluon_codec::{BUILD_LOCK_GENERATED_GLUON_MARKER, GluonBuildLockCodec},
    validation_errors::BuildLockValidationError,
};

mod closure_validation;
mod gluon_codec;
mod validation_errors;

#[cfg(test)]
mod tests;

pub const BUILD_LOCK_FILE_NAME: &str = "build.lock.glu";
pub const BUILD_LOCK_SCHEMA_VERSION: u32 = 6;

const BUILD_LOCK_HASH_DOMAIN: &[u8] = b"os-tools-build-lock\0";
const REQUESTED_INPUTS_HASH_DOMAIN: &[u8] = b"os-tools-requested-inputs-v2\0";

/// Exact package, repository, platform, policy, and selected structural
/// builder resolution used by a plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildLock {
    pub schema_version: u32,
    pub request_fingerprint: String,
    pub repositories: Vec<RepositorySnapshot>,
    pub requests: Vec<LockedRequest>,
    pub packages: Vec<LockedPackage>,
    pub build_platform: Platform,
    pub host_platform: Platform,
    pub target_platform: Platform,
    pub policy: LockedIdentity,
    pub target: LockedIdentity,
    pub profile: LockedIdentity,
    pub toolchain: LockedIdentity,
    pub builder: LockedIdentity,
}

impl BuildLock {
    /// Normalize semantically unordered lock collections.
    pub fn normalize(&mut self) {
        self.repositories
            .sort_by(|left, right| left.id.cmp(&right.id).then_with(|| left.snapshot.cmp(&right.snapshot)));
        self.requests.sort_by(|left, right| left.request.cmp(&right.request));
        for request in &mut self.requests {
            request.origins.sort();
        }
        self.packages
            .sort_by(|left, right| left.package_id.cmp(&right.package_id));
        for package in &mut self.packages {
            package.outputs.sort_by(|left, right| left.name.cmp(&right.name));
            package.dependencies.sort();
        }
    }

    /// Validate that the lock describes an exact, internally consistent
    /// package/output closure.
    pub fn validate(&self) -> Result<(), BuildLockValidationError> {
        if self.schema_version != BUILD_LOCK_SCHEMA_VERSION {
            return Err(BuildLockValidationError::UnsupportedSchema {
                found: self.schema_version,
                expected: BUILD_LOCK_SCHEMA_VERSION,
            });
        }
        require_nonempty("request_fingerprint", &self.request_fingerprint)?;
        self.build_platform.validate("build_platform")?;
        self.host_platform.validate("host_platform")?;
        self.target_platform.validate("target_platform")?;
        self.policy.validate("policy")?;
        self.target.validate("target")?;
        self.profile.validate("profile")?;
        self.toolchain.validate("toolchain")?;
        self.builder.validate("builder")?;

        let mut repositories = BTreeMap::new();
        for (index, repository) in self.repositories.iter().enumerate() {
            require_nonempty(&format!("repositories[{index}].id"), &repository.id)?;
            require_nonempty(&format!("repositories[{index}].index_uri"), &repository.index_uri)?;
            require_nonempty(&format!("repositories[{index}].snapshot"), &repository.snapshot)?;
            if let Some(first_index) = repositories.insert(repository.id.as_str(), index) {
                return Err(BuildLockValidationError::DuplicateRepository {
                    id: repository.id.clone(),
                    first_index,
                    duplicate_index: index,
                });
            }
        }

        let mut packages = BTreeMap::new();
        let mut used_repositories = BTreeSet::new();
        for (index, package) in self.packages.iter().enumerate() {
            package.validate(index)?;
            if let Some(first_index) = packages.insert(package.package_id.as_str(), index) {
                return Err(BuildLockValidationError::DuplicatePackage {
                    id: package.package_id.clone(),
                    first_index,
                    duplicate_index: index,
                });
            }
            if !repositories.contains_key(package.repository.as_str()) {
                return Err(BuildLockValidationError::UnknownRepository {
                    package: package.package_id.clone(),
                    repository: package.repository.clone(),
                });
            }
            used_repositories.insert(package.repository.as_str());
        }
        for (index, repository) in self.repositories.iter().enumerate() {
            if !used_repositories.contains(repository.id.as_str()) {
                return Err(BuildLockValidationError::UnusedRepository {
                    index,
                    id: repository.id.clone(),
                });
            }
        }

        let mut requests = BTreeMap::new();
        for (index, request) in self.requests.iter().enumerate() {
            require_nonempty(&format!("requests[{index}].request"), &request.request)?;
            require_nonempty(&format!("requests[{index}].package_id"), &request.package_id)?;
            require_nonempty(&format!("requests[{index}].output"), &request.output)?;
            if request.origins.is_empty() {
                return Err(BuildLockValidationError::MissingInputOrigins {
                    request: request.request.clone(),
                });
            }
            let mut origins = BTreeMap::new();
            for (origin_index, origin) in request.origins.iter().enumerate() {
                origin.validate(index, origin_index)?;
                if let Some(first_index) = origins.insert(origin, origin_index) {
                    return Err(BuildLockValidationError::DuplicateInputOrigin {
                        request: request.request.clone(),
                        first_index,
                        duplicate_index: origin_index,
                    });
                }
            }
            if let Some(first_index) = requests.insert(request.request.as_str(), index) {
                return Err(BuildLockValidationError::DuplicateRequest {
                    request: request.request.clone(),
                    first_index,
                    duplicate_index: index,
                });
            }
            let Some(package_index) = packages.get(request.package_id.as_str()) else {
                return Err(BuildLockValidationError::UnknownPackage {
                    field: format!("requests[{index}]"),
                    package: request.package_id.clone(),
                });
            };
            if !self.packages[*package_index]
                .outputs
                .iter()
                .any(|output| output.name == request.output)
            {
                return Err(BuildLockValidationError::UnknownOutput {
                    field: format!("requests[{index}]"),
                    package: request.package_id.clone(),
                    output: request.output.clone(),
                });
            }
        }

        for (package_index, package) in self.packages.iter().enumerate() {
            for (dependency_index, dependency) in package.dependencies.iter().enumerate() {
                let Some(target_index) = packages.get(dependency.package_id.as_str()) else {
                    return Err(BuildLockValidationError::UnknownPackage {
                        field: format!("packages[{package_index}].dependencies[{dependency_index}]"),
                        package: dependency.package_id.clone(),
                    });
                };
                let target = &self.packages[*target_index];
                if !target.outputs.iter().any(|output| output.name == dependency.output) {
                    return Err(BuildLockValidationError::UnknownOutput {
                        field: format!("packages[{package_index}].dependencies[{dependency_index}]"),
                        package: dependency.package_id.clone(),
                        output: dependency.output.clone(),
                    });
                }
            }
        }

        detect_dependency_cycles(&self.packages, &packages)?;
        require_reachable_packages(&self.packages, &self.requests, &packages)?;

        Ok(())
    }

    /// Return the stable binary encoding used when the lock contributes to a
    /// derivation identity.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut encoder = CanonicalEncoder::new(BUILD_LOCK_HASH_DOMAIN);
        self.encode_canonical(&mut encoder);
        encoder.finish()
    }

    /// Hash the canonical lock independently from the presentation encoding.
    pub fn digest(&self) -> String {
        format!("{:x}", Sha256::digest(self.canonical_bytes()))
    }

    /// Return whether an exact package/output identity is present in the
    /// locked closure.
    pub fn contains_output(&self, reference: &LockedOutputRef) -> bool {
        self.packages.iter().any(|package| {
            package.package_id == reference.package_id
                && package.outputs.iter().any(|output| output.name == reference.output)
        })
    }

    pub(super) fn encode_canonical(&self, encoder: &mut CanonicalEncoder) {
        encoder.u32(self.schema_version);
        encoder.string(&self.request_fingerprint);

        let mut repositories = self.repositories.iter().collect::<Vec<_>>();
        repositories.sort_by(|left, right| left.id.cmp(&right.id).then_with(|| left.snapshot.cmp(&right.snapshot)));
        encoder.sequence(&repositories, |encoder, repository| repository.encode(encoder));

        let mut requests = self.requests.iter().collect::<Vec<_>>();
        requests.sort_by(|left, right| left.request.cmp(&right.request));
        encoder.sequence(&requests, |encoder, request| request.encode(encoder));

        let mut packages = self.packages.iter().collect::<Vec<_>>();
        packages.sort_by(|left, right| left.package_id.cmp(&right.package_id));
        encoder.sequence(&packages, |encoder, package| package.encode(encoder));

        self.build_platform.encode(encoder);
        self.host_platform.encode(encoder);
        self.target_platform.encode(encoder);
        self.policy.encode(encoder);
        self.target.encode(encoder);
        self.profile.encode(encoder);
        self.toolchain.encode(encoder);
        self.builder.encode(encoder);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Platform {
    pub architecture: String,
    pub vendor: String,
    pub operating_system: String,
    pub abi: String,
}

impl Platform {
    fn validate(&self, field: &str) -> Result<(), BuildLockValidationError> {
        require_nonempty(&format!("{field}.architecture"), &self.architecture)?;
        require_nonempty(&format!("{field}.vendor"), &self.vendor)?;
        require_nonempty(&format!("{field}.operating_system"), &self.operating_system)?;
        require_nonempty(&format!("{field}.abi"), &self.abi)
    }

    fn encode(&self, encoder: &mut CanonicalEncoder) {
        encoder.string(&self.architecture);
        encoder.string(&self.vendor);
        encoder.string(&self.operating_system);
        encoder.string(&self.abi);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositorySnapshot {
    pub id: String,
    pub index_uri: String,
    pub snapshot: String,
}

impl RepositorySnapshot {
    fn encode(&self, encoder: &mut CanonicalEncoder) {
        encoder.string(&self.id);
        encoder.string(&self.index_uri);
        encoder.string(&self.snapshot);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockedIdentity {
    pub name: String,
    pub fingerprint: String,
}

impl LockedIdentity {
    pub(super) fn validate(&self, field: &str) -> Result<(), BuildLockValidationError> {
        require_nonempty(&format!("{field}.name"), &self.name)?;
        require_nonempty(&format!("{field}.fingerprint"), &self.fingerprint)
    }

    pub(super) fn encode(&self, encoder: &mut CanonicalEncoder) {
        encoder.string(&self.name);
        encoder.string(&self.fingerprint);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockedPackage {
    pub package_id: String,
    pub name: String,
    pub version: String,
    pub architecture: String,
    pub repository: String,
    pub outputs: Vec<LockedOutput>,
    pub dependencies: Vec<LockedOutputRef>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockedRequest {
    pub request: String,
    pub package_id: String,
    pub output: String,
    /// Every typed reason this canonical provider request entered resolution.
    /// The collection is semantically unordered and encoded canonically.
    pub origins: Vec<InputOrigin>,
}

impl LockedRequest {
    fn encode(&self, encoder: &mut CanonicalEncoder) {
        encoder.string(&self.request);
        encoder.string(&self.package_id);
        encoder.string(&self.output);
        let mut origins = self.origins.iter().collect::<Vec<_>>();
        origins.sort();
        encoder.sequence(&origins, |encoder, origin| origin.encode(encoder));
    }
}

/// One canonical provider request before Forge chooses an exact package/output.
///
/// Origins are collected before provider strings are deduplicated, then
/// aggregated here so lock reuse can prove both the root set and every reason
/// each root was requested.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestedInput {
    pub request: String,
    pub origins: Vec<InputOrigin>,
}

impl RequestedInput {
    pub fn normalize(&mut self) {
        self.origins.sort();
        self.origins.dedup();
    }

    fn encode(&self, encoder: &mut CanonicalEncoder) {
        encoder.string(&self.request);
        let mut origins = self.origins.iter().collect::<Vec<_>>();
        origins.sort();
        encoder.sequence(&origins, |encoder, origin| origin.encode(encoder));
    }
}

/// Hash the exact request-to-origin relation used by generated-lock staleness
/// checks. Construction order and repeated identical origins are irrelevant.
pub fn requested_inputs_digest(requests: &[RequestedInput]) -> String {
    let mut requests = requests.to_vec();
    for request in &mut requests {
        request.normalize();
    }
    requests.sort_by(|left, right| left.request.cmp(&right.request));
    let mut encoder = CanonicalEncoder::new(REQUESTED_INPUTS_HASH_DOMAIN);
    encoder.sequence(&requests, |encoder, request| request.encode(encoder));
    format!("{:x}", Sha256::digest(encoder.finish()))
}

/// Which package-level declaration supplied one dependency.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum PackageInputSelection {
    Package,
    Profile { name: String },
}

/// Exact position of a step within a frozen phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum JobStepSection {
    Pre,
    Steps,
    Post,
}

/// Executable position inside a typed build step.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum JobExecutableRole {
    RunProgram,
    ShellInterpreter,
    ShellDeclaredProgram { index: u32 },
}

/// Semantic role of a frozen analyzer executable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AnalyzerRole {
    PkgConfig,
    Python,
    Objcopy,
    Strip,
}

/// Semantic compiler command selected from the repository toolchain policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CompilerExecutableRole {
    Cc,
    Cxx,
    Objc,
    Objcxx,
    Cpp,
    Objcpp,
    Objcxxcpp,
    Ar,
    Ld,
    Objcopy,
    Nm,
    Ranlib,
    Strip,
}

/// Explicit cache executable selected when compiler caching is enabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CompilerCacheRole {
    Ccache,
    Sccache,
}

/// Typed reason a provider request participates in the frozen build closure.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum InputOrigin {
    BuilderTool {
        selection: PackageInputSelection,
        index: u32,
    },
    NativeBuild {
        selection: PackageInputSelection,
        index: u32,
    },
    Build {
        selection: PackageInputSelection,
        index: u32,
    },
    Check {
        selection: PackageInputSelection,
        index: u32,
    },
    OutputRuntime {
        output: String,
        index: u32,
    },
    Policy {
        source: String,
        field: String,
        index: u32,
    },
    JobExecutable {
        job: u32,
        phase: u32,
        phase_name: String,
        section: JobStepSection,
        step: u32,
        role: JobExecutableRole,
    },
    Analyzer {
        role: AnalyzerRole,
    },
    CompilerExecutable {
        role: CompilerExecutableRole,
    },
    CompilerCache {
        role: CompilerCacheRole,
    },
    MoldLinker,
}

impl InputOrigin {
    fn validate(&self, request_index: usize, origin_index: usize) -> Result<(), BuildLockValidationError> {
        let field = format!("requests[{request_index}].origins[{origin_index}]");
        match self {
            Self::BuilderTool { selection, .. }
            | Self::NativeBuild { selection, .. }
            | Self::Build { selection, .. }
            | Self::Check { selection, .. } => selection.validate(&format!("{field}.selection")),
            Self::OutputRuntime { output, .. } => require_nonempty(&format!("{field}.output"), output),
            Self::Policy {
                source,
                field: policy_field,
                ..
            } => {
                require_nonempty(&format!("{field}.source"), source)?;
                require_nonempty(&format!("{field}.field"), policy_field)
            }
            Self::JobExecutable { phase_name, .. } => require_nonempty(&format!("{field}.phase_name"), phase_name),
            Self::Analyzer { .. } | Self::CompilerExecutable { .. } | Self::CompilerCache { .. } | Self::MoldLinker => {
                Ok(())
            }
        }
    }

    fn encode(&self, encoder: &mut CanonicalEncoder) {
        match self {
            Self::BuilderTool { selection, index } => {
                encoder.variant(0);
                selection.encode(encoder);
                encoder.u32(*index);
            }
            Self::NativeBuild { selection, index } => {
                encoder.variant(1);
                selection.encode(encoder);
                encoder.u32(*index);
            }
            Self::Build { selection, index } => {
                encoder.variant(2);
                selection.encode(encoder);
                encoder.u32(*index);
            }
            Self::Check { selection, index } => {
                encoder.variant(3);
                selection.encode(encoder);
                encoder.u32(*index);
            }
            Self::OutputRuntime { output, index } => {
                encoder.variant(4);
                encoder.string(output);
                encoder.u32(*index);
            }
            Self::Policy { source, field, index } => {
                encoder.variant(5);
                encoder.string(source);
                encoder.string(field);
                encoder.u32(*index);
            }
            Self::JobExecutable {
                job,
                phase,
                phase_name,
                section,
                step,
                role,
            } => {
                encoder.variant(6);
                encoder.u32(*job);
                encoder.u32(*phase);
                encoder.string(phase_name);
                section.encode(encoder);
                encoder.u32(*step);
                role.encode(encoder);
            }
            Self::Analyzer { role } => {
                encoder.variant(7);
                role.encode(encoder);
            }
            Self::CompilerExecutable { role } => {
                encoder.variant(8);
                role.encode(encoder);
            }
            Self::CompilerCache { role } => {
                encoder.variant(9);
                role.encode(encoder);
            }
            Self::MoldLinker => encoder.variant(10),
        }
    }
}

impl PackageInputSelection {
    fn validate(&self, field: &str) -> Result<(), BuildLockValidationError> {
        match self {
            Self::Package => Ok(()),
            Self::Profile { name } => require_nonempty(&format!("{field}.name"), name),
        }
    }

    fn encode(&self, encoder: &mut CanonicalEncoder) {
        match self {
            Self::Package => encoder.variant(0),
            Self::Profile { name } => {
                encoder.variant(1);
                encoder.string(name);
            }
        }
    }
}

impl JobStepSection {
    fn encode(self, encoder: &mut CanonicalEncoder) {
        encoder.variant(match self {
            Self::Pre => 0,
            Self::Steps => 1,
            Self::Post => 2,
        });
    }
}

impl JobExecutableRole {
    fn encode(&self, encoder: &mut CanonicalEncoder) {
        match self {
            Self::RunProgram => encoder.variant(0),
            Self::ShellInterpreter => encoder.variant(1),
            Self::ShellDeclaredProgram { index } => {
                encoder.variant(2);
                encoder.u32(*index);
            }
        }
    }
}

impl AnalyzerRole {
    fn encode(self, encoder: &mut CanonicalEncoder) {
        encoder.variant(match self {
            Self::PkgConfig => 0,
            Self::Python => 1,
            Self::Objcopy => 2,
            Self::Strip => 3,
        });
    }
}

impl CompilerExecutableRole {
    pub(super) fn encode(self, encoder: &mut CanonicalEncoder) {
        encoder.variant(match self {
            Self::Cc => 0,
            Self::Cxx => 1,
            Self::Objc => 2,
            Self::Objcxx => 3,
            Self::Cpp => 4,
            Self::Objcpp => 5,
            Self::Objcxxcpp => 6,
            Self::Ar => 7,
            Self::Ld => 8,
            Self::Objcopy => 9,
            Self::Nm => 10,
            Self::Ranlib => 11,
            Self::Strip => 12,
        });
    }
}

impl CompilerCacheRole {
    fn encode(self, encoder: &mut CanonicalEncoder) {
        encoder.variant(match self {
            Self::Ccache => 0,
            Self::Sccache => 1,
        });
    }
}

impl LockedPackage {
    fn validate(&self, index: usize) -> Result<(), BuildLockValidationError> {
        for (field, value) in [
            ("package_id", &self.package_id),
            ("name", &self.name),
            ("version", &self.version),
            ("architecture", &self.architecture),
            ("repository", &self.repository),
        ] {
            require_nonempty(&format!("packages[{index}].{field}"), value)?;
        }
        if self.outputs.is_empty() {
            return Err(BuildLockValidationError::MissingOutputs {
                package: self.package_id.clone(),
            });
        }

        let mut outputs = BTreeMap::new();
        for (output_index, output) in self.outputs.iter().enumerate() {
            require_nonempty(&format!("packages[{index}].outputs[{output_index}].name"), &output.name)?;
            if let Some(first_index) = outputs.insert(output.name.as_str(), output_index) {
                return Err(BuildLockValidationError::DuplicateOutput {
                    package: self.package_id.clone(),
                    output: output.name.clone(),
                    first_index,
                    duplicate_index: output_index,
                });
            }
        }
        let mut dependencies = BTreeMap::new();
        for (dependency_index, dependency) in self.dependencies.iter().enumerate() {
            require_nonempty(
                &format!("packages[{index}].dependencies[{dependency_index}].package_id"),
                &dependency.package_id,
            )?;
            require_nonempty(
                &format!("packages[{index}].dependencies[{dependency_index}].output"),
                &dependency.output,
            )?;
            if let Some(first_index) = dependencies.insert(
                (dependency.package_id.as_str(), dependency.output.as_str()),
                dependency_index,
            ) {
                return Err(BuildLockValidationError::DuplicateDependency {
                    package: self.package_id.clone(),
                    dependency_package: dependency.package_id.clone(),
                    output: dependency.output.clone(),
                    first_index,
                    duplicate_index: dependency_index,
                });
            }
        }
        Ok(())
    }

    fn encode(&self, encoder: &mut CanonicalEncoder) {
        encoder.string(&self.package_id);
        encoder.string(&self.name);
        encoder.string(&self.version);
        encoder.string(&self.architecture);
        encoder.string(&self.repository);

        let mut outputs = self.outputs.iter().collect::<Vec<_>>();
        outputs.sort_by(|left, right| left.name.cmp(&right.name));
        encoder.sequence(&outputs, |encoder, output| output.encode(encoder));

        let mut dependencies = self.dependencies.iter().collect::<Vec<_>>();
        dependencies.sort();
        encoder.sequence(&dependencies, |encoder, dependency| dependency.encode(encoder));
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockedOutput {
    pub name: String,
}

impl LockedOutput {
    fn encode(&self, encoder: &mut CanonicalEncoder) {
        encoder.string(&self.name);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct LockedOutputRef {
    pub package_id: String,
    pub output: String,
}

impl LockedOutputRef {
    pub(super) fn encode(&self, encoder: &mut CanonicalEncoder) {
        encoder.string(&self.package_id);
        encoder.string(&self.output);
    }
}

#[cfg(test)]
pub(super) fn sample_lock() -> BuildLock {
    let platform = Platform {
        architecture: "x86_64".to_owned(),
        vendor: "unknown".to_owned(),
        operating_system: "linux".to_owned(),
        abi: "gnu".to_owned(),
    };
    BuildLock {
        schema_version: BUILD_LOCK_SCHEMA_VERSION,
        request_fingerprint: "request-fingerprint".to_owned(),
        repositories: vec![RepositorySnapshot {
            id: "volatile".to_owned(),
            index_uri: "https://example.invalid/stone.index".to_owned(),
            snapshot: "repository-snapshot".to_owned(),
        }],
        requests: vec![LockedRequest {
            request: "binary(hello)".to_owned(),
            package_id: "hello-id".to_owned(),
            output: "out".to_owned(),
            origins: vec![InputOrigin::BuilderTool {
                selection: PackageInputSelection::Package,
                index: 0,
            }],
        }],
        packages: vec![
            LockedPackage {
                package_id: "cmake-id".to_owned(),
                name: "cmake".to_owned(),
                version: "3.31.0-1".to_owned(),
                architecture: "x86_64".to_owned(),
                repository: "volatile".to_owned(),
                outputs: vec![LockedOutput { name: "out".to_owned() }],
                dependencies: Vec::new(),
            },
            LockedPackage {
                package_id: "hello-id".to_owned(),
                name: "hello".to_owned(),
                version: "1.0.0-1".to_owned(),
                architecture: "x86_64".to_owned(),
                repository: "volatile".to_owned(),
                outputs: vec![LockedOutput { name: "out".to_owned() }],
                dependencies: vec![LockedOutputRef {
                    package_id: "cmake-id".to_owned(),
                    output: "out".to_owned(),
                }],
            },
        ],
        build_platform: platform.clone(),
        host_platform: platform.clone(),
        target_platform: platform,
        policy: LockedIdentity {
            name: "aerynos".to_owned(),
            fingerprint: "policy-fingerprint".to_owned(),
        },
        target: LockedIdentity {
            name: "x86_64".to_owned(),
            fingerprint: "target-fingerprint".to_owned(),
        },
        profile: LockedIdentity {
            name: "default-x86_64".to_owned(),
            fingerprint: "profile-fingerprint".to_owned(),
        },
        toolchain: LockedIdentity {
            name: "llvm".to_owned(),
            fingerprint: "toolchain-fingerprint".to_owned(),
        },
        builder: LockedIdentity {
            name: "cmake".to_owned(),
            fingerprint: "builder-fingerprint".to_owned(),
        },
    }
}
