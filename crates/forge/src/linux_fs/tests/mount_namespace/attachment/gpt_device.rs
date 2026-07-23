use std::{
    cell::{Cell, RefCell},
    error::Error as _,
    io,
    rc::Rc,
    time::{Duration, Instant},
};

use super::super::super::super::{
    descriptor_devtmpfs_filesystem::{
        DevtmpfsDescriptorAuthenticationError, FIXTURE_TMPFS_MAGIC, FixtureDevtmpfsDescriptorIdentity,
        FixtureDevtmpfsDescriptorLimits, FixtureDevtmpfsDescriptorObservations,
        ValidatedDevtmpfsSameMountDescriptorEvidence, validate_fixture_devtmpfs_descriptor_authentication,
    },
    gpt_partition_role::GptPartitionRole,
    mount_namespace::{FixtureMountNamespaceTree, PreparedMountNamespaceAnchor, RevalidatedTaskRootedAttachment},
    mountinfo::parse_mountinfo_bytes,
    mountinfo_attachment::select_mountinfo_attachment_until,
    mountinfo_devtmpfs_policy::{
        DevtmpfsAccessMode, DevtmpfsFilesystemKind, ValidatedDevtmpfsMountInfoPolicy,
        validate_selected_devtmpfs_mount_policy_until,
    },
};
use super::super::support::SyntheticMountNamespace;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Event {
    Devtmpfs,
    Gpt,
}

#[derive(Debug)]
struct ExactExpectation {
    marker: u64,
}

#[derive(Debug)]
struct FixtureGptEvidence {
    marker: u64,
    claimed_mount_id: u64,
    drops: Rc<Cell<usize>>,
}

impl Drop for FixtureGptEvidence {
    fn drop(&mut self) {
        self.drops.set(self.drops.get() + 1);
    }
}

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(10)
}

fn prepared_anchor(fixture: &SyntheticMountNamespace) -> io::Result<PreparedMountNamespaceAnchor> {
    let (parent, tree_name) = fixture.admission()?;
    FixtureMountNamespaceTree::admit(parent, tree_name)?.prepare()
}

fn prepared_attachment(
    anchor: &PreparedMountNamespaceAnchor,
    selector: &str,
) -> io::Result<super::super::super::super::mount_namespace::PreparedTaskRootedAttachment> {
    anchor.revalidate()?.prepare_task_rooted_attachment(selector)
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

fn descriptor_evidence(
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
fn exact_dev_authenticates_first_then_passes_unchanged_gpt_inputs_into_closed_result() {
    let fixture = SyntheticMountNamespace::with_attachment(&["dev"]).unwrap();
    let anchor = prepared_anchor(&fixture).unwrap();
    let attachment = prepared_attachment(&anchor, "/dev").unwrap();
    let view = attachment.revalidate_against(&anchor).unwrap();
    let deadline = deadline();
    let policy = policy_for(&view, deadline);
    let expectation = ExactExpectation { marker: 0x5a5a };
    let events = RefCell::new(Vec::new());
    let drops = Rc::new(Cell::new(0usize));
    let mut clock = || deadline;

    let closed = view
        .validate_fixture_devtmpfs_gpt_partition_device_with(
            policy,
            &expectation,
            GptPartitionRole::Esp,
            deadline,
            |device, inode, mount_id, received_policy, received_deadline| {
                events.borrow_mut().push(Event::Devtmpfs);
                assert_eq!(received_policy, policy);
                assert_eq!(received_deadline, deadline);
                descriptor_evidence(device, inode, mount_id, received_policy, received_deadline)
            },
            |mount_id, received_expectation, role, received_deadline| {
                assert_eq!(*events.borrow(), vec![Event::Devtmpfs]);
                events.borrow_mut().push(Event::Gpt);
                assert!(std::ptr::eq(received_expectation, &expectation));
                assert_eq!(role, GptPartitionRole::Esp);
                assert_eq!(received_deadline, deadline);
                Ok(RevalidatedTaskRootedAttachment::fixture_gpt_partition_device_evidence(
                    mount_id,
                    FixtureGptEvidence {
                        marker: received_expectation.marker,
                        claimed_mount_id: mount_id,
                        drops: drops.clone(),
                    },
                ))
            },
            &mut clock,
        )
        .unwrap();

    assert_eq!(*events.borrow(), vec![Event::Devtmpfs, Event::Gpt]);
    let devtmpfs = closed.devtmpfs_attachment();
    assert_eq!(devtmpfs.selector(), "/dev");
    assert_eq!(devtmpfs.directory_device(), view.destination_device());
    assert_eq!(devtmpfs.directory_inode(), view.destination_inode());
    assert_eq!(devtmpfs.mount_id(), view.destination_mount_id());
    assert_eq!(devtmpfs.filesystem(), DevtmpfsFilesystemKind::Devtmpfs);
    assert_eq!(devtmpfs.access_mode(), DevtmpfsAccessMode::ReadWrite);
    assert_eq!(closed.gpt_partition_device().marker, expectation.marker);
    assert_eq!(
        closed.gpt_partition_device().claimed_mount_id,
        view.destination_mount_id()
    );
    assert_eq!(drops.get(), 0);
    drop(closed);
    assert_eq!(drops.get(), 1);
    fixture.assert_outside_unchanged();
}

#[test]
fn non_dev_selector_rejects_before_either_authentication_layer() {
    let fixture = SyntheticMountNamespace::with_attachment(&["devices"]).unwrap();
    let anchor = prepared_anchor(&fixture).unwrap();
    let attachment = prepared_attachment(&anchor, "/devices").unwrap();
    let view = attachment.revalidate_against(&anchor).unwrap();
    let deadline = deadline();
    let policy = policy_for(&view, deadline);
    let calls = Cell::new(0usize);
    let mut clock = || deadline;

    let error = view
        .validate_fixture_devtmpfs_gpt_partition_device_with::<_, ()>(
            policy,
            &ExactExpectation { marker: 1 },
            GptPartitionRole::Esp,
            deadline,
            |_, _, _, _, _| {
                calls.set(calls.get() + 1);
                unreachable!("non-/dev selector must fail before descriptor authentication")
            },
            |_, _, _, _| {
                calls.set(calls.get() + 1);
                unreachable!("non-/dev selector must fail before GPT authentication")
            },
            &mut clock,
        )
        .unwrap_err();

    assert!(error.to_string().contains("devtmpfs attachment authentication failed"));
    assert!(error.source().unwrap().to_string().contains("not exactly /dev"));
    assert_eq!(calls.get(), 0);
    fixture.assert_outside_unchanged();
}

#[test]
fn cross_wired_devtmpfs_identity_withholds_result_and_skips_gpt() {
    let fixture = SyntheticMountNamespace::with_attachment(&["dev"]).unwrap();
    let anchor = prepared_anchor(&fixture).unwrap();
    let attachment = prepared_attachment(&anchor, "/dev").unwrap();
    let view = attachment.revalidate_against(&anchor).unwrap();
    let deadline = deadline();
    let policy = policy_for(&view, deadline);
    let gpt_calls = Cell::new(0usize);
    let mut clock = || deadline;

    let error = view
        .validate_fixture_devtmpfs_gpt_partition_device_with::<_, ()>(
            policy,
            &ExactExpectation { marker: 2 },
            GptPartitionRole::Xbootldr,
            deadline,
            |device, inode, mount_id, policy, deadline| {
                descriptor_evidence(device, inode + 1, mount_id, policy, deadline)
            },
            |mount_id, _, _, _| {
                gpt_calls.set(gpt_calls.get() + 1);
                Ok(RevalidatedTaskRootedAttachment::fixture_gpt_partition_device_evidence(
                    mount_id,
                    (),
                ))
            },
            &mut clock,
        )
        .unwrap_err();

    assert!(
        error
            .source()
            .unwrap()
            .to_string()
            .contains("exact task-root attachment identity")
    );
    assert_eq!(gpt_calls.get(), 0);
    fixture.assert_outside_unchanged();
}

#[test]
fn injected_gpt_failure_is_preserved_and_withholds_closed_result() {
    let fixture = SyntheticMountNamespace::with_attachment(&["dev"]).unwrap();
    let anchor = prepared_anchor(&fixture).unwrap();
    let attachment = prepared_attachment(&anchor, "/dev").unwrap();
    let view = attachment.revalidate_against(&anchor).unwrap();
    let deadline = deadline();
    let policy = policy_for(&view, deadline);
    let mut clock = || deadline;

    let error = view
        .validate_fixture_devtmpfs_gpt_partition_device_with::<_, ()>(
            policy,
            &ExactExpectation { marker: 3 },
            GptPartitionRole::Esp,
            deadline,
            descriptor_evidence,
            |mount_id, _, _, _| {
                assert_eq!(mount_id, view.destination_mount_id());
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "injected owned GPT failure",
                ))
            },
            &mut clock,
        )
        .unwrap_err();

    assert_eq!(
        error.to_string(),
        "owned GPT parent authentication failed below the retained task-root /dev"
    );
    assert_eq!(error.source().unwrap().to_string(), "injected owned GPT failure");
    fixture.assert_outside_unchanged();
}

#[test]
fn foreign_gpt_payload_claim_cannot_override_structural_mount_id() {
    let fixture = SyntheticMountNamespace::with_attachment(&["dev"]).unwrap();
    let anchor = prepared_anchor(&fixture).unwrap();
    let attachment = prepared_attachment(&anchor, "/dev").unwrap();
    let view = attachment.revalidate_against(&anchor).unwrap();
    let deadline = deadline();
    let policy = policy_for(&view, deadline);
    let drops = Rc::new(Cell::new(0usize));
    let mut clock = || deadline;

    let error = view
        .validate_fixture_devtmpfs_gpt_partition_device_with(
            policy,
            &ExactExpectation { marker: 4 },
            GptPartitionRole::Esp,
            deadline,
            descriptor_evidence,
            |mount_id, expectation, _, _| {
                Ok(RevalidatedTaskRootedAttachment::fixture_gpt_partition_device_evidence(
                    mount_id + 1,
                    FixtureGptEvidence {
                        marker: expectation.marker,
                        claimed_mount_id: mount_id,
                        drops: drops.clone(),
                    },
                ))
            },
            &mut clock,
        )
        .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("does not retain the authenticated task-root devtmpfs mount ID")
    );
    assert_eq!(drops.get(), 1);
    fixture.assert_outside_unchanged();
}

#[test]
fn capture_rejects_a_foreign_gpt_root_mount_id_before_authentication() {
    let fixture = SyntheticMountNamespace::with_attachment(&["dev"]).unwrap();
    let anchor = prepared_anchor(&fixture).unwrap();
    let attachment = prepared_attachment(&anchor, "/dev").unwrap();
    let view = attachment.revalidate_against(&anchor).unwrap();

    view.validate_fixture_gpt_root_mount_id(view.destination_mount_id())
        .unwrap();
    let foreign_mount_id = view.destination_mount_id().checked_add(1).unwrap();
    let error = view.validate_fixture_gpt_root_mount_id(foreign_mount_id).unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("different attachment"));
    fixture.assert_outside_unchanged();
}

#[test]
fn expiry_after_devtmpfs_withholds_result_before_gpt_authentication() {
    let fixture = SyntheticMountNamespace::with_attachment(&["dev"]).unwrap();
    let anchor = prepared_anchor(&fixture).unwrap();
    let attachment = prepared_attachment(&anchor, "/dev").unwrap();
    let view = attachment.revalidate_against(&anchor).unwrap();
    let deadline = deadline();
    let policy = policy_for(&view, deadline);
    let gpt_calls = Cell::new(0usize);
    let clock_calls = Cell::new(0usize);
    let mut clock = || {
        clock_calls.set(clock_calls.get() + 1);
        deadline + Duration::from_secs(1)
    };

    let error = view
        .validate_fixture_devtmpfs_gpt_partition_device_with::<_, ()>(
            policy,
            &ExactExpectation { marker: 5 },
            GptPartitionRole::Esp,
            deadline,
            descriptor_evidence,
            |mount_id, _, _, _| {
                gpt_calls.set(gpt_calls.get() + 1);
                Ok(RevalidatedTaskRootedAttachment::fixture_gpt_partition_device_evidence(
                    mount_id,
                    (),
                ))
            },
            &mut clock,
        )
        .unwrap_err();

    assert!(error.to_string().contains("exceeded caller deadline"));
    assert_eq!(clock_calls.get(), 1);
    assert_eq!(gpt_calls.get(), 0);
    fixture.assert_outside_unchanged();
}

#[test]
fn expiry_after_gpt_discards_evidence_and_withholds_result() {
    let fixture = SyntheticMountNamespace::with_attachment(&["dev"]).unwrap();
    let anchor = prepared_anchor(&fixture).unwrap();
    let attachment = prepared_attachment(&anchor, "/dev").unwrap();
    let view = attachment.revalidate_against(&anchor).unwrap();
    let deadline = deadline();
    let policy = policy_for(&view, deadline);
    let drops = Rc::new(Cell::new(0usize));
    let clock_calls = Cell::new(0usize);
    let mut clock = || {
        let call = clock_calls.get() + 1;
        clock_calls.set(call);
        if call == 1 {
            deadline
        } else {
            deadline + Duration::from_secs(1)
        }
    };

    let error = view
        .validate_fixture_devtmpfs_gpt_partition_device_with(
            policy,
            &ExactExpectation { marker: 6 },
            GptPartitionRole::Esp,
            deadline,
            descriptor_evidence,
            |mount_id, expectation, _, _| {
                Ok(RevalidatedTaskRootedAttachment::fixture_gpt_partition_device_evidence(
                    mount_id,
                    FixtureGptEvidence {
                        marker: expectation.marker,
                        claimed_mount_id: mount_id,
                        drops: drops.clone(),
                    },
                ))
            },
            &mut clock,
        )
        .unwrap_err();

    assert!(error.to_string().contains("exceeded caller deadline"));
    assert_eq!(clock_calls.get(), 2);
    assert_eq!(drops.get(), 1);
    fixture.assert_outside_unchanged();
}

#[test]
fn expiry_at_terminal_checkpoint_discards_matching_evidence_and_withholds_result() {
    let fixture = SyntheticMountNamespace::with_attachment(&["dev"]).unwrap();
    let anchor = prepared_anchor(&fixture).unwrap();
    let attachment = prepared_attachment(&anchor, "/dev").unwrap();
    let view = attachment.revalidate_against(&anchor).unwrap();
    let deadline = deadline();
    let policy = policy_for(&view, deadline);
    let drops = Rc::new(Cell::new(0usize));
    let clock_calls = Cell::new(0usize);
    let mut clock = || {
        let call = clock_calls.get() + 1;
        clock_calls.set(call);
        if call < 3 {
            deadline
        } else {
            deadline + Duration::from_secs(1)
        }
    };

    let error = view
        .validate_fixture_devtmpfs_gpt_partition_device_with(
            policy,
            &ExactExpectation { marker: 7 },
            GptPartitionRole::Esp,
            deadline,
            descriptor_evidence,
            |mount_id, expectation, _, _| {
                Ok(RevalidatedTaskRootedAttachment::fixture_gpt_partition_device_evidence(
                    mount_id,
                    FixtureGptEvidence {
                        marker: expectation.marker,
                        claimed_mount_id: mount_id,
                        drops: drops.clone(),
                    },
                ))
            },
            &mut clock,
        )
        .unwrap_err();

    assert!(error.to_string().contains("exceeded caller deadline"));
    assert_eq!(clock_calls.get(), 3);
    assert_eq!(drops.get(), 1);
    fixture.assert_outside_unchanged();
}
