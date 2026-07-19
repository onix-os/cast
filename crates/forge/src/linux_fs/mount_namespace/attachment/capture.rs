use std::io;

use super::super::filesystem::Operation;
use super::{
    AttachmentCheckpoint,
    filesystem::{DirectoryWitness, directory_witness, open_directory_component, require_same_directory},
    selector::AttachmentSelector,
};
use crate::linux_fs::descriptor_boot_filesystem::{
    BootFilesystemAuthenticationError, ValidatedBootFilesystemDescriptorEvidence,
    authenticate_boot_filesystem_directory_until,
};
use crate::linux_fs::{
    descriptor_boot_namespace::{
        BootNamespaceAssessmentLimits, BootNamespaceRequest, RetainedBootNamespaceAssessmentError,
        RetainedBootNamespaceAssessmentLimits, RetainedBootNamespaceExpectedSource,
        ValidatedRetainedBootNamespaceAssessment, assess_retained_boot_namespace_until,
    },
    descriptor_devtmpfs_filesystem::{
        DevtmpfsDescriptorAuthenticationError, ValidatedDevtmpfsSameMountDescriptorEvidence,
        authenticate_devtmpfs_same_mount_directory_until,
    },
    gpt_partition_device::{
        LiveAuthenticatedGptPartitionDeviceEvidence, authenticate_retained_devtmpfs_gpt_partition_device_until,
    },
    gpt_partition_role::GptPartitionRole,
    mountinfo_devtmpfs_policy::ValidatedDevtmpfsMountInfoPolicy,
    sysfs_identity::SysfsGptDeviceExpectation,
};

struct PinnedComponent {
    name: std::ffi::CString,
    file: std::fs::File,
    witness: DirectoryWitness,
}

pub(super) struct AttachmentCapture {
    root_witness: DirectoryWitness,
    components: Vec<PinnedComponent>,
    final_parent_witness: DirectoryWitness,
    final_name: std::ffi::CString,
    destination_witness: DirectoryWitness,
}

impl AttachmentCapture {
    pub(super) fn component_count(&self) -> usize {
        self.components.len()
    }

    pub(super) const fn destination_witness(&self) -> DirectoryWitness {
        self.destination_witness
    }

    pub(super) fn authenticate_boot_filesystem_until(
        &self,
        deadline: std::time::Instant,
    ) -> Result<ValidatedBootFilesystemDescriptorEvidence, BootFilesystemAuthenticationError> {
        let destination = self
            .components
            .last()
            .unwrap_or_else(|| unreachable!("validated attachment capture always retains one destination component"));
        authenticate_boot_filesystem_directory_until(
            &destination.file,
            self.destination_witness.device,
            self.destination_witness.inode,
            deadline,
        )
    }

    pub(super) fn assess_retained_boot_namespace_until(
        &self,
        requests: &[BootNamespaceRequest<'_>],
        expected: &[RetainedBootNamespaceExpectedSource<'_>],
        namespace_limits: BootNamespaceAssessmentLimits,
        live_limits: RetainedBootNamespaceAssessmentLimits,
        deadline: std::time::Instant,
    ) -> Result<ValidatedRetainedBootNamespaceAssessment, RetainedBootNamespaceAssessmentError> {
        let destination = self
            .components
            .last()
            .unwrap_or_else(|| unreachable!("validated attachment capture always retains one destination component"));
        assess_retained_boot_namespace_until(
            &destination.file,
            requests,
            expected,
            namespace_limits,
            live_limits,
            deadline,
        )
    }

    pub(super) fn authenticate_devtmpfs_same_mount_until(
        &self,
        policy: ValidatedDevtmpfsMountInfoPolicy,
        deadline: std::time::Instant,
    ) -> Result<ValidatedDevtmpfsSameMountDescriptorEvidence, DevtmpfsDescriptorAuthenticationError> {
        let destination = self
            .components
            .last()
            .unwrap_or_else(|| unreachable!("validated attachment capture always retains one destination component"));
        authenticate_devtmpfs_same_mount_directory_until(
            &destination.file,
            self.destination_witness.device,
            self.destination_witness.inode,
            self.destination_witness.mount_id,
            policy,
            deadline,
        )
    }

    /// Authenticate a sysfs-selected GPT parent below the same retained
    /// destination descriptor used by the devtmpfs attachment binding.
    pub(super) fn authenticate_gpt_parent_until(
        &self,
        authenticated_root_mount_id: u64,
        expected: &SysfsGptDeviceExpectation<'_>,
        expected_role: GptPartitionRole,
        deadline: std::time::Instant,
    ) -> io::Result<LiveAuthenticatedGptPartitionDeviceEvidence> {
        self.require_gpt_root_mount_id(authenticated_root_mount_id)?;
        let destination = self
            .components
            .last()
            .unwrap_or_else(|| unreachable!("validated attachment capture always retains one destination component"));
        authenticate_retained_devtmpfs_gpt_partition_device_until(
            &destination.file,
            authenticated_root_mount_id,
            expected,
            expected_role,
            deadline,
        )
    }

    fn require_gpt_root_mount_id(&self, authenticated_root_mount_id: u64) -> io::Result<()> {
        if authenticated_root_mount_id != self.destination_witness.mount_id {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "GPT authentication received a mount ID from a different attachment",
            ));
        }
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn validate_fixture_gpt_root_mount_id(&self, authenticated_root_mount_id: u64) -> io::Result<()> {
        self.require_gpt_root_mount_id(authenticated_root_mount_id)
    }

    #[cfg(test)]
    pub(super) fn component_witness(&self, index: usize) -> Option<DirectoryWitness> {
        self.components.get(index).map(|component| component.witness)
    }

    pub(super) fn require_retained(&self, root: &std::fs::File, operation: &mut Operation<'_>) -> io::Result<()> {
        require_same_directory(
            self.root_witness,
            directory_witness(root, operation, "retained attachment task root")?,
            "retained attachment task root",
        )?;
        for component in &self.components {
            require_same_directory(
                component.witness,
                directory_witness(&component.file, operation, "retained attachment component")?,
                "retained attachment component",
            )?;
        }
        Ok(())
    }

    pub(super) fn require_terminal_names(&self, root: &std::fs::File, operation: &mut Operation<'_>) -> io::Result<()> {
        self.require_retained(root, operation)?;

        operation.emit_attachment(AttachmentCheckpoint::TerminalFullChain { round: 1 })?;
        self.require_full_chain_rebind(root, operation, "first terminal attachment full-chain rebind")?;
        self.require_root(root, operation, "task root after first terminal full-chain rebind")?;

        operation.emit_attachment(AttachmentCheckpoint::TerminalParent)?;
        let parents = self.rebind_final_parent(root, operation)?;
        let parent = parents.last().map_or(root, |parent| parent);
        require_same_directory(
            self.final_parent_witness,
            directory_witness(parent, operation, "terminal attachment final parent")?,
            "terminal attachment final parent",
        )?;

        operation.emit_attachment(AttachmentCheckpoint::TerminalName)?;
        let destination = open_directory_component(
            parent,
            &self.final_name,
            operation,
            "terminal attachment destination name",
        )?;
        require_same_directory(
            self.destination_witness,
            directory_witness(&destination, operation, "terminal attachment destination")?,
            "terminal attachment destination",
        )?;
        self.require_root(root, operation, "task root after terminal attachment name rebind")?;

        operation.emit_attachment(AttachmentCheckpoint::TerminalFullChain { round: 2 })?;
        self.require_full_chain_rebind(root, operation, "closing terminal attachment full-chain rebind")?;
        self.require_root(root, operation, "task root after closing terminal full-chain rebind")?;
        self.require_retained(root, operation)
    }

    fn require_root(
        &self,
        root: &std::fs::File,
        operation: &mut Operation<'_>,
        context: &'static str,
    ) -> io::Result<()> {
        require_same_directory(self.root_witness, directory_witness(root, operation, context)?, context)
    }

    fn require_full_chain_rebind(
        &self,
        root: &std::fs::File,
        operation: &mut Operation<'_>,
        context: &'static str,
    ) -> io::Result<()> {
        let rebound = rebind_components(root, &self.components, self.components.len(), operation)?;
        if rebound.len() != self.components.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "attachment full-chain rebind changed length",
            ));
        }
        for (actual, expected) in rebound.iter().zip(&self.components) {
            require_same_directory(
                expected.witness,
                directory_witness(actual, operation, context)?,
                context,
            )?;
        }
        Ok(())
    }

    fn rebind_final_parent(
        &self,
        root: &std::fs::File,
        operation: &mut Operation<'_>,
    ) -> io::Result<Vec<std::fs::File>> {
        let parent_count = self
            .components
            .len()
            .checked_sub(1)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "attachment has no final component"))?;
        let parents = rebind_components(root, &self.components, parent_count, operation)?;
        for (actual, expected) in parents.iter().zip(&self.components[..parent_count]) {
            require_same_directory(
                expected.witness,
                directory_witness(actual, operation, "terminal attachment parent chain")?,
                "terminal attachment parent chain",
            )?;
        }
        Ok(parents)
    }
}

pub(super) fn capture_twice(
    root: &std::fs::File,
    root_witness: DirectoryWitness,
    selector: &AttachmentSelector,
    operation: &mut Operation<'_>,
) -> io::Result<AttachmentCapture> {
    let first = capture_once(root, root_witness, selector, 1, operation)?;
    let second = capture_once(root, root_witness, selector, 2, operation)?;
    require_capture_matches(&first, &second, "two complete attachment-chain passes")?;
    first.require_retained(root, operation)?;
    second.require_retained(root, operation)?;
    Ok(first)
}

fn capture_once(
    root: &std::fs::File,
    root_witness: DirectoryWitness,
    selector: &AttachmentSelector,
    pass: usize,
    operation: &mut Operation<'_>,
) -> io::Result<AttachmentCapture> {
    require_same_directory(
        root_witness,
        directory_witness(root, operation, "attachment task root at pass open")?,
        "attachment task root at pass open",
    )?;
    let mut components = Vec::new();
    components
        .try_reserve_exact(selector.components().len())
        .map_err(|source| io::Error::other(format!("could not allocate retained attachment chain: {source}")))?;
    operation.charge(
        selector.components().len(),
        "allocating retained attachment descriptor chain",
    )?;

    for (index, name) in selector.components().iter().enumerate() {
        let file = {
            let parent = components
                .last()
                .map_or(root, |component: &PinnedComponent| &component.file);
            open_directory_component(parent, name, operation, "opening attachment selector component")?
        };
        let witness = directory_witness(&file, operation, "attachment selector component")?;
        let name = super::filesystem::copy_component(name, operation)?;
        components.push(PinnedComponent { name, file, witness });
        operation.emit_attachment(AttachmentCheckpoint::ComponentPinned { pass, index })?;
    }

    let destination = components.last().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "attachment selector has no destination component",
        )
    })?;
    let final_parent_witness = components
        .len()
        .checked_sub(2)
        .and_then(|index| components.get(index))
        .map_or(root_witness, |parent| parent.witness);
    let final_name = super::filesystem::copy_component(&destination.name, operation)?;
    let capture = AttachmentCapture {
        root_witness,
        final_parent_witness,
        final_name,
        destination_witness: destination.witness,
        components,
    };
    capture.require_root(root, operation, "attachment task root at pass close")?;
    capture.require_retained(root, operation)?;
    operation.emit_attachment(AttachmentCheckpoint::PassComplete { pass })?;
    Ok(capture)
}

pub(super) fn require_capture_matches(
    expected: &AttachmentCapture,
    actual: &AttachmentCapture,
    context: &'static str,
) -> io::Result<()> {
    require_same_directory(expected.root_witness, actual.root_witness, context)?;
    require_same_directory(expected.final_parent_witness, actual.final_parent_witness, context)?;
    require_same_directory(expected.destination_witness, actual.destination_witness, context)?;
    if expected.final_name.as_bytes() != actual.final_name.as_bytes()
        || expected.components.len() != actual.components.len()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{context} changed component names or length"),
        ));
    }
    for (expected, actual) in expected.components.iter().zip(&actual.components) {
        if expected.name.as_bytes() != actual.name.as_bytes() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{context} changed an authored raw component name"),
            ));
        }
        require_same_directory(expected.witness, actual.witness, context)?;
    }
    Ok(())
}

fn rebind_components(
    root: &std::fs::File,
    expected: &[PinnedComponent],
    count: usize,
    operation: &mut Operation<'_>,
) -> io::Result<Vec<std::fs::File>> {
    if count > expected.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "attachment rebind count exceeds captured chain",
        ));
    }
    let mut rebound: Vec<std::fs::File> = Vec::new();
    rebound
        .try_reserve_exact(count)
        .map_err(|source| io::Error::other(format!("could not allocate attachment rebind chain: {source}")))?;
    operation.charge(count, "allocating attachment rebind descriptor chain")?;
    for expected in &expected[..count] {
        let parent = rebound.last().map_or(root, |parent| parent);
        let directory = open_directory_component(
            parent,
            &expected.name,
            operation,
            "rebinding attachment selector component",
        )?;
        rebound.push(directory);
    }
    Ok(rebound)
}
