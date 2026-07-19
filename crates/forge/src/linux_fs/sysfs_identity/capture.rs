use std::{ffi::CString, io};

use super::{
    super::sysfs_block::{
        SYSFS_DEV_ATTRIBUTE_MAX_BYTES, SYSFS_LINK_TARGET_MAX_BYTES, SYSFS_PARTITION_ATTRIBUTE_MAX_BYTES,
        SYSFS_PARTITION_GEOMETRY_ATTRIBUTE_MAX_BYTES, SYSFS_UEVENT_MAX_BYTES, SysfsBlockDeviceName, SysfsDeviceNumber,
        SysfsDiskSequence, SysfsPartitionGeometry, SysfsPartitionNumber, SysfsPartitionUuid,
        normalize_sysfs_dev_block_target_until, parse_sysfs_disk_identity_until, parse_sysfs_partition_geometry_until,
        parse_sysfs_partition_identity_until, parse_sysfs_subsystem_target_until, require_matching_disk_sequence_until,
    },
    filesystem::{
        AttributeEvidence, CaptureAttribute, CaptureCheckpoint, CaptureNode, FileWitness, Operation, RootHandle,
        attribute_absent, open_directory, pin_symlink, read_attribute, read_pinned_symlink, require_same, witness,
    },
};

// The pure parsers maintain their own exact internal ledgers. Reserve their
// complete fixed ceilings in the enclosing descriptor operation before entry
// so `Operation::max_work` remains one conservative global upper bound rather
// than ignoring nested parser work.
const LINK_PARSER_WORK_RESERVATION: usize = 64 * 1024;
const IDENTITY_PARSER_WORK_RESERVATION: usize = 3 * 1024 * 1024;
const GEOMETRY_PARSER_WORK_RESERVATION: usize = 4 * 1024;

#[derive(PartialEq, Eq)]
struct SubsystemEvidence {
    witness: FileWitness,
    target: Vec<u8>,
    name: Vec<u8>,
}

#[derive(PartialEq, Eq)]
enum AncestorEvidence {
    Missing,
    NonBlock(SubsystemEvidence),
    Parent(SubsystemEvidence),
}

#[derive(PartialEq, Eq)]
struct Snapshot {
    root: FileWitness,
    lookup: FileWitness,
    target_components: Vec<Vec<u8>>,
    target_directory_witnesses: Vec<FileWitness>,
    normalized_devpath: Vec<u8>,
    partition_witness: FileWitness,
    parent_witness: FileWitness,
    parent_component_count: usize,
    partition_dev: AttributeEvidence,
    partition_number_attribute: AttributeEvidence,
    partition_start_attribute: AttributeEvidence,
    partition_size_attribute: AttributeEvidence,
    partition_uevent: AttributeEvidence,
    partition_subsystem: SubsystemEvidence,
    parent_dev: AttributeEvidence,
    parent_uevent: AttributeEvidence,
    ancestors: Vec<AncestorEvidence>,
    device: SysfsDeviceNumber,
    partition_number: SysfsPartitionNumber,
    partition_uuid: SysfsPartitionUuid,
    disk_sequence: Option<SysfsDiskSequence>,
    parent_device: SysfsDeviceNumber,
    partition_device_name: Vec<u8>,
    parent_device_name: Vec<u8>,
    partition_geometry: SysfsPartitionGeometry,
}

pub(super) struct Capture {
    snapshot: Snapshot,
    directories: Vec<PinnedDirectory>,
}

struct PinnedDirectory {
    file: std::fs::File,
    witness: FileWitness,
}

impl Capture {
    pub(super) const fn device(&self) -> SysfsDeviceNumber {
        self.snapshot.device
    }

    pub(super) const fn partition_number(&self) -> SysfsPartitionNumber {
        self.snapshot.partition_number
    }

    pub(super) const fn partition_uuid(&self) -> SysfsPartitionUuid {
        self.snapshot.partition_uuid
    }

    pub(super) const fn disk_sequence(&self) -> Option<SysfsDiskSequence> {
        self.snapshot.disk_sequence
    }

    pub(super) const fn parent_device(&self) -> SysfsDeviceNumber {
        self.snapshot.parent_device
    }

    pub(super) fn normalized_devpath(&self) -> &[u8] {
        &self.snapshot.normalized_devpath
    }

    pub(super) fn partition_device_name(&self) -> &[u8] {
        &self.snapshot.partition_device_name
    }

    pub(super) fn parent_device_name(&self) -> &[u8] {
        &self.snapshot.parent_device_name
    }

    pub(super) const fn partition_start_512_sectors(&self) -> u64 {
        self.snapshot.partition_geometry.start_512_sectors()
    }

    pub(super) const fn partition_size_512_sectors(&self) -> u64 {
        self.snapshot.partition_geometry.size_512_sectors()
    }

    pub(super) fn require_retained(&self, root: &RootHandle, operation: &mut Operation<'_>) -> io::Result<()> {
        require_same(
            root.witness(),
            witness(root.file(), operation, "retained authenticated sysfs root")?,
            "retained authenticated sysfs root",
        )?;
        if self.directories.len() != self.snapshot.target_directory_witnesses.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "retained sysfs descriptor chain changed length",
            ));
        }
        for (index, (directory, expected)) in self
            .directories
            .iter()
            .zip(&self.snapshot.target_directory_witnesses)
            .enumerate()
        {
            operation.charge(1, "revalidating retained sysfs prefix descriptor")?;
            require_same(
                *expected,
                witness(&directory.file, operation, "retained sysfs prefix directory")?,
                "retained sysfs prefix directory",
            )?;
            if directory.witness != *expected {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("retained sysfs prefix descriptor {index} changed its captured witness"),
                ));
            }
        }
        Ok(())
    }

    pub(super) fn require_terminal_names(&self, root: &RootHandle, operation: &mut Operation<'_>) -> io::Result<()> {
        operation.emit(CaptureCheckpoint::TerminalRebind)?;
        let (lookup, lookup_witness) = pin_lookup(root, self.snapshot.device, operation)?;
        require_same(self.snapshot.lookup, lookup_witness, "terminal sysfs lookup rebind")?;
        let target = read_pinned_symlink(
            &lookup,
            SYSFS_LINK_TARGET_MAX_BYTES,
            operation,
            "terminal sysfs lookup link",
        )?;
        operation.charge(
            LINK_PARSER_WORK_RESERVATION,
            "reserving terminal sysfs lookup parser work",
        )?;
        let parsed = normalize_sysfs_dev_block_target_until(&target, operation.deadline())?;
        let target_components = copy_components(parsed.components(), operation)?;
        if target_components != self.snapshot.target_components {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "terminal sysfs lookup target changed",
            ));
        }
        let directories = open_target_chain(root, &target_components, operation)?;
        if directories.len() != self.snapshot.target_directory_witnesses.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "terminal sysfs prefix chain changed length",
            ));
        }
        for (directory, expected) in directories.iter().zip(&self.snapshot.target_directory_witnesses) {
            require_same(*expected, directory.witness, "terminal sysfs prefix name rebind")?;
        }
        let terminal = directories.last().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "normalized sysfs target has no terminal component",
            )
        })?;
        require_same(
            self.snapshot.partition_witness,
            terminal.witness,
            "terminal partition name rebind",
        )?;
        let parent_index = self
            .snapshot
            .parent_component_count
            .checked_sub(1)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "sysfs parent depth underflowed"))?;
        let named_parent = directories.get(parent_index).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "sysfs parent depth exceeds normalized target",
            )
        })?;
        require_same(
            self.snapshot.parent_witness,
            named_parent.witness,
            "terminal block-parent name rebind",
        )?;

        let partition_subsystem = require_block_subsystem(root, &terminal.file, 0, operation)?;
        require_exact_evidence(
            &self.snapshot.partition_subsystem,
            &partition_subsystem,
            "terminal partition subsystem rebind",
        )?;
        let partition_dev = read_attribute(
            root,
            &terminal.file,
            c"dev",
            SYSFS_DEV_ATTRIBUTE_MAX_BYTES,
            CaptureNode::Partition,
            CaptureAttribute::Dev,
            operation,
        )?;
        require_exact_evidence(
            &self.snapshot.partition_dev,
            &partition_dev,
            "terminal partition dev rebind",
        )?;
        let partition_number = read_attribute(
            root,
            &terminal.file,
            c"partition",
            SYSFS_PARTITION_ATTRIBUTE_MAX_BYTES,
            CaptureNode::Partition,
            CaptureAttribute::Partition,
            operation,
        )?;
        require_exact_evidence(
            &self.snapshot.partition_number_attribute,
            &partition_number,
            "terminal partition number rebind",
        )?;
        let partition_start = read_attribute(
            root,
            &terminal.file,
            c"start",
            SYSFS_PARTITION_GEOMETRY_ATTRIBUTE_MAX_BYTES,
            CaptureNode::Partition,
            CaptureAttribute::Start,
            operation,
        )?;
        require_exact_evidence(
            &self.snapshot.partition_start_attribute,
            &partition_start,
            "terminal partition start rebind",
        )?;
        let partition_size = read_attribute(
            root,
            &terminal.file,
            c"size",
            SYSFS_PARTITION_GEOMETRY_ATTRIBUTE_MAX_BYTES,
            CaptureNode::Partition,
            CaptureAttribute::Size,
            operation,
        )?;
        require_exact_evidence(
            &self.snapshot.partition_size_attribute,
            &partition_size,
            "terminal partition size rebind",
        )?;
        let partition_uevent = read_attribute(
            root,
            &terminal.file,
            c"uevent",
            SYSFS_UEVENT_MAX_BYTES,
            CaptureNode::Partition,
            CaptureAttribute::Uevent,
            operation,
        )?;
        require_exact_evidence(
            &self.snapshot.partition_uevent,
            &partition_uevent,
            "terminal partition uevent rebind",
        )?;

        let terminal_ancestors = capture_terminal_ancestors(root, &directories, parent_index, operation)?;
        require_exact_evidence(
            &self.snapshot.ancestors,
            &terminal_ancestors,
            "terminal closer-ancestor classification rebind",
        )?;
        attribute_absent(&named_parent.file, c"partition", operation)?;
        let parent_dev = read_attribute(
            root,
            &named_parent.file,
            c"dev",
            SYSFS_DEV_ATTRIBUTE_MAX_BYTES,
            CaptureNode::Parent,
            CaptureAttribute::Dev,
            operation,
        )?;
        require_exact_evidence(&self.snapshot.parent_dev, &parent_dev, "terminal parent dev rebind")?;
        let parent_uevent = read_attribute(
            root,
            &named_parent.file,
            c"uevent",
            SYSFS_UEVENT_MAX_BYTES,
            CaptureNode::Parent,
            CaptureAttribute::Uevent,
            operation,
        )?;
        require_exact_evidence(
            &self.snapshot.parent_uevent,
            &parent_uevent,
            "terminal parent uevent rebind",
        )?;
        attribute_absent(&named_parent.file, c"partition", operation)?;
        self.require_retained(root, operation)?;
        root.require_named(operation)?;
        self.require_final_name_rebind(root, operation)
    }

    fn require_final_name_rebind(&self, root: &RootHandle, operation: &mut Operation<'_>) -> io::Result<()> {
        operation.emit(CaptureCheckpoint::FinalNameRebind)?;
        root.require_named(operation)?;
        self.require_lookup_chain_rebind(root, operation)?;
        // A root-name check after the first chain closes a concurrent root
        // substitution window. Repeating the complete chain afterwards makes
        // the child-name proof, rather than an unrelated hook/checkpoint, the
        // final public-name observation before retained-descriptor checks.
        root.require_named(operation)?;
        self.require_lookup_chain_rebind(root, operation)?;
        self.require_retained(root, operation)
    }

    fn require_lookup_chain_rebind(&self, root: &RootHandle, operation: &mut Operation<'_>) -> io::Result<()> {
        let (lookup, lookup_witness) = pin_lookup(root, self.snapshot.device, operation)?;
        require_same(self.snapshot.lookup, lookup_witness, "final sysfs lookup name rebind")?;
        let target = read_pinned_symlink(
            &lookup,
            SYSFS_LINK_TARGET_MAX_BYTES,
            operation,
            "final sysfs lookup target rebind",
        )?;
        operation.charge(LINK_PARSER_WORK_RESERVATION, "reserving final sysfs lookup parser work")?;
        let parsed = normalize_sysfs_dev_block_target_until(&target, operation.deadline())?;
        let components = copy_components(parsed.components(), operation)?;
        if components != self.snapshot.target_components {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "final sysfs lookup target changed",
            ));
        }
        let directories = open_target_chain(root, &components, operation)?;
        if directories.len() != self.snapshot.target_directory_witnesses.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "final sysfs prefix chain changed length",
            ));
        }
        for (directory, expected) in directories.iter().zip(&self.snapshot.target_directory_witnesses) {
            require_same(*expected, directory.witness, "final sysfs prefix name rebind")?;
        }
        Ok(())
    }

    pub(super) fn has_same_parent_snapshot(&self, other: &Self) -> bool {
        self.snapshot.root == other.snapshot.root
            && self.snapshot.parent_witness == other.snapshot.parent_witness
            && self.snapshot.parent_device == other.snapshot.parent_device
            && self.snapshot.parent_dev == other.snapshot.parent_dev
            && self.snapshot.parent_uevent == other.snapshot.parent_uevent
            && self.snapshot.parent_device_name == other.snapshot.parent_device_name
            && self.parent_snapshot() == other.parent_snapshot()
    }

    fn parent_snapshot(&self) -> Option<&AncestorEvidence> {
        self.snapshot
            .ancestors
            .iter()
            .find(|ancestor| matches!(ancestor, AncestorEvidence::Parent(_)))
    }
}

pub(super) fn capture_twice(
    root: &RootHandle,
    device: SysfsDeviceNumber,
    operation: &mut Operation<'_>,
) -> io::Result<Capture> {
    let first = capture_once(root, device, operation)?;
    let second = capture_once(root, device, operation)?;
    require_capture_matches(&first, &second)?;
    Ok(second)
}

pub(super) fn require_capture_matches(expected: &Capture, actual: &Capture) -> io::Result<()> {
    if expected.snapshot == actual.snapshot {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "two complete sysfs identity captures disagree",
        ))
    }
}

fn capture_terminal_ancestors(
    root: &RootHandle,
    directories: &[PinnedDirectory],
    parent_index: usize,
    operation: &mut Operation<'_>,
) -> io::Result<Vec<AncestorEvidence>> {
    let partition_index = directories
        .len()
        .checked_sub(1)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "terminal sysfs chain is empty"))?;
    if parent_index >= partition_index {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "terminal sysfs parent is not above the partition",
        ));
    }
    let count = partition_index - parent_index;
    if count > operation.max_ancestors {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "terminal sysfs ancestor classification exceeds its bound",
        ));
    }
    let mut ancestors = Vec::new();
    ancestors
        .try_reserve_exact(count)
        .map_err(|source| io::Error::other(format!("could not allocate terminal sysfs ancestor evidence: {source}")))?;
    for reverse_index in (parent_index..partition_index).rev() {
        let depth = partition_index - reverse_index;
        operation.charge(1, "terminally classifying bounded sysfs ancestor")?;
        operation.emit(CaptureCheckpoint::AncestorExamined { depth })?;
        let subsystem = read_optional_subsystem(root, &directories[reverse_index].file, depth, operation)?;
        if reverse_index == parent_index {
            let subsystem = subsystem.ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "terminal sysfs block parent lost subsystem")
            })?;
            if subsystem.name != b"block" {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "terminal selected parent subsystem is not exactly block",
                ));
            }
            ancestors.push(AncestorEvidence::Parent(subsystem));
        } else {
            match subsystem {
                None => ancestors.push(AncestorEvidence::Missing),
                Some(subsystem) if subsystem.name != b"block" => {
                    ancestors.push(AncestorEvidence::NonBlock(subsystem));
                }
                Some(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "terminal closer ancestor unexpectedly became a block object",
                    ));
                }
            }
        }
    }
    Ok(ancestors)
}

fn capture_once(
    root: &RootHandle,
    requested_device: SysfsDeviceNumber,
    operation: &mut Operation<'_>,
) -> io::Result<Capture> {
    root.require_named(operation)?;
    let (lookup, lookup_witness) = pin_lookup(root, requested_device, operation)?;
    operation.emit(CaptureCheckpoint::LookupPinned)?;
    root.require_descendant(lookup_witness, "sysfs dev/block lookup")?;
    let raw_target = read_pinned_symlink(
        &lookup,
        SYSFS_LINK_TARGET_MAX_BYTES,
        operation,
        "reading sysfs dev/block lookup",
    )?;
    operation.charge(LINK_PARSER_WORK_RESERVATION, "reserving sysfs lookup parser work")?;
    let parsed_target = normalize_sysfs_dev_block_target_until(&raw_target, operation.deadline())?;
    let target_components = copy_components(parsed_target.components(), operation)?;
    if target_components.len() < 2 || target_components.len().saturating_sub(1) > operation.max_ancestors {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "normalized sysfs target exceeds the ancestor bound",
        ));
    }
    let normalized_devpath = normalized_devpath(&target_components, operation)?;

    operation.emit(CaptureCheckpoint::LookupRebound)?;
    let (rebound_lookup, rebound_witness) = pin_lookup(root, requested_device, operation)?;
    require_same(lookup_witness, rebound_witness, "sysfs dev/block lookup rebind")?;
    let rebound_target = read_pinned_symlink(
        &rebound_lookup,
        SYSFS_LINK_TARGET_MAX_BYTES,
        operation,
        "reading rebound sysfs dev/block lookup",
    )?;
    if rebound_target != raw_target {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "sysfs dev/block lookup content changed during capture",
        ));
    }

    let directories = open_target_chain(root, &target_components, operation)?;
    operation.emit(CaptureCheckpoint::TargetPinned)?;
    let partition_index = directories.len().checked_sub(1).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "normalized sysfs target has no partition directory",
        )
    })?;
    let partition = &directories[partition_index];
    let partition_witness = partition.witness;
    root.require_descendant(partition_witness, "sysfs partition directory")?;

    let partition_subsystem = require_block_subsystem(root, &partition.file, 0, operation)?;
    let partition_dev = read_attribute(
        root,
        &partition.file,
        c"dev",
        SYSFS_DEV_ATTRIBUTE_MAX_BYTES,
        CaptureNode::Partition,
        CaptureAttribute::Dev,
        operation,
    )?;
    let partition_number_attribute = read_attribute(
        root,
        &partition.file,
        c"partition",
        SYSFS_PARTITION_ATTRIBUTE_MAX_BYTES,
        CaptureNode::Partition,
        CaptureAttribute::Partition,
        operation,
    )?;
    let partition_start_attribute = read_attribute(
        root,
        &partition.file,
        c"start",
        SYSFS_PARTITION_GEOMETRY_ATTRIBUTE_MAX_BYTES,
        CaptureNode::Partition,
        CaptureAttribute::Start,
        operation,
    )?;
    let partition_size_attribute = read_attribute(
        root,
        &partition.file,
        c"size",
        SYSFS_PARTITION_GEOMETRY_ATTRIBUTE_MAX_BYTES,
        CaptureNode::Partition,
        CaptureAttribute::Size,
        operation,
    )?;
    let partition_uevent = read_attribute(
        root,
        &partition.file,
        c"uevent",
        SYSFS_UEVENT_MAX_BYTES,
        CaptureNode::Partition,
        CaptureAttribute::Uevent,
        operation,
    )?;
    operation.charge(
        IDENTITY_PARSER_WORK_RESERVATION,
        "reserving sysfs partition-identity parser work",
    )?;
    let partition_identity = parse_sysfs_partition_identity_until(
        &partition_dev.bytes,
        &partition_number_attribute.bytes,
        &partition_uevent.bytes,
        operation.deadline(),
    )?;
    operation.charge(
        GEOMETRY_PARSER_WORK_RESERVATION,
        "reserving sysfs partition-geometry parser work",
    )?;
    let partition_geometry = parse_sysfs_partition_geometry_until(
        &partition_start_attribute.bytes,
        &partition_size_attribute.bytes,
        operation.deadline(),
    )?;
    if partition_identity.device() != requested_device {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "sysfs lookup device disagrees with captured partition identity",
        ));
    }
    let partition_device_name = copy_device_name(partition_identity.device_name(), operation)?;

    let mut ancestor_evidence = Vec::new();
    ancestor_evidence
        .try_reserve(partition_index)
        .map_err(|source| io::Error::other(format!("could not allocate bounded sysfs ancestor evidence: {source}")))?;
    let mut selected = None;
    for (reverse_index, directory) in directories[..partition_index].iter().enumerate().rev() {
        let depth = partition_index - reverse_index;
        if depth > operation.max_ancestors {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "sysfs parent search exceeded its ancestor bound",
            ));
        }
        operation.charge(1, "examining bounded sysfs ancestor")?;
        operation.emit(CaptureCheckpoint::AncestorExamined { depth })?;
        let subsystem = read_optional_subsystem(root, &directory.file, depth, operation)?;
        match subsystem {
            None => ancestor_evidence.push(AncestorEvidence::Missing),
            Some(subsystem) if subsystem.name != b"block" => {
                ancestor_evidence.push(AncestorEvidence::NonBlock(subsystem));
            }
            Some(subsystem) => {
                operation.emit(CaptureCheckpoint::ParentSelected { depth })?;
                attribute_absent(&directory.file, c"partition", operation)?;
                let parent_dev = read_attribute(
                    root,
                    &directory.file,
                    c"dev",
                    SYSFS_DEV_ATTRIBUTE_MAX_BYTES,
                    CaptureNode::Parent,
                    CaptureAttribute::Dev,
                    operation,
                )?;
                let parent_uevent = read_attribute(
                    root,
                    &directory.file,
                    c"uevent",
                    SYSFS_UEVENT_MAX_BYTES,
                    CaptureNode::Parent,
                    CaptureAttribute::Uevent,
                    operation,
                )?;
                attribute_absent(&directory.file, c"partition", operation)?;
                operation.charge(
                    IDENTITY_PARSER_WORK_RESERVATION,
                    "reserving sysfs disk-identity parser work",
                )?;
                let disk_identity =
                    parse_sysfs_disk_identity_until(&parent_dev.bytes, &parent_uevent.bytes, operation.deadline())?;
                let parent_device_name = copy_device_name(disk_identity.device_name(), operation)?;
                let disk_sequence =
                    require_matching_disk_sequence_until(&partition_identity, &disk_identity, operation.deadline())?;
                if disk_identity.device() == partition_identity.device() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "sysfs partition and whole-disk parent have the same device number",
                    ));
                }
                let parent_witness = directory.witness;
                root.require_descendant(parent_witness, "sysfs whole-disk parent")?;
                ancestor_evidence.push(AncestorEvidence::Parent(subsystem));
                selected = Some((
                    reverse_index,
                    parent_witness,
                    parent_dev,
                    parent_uevent,
                    disk_identity.device(),
                    disk_sequence,
                    parent_device_name,
                ));
                break;
            }
        }
        operation.checkpoint()?;
    }
    let (parent_index, parent_witness, parent_dev, parent_uevent, parent_device, disk_sequence, parent_device_name) =
        selected.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "sysfs partition has no bounded whole-disk block ancestor",
            )
        })?;
    let parent_component_count = parent_index
        .checked_add(1)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "sysfs parent component count overflowed"))?;

    require_same(
        partition_witness,
        witness(
            &directories[partition_index].file,
            operation,
            "retained sysfs partition after capture",
        )?,
        "retained sysfs partition after capture",
    )?;
    require_same(
        parent_witness,
        witness(
            &directories[parent_index].file,
            operation,
            "retained sysfs parent after capture",
        )?,
        "retained sysfs parent after capture",
    )?;
    let target_directory_witnesses = copy_directory_witnesses(&directories, operation)?;
    root.require_named(operation)?;
    operation.checkpoint()?;
    Ok(Capture {
        snapshot: Snapshot {
            root: root.witness(),
            lookup: lookup_witness,
            target_components,
            target_directory_witnesses,
            normalized_devpath,
            partition_witness,
            parent_witness,
            parent_component_count,
            partition_dev,
            partition_number_attribute,
            partition_start_attribute,
            partition_size_attribute,
            partition_uevent,
            partition_subsystem,
            parent_dev,
            parent_uevent,
            ancestors: ancestor_evidence,
            device: partition_identity.device(),
            partition_number: partition_identity.partition_number(),
            partition_uuid: partition_identity.partition_uuid(),
            disk_sequence,
            parent_device,
            partition_device_name,
            parent_device_name,
            partition_geometry,
        },
        directories,
    })
}

fn copy_device_name(name: &SysfsBlockDeviceName, operation: &mut Operation<'_>) -> io::Result<Vec<u8>> {
    let bytes = name.as_bytes();
    operation.charge(
        bytes.len().saturating_add(1),
        "retaining authenticated block-device name",
    )?;
    let mut retained = Vec::new();
    retained
        .try_reserve_exact(bytes.len())
        .map_err(|source| io::Error::other(format!("could not allocate authenticated block-device name: {source}")))?;
    retained.extend_from_slice(bytes);
    Ok(retained)
}

fn pin_lookup(
    root: &RootHandle,
    device: SysfsDeviceNumber,
    operation: &mut Operation<'_>,
) -> io::Result<(std::fs::File, FileWitness)> {
    let dev = open_directory(root.file(), c"dev", operation, "opening sysfs dev directory")?;
    let block = open_directory(&dev, c"block", operation, "opening sysfs dev/block directory")?;
    let name = device_component(device)?;
    pin_symlink(&block, &name, operation, "pinning sysfs dev/block lookup")
}

fn device_component(device: SysfsDeviceNumber) -> io::Result<CString> {
    let mut encoded = [0_u8; 21];
    let major = encode_decimal(device.major(), &mut encoded[..10])?;
    encoded[major] = b':';
    let minor_start = major + 1;
    let minor = encode_decimal(device.minor(), &mut encoded[minor_start..])?;
    let length = minor_start + minor;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(length.saturating_add(1))
        .map_err(|source| io::Error::other(format!("could not allocate sysfs device component: {source}")))?;
    bytes.extend_from_slice(&encoded[..length]);
    CString::new(bytes).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "sysfs device component unexpectedly contains NUL",
        )
    })
}

fn encode_decimal(value: u32, output: &mut [u8]) -> io::Result<usize> {
    let mut reverse = [0_u8; 10];
    let mut remaining = value;
    let mut count = 0;
    loop {
        reverse[count] = b'0'
            + u8::try_from(remaining % 10)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "sysfs device digit exceeds u8"))?;
        count += 1;
        remaining /= 10;
        if remaining == 0 {
            break;
        }
    }
    for index in 0..count {
        output[index] = reverse[count - index - 1];
    }
    Ok(count)
}

fn copy_components<'a>(
    components: impl ExactSizeIterator<Item = &'a [u8]>,
    operation: &mut Operation<'_>,
) -> io::Result<Vec<Vec<u8>>> {
    let mut copied = Vec::new();
    copied.try_reserve_exact(components.len()).map_err(|source| {
        io::Error::other(format!(
            "could not allocate bounded normalized sysfs components: {source}"
        ))
    })?;
    for component in components {
        operation.charge(
            component.len().saturating_add(1),
            "retaining normalized sysfs component",
        )?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(component.len())
            .map_err(|source| io::Error::other(format!("could not allocate normalized sysfs component: {source}")))?;
        bytes.extend_from_slice(component);
        copied.push(bytes);
    }
    Ok(copied)
}

fn normalized_devpath(components: &[Vec<u8>], operation: &mut Operation<'_>) -> io::Result<Vec<u8>> {
    let size = components.iter().try_fold(4_usize, |size, component| {
        size.checked_add(component.len().saturating_add(1))
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "normalized sysfs devpath length overflowed"))
    })?;
    operation.charge(size, "retaining normalized sysfs devpath")?;
    let mut path = Vec::new();
    path.try_reserve_exact(size)
        .map_err(|source| io::Error::other(format!("could not allocate normalized sysfs devpath: {source}")))?;
    path.extend_from_slice(b"/sys");
    for component in components {
        path.push(b'/');
        path.extend_from_slice(component);
    }
    Ok(path)
}

fn open_target_chain(
    root: &RootHandle,
    components: &[Vec<u8>],
    operation: &mut Operation<'_>,
) -> io::Result<Vec<PinnedDirectory>> {
    let mut directories = Vec::new();
    directories
        .try_reserve_exact(components.len())
        .map_err(|source| io::Error::other(format!("could not allocate bounded sysfs descriptor chain: {source}")))?;
    for component in components {
        let name = cstring(component, "normalized sysfs target component")?;
        let parent = directories
            .last()
            .map(|directory: &PinnedDirectory| &directory.file)
            .unwrap_or_else(|| root.file());
        let directory = open_directory(parent, &name, operation, "opening normalized sysfs target component")?;
        let evidence = witness(&directory, operation, "normalized sysfs target directory")?
            .require_kind(nix::libc::S_IFDIR, "normalized sysfs target directory")?;
        root.require_descendant(evidence, "normalized sysfs target directory")?;
        directories.push(PinnedDirectory {
            file: directory,
            witness: evidence,
        });
    }
    Ok(directories)
}

fn copy_directory_witnesses(
    directories: &[PinnedDirectory],
    operation: &mut Operation<'_>,
) -> io::Result<Vec<FileWitness>> {
    operation.charge(directories.len(), "retaining bounded sysfs prefix-directory witnesses")?;
    let mut witnesses = Vec::new();
    witnesses
        .try_reserve_exact(directories.len())
        .map_err(|source| io::Error::other(format!("could not allocate sysfs prefix-directory witnesses: {source}")))?;
    witnesses.extend(directories.iter().map(|directory| directory.witness));
    Ok(witnesses)
}

fn require_block_subsystem(
    root: &RootHandle,
    node: &std::fs::File,
    depth: usize,
    operation: &mut Operation<'_>,
) -> io::Result<SubsystemEvidence> {
    let subsystem = read_optional_subsystem(root, node, depth, operation)?
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "sysfs partition lacks its subsystem link"))?;
    if subsystem.name != b"block" {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "sysfs partition subsystem is not exactly block",
        ));
    }
    Ok(subsystem)
}

fn read_optional_subsystem(
    root: &RootHandle,
    node: &std::fs::File,
    depth: usize,
    operation: &mut Operation<'_>,
) -> io::Result<Option<SubsystemEvidence>> {
    let (link, link_witness) = match pin_symlink(node, c"subsystem", operation, "pinning sysfs subsystem link") {
        Ok(link) => link,
        Err(source) if source.raw_os_error() == Some(nix::libc::ENOENT) => return Ok(None),
        Err(source) => return Err(source),
    };
    root.require_descendant(link_witness, "sysfs subsystem link")?;
    operation.emit(CaptureCheckpoint::SubsystemPinned { depth })?;
    let target = read_pinned_symlink(
        &link,
        SYSFS_LINK_TARGET_MAX_BYTES,
        operation,
        "reading sysfs subsystem link",
    )?;
    operation.emit(CaptureCheckpoint::SubsystemRead { depth })?;
    operation.charge(LINK_PARSER_WORK_RESERVATION, "reserving sysfs subsystem parser work")?;
    let parsed = parse_sysfs_subsystem_target_until(&target, operation.deadline())?;
    let mut name = Vec::new();
    name.try_reserve_exact(parsed.as_bytes().len())
        .map_err(|source| io::Error::other(format!("could not allocate sysfs subsystem name: {source}")))?;
    name.extend_from_slice(parsed.as_bytes());

    operation.emit(CaptureCheckpoint::SubsystemRebound { depth })?;
    let (rebound, rebound_witness) = pin_symlink(node, c"subsystem", operation, "rebinding sysfs subsystem link")?;
    require_same(link_witness, rebound_witness, "rebound sysfs subsystem link")?;
    let rebound_target = read_pinned_symlink(
        &rebound,
        SYSFS_LINK_TARGET_MAX_BYTES,
        operation,
        "reading rebound sysfs subsystem link",
    )?;
    if rebound_target != target {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "sysfs subsystem link changed during capture",
        ));
    }
    require_same(
        link_witness,
        witness(&link, operation, "retained sysfs subsystem link")?,
        "retained sysfs subsystem link",
    )?;
    Ok(Some(SubsystemEvidence {
        witness: link_witness,
        target,
        name,
    }))
}

fn cstring(bytes: &[u8], context: &'static str) -> io::Result<CString> {
    let mut owned = Vec::new();
    owned
        .try_reserve_exact(bytes.len().saturating_add(1))
        .map_err(|source| io::Error::other(format!("could not allocate {context}: {source}")))?;
    owned.extend_from_slice(bytes);
    CString::new(owned).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, format!("{context} contains NUL")))
}

fn require_exact_evidence<T: PartialEq>(expected: &T, actual: &T, context: &'static str) -> io::Result<()> {
    if expected == actual {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{context} changed exact retained evidence"),
        ))
    }
}
