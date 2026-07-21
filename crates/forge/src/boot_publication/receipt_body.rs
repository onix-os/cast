//! Authority-free canonical data carried by a boot-publication receipt.
//!
//! Every value in this module is inert owned data. Historical runtime witness
//! scalars describe the observation used when a receipt was prepared; they do
//! not authenticate the current mount namespace or grant filesystem access.
//! Likewise, an output provenance claim records an assertion and never grants
//! write, replacement, removal, or deletion authority.

use std::{cmp::Ordering, collections::BTreeMap, fmt};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use thiserror::Error;

use super::BootPublicationReceiptFingerprint;
use crate::state::TransitionId;

pub(crate) const BOOT_PUBLICATION_RECEIPT_FORMAT: &str = "cast-boot-publication-receipt";
pub(crate) const BOOT_PUBLICATION_RECEIPT_BODY_VERSION: u16 = 1;
pub(crate) const MAX_BOOT_PUBLICATION_RECEIPT_OUTPUTS: usize = 8_336;
pub(crate) const MAX_BOOT_PUBLICATION_RECEIPT_PATH_BYTES: usize = 8 * 1024 * 1024;
pub(crate) const MAX_BOOT_PUBLICATION_RECEIPT_SINGLE_PATH_BYTES: usize = nix::libc::PATH_MAX as usize - 1;
const MAX_BOOT_PUBLICATION_RECEIPT_PATH_COMPONENTS: usize = 16;
const MAX_BOOT_PUBLICATION_RECEIPT_FAT_COMPONENT_BYTES: usize = 255;
const MAX_BOOT_PUBLICATION_RECEIPT_LOGICAL_BYTES: u64 = 10 * 1024 * 1024 * 1024;
const CANONICAL_ACTIVE_REBLIT_OUTPUT_MODE: u32 = 0o644;

/// One exact SHA-256 value with one canonical lowercase text representation.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct BootPublicationSha256([u8; 32]);

impl BootPublicationSha256 {
    pub(crate) const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub(crate) const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl Serialize for BootPublicationSha256 {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&encode_lower_hex(&self.0))
    }
}

impl<'de> Deserialize<'de> for BootPublicationSha256 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserialize_lower_hex(deserializer, "exactly 64 lowercase hexadecimal characters").map(Self)
    }
}

/// Exact XXH3-128 output, encoded as a fixed-width lowercase hexadecimal value.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct BootPublicationXxh3([u8; 16]);

impl BootPublicationXxh3 {
    pub(crate) const fn from_u128(value: u128) -> Self {
        Self(value.to_be_bytes())
    }

    pub(crate) const fn as_u128(self) -> u128 {
        u128::from_be_bytes(self.0)
    }
}

impl Serialize for BootPublicationXxh3 {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&encode_lower_hex(&self.0))
    }
}

impl<'de> Deserialize<'de> for BootPublicationXxh3 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserialize_lower_hex(deserializer, "exactly 32 lowercase hexadecimal characters").map(Self)
    }
}

/// Scalars captured from one authenticated runtime observation in the past.
///
/// These numbers are witnesses in the receipt only. A consumer must perform a
/// fresh descriptor-retained observation before it trusts a live destination.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BootPublicationHistoricalRuntimeWitness {
    destination_device: u64,
    destination_inode: u64,
    mount_id: u64,
    partition_device_major: u32,
    partition_device_minor: u32,
    disk_sequence: Option<u64>,
}

impl BootPublicationHistoricalRuntimeWitness {
    pub(crate) const fn new(
        destination_device: u64,
        destination_inode: u64,
        mount_id: u64,
        partition_device_major: u32,
        partition_device_minor: u32,
        disk_sequence: Option<u64>,
    ) -> Self {
        Self {
            destination_device,
            destination_inode,
            mount_id,
            partition_device_major,
            partition_device_minor,
            disk_sequence,
        }
    }

    pub(crate) const fn destination_device(self) -> u64 {
        self.destination_device
    }

    pub(crate) const fn destination_inode(self) -> u64 {
        self.destination_inode
    }

    pub(crate) const fn mount_id(self) -> u64 {
        self.mount_id
    }

    pub(crate) const fn partition_device_major(self) -> u32 {
        self.partition_device_major
    }

    pub(crate) const fn partition_device_minor(self) -> u32 {
        self.partition_device_minor
    }

    pub(crate) const fn disk_sequence(self) -> Option<u64> {
        self.disk_sequence
    }

    fn validate(self, destination: &'static str) -> Result<(), BootPublicationReceiptBodyError> {
        if self.destination_device == 0
            || self.destination_inode == 0
            || self.mount_id == 0
            || (self.partition_device_major == 0 && self.partition_device_minor == 0)
            || self.disk_sequence == Some(0)
        {
            return Err(BootPublicationReceiptBodyError::InvalidHistoricalRuntimeWitness { destination });
        }
        let raw_device: nix::libc::dev_t = self
            .destination_device
            .try_into()
            .map_err(|_| BootPublicationReceiptBodyError::InvalidHistoricalDestinationDevice { destination })?;
        let device_major = nix::libc::major(raw_device);
        let device_minor = nix::libc::minor(raw_device);
        if nix::libc::makedev(device_major, device_minor) != raw_device {
            return Err(BootPublicationReceiptBodyError::InvalidHistoricalDestinationDevice { destination });
        }
        if device_major != self.partition_device_major || device_minor != self.partition_device_minor {
            return Err(BootPublicationReceiptBodyError::HistoricalPartitionDeviceMismatch { destination });
        }
        Ok(())
    }
}

/// Stable partition identity plus explicitly historical runtime evidence.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BootPublicationDestination {
    partuuid: Box<str>,
    partition_number: u32,
    historical_runtime_witness: BootPublicationHistoricalRuntimeWitness,
}

impl BootPublicationDestination {
    pub(crate) fn new(
        partuuid: impl Into<Box<str>>,
        partition_number: u32,
        historical_runtime_witness: BootPublicationHistoricalRuntimeWitness,
    ) -> Self {
        Self {
            partuuid: partuuid.into(),
            partition_number,
            historical_runtime_witness,
        }
    }

    pub(crate) fn partuuid(&self) -> &str {
        &self.partuuid
    }

    pub(crate) const fn partition_number(&self) -> u32 {
        self.partition_number
    }

    pub(crate) const fn historical_runtime_witness(&self) -> BootPublicationHistoricalRuntimeWitness {
        self.historical_runtime_witness
    }

    fn validate(&self, destination: &'static str) -> Result<(), BootPublicationReceiptBodyError> {
        if !is_canonical_nonzero_uuid(&self.partuuid) {
            return Err(BootPublicationReceiptBodyError::InvalidPartuuid { destination });
        }
        if self.partition_number == 0 {
            return Err(BootPublicationReceiptBodyError::InvalidPartitionNumber { destination });
        }
        self.historical_runtime_witness.validate(destination)
    }
}

/// Exact physical destination shape captured for the receipt.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "layout", rename_all = "kebab-case", deny_unknown_fields)]
pub(crate) enum BootPublicationDestinations {
    BootAliasesEsp {
        esp: BootPublicationDestination,
    },
    DistinctXbootldr {
        esp: BootPublicationDestination,
        xbootldr: BootPublicationDestination,
    },
}

impl BootPublicationDestinations {
    pub(crate) const fn boot_aliases_esp(esp: BootPublicationDestination) -> Self {
        Self::BootAliasesEsp { esp }
    }

    pub(crate) const fn distinct_xbootldr(
        esp: BootPublicationDestination,
        xbootldr: BootPublicationDestination,
    ) -> Self {
        Self::DistinctXbootldr { esp, xbootldr }
    }

    pub(crate) const fn esp(&self) -> &BootPublicationDestination {
        match self {
            Self::BootAliasesEsp { esp } | Self::DistinctXbootldr { esp, .. } => esp,
        }
    }

    pub(crate) const fn xbootldr(&self) -> Option<&BootPublicationDestination> {
        match self {
            Self::BootAliasesEsp { .. } => None,
            Self::DistinctXbootldr { xbootldr, .. } => Some(xbootldr),
        }
    }

    pub(crate) const fn aliases_esp(&self) -> bool {
        matches!(self, Self::BootAliasesEsp { .. })
    }

    fn validate(&self) -> Result<(), BootPublicationReceiptBodyError> {
        self.esp().validate("esp")?;
        let Some(xbootldr) = self.xbootldr() else {
            return Ok(());
        };
        xbootldr.validate("xbootldr")?;
        let esp = self.esp();
        if esp.partuuid == xbootldr.partuuid {
            return Err(BootPublicationReceiptBodyError::DistinctPartuuidCollision);
        }
        if esp.historical_runtime_witness.partition_device_major
            == xbootldr.historical_runtime_witness.partition_device_major
            && esp.historical_runtime_witness.partition_device_minor
                == xbootldr.historical_runtime_witness.partition_device_minor
        {
            return Err(BootPublicationReceiptBodyError::DistinctPartitionIdentityCollision);
        }
        if esp.historical_runtime_witness.disk_sequence != xbootldr.historical_runtime_witness.disk_sequence {
            return Err(BootPublicationReceiptBodyError::DistinctDiskSequenceMismatch);
        }
        if esp.historical_runtime_witness.destination_device
            == xbootldr.historical_runtime_witness.destination_device
            && esp.historical_runtime_witness.destination_inode
                == xbootldr.historical_runtime_witness.destination_inode
        {
            return Err(BootPublicationReceiptBodyError::DistinctRuntimeDestinationCollision);
        }
        if esp.historical_runtime_witness.mount_id == xbootldr.historical_runtime_witness.mount_id {
            return Err(BootPublicationReceiptBodyError::DistinctRuntimeMountCollision);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum BootPublicationRoot {
    Esp,
    Boot,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum BootPublicationPublicationPhase {
    Payload,
    Entry,
    LoaderControl,
    Bootloader,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum BootPublicationOutputRole {
    Payload,
    Entry,
    LoaderControl,
    FallbackBootloader,
    SystemdBootloader,
}

/// Data-only claim about what was observed or asserted for one output.
///
/// In particular, `ClaimedPublishedByCast` is not proof of that assertion and
/// cannot be converted into deletion authority. Authentication belongs to the
/// durable chain and a fresh descriptor-retained publisher/reconciler.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum BootPublicationOutputProvenanceClaim {
    BorrowedFirstAdoption,
    UnclaimedAbsent,
    ClaimedPublishedByCast,
}

/// One complete output fact in canonical inventory order.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BootPublicationOutput {
    root: BootPublicationRoot,
    phase: BootPublicationPublicationPhase,
    role: BootPublicationOutputRole,
    relative_path: Box<str>,
    mode: u32,
    xxh3: BootPublicationXxh3,
    length: u64,
    content_sha256: BootPublicationSha256,
    provenance_claim: BootPublicationOutputProvenanceClaim,
}

impl BootPublicationOutput {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        root: BootPublicationRoot,
        phase: BootPublicationPublicationPhase,
        role: BootPublicationOutputRole,
        relative_path: impl Into<Box<str>>,
        mode: u32,
        xxh3: BootPublicationXxh3,
        length: u64,
        content_sha256: BootPublicationSha256,
        provenance_claim: BootPublicationOutputProvenanceClaim,
    ) -> Self {
        Self {
            root,
            phase,
            role,
            relative_path: relative_path.into(),
            mode,
            xxh3,
            length,
            content_sha256,
            provenance_claim,
        }
    }

    pub(crate) const fn root(&self) -> BootPublicationRoot {
        self.root
    }

    pub(crate) const fn phase(&self) -> BootPublicationPublicationPhase {
        self.phase
    }

    pub(crate) const fn role(&self) -> BootPublicationOutputRole {
        self.role
    }

    pub(crate) fn relative_path(&self) -> &str {
        &self.relative_path
    }

    pub(crate) const fn mode(&self) -> u32 {
        self.mode
    }

    pub(crate) const fn xxh3(&self) -> BootPublicationXxh3 {
        self.xxh3
    }

    pub(crate) const fn length(&self) -> u64 {
        self.length
    }

    pub(crate) const fn content_sha256(&self) -> BootPublicationSha256 {
        self.content_sha256
    }

    pub(crate) const fn provenance_claim(&self) -> BootPublicationOutputProvenanceClaim {
        self.provenance_claim
    }

    fn validate(&self, index: usize) -> Result<(), BootPublicationReceiptBodyError> {
        validate_path(index, &self.relative_path)?;
        if self.mode != CANONICAL_ACTIVE_REBLIT_OUTPUT_MODE {
            return Err(BootPublicationReceiptBodyError::NonCanonicalOutputMode {
                index,
                actual: self.mode,
            });
        }
        let (expected_root, expected_phase) = role_binding(self.role);
        if self.root != expected_root {
            return Err(BootPublicationReceiptBodyError::OutputRoleRootMismatch { index });
        }
        if self.phase != expected_phase {
            return Err(BootPublicationReceiptBodyError::OutputRolePhaseMismatch { index });
        }
        Ok(())
    }
}

/// Versioned, complete and authority-free receipt body.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BootPublicationReceiptBody {
    format: Box<str>,
    version: u16,
    transition_id: TransitionId,
    committed_predecessor: Option<BootPublicationReceiptFingerprint>,
    predecessor_journal_sha256: BootPublicationSha256,
    desired_inventory_sha256: BootPublicationSha256,
    destinations: BootPublicationDestinations,
    outputs: Vec<BootPublicationOutput>,
}

impl BootPublicationReceiptBody {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        transition_id: TransitionId,
        committed_predecessor: Option<BootPublicationReceiptFingerprint>,
        predecessor_journal_sha256: BootPublicationSha256,
        desired_inventory_sha256: BootPublicationSha256,
        destinations: BootPublicationDestinations,
        outputs: Vec<BootPublicationOutput>,
    ) -> Result<Self, BootPublicationReceiptBodyError> {
        let body = Self {
            format: BOOT_PUBLICATION_RECEIPT_FORMAT.into(),
            version: BOOT_PUBLICATION_RECEIPT_BODY_VERSION,
            transition_id,
            committed_predecessor,
            predecessor_journal_sha256,
            desired_inventory_sha256,
            destinations,
            outputs,
        };
        body.validate()?;
        Ok(body)
    }

    pub(crate) fn transition_id(&self) -> &TransitionId {
        &self.transition_id
    }

    pub(crate) const fn committed_predecessor(&self) -> Option<BootPublicationReceiptFingerprint> {
        self.committed_predecessor
    }

    pub(crate) const fn predecessor_journal_sha256(&self) -> BootPublicationSha256 {
        self.predecessor_journal_sha256
    }

    pub(crate) const fn desired_inventory_sha256(&self) -> BootPublicationSha256 {
        self.desired_inventory_sha256
    }

    pub(crate) const fn destinations(&self) -> &BootPublicationDestinations {
        &self.destinations
    }

    pub(crate) fn outputs(&self) -> &[BootPublicationOutput] {
        &self.outputs
    }

    pub(super) fn validate(&self) -> Result<(), BootPublicationReceiptBodyError> {
        if &*self.format != BOOT_PUBLICATION_RECEIPT_FORMAT {
            return Err(BootPublicationReceiptBodyError::UnsupportedFormat(self.format.clone()));
        }
        if self.version != BOOT_PUBLICATION_RECEIPT_BODY_VERSION {
            return Err(BootPublicationReceiptBodyError::UnsupportedVersion(self.version));
        }
        self.destinations.validate()?;
        if self.outputs.is_empty() {
            return Err(BootPublicationReceiptBodyError::EmptyOutputInventory);
        }
        if self.outputs.len() > MAX_BOOT_PUBLICATION_RECEIPT_OUTPUTS {
            return Err(BootPublicationReceiptBodyError::OutputCountLimit {
                actual: self.outputs.len(),
            });
        }

        let mut path_bytes = 0usize;
        let mut logical_bytes = 0u64;
        let mut destinations = BTreeMap::<(CollisionDomain, String), (usize, &str)>::new();
        let mut previous = None;
        for (index, output) in self.outputs.iter().enumerate() {
            output.validate(index)?;
            path_bytes = path_bytes
                .checked_add(output.relative_path.len())
                .ok_or(BootPublicationReceiptBodyError::AggregatePathByteLimit { actual: usize::MAX })?;
            if path_bytes > MAX_BOOT_PUBLICATION_RECEIPT_PATH_BYTES {
                return Err(BootPublicationReceiptBodyError::AggregatePathByteLimit { actual: path_bytes });
            }
            logical_bytes = logical_bytes
                .checked_add(output.length)
                .ok_or(BootPublicationReceiptBodyError::LogicalByteLimit { actual: u64::MAX })?;
            if logical_bytes > MAX_BOOT_PUBLICATION_RECEIPT_LOGICAL_BYTES {
                return Err(BootPublicationReceiptBodyError::LogicalByteLimit { actual: logical_bytes });
            }
            admit_collision_path(&mut destinations, self.destinations.aliases_esp(), index, output)?;
            if let Some(previous) = previous
                && compare_outputs(previous, output) != Ordering::Less
            {
                return Err(BootPublicationReceiptBodyError::NonCanonicalOutputOrder { index });
            }
            previous = Some(output);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub(crate) enum BootPublicationReceiptBodyError {
    #[error("unsupported boot-publication receipt format `{0}`")]
    UnsupportedFormat(Box<str>),
    #[error("unsupported boot-publication receipt body version {0}")]
    UnsupportedVersion(u16),
    #[error("{destination} PARTUUID is not one nonzero canonical lowercase UUID")]
    InvalidPartuuid { destination: &'static str },
    #[error("{destination} partition number must be nonzero")]
    InvalidPartitionNumber { destination: &'static str },
    #[error("{destination} historical runtime witness contains an invalid zero scalar")]
    InvalidHistoricalRuntimeWitness { destination: &'static str },
    #[error("{destination} historical destination device is not one canonical Linux dev_t")]
    InvalidHistoricalDestinationDevice { destination: &'static str },
    #[error("{destination} historical destination device does not match its partition device identity")]
    HistoricalPartitionDeviceMismatch { destination: &'static str },
    #[error("distinct ESP and XBOOTLDR PARTUUIDs alias")]
    DistinctPartuuidCollision,
    #[error("distinct ESP and XBOOTLDR partition identities alias")]
    DistinctPartitionIdentityCollision,
    #[error("distinct ESP and XBOOTLDR historical disk-sequence witnesses differ")]
    DistinctDiskSequenceMismatch,
    #[error("distinct ESP and XBOOTLDR historical destination witnesses alias")]
    DistinctRuntimeDestinationCollision,
    #[error("distinct ESP and XBOOTLDR historical mount IDs alias")]
    DistinctRuntimeMountCollision,
    #[error("a complete boot-publication receipt must contain at least one output")]
    EmptyOutputInventory,
    #[error("boot-publication output count {actual} exceeds limit {MAX_BOOT_PUBLICATION_RECEIPT_OUTPUTS}")]
    OutputCountLimit { actual: usize },
    #[error("boot-publication output path {index} is unsafe: {reason}")]
    UnsafeOutputPath {
        index: usize,
        reason: BootPublicationPathError,
    },
    #[error("boot-publication output {index} has a root inconsistent with its role")]
    OutputRoleRootMismatch { index: usize },
    #[error("boot-publication output {index} has a phase inconsistent with its role")]
    OutputRolePhaseMismatch { index: usize },
    #[error("boot-publication output {index} has noncanonical mode {actual}; ActiveReblit outputs require mode 0644")]
    NonCanonicalOutputMode { index: usize, actual: u32 },
    #[error("boot-publication aggregate path bytes {actual} exceed limit {MAX_BOOT_PUBLICATION_RECEIPT_PATH_BYTES}")]
    AggregatePathByteLimit { actual: usize },
    #[error("boot-publication logical bytes {actual} exceed the canonical receipt limit")]
    LogicalByteLimit { actual: u64 },
    #[error("boot-publication output {index} is not in strict canonical order")]
    NonCanonicalOutputOrder { index: usize },
    #[error("boot-publication outputs {first} and {second} collide in one FAT destination namespace")]
    OutputPathCollision { first: usize, second: usize },
    #[error("boot-publication outputs {ancestor} and {descendant} have a file/directory hierarchy collision")]
    OutputHierarchyCollision { ancestor: usize, descendant: usize },
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub(crate) enum BootPublicationPathError {
    #[error("path is empty")]
    Empty,
    #[error("path is absolute")]
    Absolute,
    #[error("path has too many bytes")]
    ByteLimit,
    #[error("path has too many components")]
    ComponentLimit,
    #[error("path contains an empty component")]
    EmptyComponent,
    #[error("path contains a dot component")]
    DotComponent,
    #[error("path contains a parent component")]
    ParentComponent,
    #[error("path contains non-ASCII or control data")]
    NonCanonicalText,
    #[error("path contains a FAT-forbidden character")]
    FatForbiddenCharacter,
    #[error("path contains a FAT component that is too long")]
    FatComponentByteLimit,
    #[error("path contains a component ending in a dot or space")]
    FatTrailingDotOrSpace,
    #[error("path contains a FAT short-name marker")]
    FatShortNameMarker,
    #[error("path contains a DOS-reserved component")]
    FatReservedName,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum CollisionDomain {
    Shared,
    Esp,
    Boot,
}

fn admit_collision_path<'output>(
    destinations: &mut BTreeMap<(CollisionDomain, String), (usize, &'output str)>,
    aliases_esp: bool,
    index: usize,
    output: &'output BootPublicationOutput,
) -> Result<(), BootPublicationReceiptBodyError> {
    let domain = match (aliases_esp, output.root) {
        (true, _) => CollisionDomain::Shared,
        (false, BootPublicationRoot::Esp) => CollisionDomain::Esp,
        (false, BootPublicationRoot::Boot) => CollisionDomain::Boot,
    };
    let folded = output.relative_path.to_ascii_lowercase();
    if let Some((first, _)) = destinations.get(&(domain, folded.clone())) {
        return Err(BootPublicationReceiptBodyError::OutputPathCollision {
            first: *first,
            second: index,
        });
    }
    let mut ancestor = folded.as_str();
    while let Some(separator) = ancestor.rfind('/') {
        ancestor = &ancestor[..separator];
        if let Some((first, _)) = destinations.get(&(domain, ancestor.to_owned())) {
            return Err(BootPublicationReceiptBodyError::OutputHierarchyCollision {
                ancestor: *first,
                descendant: index,
            });
        }
    }
    let mut prefix = folded.clone();
    prefix.push('/');
    if let Some((_, (descendant, _))) = destinations
        .range((domain, prefix.clone())..)
        .next()
        .filter(|((candidate_domain, candidate), _)| *candidate_domain == domain && candidate.starts_with(&prefix))
    {
        return Err(BootPublicationReceiptBodyError::OutputHierarchyCollision {
            ancestor: index,
            descendant: *descendant,
        });
    }
    destinations.insert((domain, folded), (index, &output.relative_path));
    Ok(())
}

fn compare_outputs(left: &BootPublicationOutput, right: &BootPublicationOutput) -> Ordering {
    left.phase
        .cmp(&right.phase)
        .then_with(|| left.root.cmp(&right.root))
        .then_with(|| ascii_fold_cmp(left.relative_path.as_bytes(), right.relative_path.as_bytes()))
        .then_with(|| left.relative_path.as_bytes().cmp(right.relative_path.as_bytes()))
}

fn ascii_fold_cmp(left: &[u8], right: &[u8]) -> Ordering {
    left.iter()
        .map(u8::to_ascii_lowercase)
        .cmp(right.iter().map(u8::to_ascii_lowercase))
}

fn role_binding(role: BootPublicationOutputRole) -> (BootPublicationRoot, BootPublicationPublicationPhase) {
    match role {
        BootPublicationOutputRole::Payload => (BootPublicationRoot::Boot, BootPublicationPublicationPhase::Payload),
        BootPublicationOutputRole::Entry => (BootPublicationRoot::Boot, BootPublicationPublicationPhase::Entry),
        BootPublicationOutputRole::LoaderControl => {
            (BootPublicationRoot::Boot, BootPublicationPublicationPhase::LoaderControl)
        }
        BootPublicationOutputRole::FallbackBootloader | BootPublicationOutputRole::SystemdBootloader => {
            (BootPublicationRoot::Esp, BootPublicationPublicationPhase::Bootloader)
        }
    }
}

fn validate_path(index: usize, path: &str) -> Result<(), BootPublicationReceiptBodyError> {
    let fail = |reason| BootPublicationReceiptBodyError::UnsafeOutputPath { index, reason };
    if path.is_empty() {
        return Err(fail(BootPublicationPathError::Empty));
    }
    if path.starts_with('/') {
        return Err(fail(BootPublicationPathError::Absolute));
    }
    if path.len() > MAX_BOOT_PUBLICATION_RECEIPT_SINGLE_PATH_BYTES {
        return Err(fail(BootPublicationPathError::ByteLimit));
    }
    let mut components = 0usize;
    for component in path.split('/') {
        components = components.saturating_add(1);
        if components > MAX_BOOT_PUBLICATION_RECEIPT_PATH_COMPONENTS {
            return Err(fail(BootPublicationPathError::ComponentLimit));
        }
        if component.is_empty() {
            return Err(fail(BootPublicationPathError::EmptyComponent));
        }
        if component == "." {
            return Err(fail(BootPublicationPathError::DotComponent));
        }
        if component == ".." {
            return Err(fail(BootPublicationPathError::ParentComponent));
        }
        if !component.is_ascii() || component.chars().any(char::is_control) {
            return Err(fail(BootPublicationPathError::NonCanonicalText));
        }
        if component.len() > MAX_BOOT_PUBLICATION_RECEIPT_FAT_COMPONENT_BYTES {
            return Err(fail(BootPublicationPathError::FatComponentByteLimit));
        }
        if component
            .bytes()
            .any(|byte| matches!(byte, b'<' | b'>' | b':' | b'"' | b'\\' | b'|' | b'?' | b'*'))
        {
            return Err(fail(BootPublicationPathError::FatForbiddenCharacter));
        }
        if component.ends_with('.') || component.ends_with(' ') {
            return Err(fail(BootPublicationPathError::FatTrailingDotOrSpace));
        }
        if component.contains('~') {
            return Err(fail(BootPublicationPathError::FatShortNameMarker));
        }
        if is_dos_reserved_component(component) {
            return Err(fail(BootPublicationPathError::FatReservedName));
        }
    }
    Ok(())
}

fn is_dos_reserved_component(component: &str) -> bool {
    let stem = component
        .split('.')
        .next()
        .unwrap_or(component)
        .trim_end_matches([' ', '.'])
        .to_ascii_uppercase();
    if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL") {
        return true;
    }
    let bytes = stem.as_bytes();
    bytes.len() == 4 && (&bytes[..3] == b"COM" || &bytes[..3] == b"LPT") && matches!(bytes[3], b'1'..=b'9')
}

fn is_canonical_nonzero_uuid(value: &str) -> bool {
    if value.len() != 36 {
        return false;
    }
    let bytes = value.as_bytes();
    let mut nonzero = false;
    for (index, byte) in bytes.iter().copied().enumerate() {
        if matches!(index, 8 | 13 | 18 | 23) {
            if byte != b'-' {
                return false;
            }
        } else if !matches!(byte, b'0'..=b'9' | b'a'..=b'f') {
            return false;
        } else if byte != b'0' {
            nonzero = true;
        }
    }
    nonzero
}

fn encode_lower_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes.iter().copied() {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

fn deserialize_lower_hex<'de, D, const N: usize>(
    deserializer: D,
    expected: &'static str,
) -> Result<[u8; N], D::Error>
where
    D: Deserializer<'de>,
{
    struct LowerHexVisitor<const N: usize> {
        expected: &'static str,
    }

    impl<'de, const N: usize> de::Visitor<'de> for LowerHexVisitor<N> {
        type Value = [u8; N];

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str(self.expected)
        }

        fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            let encoded = value.as_bytes();
            if encoded.len() != N * 2 {
                return Err(E::invalid_value(de::Unexpected::Str(value), &self));
            }
            let mut bytes = [0_u8; N];
            for (index, pair) in encoded.chunks_exact(2).enumerate() {
                let Some(high) = decode_nibble(pair[0]) else {
                    return Err(E::invalid_value(de::Unexpected::Str(value), &self));
                };
                let Some(low) = decode_nibble(pair[1]) else {
                    return Err(E::invalid_value(de::Unexpected::Str(value), &self));
                };
                bytes[index] = (high << 4) | low;
            }
            Ok(bytes)
        }
    }

    deserializer.deserialize_str(LowerHexVisitor::<N> { expected })
}

const fn decode_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

#[cfg(test)]
#[path = "receipt_body_tests.rs"]
mod tests;
