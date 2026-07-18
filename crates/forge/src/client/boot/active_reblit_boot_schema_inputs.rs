//! Authenticated semantic schemas for one ActiveReblit boot repair attempt.
//!
//! Package-owned `os-info.json` bytes are read only through their exact sealed
//! Stone coordinate. Cast-generated `lib/os-release` is read only beneath a
//! revalidated state-root descriptor. Historical structural or semantic
//! failures may select the already-authenticated head schema, but operational
//! failures never become absence and the selected fallback remains sticky for
//! the lifetime of this value.

use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    path::Path,
    time::{Duration, Instant},
};

use thiserror::Error;

use crate::{state, transition_identity::RevalidatedActiveReblitBootStateRoots};

use super::{
    active_reblit_boot_inputs::{BoundActiveReblitBootAsset, PreparedActiveReblitStoneBootInputs},
    active_reblit_boot_projection::{
        BootAssetRole, BootSchemaFallback, BootSchemaSource, PlannedBootSchemaRequirement,
    },
};

const OS_INFO_LOGICAL_PATH: &str = "/usr/lib/os-info.json";
const MAX_SCHEMA_SOURCE_BYTES: usize = 1024 * 1024;
const MAX_SCHEMA_TOTAL_BYTES: usize = 5 * MAX_SCHEMA_SOURCE_BYTES;
const MAX_SCHEMA_WORK: usize = 4_096;
const MAX_SCHEMA_STATES: usize = 5;
const SCHEMA_TIMEOUT: Duration = Duration::from_secs(30);

#[path = "active_reblit_boot_schema_inputs/generated_os_release.rs"]
mod generated_os_release;
#[path = "active_reblit_boot_schema_inputs/schema_validation.rs"]
mod schema_validation;

use generated_os_release::{GeneratedPreparation, RetainedGeneratedOsRelease, read_exact_descriptor};
#[cfg(test)]
use generated_os_release::{
    arm_after_generated_name_file_open, arm_after_generated_name_lib_open, arm_after_generated_read,
    arm_after_generated_revalidation_read, arm_generated_operational_fault,
};
use schema_validation::{parse_os_info, parse_os_release};

const SCHEMA_POLICY: BootSchemaInputPolicy = BootSchemaInputPolicy {
    max_source_bytes: MAX_SCHEMA_SOURCE_BYTES,
    max_total_bytes: MAX_SCHEMA_TOTAL_BYTES,
    max_work: MAX_SCHEMA_WORK,
    timeout: SCHEMA_TIMEOUT,
};

/// Semantic fields admitted for deterministic BLS rendering.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::client) struct ValidatedActiveReblitBootSchema {
    os_name: Box<str>,
    os_id: Box<str>,
    namespace: Box<str>,
    display_name: Box<str>,
    former_identities: Box<[ValidatedActiveReblitFormerIdentity]>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::client) struct ValidatedActiveReblitFormerIdentity {
    id: Box<str>,
    name: Box<str>,
}

impl ValidatedActiveReblitBootSchema {
    pub(in crate::client) fn os_name(&self) -> &str {
        &self.os_name
    }

    pub(in crate::client) fn os_id(&self) -> &str {
        &self.os_id
    }

    pub(in crate::client) fn namespace(&self) -> &str {
        &self.namespace
    }

    pub(in crate::client) fn display_name(&self) -> &str {
        &self.display_name
    }

    pub(in crate::client) fn former_identities(&self) -> &[ValidatedActiveReblitFormerIdentity] {
        &self.former_identities
    }
}

impl ValidatedActiveReblitFormerIdentity {
    pub(in crate::client) fn id(&self) -> &str {
        &self.id
    }

    pub(in crate::client) fn name(&self) -> &str {
        &self.name
    }
}

/// Exact provenance of one selected schema. A Stone coordinate is meaningful
/// only together with the non-`Clone` Stone owner supplied to revalidation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) enum ActiveReblitBootSchemaSourceBinding {
    StoneOsInfo {
        binding_index: u16,
        digest: u128,
        length: u64,
    },
    GeneratedOsRelease {
        state_id: state::Id,
        digest: u128,
        length: u64,
    },
    GlobalFallback {
        failed_local: ActiveReblitBootSchemaFallbackReason,
        global_state: state::Id,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) enum ActiveReblitBootSchemaFallbackReason {
    Structural(ActiveReblitBootSchemaStructuralReason),
    Semantic(ActiveReblitBootSchemaSemanticReason),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) enum ActiveReblitBootSchemaStructuralReason {
    MissingLib,
    MissingOsRelease,
    UnsafeLib,
    UnsafeOsRelease,
    AccessAcl,
    DefaultAcl,
    ExtendedAttributes,
    SourceTooLarge,
    ChangedDuringRead,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) enum ActiveReblitBootSchemaSemanticReason {
    NonUtf8,
    InvalidDocument,
    MissingIdentity,
    UnsafeIdentifier,
    UnsafeText,
    TooManyFormerIdentities,
    DuplicateFormerIdentity,
}

/// One state schema and its exact selected provenance.
pub(in crate::client) struct PreparedActiveReblitStateBootSchema {
    state_id: state::Id,
    schema: ValidatedActiveReblitBootSchema,
    source: ActiveReblitBootSchemaSourceBinding,
    retained_generated: Option<RetainedGeneratedOsRelease>,
}

impl PreparedActiveReblitStateBootSchema {
    pub(in crate::client) fn state_id(&self) -> state::Id {
        self.state_id
    }

    pub(in crate::client) fn schema(&self) -> &ValidatedActiveReblitBootSchema {
        &self.schema
    }

    pub(in crate::client) fn source(&self) -> ActiveReblitBootSchemaSourceBinding {
        self.source
    }
}

/// Complete, bounded schemas for the eligible boot states in one attempt.
///
/// This value is intentionally not `Clone`. Successful generated sources keep
/// their `/usr`, `lib`, pinned-file and readable-file descriptors alive until
/// the final pre-claim source revalidation.
pub(in crate::client) struct PreparedActiveReblitBootSchemas {
    projected_state_ids: Box<[state::Id]>,
    projected_requirements: Box<[PlannedBootSchemaRequirement]>,
    eligible_state_ids: Box<[state::Id]>,
    global_state: state::Id,
    schemas: Box<[PreparedActiveReblitStateBootSchema]>,
    total_source_bytes: usize,
    preparation_work: usize,
}

impl PreparedActiveReblitBootSchemas {
    pub(in crate::client) fn prepare(
        stone: &PreparedActiveReblitStoneBootInputs,
        roots: &RevalidatedActiveReblitBootStateRoots<'_>,
    ) -> Result<Self, ActiveReblitBootSchemaInputsError> {
        prepare_with_policy(stone, roots, BootSchemaInputPolicy::production())
    }

    pub(in crate::client) fn global_state(&self) -> state::Id {
        self.global_state
    }

    pub(in crate::client) fn schemas(&self) -> &[PreparedActiveReblitStateBootSchema] {
        &self.schemas
    }

    pub(in crate::client) fn schema_for_state(
        &self,
        state_id: state::Id,
    ) -> Option<&PreparedActiveReblitStateBootSchema> {
        self.schemas.iter().find(|schema| schema.state_id == state_id)
    }

    pub(in crate::client) fn total_source_bytes(&self) -> usize {
        self.total_source_bytes
    }

    pub(in crate::client) fn preparation_work(&self) -> usize {
        self.preparation_work
    }

    /// Rebind every selected source to the same Stone owner and a fresh
    /// epoch-sandwiched state-root view. Sticky fallbacks are deliberately not
    /// promoted even if a formerly absent historical file appears.
    pub(in crate::client) fn revalidate_sources(
        &self,
        stone: &PreparedActiveReblitStoneBootInputs,
        roots: &RevalidatedActiveReblitBootStateRoots<'_>,
    ) -> Result<(), ActiveReblitBootSchemaInputsError> {
        if stone.state_ids() != self.projected_state_ids.as_ref() {
            return Err(ActiveReblitBootSchemaInputsError::StateProjectionChanged);
        }
        if stone.schema_requirements() != self.projected_requirements.as_ref() {
            return Err(ActiveReblitBootSchemaInputsError::RequirementProjectionChanged);
        }
        if roots.eligible_state_ids() != self.eligible_state_ids.as_ref() {
            return Err(ActiveReblitBootSchemaInputsError::EligibleStateProjectionChanged);
        }
        let mut budget = SchemaBudget::new(BootSchemaInputPolicy::production())?;
        for prepared in &self.schemas {
            budget.step()?;
            match prepared.source {
                ActiveReblitBootSchemaSourceBinding::StoneOsInfo {
                    binding_index,
                    digest,
                    length,
                } => revalidate_stone_source(stone, prepared.state_id, binding_index, digest, length, &mut budget)?,
                ActiveReblitBootSchemaSourceBinding::GeneratedOsRelease { .. } => {
                    let retained = prepared.retained_generated.as_ref().ok_or(
                        ActiveReblitBootSchemaInputsError::MissingRetainedGeneratedSource {
                            state: i32::from(prepared.state_id),
                        },
                    )?;
                    retained.revalidate(roots, &mut budget)?;
                }
                ActiveReblitBootSchemaSourceBinding::GlobalFallback { global_state, .. } => {
                    if global_state != self.global_state || prepared.retained_generated.is_some() {
                        return Err(ActiveReblitBootSchemaInputsError::InvalidPreparedFallback {
                            state: i32::from(prepared.state_id),
                        });
                    }
                }
            }
        }
        budget.require_deadline()
    }
}

#[derive(Clone, Copy)]
struct BootSchemaInputPolicy {
    max_source_bytes: usize,
    max_total_bytes: usize,
    max_work: usize,
    timeout: Duration,
}

impl BootSchemaInputPolicy {
    const fn production() -> Self {
        SCHEMA_POLICY
    }
}

struct SchemaBudget {
    policy: BootSchemaInputPolicy,
    deadline: Instant,
    work: usize,
    source_bytes: usize,
}

impl SchemaBudget {
    fn new(policy: BootSchemaInputPolicy) -> Result<Self, ActiveReblitBootSchemaInputsError> {
        let deadline =
            Instant::now()
                .checked_add(policy.timeout)
                .ok_or(ActiveReblitBootSchemaInputsError::InvalidDeadline {
                    timeout: policy.timeout,
                })?;
        Ok(Self {
            policy,
            deadline,
            work: 0,
            source_bytes: 0,
        })
    }

    fn step(&mut self) -> Result<(), ActiveReblitBootSchemaInputsError> {
        self.require_deadline()?;
        let actual = self.work.saturating_add(1);
        if actual > self.policy.max_work {
            return Err(ActiveReblitBootSchemaInputsError::WorkLimit {
                limit: self.policy.max_work,
                actual,
            });
        }
        self.work = actual;
        Ok(())
    }

    fn admit_source_bytes(&mut self, actual: usize) -> Result<(), ActiveReblitBootSchemaInputsError> {
        if actual > self.policy.max_source_bytes {
            return Err(ActiveReblitBootSchemaInputsError::SourceByteLimit {
                limit: self.policy.max_source_bytes,
                actual,
            });
        }
        let total = self.source_bytes.checked_add(actual).unwrap_or(usize::MAX);
        if total > self.policy.max_total_bytes {
            return Err(ActiveReblitBootSchemaInputsError::TotalByteLimit {
                limit: self.policy.max_total_bytes,
                actual: total,
            });
        }
        self.source_bytes = total;
        Ok(())
    }

    fn require_deadline(&self) -> Result<(), ActiveReblitBootSchemaInputsError> {
        if Instant::now() > self.deadline {
            Err(ActiveReblitBootSchemaInputsError::DeadlineExceeded {
                timeout: self.policy.timeout,
            })
        } else {
            Ok(())
        }
    }
}

fn prepare_with_policy(
    stone: &PreparedActiveReblitStoneBootInputs,
    roots: &RevalidatedActiveReblitBootStateRoots<'_>,
    policy: BootSchemaInputPolicy,
) -> Result<PreparedActiveReblitBootSchemas, ActiveReblitBootSchemaInputsError> {
    validate_correlated_inputs(stone, roots)?;
    let requirements = stone.schema_requirements();
    let global_requirement = requirements
        .first()
        .copied()
        .ok_or(ActiveReblitBootSchemaInputsError::MissingGlobalRequirement)?;
    let global_state = global_requirement.state_id();
    let mut budget = SchemaBudget::new(policy)?;
    let mut schemas: Vec<PreparedActiveReblitStateBootSchema> = Vec::with_capacity(requirements.len());

    for requirement in requirements.iter().copied() {
        budget.step()?;
        if !roots.eligible_state_ids().contains(&requirement.state_id()) {
            continue;
        }
        let local = resolve_local_schema(stone, roots, requirement, &mut budget)?;
        let (schema, source, retained_generated) = match local {
            LocalSchemaResolution::Ready {
                schema,
                source,
                retained_generated,
            } => (schema, source, retained_generated),
            LocalSchemaResolution::Unavailable(reason) => match requirement.fallback() {
                BootSchemaFallback::Required => {
                    return Err(ActiveReblitBootSchemaInputsError::RequiredSchemaUnavailable {
                        state: i32::from(requirement.state_id()),
                        reason,
                    });
                }
                BootSchemaFallback::Global => {
                    let global =
                        schemas
                            .first()
                            .ok_or(ActiveReblitBootSchemaInputsError::GlobalFallbackBeforeHead {
                                state: i32::from(requirement.state_id()),
                            })?;
                    (
                        global.schema.clone(),
                        ActiveReblitBootSchemaSourceBinding::GlobalFallback {
                            failed_local: reason,
                            global_state,
                        },
                        None,
                    )
                }
            },
        };
        schemas.push(PreparedActiveReblitStateBootSchema {
            state_id: requirement.state_id(),
            schema,
            source,
            retained_generated,
        });
    }
    budget.require_deadline()?;
    if schemas.first().map(|schema| schema.state_id) != Some(global_state) {
        return Err(ActiveReblitBootSchemaInputsError::MissingGlobalSchema {
            state: i32::from(global_state),
        });
    }
    Ok(PreparedActiveReblitBootSchemas {
        projected_state_ids: stone.state_ids().to_vec().into_boxed_slice(),
        projected_requirements: requirements.to_vec().into_boxed_slice(),
        eligible_state_ids: roots.eligible_state_ids().to_vec().into_boxed_slice(),
        global_state,
        schemas: schemas.into_boxed_slice(),
        total_source_bytes: budget.source_bytes,
        preparation_work: budget.work,
    })
}

fn validate_correlated_inputs(
    stone: &PreparedActiveReblitStoneBootInputs,
    roots: &RevalidatedActiveReblitBootStateRoots<'_>,
) -> Result<(), ActiveReblitBootSchemaInputsError> {
    let states = stone.state_ids();
    let requirements = stone.schema_requirements();
    if states.is_empty() || states.len() > MAX_SCHEMA_STATES {
        return Err(ActiveReblitBootSchemaInputsError::StateCount {
            limit: MAX_SCHEMA_STATES,
            actual: states.len(),
        });
    }
    let head = states[0];
    if roots.eligible_state_ids().first() != Some(&head) {
        return Err(ActiveReblitBootSchemaInputsError::HeadRootMismatch {
            expected: i32::from(head),
            actual: roots.eligible_state_ids().first().copied().map(i32::from),
        });
    }
    if requirements.first().is_none_or(|requirement| {
        requirement.state_id() != head || requirement.fallback() != BootSchemaFallback::Required
    }) {
        return Err(ActiveReblitBootSchemaInputsError::MissingGlobalRequirement);
    }
    let positions = states
        .iter()
        .enumerate()
        .map(|(index, state)| (*state, index))
        .collect::<BTreeMap<_, _>>();
    let mut seen = BTreeSet::new();
    let mut previous = None;
    for requirement in requirements.iter().copied() {
        let position = positions.get(&requirement.state_id()).copied().ok_or(
            ActiveReblitBootSchemaInputsError::RequirementOutsideProjection {
                state: i32::from(requirement.state_id()),
            },
        )?;
        if !seen.insert(requirement.state_id()) {
            return Err(ActiveReblitBootSchemaInputsError::DuplicateRequirement {
                state: i32::from(requirement.state_id()),
            });
        }
        if previous.is_some_and(|previous| position <= previous) {
            return Err(ActiveReblitBootSchemaInputsError::RequirementOrder {
                state: i32::from(requirement.state_id()),
            });
        }
        if requirement.state_id() != head && requirement.fallback() != BootSchemaFallback::Global {
            return Err(ActiveReblitBootSchemaInputsError::HistoryFallbackPolicy {
                state: i32::from(requirement.state_id()),
            });
        }
        previous = Some(position);
    }
    let mut previous_root_position = None;
    for state in roots.eligible_state_ids() {
        let position = positions
            .get(state)
            .copied()
            .ok_or(ActiveReblitBootSchemaInputsError::EligibleRootOutsideProjection)?;
        if previous_root_position.is_some_and(|previous| position <= previous) {
            return Err(ActiveReblitBootSchemaInputsError::EligibleRootOrder {
                state: i32::from(*state),
            });
        }
        previous_root_position = Some(position);
    }
    Ok(())
}

enum LocalSchemaResolution {
    Ready {
        schema: ValidatedActiveReblitBootSchema,
        source: ActiveReblitBootSchemaSourceBinding,
        retained_generated: Option<RetainedGeneratedOsRelease>,
    },
    Unavailable(ActiveReblitBootSchemaFallbackReason),
}

fn resolve_local_schema(
    stone: &PreparedActiveReblitStoneBootInputs,
    roots: &RevalidatedActiveReblitBootStateRoots<'_>,
    requirement: PlannedBootSchemaRequirement,
    budget: &mut SchemaBudget,
) -> Result<LocalSchemaResolution, ActiveReblitBootSchemaInputsError> {
    match requirement.source() {
        BootSchemaSource::OsInfoAsset => resolve_os_info(stone, requirement.state_id(), budget),
        BootSchemaSource::GeneratedOsRelease => {
            let root = roots
                .roots()
                .find(|root| root.state_id() == requirement.state_id())
                .ok_or(ActiveReblitBootSchemaInputsError::MissingEligibleRoot {
                    state: i32::from(requirement.state_id()),
                })?;
            let retained = match RetainedGeneratedOsRelease::prepare(requirement.state_id(), root.usr(), budget)? {
                GeneratedPreparation::Ready(retained) => retained,
                GeneratedPreparation::Unavailable(reason) => return Ok(LocalSchemaResolution::Unavailable(reason)),
            };
            let schema = match parse_os_release(retained.bytes()) {
                Ok(schema) => schema,
                Err(reason) => {
                    return Ok(LocalSchemaResolution::Unavailable(
                        ActiveReblitBootSchemaFallbackReason::Semantic(reason),
                    ));
                }
            };
            let length = u64::try_from(retained.bytes().len()).expect("bounded schema bytes fit u64");
            let digest = xxhash_rust::xxh3::xxh3_128(retained.bytes());
            Ok(LocalSchemaResolution::Ready {
                schema,
                source: ActiveReblitBootSchemaSourceBinding::GeneratedOsRelease {
                    state_id: requirement.state_id(),
                    digest,
                    length,
                },
                retained_generated: Some(retained),
            })
        }
    }
}

fn resolve_os_info(
    stone: &PreparedActiveReblitStoneBootInputs,
    state_id: state::Id,
    budget: &mut SchemaBudget,
) -> Result<LocalSchemaResolution, ActiveReblitBootSchemaInputsError> {
    let mut matches = stone
        .assets()
        .enumerate()
        .filter(|(_, asset)| asset.state_id() == state_id && matches!(asset.role(), BootAssetRole::OsInfo));
    let (index, asset) = matches
        .next()
        .ok_or(ActiveReblitBootSchemaInputsError::MissingStoneOsInfo {
            state: i32::from(state_id),
        })?;
    if matches.next().is_some() {
        return Err(ActiveReblitBootSchemaInputsError::AmbiguousStoneOsInfo {
            state: i32::from(state_id),
        });
    }
    require_os_info_coordinate(state_id, &asset)?;
    let index = u16::try_from(index).map_err(|_| ActiveReblitBootSchemaInputsError::BindingIndexLimit {
        actual: index,
        limit: u16::MAX as usize,
    })?;
    let length = usize::try_from(asset.length()).map_err(|_| ActiveReblitBootSchemaInputsError::SourceByteLimit {
        limit: budget.policy.max_source_bytes,
        actual: usize::MAX,
    })?;
    budget.admit_source_bytes(length)?;
    let bytes = read_exact_descriptor(asset.descriptor(), length, budget).map_err(|source| {
        ActiveReblitBootSchemaInputsError::StoneRead {
            state: i32::from(state_id),
            source,
        }
    })?;
    let digest = xxhash_rust::xxh3::xxh3_128(&bytes);
    if digest != asset.digest() {
        return Err(ActiveReblitBootSchemaInputsError::StoneDigestMismatch {
            state: i32::from(state_id),
            expected: asset.digest(),
            actual: digest,
        });
    }
    let source = ActiveReblitBootSchemaSourceBinding::StoneOsInfo {
        binding_index: index,
        digest,
        length: asset.length(),
    };
    let text = match std::str::from_utf8(&bytes) {
        Ok(text) => text,
        Err(_) => {
            return Ok(LocalSchemaResolution::Unavailable(
                ActiveReblitBootSchemaFallbackReason::Semantic(ActiveReblitBootSchemaSemanticReason::NonUtf8),
            ));
        }
    };
    let schema = match parse_os_info(text) {
        Ok(schema) => schema,
        Err(reason) => {
            return Ok(LocalSchemaResolution::Unavailable(
                ActiveReblitBootSchemaFallbackReason::Semantic(reason),
            ));
        }
    };
    Ok(LocalSchemaResolution::Ready {
        schema,
        source,
        retained_generated: None,
    })
}

fn require_os_info_coordinate(
    state_id: state::Id,
    asset: &BoundActiveReblitBootAsset<'_>,
) -> Result<(), ActiveReblitBootSchemaInputsError> {
    if asset.state_id() == state_id
        && matches!(asset.role(), BootAssetRole::OsInfo)
        && asset.logical_path() == Path::new(OS_INFO_LOGICAL_PATH)
    {
        Ok(())
    } else {
        Err(ActiveReblitBootSchemaInputsError::InvalidStoneOsInfoCoordinate {
            state: i32::from(state_id),
        })
    }
}

fn revalidate_stone_source(
    stone: &PreparedActiveReblitStoneBootInputs,
    state_id: state::Id,
    binding_index: u16,
    digest: u128,
    length: u64,
    budget: &mut SchemaBudget,
) -> Result<(), ActiveReblitBootSchemaInputsError> {
    let asset =
        stone
            .asset_at(usize::from(binding_index))
            .ok_or(ActiveReblitBootSchemaInputsError::StoneSourceChanged {
                state: i32::from(state_id),
            })?;
    require_os_info_coordinate(state_id, &asset)?;
    if asset.digest() != digest || asset.length() != length {
        return Err(ActiveReblitBootSchemaInputsError::StoneSourceChanged {
            state: i32::from(state_id),
        });
    }
    let length = usize::try_from(length).map_err(|_| ActiveReblitBootSchemaInputsError::StoneSourceChanged {
        state: i32::from(state_id),
    })?;
    budget.admit_source_bytes(length)?;
    let bytes = read_exact_descriptor(asset.descriptor(), length, budget).map_err(|source| {
        ActiveReblitBootSchemaInputsError::StoneRead {
            state: i32::from(state_id),
            source,
        }
    })?;
    if xxhash_rust::xxh3::xxh3_128(&bytes) != digest {
        return Err(ActiveReblitBootSchemaInputsError::StoneSourceChanged {
            state: i32::from(state_id),
        });
    }
    Ok(())
}

#[derive(Debug, Error)]
pub(in crate::client) enum ActiveReblitBootSchemaInputsError {
    #[error("ActiveReblit schema projection contains {actual} states, limit {limit}")]
    StateCount { limit: usize, actual: usize },
    #[error("ActiveReblit schema head root mismatch: expected {expected}, found {actual:?}")]
    HeadRootMismatch { expected: i32, actual: Option<i32> },
    #[error("ActiveReblit boot schema plan has no required head requirement")]
    MissingGlobalRequirement,
    #[error("boot schema requirement for state {state} is outside the Stone projection")]
    RequirementOutsideProjection { state: i32 },
    #[error("duplicate boot schema requirement for state {state}")]
    DuplicateRequirement { state: i32 },
    #[error("boot schema requirement for state {state} is not in projection order")]
    RequirementOrder { state: i32 },
    #[error("historical boot schema requirement for state {state} is not explicit global fallback")]
    HistoryFallbackPolicy { state: i32 },
    #[error("eligible boot state root is outside the Stone projection")]
    EligibleRootOutsideProjection,
    #[error("eligible boot state root {state} is not in Stone projection order")]
    EligibleRootOrder { state: i32 },
    #[error("eligible boot state {state} has no retained state root")]
    MissingEligibleRoot { state: i32 },
    #[error("required boot schema for state {state} is unavailable: {reason:?}")]
    RequiredSchemaUnavailable {
        state: i32,
        reason: ActiveReblitBootSchemaFallbackReason,
    },
    #[error("historical state {state} requested global fallback before the head schema was authenticated")]
    GlobalFallbackBeforeHead { state: i32 },
    #[error("authenticated global boot schema for state {state} is missing")]
    MissingGlobalSchema { state: i32 },
    #[error("state {state} has no exact sealed Stone os-info coordinate")]
    MissingStoneOsInfo { state: i32 },
    #[error("state {state} has multiple sealed Stone os-info coordinates")]
    AmbiguousStoneOsInfo { state: i32 },
    #[error("state {state} has an invalid sealed Stone os-info coordinate")]
    InvalidStoneOsInfoCoordinate { state: i32 },
    #[error("Stone binding index {actual} exceeds {limit}")]
    BindingIndexLimit { limit: usize, actual: usize },
    #[error("read sealed Stone os-info for state {state}")]
    StoneRead {
        state: i32,
        #[source]
        source: io::Error,
    },
    #[error("sealed Stone os-info digest mismatch for state {state}: expected {expected:032x}, got {actual:032x}")]
    StoneDigestMismatch { state: i32, expected: u128, actual: u128 },
    #[error("sealed Stone os-info source changed for state {state}")]
    StoneSourceChanged { state: i32 },
    #[error("{operation} for generated boot schema state {state}")]
    GeneratedIo {
        state: i32,
        operation: &'static str,
        #[source]
        source: io::Error,
    },
    #[error("{operation} while revalidating generated boot schema")]
    GeneratedRevalidationIo {
        operation: &'static str,
        #[source]
        source: io::Error,
    },
    #[error("generated os-release source changed for state {state}")]
    GeneratedSourceChanged { state: i32 },
    #[error("prepared generated boot schema for state {state} lost its retained source")]
    MissingRetainedGeneratedSource { state: i32 },
    #[error("prepared global fallback for state {state} is internally inconsistent")]
    InvalidPreparedFallback { state: i32 },
    #[error("Stone boot schema requirements changed from the prepared projection")]
    RequirementProjectionChanged,
    #[error("Stone boot state projection changed from the prepared projection")]
    StateProjectionChanged,
    #[error("eligible boot state roots changed from the prepared projection")]
    EligibleStateProjectionChanged,
    #[error("boot schema source exceeds {limit} bytes (got {actual})")]
    SourceByteLimit { limit: usize, actual: usize },
    #[error("boot schema sources exceed {limit} total bytes (got {actual})")]
    TotalByteLimit { limit: usize, actual: usize },
    #[error("boot schema preparation exceeds {limit} work units (got {actual})")]
    WorkLimit { limit: usize, actual: usize },
    #[error("boot schema deadline cannot represent {timeout:?}")]
    InvalidDeadline { timeout: Duration },
    #[error("boot schema preparation exceeded its {timeout:?} deadline")]
    DeadlineExceeded { timeout: Duration },
}
#[cfg(test)]
#[path = "active_reblit_boot_schema_inputs_tests.rs"]
mod tests;
