use std::{
    cell::{Cell, RefCell},
    io,
    rc::Rc,
    time::{Duration, Instant},
};

use xxhash_rust::xxh3::xxh3_128;

use crate::linux_fs::{
    descriptor_boot_filesystem::{
        BootFilesystemAuthenticationError, BootFilesystemMagicFamily, BootFilesystemObservationPhase,
        FIXTURE_MSDOS_SUPER_MAGIC, FixtureBootFilesystemIdentity, FixtureBootFilesystemLimits,
        FixtureBootFilesystemObservations, ValidatedBootFilesystemDescriptorEvidence,
        validate_fixture_boot_filesystem_authentication,
    },
    descriptor_boot_namespace::{
        BootNamespaceAssessmentLimits, BootNamespaceDestinationState, BootNamespaceNodeIdentity,
        BootNamespaceRequest, RetainedBootNamespaceAssessmentError, RetainedBootNamespaceAssessmentLimits,
    },
    mount_namespace::{
        FixtureMountNamespaceTree, PreparedMountNamespaceAnchor, RevalidatedTaskRootedAttachment,
        TaskRootBootNamespaceAssessmentError,
    },
};

use super::super::support::SyntheticMountNamespace;

const EXPECTED: &[u8] = b"synthetic desired boot output\n";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Event {
    OpeningFilesystem,
    Namespace,
    ClosingFilesystem,
}

#[derive(Debug)]
struct FixtureNamespacePayload {
    states: Vec<BootNamespaceDestinationState>,
    drops: Rc<Cell<usize>>,
}

impl Drop for FixtureNamespacePayload {
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
) -> io::Result<crate::linux_fs::mount_namespace::PreparedTaskRootedAttachment> {
    anchor
        .revalidate()?
        .prepare_task_rooted_attachment("/firmware")
}

fn request() -> BootNamespaceRequest<'static> {
    BootNamespaceRequest::new("loader/entries/cast.conf", EXPECTED.len() as u64, xxh3_128(EXPECTED))
}

fn filesystem_evidence(
    device: u64,
    inode: u64,
    deadline: Instant,
) -> Result<ValidatedBootFilesystemDescriptorEvidence, BootFilesystemAuthenticationError> {
    let identity = FixtureBootFilesystemIdentity {
        device,
        inode,
        kind: nix::libc::S_IFDIR,
    };
    let observations = FixtureBootFilesystemObservations {
        opening_identity: identity,
        opening_magic: FIXTURE_MSDOS_SUPER_MAGIC,
        closing_magic: FIXTURE_MSDOS_SUPER_MAGIC,
        closing_identity: identity,
    };
    let mut clock = || deadline;
    let mut hook = |_phase: BootFilesystemObservationPhase| Ok(());
    validate_fixture_boot_filesystem_authentication(
        observations,
        device,
        inode,
        FixtureBootFilesystemLimits::default(),
        deadline,
        &mut clock,
        &mut hook,
    )
    .map(|(evidence, _usage)| evidence)
}

fn observed_root(view: &RevalidatedTaskRootedAttachment<'_>) -> BootNamespaceNodeIdentity {
    BootNamespaceNodeIdentity::new(
        view.destination_device(),
        view.destination_inode(),
        view.destination_mount_id(),
    )
}

fn payload(drops: &Rc<Cell<usize>>, states: Vec<BootNamespaceDestinationState>) -> FixtureNamespacePayload {
    FixtureNamespacePayload {
        states,
        drops: drops.clone(),
    }
}

#[test]
fn success_orders_stages_and_retains_only_exact_scalars_and_states() {
    let fixture = SyntheticMountNamespace::with_attachment(&["firmware"]).unwrap();
    let anchor = prepared_anchor(&fixture).unwrap();
    let attachment = prepared_attachment(&anchor).unwrap();
    let view = attachment.revalidate_against(&anchor).unwrap();
    let requests = [request()];
    let expected = [EXPECTED];
    let namespace_limits = BootNamespaceAssessmentLimits::default();
    let live_limits = RetainedBootNamespaceAssessmentLimits::default();
    let deadline = deadline();
    let events = RefCell::new(Vec::new());
    let drops = Rc::new(Cell::new(0usize));
    let clock_calls = Cell::new(0usize);
    let mut clock = || {
        clock_calls.set(clock_calls.get() + 1);
        deadline
    };

    let result = view
        .validate_fixture_retained_boot_namespace_with(
            &requests,
            &expected,
            namespace_limits,
            live_limits,
            deadline,
            |device, inode, received_deadline| {
                events.borrow_mut().push(Event::OpeningFilesystem);
                assert_eq!((device, inode), (view.destination_device(), view.destination_inode()));
                assert_eq!(received_deadline, deadline);
                filesystem_evidence(device, inode, received_deadline)
            },
            |received_requests, received_expected, received_namespace_limits, received_live_limits, received_deadline| {
                assert_eq!(*events.borrow(), vec![Event::OpeningFilesystem]);
                events.borrow_mut().push(Event::Namespace);
                assert_eq!(received_requests, &requests);
                assert_eq!(received_expected, &expected);
                assert_eq!(received_namespace_limits, namespace_limits);
                assert_eq!(received_live_limits, live_limits);
                assert_eq!(received_deadline, deadline);
                Ok(RevalidatedTaskRootedAttachment::fixture_retained_boot_namespace_assessment(
                    Some(observed_root(&view)),
                    payload(&drops, vec![BootNamespaceDestinationState::Exact]),
                ))
            },
            |device, inode, received_deadline| {
                assert_eq!(*events.borrow(), vec![Event::OpeningFilesystem, Event::Namespace]);
                events.borrow_mut().push(Event::ClosingFilesystem);
                assert_eq!((device, inode), (view.destination_device(), view.destination_inode()));
                assert_eq!(received_deadline, deadline);
                filesystem_evidence(device, inode, received_deadline)
            },
            &mut clock,
        )
        .unwrap();

    assert_eq!(
        *events.borrow(),
        vec![Event::OpeningFilesystem, Event::Namespace, Event::ClosingFilesystem]
    );
    assert_eq!(clock_calls.get(), 4);
    assert_eq!(result.destination_device(), view.destination_device());
    assert_eq!(result.destination_inode(), view.destination_inode());
    assert_eq!(result.destination_mount_id(), view.destination_mount_id());
    assert_eq!(
        result.boot_filesystem_magic_family(),
        BootFilesystemMagicFamily::LinuxMsdos
    );
    assert_eq!(result.payload().states, vec![BootNamespaceDestinationState::Exact]);
    assert_eq!(drops.get(), 0);
    drop(result);
    assert_eq!(drops.get(), 1);
    fixture.assert_outside_unchanged();
}

#[test]
fn opening_failure_skips_namespace_and_closing_without_a_result() {
    let fixture = SyntheticMountNamespace::with_attachment(&["firmware"]).unwrap();
    let anchor = prepared_anchor(&fixture).unwrap();
    let attachment = prepared_attachment(&anchor).unwrap();
    let view = attachment.revalidate_against(&anchor).unwrap();
    let requests = [request()];
    let expected = [EXPECTED];
    let calls = Cell::new(0usize);
    let deadline = deadline();
    let mut clock = || deadline;

    let error = view
        .validate_fixture_retained_boot_namespace_with::<FixtureNamespacePayload>(
            &requests,
            &expected,
            BootNamespaceAssessmentLimits::default(),
            RetainedBootNamespaceAssessmentLimits::default(),
            deadline,
            |_, _, _| {
                calls.set(calls.get() + 1);
                Err(BootFilesystemAuthenticationError::DeadlineExceeded { deadline })
            },
            |_, _, _, _, _| unreachable!("namespace must not run after opening failure"),
            |_, _, _| unreachable!("closing authentication must not run after opening failure"),
            &mut clock,
        )
        .unwrap_err();

    assert!(matches!(
        error,
        TaskRootBootNamespaceAssessmentError::OpeningBootFilesystem { .. }
    ));
    assert_eq!(calls.get(), 1);
    fixture.assert_outside_unchanged();
}

#[test]
fn namespace_failure_skips_closing_and_returns_no_result() {
    let fixture = SyntheticMountNamespace::with_attachment(&["firmware"]).unwrap();
    let anchor = prepared_anchor(&fixture).unwrap();
    let attachment = prepared_attachment(&anchor).unwrap();
    let view = attachment.revalidate_against(&anchor).unwrap();
    let requests = [request()];
    let expected = [EXPECTED];
    let closing_calls = Cell::new(0usize);
    let deadline = deadline();
    let mut clock = || deadline;

    let error = view
        .validate_fixture_retained_boot_namespace_with::<FixtureNamespacePayload>(
            &requests,
            &expected,
            BootNamespaceAssessmentLimits::default(),
            RetainedBootNamespaceAssessmentLimits::default(),
            deadline,
            filesystem_evidence,
            |_, _, _, _, _| {
                Err(RetainedBootNamespaceAssessmentError::ExpectedCountMismatch {
                    expected: 1,
                    found: 0,
                })
            },
            |_, _, _| {
                closing_calls.set(closing_calls.get() + 1);
                unreachable!("closing authentication must not run after namespace failure")
            },
            &mut clock,
        )
        .unwrap_err();

    assert!(matches!(
        error,
        TaskRootBootNamespaceAssessmentError::NamespaceAssessment { .. }
    ));
    assert_eq!(closing_calls.get(), 0);
    fixture.assert_outside_unchanged();
}

#[test]
fn closing_failure_discards_namespace_assessment() {
    let fixture = SyntheticMountNamespace::with_attachment(&["firmware"]).unwrap();
    let anchor = prepared_anchor(&fixture).unwrap();
    let attachment = prepared_attachment(&anchor).unwrap();
    let view = attachment.revalidate_against(&anchor).unwrap();
    let requests = [request()];
    let expected = [EXPECTED];
    let drops = Rc::new(Cell::new(0usize));
    let deadline = deadline();
    let mut clock = || deadline;

    let error = view
        .validate_fixture_retained_boot_namespace_with(
            &requests,
            &expected,
            BootNamespaceAssessmentLimits::default(),
            RetainedBootNamespaceAssessmentLimits::default(),
            deadline,
            filesystem_evidence,
            |_, _, _, _, _| {
                Ok(RevalidatedTaskRootedAttachment::fixture_retained_boot_namespace_assessment(
                    Some(observed_root(&view)),
                    payload(&drops, vec![BootNamespaceDestinationState::Absent]),
                ))
            },
            |_, _, _| Err(BootFilesystemAuthenticationError::DeadlineExceeded { deadline }),
            &mut clock,
        )
        .unwrap_err();

    assert!(matches!(
        error,
        TaskRootBootNamespaceAssessmentError::ClosingBootFilesystem { .. }
    ));
    assert_eq!(drops.get(), 1);
    fixture.assert_outside_unchanged();
}

#[test]
fn opening_and_closing_filesystem_drift_discards_namespace_assessment() {
    let fixture = SyntheticMountNamespace::with_attachment(&["firmware"]).unwrap();
    let anchor = prepared_anchor(&fixture).unwrap();
    let attachment = prepared_attachment(&anchor).unwrap();
    let view = attachment.revalidate_against(&anchor).unwrap();
    let requests = [request()];
    let expected = [EXPECTED];
    let drops = Rc::new(Cell::new(0usize));
    let deadline = deadline();
    let mut clock = || deadline;

    let error = view
        .validate_fixture_retained_boot_namespace_with(
            &requests,
            &expected,
            BootNamespaceAssessmentLimits::default(),
            RetainedBootNamespaceAssessmentLimits::default(),
            deadline,
            filesystem_evidence,
            |_, _, _, _, _| {
                Ok(RevalidatedTaskRootedAttachment::fixture_retained_boot_namespace_assessment(
                    Some(observed_root(&view)),
                    payload(&drops, vec![BootNamespaceDestinationState::Different]),
                ))
            },
            |device, inode, deadline| filesystem_evidence(device, inode + 1, deadline),
            &mut clock,
        )
        .unwrap_err();

    assert!(matches!(
        error,
        TaskRootBootNamespaceAssessmentError::BootFilesystemEvidenceDrift
    ));
    assert_eq!(drops.get(), 1);
    fixture.assert_outside_unchanged();
}

#[test]
fn identical_foreign_filesystem_identity_mismatches_discard_namespace_assessment() {
    let fixture = SyntheticMountNamespace::with_attachment(&["firmware"]).unwrap();
    let anchor = prepared_anchor(&fixture).unwrap();
    let attachment = prepared_attachment(&anchor).unwrap();
    let view = attachment.revalidate_against(&anchor).unwrap();
    let requests = [request()];
    let expected = [EXPECTED];
    let foreign_identities = [
        (view.destination_device() + 1, view.destination_inode()),
        (view.destination_device(), view.destination_inode() + 1),
    ];
    let drops = Rc::new(Cell::new(0usize));

    for (index, (foreign_device, foreign_inode)) in foreign_identities.into_iter().enumerate() {
        let deadline = deadline();
        let mut clock = || deadline;
        let error = view
            .validate_fixture_retained_boot_namespace_with(
                &requests,
                &expected,
                BootNamespaceAssessmentLimits::default(),
                RetainedBootNamespaceAssessmentLimits::default(),
                deadline,
                |_, _, deadline| filesystem_evidence(foreign_device, foreign_inode, deadline),
                |_, _, _, _, _| {
                    Ok(RevalidatedTaskRootedAttachment::fixture_retained_boot_namespace_assessment(
                        Some(observed_root(&view)),
                        payload(&drops, vec![BootNamespaceDestinationState::Exact]),
                    ))
                },
                |_, _, deadline| filesystem_evidence(foreign_device, foreign_inode, deadline),
                &mut clock,
            )
            .unwrap_err();

        match error {
            TaskRootBootNamespaceAssessmentError::BootFilesystemIdentityMismatch {
                expected_device,
                expected_inode,
                found_device,
                found_inode,
            } => {
                assert_eq!(expected_device, view.destination_device());
                assert_eq!(expected_inode, view.destination_inode());
                assert_eq!(found_device, foreign_device);
                assert_eq!(found_inode, foreign_inode);
            }
            other => panic!("expected foreign boot-filesystem identity rejection, found {other:?}"),
        }
        assert_eq!(drops.get(), index + 1);
    }
    fixture.assert_outside_unchanged();
}

#[test]
fn every_observed_root_scalar_mismatch_discards_namespace_assessment() {
    let fixture = SyntheticMountNamespace::with_attachment(&["firmware"]).unwrap();
    let anchor = prepared_anchor(&fixture).unwrap();
    let attachment = prepared_attachment(&anchor).unwrap();
    let view = attachment.revalidate_against(&anchor).unwrap();
    let requests = [request()];
    let expected = [EXPECTED];
    let roots = [
        BootNamespaceNodeIdentity::new(
            view.destination_device() + 1,
            view.destination_inode(),
            view.destination_mount_id(),
        ),
        BootNamespaceNodeIdentity::new(
            view.destination_device(),
            view.destination_inode() + 1,
            view.destination_mount_id(),
        ),
        BootNamespaceNodeIdentity::new(
            view.destination_device(),
            view.destination_inode(),
            view.destination_mount_id() + 1,
        ),
    ];
    let drops = Rc::new(Cell::new(0usize));

    for (index, root) in roots.into_iter().enumerate() {
        let deadline = deadline();
        let mut clock = || deadline;
        let error = view
            .validate_fixture_retained_boot_namespace_with(
                &requests,
                &expected,
                BootNamespaceAssessmentLimits::default(),
                RetainedBootNamespaceAssessmentLimits::default(),
                deadline,
                filesystem_evidence,
                |_, _, _, _, _| {
                    Ok(RevalidatedTaskRootedAttachment::fixture_retained_boot_namespace_assessment(
                        Some(root),
                        payload(&drops, vec![BootNamespaceDestinationState::Exact]),
                    ))
                },
                filesystem_evidence,
                &mut clock,
            )
            .unwrap_err();
        assert!(matches!(
            error,
            TaskRootBootNamespaceAssessmentError::ObservedRootIdentityMismatch { .. }
        ));
        assert_eq!(drops.get(), index + 1);
    }
    fixture.assert_outside_unchanged();
}

#[test]
fn missing_observed_root_discards_namespace_assessment() {
    let fixture = SyntheticMountNamespace::with_attachment(&["firmware"]).unwrap();
    let anchor = prepared_anchor(&fixture).unwrap();
    let attachment = prepared_attachment(&anchor).unwrap();
    let view = attachment.revalidate_against(&anchor).unwrap();
    let requests = [request()];
    let expected = [EXPECTED];
    let drops = Rc::new(Cell::new(0usize));
    let deadline = deadline();
    let mut clock = || deadline;

    let error = view
        .validate_fixture_retained_boot_namespace_with(
            &requests,
            &expected,
            BootNamespaceAssessmentLimits::default(),
            RetainedBootNamespaceAssessmentLimits::default(),
            deadline,
            filesystem_evidence,
            |_, _, _, _, _| {
                Ok(RevalidatedTaskRootedAttachment::fixture_retained_boot_namespace_assessment(
                    None,
                    payload(&drops, vec![BootNamespaceDestinationState::Exact]),
                ))
            },
            filesystem_evidence,
            &mut clock,
        )
        .unwrap_err();

    assert!(matches!(
        error,
        TaskRootBootNamespaceAssessmentError::MissingObservedRootIdentity
    ));
    assert_eq!(drops.get(), 1);
    fixture.assert_outside_unchanged();
}

#[test]
fn empty_request_set_is_rejected_before_clock_or_any_stage() {
    let fixture = SyntheticMountNamespace::with_attachment(&["firmware"]).unwrap();
    let anchor = prepared_anchor(&fixture).unwrap();
    let attachment = prepared_attachment(&anchor).unwrap();
    let view = attachment.revalidate_against(&anchor).unwrap();
    let requests: [BootNamespaceRequest<'static>; 0] = [];
    let expected: [&[u8]; 0] = [];
    let calls = Cell::new(0usize);
    let mut clock = || {
        calls.set(calls.get() + 1);
        Instant::now()
    };

    let error = view
        .validate_fixture_retained_boot_namespace_with::<FixtureNamespacePayload>(
            &requests,
            &expected,
            BootNamespaceAssessmentLimits::default(),
            RetainedBootNamespaceAssessmentLimits::default(),
            deadline(),
            |_, _, _| unreachable!("opening authentication must not run for an empty request set"),
            |_, _, _, _, _| unreachable!("namespace must not run for an empty request set"),
            |_, _, _| unreachable!("closing authentication must not run for an empty request set"),
            &mut clock,
        )
        .unwrap_err();

    assert!(matches!(
        error,
        TaskRootBootNamespaceAssessmentError::EmptyRequestSet
    ));
    assert_eq!(calls.get(), 0);
    fixture.assert_outside_unchanged();
}

#[test]
fn expiry_after_opening_skips_namespace_and_closing() {
    let fixture = SyntheticMountNamespace::with_attachment(&["firmware"]).unwrap();
    let anchor = prepared_anchor(&fixture).unwrap();
    let attachment = prepared_attachment(&anchor).unwrap();
    let view = attachment.revalidate_against(&anchor).unwrap();
    let requests = [request()];
    let expected = [EXPECTED];
    let deadline = deadline();
    let clock_calls = Cell::new(0usize);
    let events = RefCell::new(Vec::new());
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
        .validate_fixture_retained_boot_namespace_with::<FixtureNamespacePayload>(
            &requests,
            &expected,
            BootNamespaceAssessmentLimits::default(),
            RetainedBootNamespaceAssessmentLimits::default(),
            deadline,
            |device, inode, deadline| {
                events.borrow_mut().push(Event::OpeningFilesystem);
                filesystem_evidence(device, inode, deadline)
            },
            |_, _, _, _, _| unreachable!("namespace must not run after expiry following opening"),
            |_, _, _| unreachable!("closing authentication must not run after expiry following opening"),
            &mut clock,
        )
        .unwrap_err();

    assert!(matches!(
        error,
        TaskRootBootNamespaceAssessmentError::DeadlineExceeded { .. }
    ));
    assert_eq!(clock_calls.get(), 2);
    assert_eq!(*events.borrow(), vec![Event::OpeningFilesystem]);
    fixture.assert_outside_unchanged();
}

#[test]
fn expiry_after_namespace_discards_assessment_and_skips_closing() {
    let fixture = SyntheticMountNamespace::with_attachment(&["firmware"]).unwrap();
    let anchor = prepared_anchor(&fixture).unwrap();
    let attachment = prepared_attachment(&anchor).unwrap();
    let view = attachment.revalidate_against(&anchor).unwrap();
    let requests = [request()];
    let expected = [EXPECTED];
    let drops = Rc::new(Cell::new(0usize));
    let deadline = deadline();
    let clock_calls = Cell::new(0usize);
    let closing_calls = Cell::new(0usize);
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
        .validate_fixture_retained_boot_namespace_with(
            &requests,
            &expected,
            BootNamespaceAssessmentLimits::default(),
            RetainedBootNamespaceAssessmentLimits::default(),
            deadline,
            filesystem_evidence,
            |_, _, _, _, _| {
                Ok(RevalidatedTaskRootedAttachment::fixture_retained_boot_namespace_assessment(
                    Some(observed_root(&view)),
                    payload(&drops, vec![BootNamespaceDestinationState::Exact]),
                ))
            },
            |_, _, _| {
                closing_calls.set(closing_calls.get() + 1);
                unreachable!("closing authentication must not run after expiry following namespace")
            },
            &mut clock,
        )
        .unwrap_err();

    assert!(matches!(
        error,
        TaskRootBootNamespaceAssessmentError::DeadlineExceeded { .. }
    ));
    assert_eq!(clock_calls.get(), 3);
    assert_eq!(closing_calls.get(), 0);
    assert_eq!(drops.get(), 1);
    fixture.assert_outside_unchanged();
}

#[test]
fn terminal_deadline_expiry_discards_an_otherwise_complete_assessment() {
    let fixture = SyntheticMountNamespace::with_attachment(&["firmware"]).unwrap();
    let anchor = prepared_anchor(&fixture).unwrap();
    let attachment = prepared_attachment(&anchor).unwrap();
    let view = attachment.revalidate_against(&anchor).unwrap();
    let requests = [request()];
    let expected = [EXPECTED];
    let drops = Rc::new(Cell::new(0usize));
    let deadline = deadline();
    let clock_calls = Cell::new(0usize);
    let mut clock = || {
        let call = clock_calls.get() + 1;
        clock_calls.set(call);
        if call < 4 {
            deadline
        } else {
            deadline + Duration::from_secs(1)
        }
    };

    let error = view
        .validate_fixture_retained_boot_namespace_with(
            &requests,
            &expected,
            BootNamespaceAssessmentLimits::default(),
            RetainedBootNamespaceAssessmentLimits::default(),
            deadline,
            filesystem_evidence,
            |_, _, _, _, _| {
                Ok(RevalidatedTaskRootedAttachment::fixture_retained_boot_namespace_assessment(
                    Some(observed_root(&view)),
                    payload(&drops, vec![BootNamespaceDestinationState::Exact]),
                ))
            },
            filesystem_evidence,
            &mut clock,
        )
        .unwrap_err();

    assert!(matches!(
        error,
        TaskRootBootNamespaceAssessmentError::DeadlineExceeded { .. }
    ));
    assert_eq!(clock_calls.get(), 4);
    assert_eq!(drops.get(), 1);
    fixture.assert_outside_unchanged();
}
