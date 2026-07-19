use std::{
    cell::RefCell,
    io,
    rc::Rc,
    time::{Duration, Instant},
};

use super::super::super::{
    gpt_partition_device::{
        BlockDeviceObservation, BlockDeviceObserver, LiveAuthenticatedGptPartitionDeviceEvidence, ObservedDeviceAccess,
        ObservedNodeKind, authenticate_retained_gpt_partition_device_sources_fixture_with_interpass_until,
    },
    gpt_partition_role::{GptPartitionRole, GptPartitionRoleImage},
};

#[allow(dead_code)]
#[path = "../gpt_partition_role/support.rs"]
mod gpt_fixture;

const CONTAINING_DEVICE: u64 = 41;
const INODE: u64 = 52;
const MOUNT_ID: u64 = 63;
const PARENT_MAJOR: u32 = 8;
const PARENT_MINOR: u32 = 0;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Event {
    Observation(usize),
    ParentNameRebind,
    FirstPassRead,
    SecondPassRead,
}

struct EventObserver {
    events: Rc<RefCell<Vec<Event>>>,
    observations: Vec<BlockDeviceObservation>,
    calls: usize,
    fail_on_call: Option<(usize, io::ErrorKind)>,
}

impl EventObserver {
    fn stable(events: Rc<RefCell<Vec<Event>>>, observation: BlockDeviceObservation) -> Self {
        Self {
            events,
            observations: vec![observation; 3],
            calls: 0,
            fail_on_call: None,
        }
    }

    fn changing(
        events: Rc<RefCell<Vec<Event>>>,
        opening: BlockDeviceObservation,
        interpass: BlockDeviceObservation,
        closing: BlockDeviceObservation,
    ) -> Self {
        Self {
            events,
            observations: vec![opening, interpass, closing],
            calls: 0,
            fail_on_call: None,
        }
    }

    fn failing(
        events: Rc<RefCell<Vec<Event>>>,
        observation: BlockDeviceObservation,
        call: usize,
        kind: io::ErrorKind,
    ) -> Self {
        Self {
            events,
            observations: vec![observation; 3],
            calls: 0,
            fail_on_call: Some((call, kind)),
        }
    }
}

impl BlockDeviceObserver for EventObserver {
    fn observe_until(&mut self, _deadline: Instant) -> io::Result<BlockDeviceObservation> {
        let call = self.calls;
        self.calls += 1;
        self.events.borrow_mut().push(Event::Observation(call + 1));
        match self.fail_on_call {
            Some((failed, kind)) if failed == call => return Err(io::Error::from(kind)),
            _ => {}
        }
        self.observations
            .get(call)
            .copied()
            .ok_or_else(|| io::Error::other("unexpected extra live GPT-device observation"))
    }
}

struct EventImage<'a> {
    bytes: &'a [u8],
    event: Event,
    events: Rc<RefCell<Vec<Event>>>,
}

impl GptPartitionRoleImage for EventImage<'_> {
    fn length(&self) -> u64 {
        self.bytes.len().try_into().unwrap()
    }

    fn read(&mut self, offset: u64, output: &mut [u8]) -> io::Result<usize> {
        self.events.borrow_mut().push(self.event);
        let offset: usize = offset
            .try_into()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "fixture offset is not representable"))?;
        if offset >= self.bytes.len() || output.is_empty() {
            return Ok(0);
        }
        let count = output.len().min(self.bytes.len() - offset);
        output[..count].copy_from_slice(&self.bytes[offset..offset + count]);
        Ok(count)
    }
}

#[test]
fn live_coordinator_orders_rebind_and_three_observations_around_exact_gpt_passes() {
    let fixture = gpt_fixture::Fixture::esp(512);
    let events = Rc::new(RefCell::new(Vec::new()));
    let opening = observation(&fixture, INODE);
    let mut observer = EventObserver::stable(events.clone(), opening);
    let deadline = live_deadline();
    let mut rebind_calls = 0;
    let mut rebind = |received_deadline| {
        assert_eq!(received_deadline, deadline);
        rebind_calls += 1;
        events.borrow_mut().push(Event::ParentNameRebind);
        Ok(())
    };

    let evidence = authenticate(&fixture, &mut observer, events.clone(), deadline, &mut rebind).unwrap();

    assert_eq!(observer.calls, 3);
    assert_eq!(rebind_calls, 1);
    assert_eq!(evidence.containing_device(), CONTAINING_DEVICE);
    assert_eq!(evidence.inode(), INODE);
    assert_eq!(evidence.mount_id(), MOUNT_ID);
    assert_eq!(
        (evidence.parent_major(), evidence.parent_minor()),
        (PARENT_MAJOR, PARENT_MINOR)
    );
    assert_eq!(evidence.logical_block_size(), 512);
    assert_eq!(evidence.device_byte_length(), fixture.bytes.len() as u64);
    assert_eq!(evidence.partition_number(), 1);
    assert_eq!(evidence.partition_uuid(), gpt_fixture::ESP_UUID);
    assert_eq!(
        evidence.partition_start_bytes(),
        fixture.selected_start_lba * u64::from(fixture.block_size)
    );
    assert_eq!(
        evidence.partition_size_bytes(),
        fixture.selected_size_lba * u64::from(fixture.block_size)
    );
    assert_eq!(evidence.role(), GptPartitionRole::Esp);
    assert_ne!(evidence.table_sha256(), &[0_u8; 32]);
    require_exact_schedule(&events.borrow());
}

#[test]
fn parent_name_rebind_failure_prevents_interpass_observation_and_pass_two() {
    let fixture = gpt_fixture::Fixture::esp(512);
    let events = Rc::new(RefCell::new(Vec::new()));
    let mut observer = EventObserver::stable(events.clone(), observation(&fixture, INODE));
    let mut rebind = |_deadline| {
        events.borrow_mut().push(Event::ParentNameRebind);
        Err(io::Error::from(io::ErrorKind::PermissionDenied))
    };

    let error = authenticate(&fixture, &mut observer, events.clone(), live_deadline(), &mut rebind).unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    assert_eq!(observer.calls, 1);
    assert!(events.borrow().contains(&Event::FirstPassRead));
    assert!(!events.borrow().contains(&Event::SecondPassRead));
    assert!(!events.borrow().contains(&Event::Observation(2)));
}

#[test]
fn wrong_sysfs_parent_is_rejected_before_any_gpt_source_read() {
    let fixture = gpt_fixture::Fixture::esp(512);
    let events = Rc::new(RefCell::new(Vec::new()));
    let mut observer = EventObserver::stable(events.clone(), observation(&fixture, INODE));
    let mut rebind = |_deadline| {
        events.borrow_mut().push(Event::ParentNameRebind);
        Ok(())
    };
    let mut first_source = EventImage {
        bytes: &fixture.bytes,
        event: Event::FirstPassRead,
        events: events.clone(),
    };
    let mut second_source = EventImage {
        bytes: &fixture.bytes,
        event: Event::SecondPassRead,
        events: events.clone(),
    };

    let error = authenticate_retained_gpt_partition_device_sources_fixture_with_interpass_until(
        &mut observer,
        &mut first_source,
        &mut second_source,
        PARENT_MAJOR + 1,
        PARENT_MINOR,
        1,
        fixture.selected_uuid,
        fixture.selected_start_lba,
        fixture.selected_size_lba,
        fixture.selected_role,
        live_deadline(),
        &mut rebind,
    )
    .unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert_eq!(observer.calls, 1);
    assert_eq!(&*events.borrow(), &[Event::Observation(1)]);
}

#[test]
fn interpass_observation_error_propagates_without_pass_two_or_retry() {
    let fixture = gpt_fixture::Fixture::esp(512);
    let events = Rc::new(RefCell::new(Vec::new()));
    let mut observer = EventObserver::failing(
        events.clone(),
        observation(&fixture, INODE),
        1,
        io::ErrorKind::Interrupted,
    );
    let mut rebind = |_deadline| {
        events.borrow_mut().push(Event::ParentNameRebind);
        Ok(())
    };

    let error = authenticate(&fixture, &mut observer, events.clone(), live_deadline(), &mut rebind).unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    assert_eq!(observer.calls, 2);
    assert!(!events.borrow().contains(&Event::SecondPassRead));
    assert_eq!(
        events
            .borrow()
            .iter()
            .filter(|event| **event == Event::Observation(2))
            .count(),
        1
    );
}

#[test]
fn interpass_identity_or_geometry_drift_prevents_every_pass_two_read() {
    let fixture = gpt_fixture::Fixture::esp(512);
    let events = Rc::new(RefCell::new(Vec::new()));
    let opening = observation(&fixture, INODE);
    let changed = observation(&fixture, INODE + 1);
    let mut observer = EventObserver::changing(events.clone(), opening, changed, opening);
    let mut rebind = |_deadline| {
        events.borrow_mut().push(Event::ParentNameRebind);
        Ok(())
    };

    let error = authenticate(&fixture, &mut observer, events.clone(), live_deadline(), &mut rebind).unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert_eq!(observer.calls, 2);
    assert!(!events.borrow().contains(&Event::SecondPassRead));
    assert!(!events.borrow().contains(&Event::Observation(3)));
}

#[test]
fn closing_drift_is_rejected_without_any_hidden_reconciliation_observation() {
    let fixture = gpt_fixture::Fixture::esp(512);
    let events = Rc::new(RefCell::new(Vec::new()));
    let opening = observation(&fixture, INODE);
    let changed = observation(&fixture, INODE + 1);
    let mut observer = EventObserver::changing(events.clone(), opening, opening, changed);
    let mut rebind = |_deadline| {
        events.borrow_mut().push(Event::ParentNameRebind);
        Ok(())
    };

    let error = authenticate(&fixture, &mut observer, events.clone(), live_deadline(), &mut rebind).unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert_eq!(observer.calls, 3);
    assert!(events.borrow().contains(&Event::SecondPassRead));
    assert_eq!(
        events
            .borrow()
            .iter()
            .filter(|event| matches!(event, Event::Observation(_)))
            .count(),
        3
    );
}

#[test]
fn interpass_timeout_propagates_and_prevents_pass_two_and_closing_work() {
    let fixture = gpt_fixture::Fixture::esp(512);
    let events = Rc::new(RefCell::new(Vec::new()));
    let mut observer = EventObserver::stable(events.clone(), observation(&fixture, INODE));
    let mut rebind = |_deadline| {
        events.borrow_mut().push(Event::ParentNameRebind);
        Err(io::Error::from(io::ErrorKind::TimedOut))
    };

    let error = authenticate(&fixture, &mut observer, events.clone(), live_deadline(), &mut rebind).unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert_eq!(observer.calls, 1);
    assert!(!events.borrow().contains(&Event::SecondPassRead));
    assert!(!events.borrow().contains(&Event::Observation(2)));
}

#[test]
fn expired_initial_deadline_fails_before_observation_rebind_or_read() {
    let fixture = gpt_fixture::Fixture::esp(512);
    let events = Rc::new(RefCell::new(Vec::new()));
    let mut observer = EventObserver::stable(events.clone(), observation(&fixture, INODE));
    let mut rebind = |_deadline| {
        events.borrow_mut().push(Event::ParentNameRebind);
        Ok(())
    };

    let error = authenticate(
        &fixture,
        &mut observer,
        events.clone(),
        Instant::now() - Duration::from_secs(1),
        &mut rebind,
    )
    .unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert_eq!(observer.calls, 0);
    assert!(events.borrow().is_empty());
}

fn authenticate(
    fixture: &gpt_fixture::Fixture,
    observer: &mut EventObserver,
    events: Rc<RefCell<Vec<Event>>>,
    deadline: Instant,
    rebind: &mut impl FnMut(Instant) -> io::Result<()>,
) -> io::Result<LiveAuthenticatedGptPartitionDeviceEvidence> {
    let mut first_source = EventImage {
        bytes: &fixture.bytes,
        event: Event::FirstPassRead,
        events: events.clone(),
    };
    let mut second_source = EventImage {
        bytes: &fixture.bytes,
        event: Event::SecondPassRead,
        events,
    };
    authenticate_retained_gpt_partition_device_sources_fixture_with_interpass_until(
        observer,
        &mut first_source,
        &mut second_source,
        PARENT_MAJOR,
        PARENT_MINOR,
        1,
        fixture.selected_uuid,
        fixture.selected_start_lba,
        fixture.selected_size_lba,
        fixture.selected_role,
        deadline,
        rebind,
    )
}

fn observation(fixture: &gpt_fixture::Fixture, inode: u64) -> BlockDeviceObservation {
    BlockDeviceObservation::new(
        ObservedNodeKind::BlockDevice,
        ObservedDeviceAccess::ReadOnly,
        CONTAINING_DEVICE,
        inode,
        MOUNT_ID,
        PARENT_MAJOR,
        PARENT_MINOR,
        fixture.block_size,
        fixture.bytes.len().try_into().unwrap(),
    )
}

fn require_exact_schedule(events: &[Event]) {
    assert_eq!(events.first(), Some(&Event::Observation(1)));
    assert_eq!(events.last(), Some(&Event::Observation(3)));
    let rebind = events
        .iter()
        .position(|event| *event == Event::ParentNameRebind)
        .unwrap();
    let interpass = events.iter().position(|event| *event == Event::Observation(2)).unwrap();
    let closing = events.iter().position(|event| *event == Event::Observation(3)).unwrap();
    assert!(events[1..rebind].iter().all(|event| *event == Event::FirstPassRead));
    assert!(rebind > 1);
    assert_eq!(interpass, rebind + 1);
    assert!(
        events[interpass + 1..closing]
            .iter()
            .all(|event| *event == Event::SecondPassRead)
    );
    assert!(closing > interpass + 1);
}

fn live_deadline() -> Instant {
    Instant::now() + Duration::from_secs(30)
}
