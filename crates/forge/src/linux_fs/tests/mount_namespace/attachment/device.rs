use std::{
    cell::Cell,
    error::Error as _,
    io,
    time::{Duration, Instant},
};

use super::super::super::super::{
    descriptor_devtmpfs_filesystem::{
        DevtmpfsDescriptorAuthenticationError, DevtmpfsDescriptorMagicFamily, DevtmpfsDescriptorObservationPhase,
        FIXTURE_TMPFS_MAGIC, FixtureDevtmpfsDescriptorIdentity, FixtureDevtmpfsDescriptorLimits,
        FixtureDevtmpfsDescriptorObservations, ValidatedDevtmpfsSameMountDescriptorEvidence,
        validate_fixture_devtmpfs_descriptor_authentication,
    },
    mount_namespace::{FixtureMountNamespaceTree, PreparedMountNamespaceAnchor, RevalidatedTaskRootedAttachment},
    mountinfo::parse_mountinfo_bytes,
    mountinfo_attachment::select_mountinfo_attachment_until,
    mountinfo_devtmpfs_policy::{
        DevtmpfsAccessMode, DevtmpfsFilesystemKind, ValidatedDevtmpfsMountInfoPolicy,
        validate_selected_devtmpfs_mount_policy_until,
    },
};
use super::super::support::SyntheticMountNamespace;

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(10)
}

fn prepared_anchor(fixture: &SyntheticMountNamespace) -> io::Result<PreparedMountNamespaceAnchor> {
    let (parent, tree_name) = fixture.admission()?;
    FixtureMountNamespaceTree::admit(parent, tree_name)?.prepare()
}

fn policy_for(view: &RevalidatedTaskRootedAttachment<'_>, deadline: Instant) -> ValidatedDevtmpfsMountInfoPolicy {
    let raw: nix::libc::dev_t = view.destination_device();
    let major = nix::libc::major(raw);
    let minor = nix::libc::minor(raw);
    let mount_id = view.destination_mount_id();
    let record = format!("{mount_id} 1 {major}:{minor} / /dev rw - devtmpfs devtmpfs rw\n");
    let parsed = parse_mountinfo_bytes(record.as_bytes()).unwrap();
    let selected = select_mountinfo_attachment_until(&parsed, b"/dev", mount_id, major, minor, deadline).unwrap();
    validate_selected_devtmpfs_mount_policy_until(selected, deadline).unwrap()
}

fn observations(device: u64, inode: u64, mount_id: u64) -> FixtureDevtmpfsDescriptorObservations {
    let identity = FixtureDevtmpfsDescriptorIdentity {
        device,
        inode,
        kind: nix::libc::S_IFDIR,
    };
    FixtureDevtmpfsDescriptorObservations {
        opening_identity: identity,
        opening_mount_id: mount_id,
        opening_magic: FIXTURE_TMPFS_MAGIC,
        closing_magic: FIXTURE_TMPFS_MAGIC,
        closing_mount_id: mount_id,
        closing_identity: identity,
    }
}

fn injected_descriptor_evidence(
    device: u64,
    inode: u64,
    mount_id: u64,
    policy: ValidatedDevtmpfsMountInfoPolicy,
    deadline: Instant,
) -> Result<ValidatedDevtmpfsSameMountDescriptorEvidence, DevtmpfsDescriptorAuthenticationError> {
    let mut clock = || deadline;
    let mut hook = |_| Ok(());
    validate_fixture_devtmpfs_descriptor_authentication(
        observations(device, inode, mount_id),
        device,
        inode,
        mount_id,
        policy,
        FixtureDevtmpfsDescriptorLimits::default(),
        deadline,
        &mut clock,
        &mut hook,
    )
    .map(|(evidence, _usage)| evidence)
}

#[test]
fn non_dev_selector_is_rejected_before_injected_descriptor_observation() {
    let fixture = SyntheticMountNamespace::with_attachment(&["devices"]).unwrap();
    let anchor = prepared_anchor(&fixture).unwrap();
    let attachment = anchor
        .revalidate()
        .unwrap()
        .prepare_task_rooted_attachment("/devices")
        .unwrap();
    let view = attachment.revalidate_against(&anchor).unwrap();
    let deadline = deadline();
    let policy = policy_for(&view, deadline);
    let calls = Cell::new(0usize);

    let error = view
        .validate_fixture_devtmpfs_attachment_with(policy, deadline, |_, _, _, _, _| {
            calls.set(calls.get() + 1);
            Err(DevtmpfsDescriptorAuthenticationError::DeadlineExceeded { deadline })
        })
        .unwrap_err();

    assert_eq!(
        error.to_string(),
        "task-root devtmpfs attachment selector is not exactly /dev"
    );
    assert_eq!(calls.get(), 0);
    fixture.assert_outside_unchanged();
}

#[test]
fn exact_dev_selector_retains_attachment_and_policy_scalars_in_closed_evidence() {
    let fixture = SyntheticMountNamespace::with_attachment(&["dev"]).unwrap();
    let anchor = prepared_anchor(&fixture).unwrap();
    let attachment = anchor
        .revalidate()
        .unwrap()
        .prepare_task_rooted_attachment("/dev")
        .unwrap();
    let view = attachment.revalidate_against(&anchor).unwrap();
    let deadline = deadline();
    let policy = policy_for(&view, deadline);
    let calls = Cell::new(0usize);

    let evidence = view
        .validate_fixture_devtmpfs_attachment_with(policy, deadline, |device, inode, mount_id, policy, deadline| {
            calls.set(calls.get() + 1);
            injected_descriptor_evidence(device, inode, mount_id, policy, deadline)
        })
        .unwrap();

    assert_eq!(calls.get(), 1);
    assert_eq!(evidence.selector(), "/dev");
    assert_eq!(evidence.directory_device(), view.destination_device());
    assert_eq!(evidence.directory_inode(), view.destination_inode());
    assert_eq!(evidence.mount_id(), view.destination_mount_id());
    assert_eq!(evidence.filesystem(), DevtmpfsFilesystemKind::Devtmpfs);
    assert_eq!(evidence.access_mode(), DevtmpfsAccessMode::ReadWrite);
    assert_eq!(evidence.magic_family(), DevtmpfsDescriptorMagicFamily::LinuxTmpfs);
    fixture.assert_outside_unchanged();
}

#[test]
fn expired_deadline_rejects_exact_dev_before_injected_descriptor_observation() {
    let fixture = SyntheticMountNamespace::with_attachment(&["dev"]).unwrap();
    let anchor = prepared_anchor(&fixture).unwrap();
    let attachment = anchor
        .revalidate()
        .unwrap()
        .prepare_task_rooted_attachment("/dev")
        .unwrap();
    let view = attachment.revalidate_against(&anchor).unwrap();
    let policy = policy_for(&view, deadline());
    let expired = Instant::now() - Duration::from_secs(1);
    let calls = Cell::new(0usize);

    let error = view
        .validate_fixture_devtmpfs_attachment_with(policy, expired, |_, _, _, _, _| {
            calls.set(calls.get() + 1);
            Err(DevtmpfsDescriptorAuthenticationError::DeadlineExceeded { deadline: expired })
        })
        .unwrap_err();

    assert!(error.to_string().contains("exceeded caller deadline"));
    assert_eq!(calls.get(), 0);
    fixture.assert_outside_unchanged();
}

#[test]
fn injected_descriptor_error_is_preserved_as_the_composition_source() {
    let fixture = SyntheticMountNamespace::with_attachment(&["dev"]).unwrap();
    let anchor = prepared_anchor(&fixture).unwrap();
    let attachment = anchor
        .revalidate()
        .unwrap()
        .prepare_task_rooted_attachment("/dev")
        .unwrap();
    let view = attachment.revalidate_against(&anchor).unwrap();
    let deadline = deadline();
    let policy = policy_for(&view, deadline);

    let error = view
        .validate_fixture_devtmpfs_attachment_with(policy, deadline, |_, _, _, _, _| {
            Err(DevtmpfsDescriptorAuthenticationError::ObservationFailed {
                phase: DevtmpfsDescriptorObservationPhase::OpeningDirectoryIdentity,
                source: io::Error::other("injected retained-destination observation failure"),
            })
        })
        .unwrap_err();

    assert_eq!(
        error.to_string(),
        "retained task-root devtmpfs destination authentication failed"
    );
    let descriptor_error = error.source().unwrap();
    assert!(
        descriptor_error
            .to_string()
            .contains("OpeningDirectoryIdentity observation failed")
    );
    assert_eq!(
        descriptor_error.source().unwrap().to_string(),
        "injected retained-destination observation failure"
    );
    fixture.assert_outside_unchanged();
}

#[test]
fn injected_descriptor_identity_mismatch_fails_closed() {
    let fixture = SyntheticMountNamespace::with_attachment(&["dev"]).unwrap();
    let anchor = prepared_anchor(&fixture).unwrap();
    let attachment = anchor
        .revalidate()
        .unwrap()
        .prepare_task_rooted_attachment("/dev")
        .unwrap();
    let view = attachment.revalidate_against(&anchor).unwrap();
    let deadline = deadline();
    let policy = policy_for(&view, deadline);

    let error = view
        .validate_fixture_devtmpfs_attachment_with(policy, deadline, |device, inode, mount_id, policy, deadline| {
            injected_descriptor_evidence(device, inode + 1, mount_id, policy, deadline)
        })
        .unwrap_err();

    assert_eq!(
        error.to_string(),
        "descriptor evidence does not retain the exact task-root attachment identity"
    );
    fixture.assert_outside_unchanged();
}
