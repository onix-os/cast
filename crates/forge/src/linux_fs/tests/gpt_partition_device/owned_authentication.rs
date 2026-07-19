use std::{
    cell::RefCell,
    io,
    rc::Rc,
    time::{Duration, Instant},
};

use super::super::super::{
    gpt_partition_device::authenticate_owned_gpt_parent_fixture_until, gpt_partition_role::GptPartitionRole,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Event {
    AuthenticationStarted { role: GptPartitionRole, deadline: Instant },
    SameDescriptorObserverBorrowed,
    FirstPassAuthenticated,
    MandatoryNameRebind,
    SecondPassAuthenticated,
    SameDescriptorObserverDropped,
    ClosingNameRebind { deadline: Instant },
    ParentAuthorityReleased,
    EvidenceDiscarded,
}

struct FixtureParent {
    events: Rc<RefCell<Vec<Event>>>,
}

impl Drop for FixtureParent {
    fn drop(&mut self) {
        self.events.borrow_mut().push(Event::ParentAuthorityReleased);
    }
}

struct FixtureEvidence {
    events: Rc<RefCell<Vec<Event>>>,
}

impl Drop for FixtureEvidence {
    fn drop(&mut self) {
        self.events.borrow_mut().push(Event::EvidenceDiscarded);
    }
}

#[test]
fn owned_composition_releases_evidence_only_after_observer_drop_and_consuming_close() {
    let events = Rc::new(RefCell::new(Vec::new()));
    let deadline = Instant::now() + Duration::from_secs(5);
    let parent = FixtureParent { events: events.clone() };
    let mut authenticate = |parent: &FixtureParent, role, received_deadline| {
        record_successful_authentication(&parent.events, role, received_deadline);
        Ok(FixtureEvidence {
            events: parent.events.clone(),
        })
    };
    let mut close = |parent: FixtureParent, received_deadline| {
        assert_eq!(
            parent.events.borrow().last(),
            Some(&Event::SameDescriptorObserverDropped)
        );
        parent.events.borrow_mut().push(Event::ClosingNameRebind {
            deadline: received_deadline,
        });
        drop(parent);
        Ok(())
    };

    let evidence = authenticate_owned_gpt_parent_fixture_until(
        parent,
        GptPartitionRole::Esp,
        deadline,
        &mut authenticate,
        &mut close,
    )
    .unwrap();

    assert_eq!(
        *events.borrow(),
        successful_schedule(GptPartitionRole::Esp, deadline, false)
    );
    drop(evidence);
    assert_eq!(events.borrow().last(), Some(&Event::EvidenceDiscarded));
}

#[test]
fn authentication_failure_releases_parent_without_running_terminal_close() {
    let events = Rc::new(RefCell::new(Vec::new()));
    let deadline = Instant::now() + Duration::from_secs(5);
    let parent = FixtureParent { events: events.clone() };
    let mut authenticate = |parent: &FixtureParent, role, received_deadline| -> io::Result<FixtureEvidence> {
        parent.events.borrow_mut().push(Event::AuthenticationStarted {
            role,
            deadline: received_deadline,
        });
        parent.events.borrow_mut().push(Event::SameDescriptorObserverBorrowed);
        parent.events.borrow_mut().push(Event::SameDescriptorObserverDropped);
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "injected authentication failure",
        ))
    };
    let mut close = |_parent: FixtureParent, _deadline| -> io::Result<()> {
        panic!("terminal close must not run after authentication failed")
    };

    let error = authenticate_owned_gpt_parent_fixture_until(
        parent,
        GptPartitionRole::Xbootldr,
        deadline,
        &mut authenticate,
        &mut close,
    )
    .err()
    .unwrap();

    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    assert_eq!(
        *events.borrow(),
        vec![
            Event::AuthenticationStarted {
                role: GptPartitionRole::Xbootldr,
                deadline,
            },
            Event::SameDescriptorObserverBorrowed,
            Event::SameDescriptorObserverDropped,
            Event::ParentAuthorityReleased,
        ]
    );
}

#[test]
fn terminal_close_failure_discards_authenticated_evidence() {
    let events = Rc::new(RefCell::new(Vec::new()));
    let deadline = Instant::now() + Duration::from_secs(5);
    let parent = FixtureParent { events: events.clone() };
    let mut authenticate = |parent: &FixtureParent, role, received_deadline| {
        record_successful_authentication(&parent.events, role, received_deadline);
        Ok(FixtureEvidence {
            events: parent.events.clone(),
        })
    };
    let mut close = |parent: FixtureParent, received_deadline| {
        parent.events.borrow_mut().push(Event::ClosingNameRebind {
            deadline: received_deadline,
        });
        drop(parent);
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "injected closing rebind failure",
        ))
    };

    let error = authenticate_owned_gpt_parent_fixture_until(
        parent,
        GptPartitionRole::Esp,
        deadline,
        &mut authenticate,
        &mut close,
    )
    .err()
    .unwrap();

    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert_eq!(
        *events.borrow(),
        successful_schedule(GptPartitionRole::Esp, deadline, true)
    );
}

fn record_successful_authentication(events: &Rc<RefCell<Vec<Event>>>, role: GptPartitionRole, deadline: Instant) {
    events.borrow_mut().extend([
        Event::AuthenticationStarted { role, deadline },
        Event::SameDescriptorObserverBorrowed,
        Event::FirstPassAuthenticated,
        Event::MandatoryNameRebind,
        Event::SecondPassAuthenticated,
        Event::SameDescriptorObserverDropped,
    ]);
}

fn successful_schedule(role: GptPartitionRole, deadline: Instant, evidence_discarded: bool) -> Vec<Event> {
    let mut expected = vec![
        Event::AuthenticationStarted { role, deadline },
        Event::SameDescriptorObserverBorrowed,
        Event::FirstPassAuthenticated,
        Event::MandatoryNameRebind,
        Event::SecondPassAuthenticated,
        Event::SameDescriptorObserverDropped,
        Event::ClosingNameRebind { deadline },
        Event::ParentAuthorityReleased,
    ];
    if evidence_discarded {
        expected.push(Event::EvidenceDiscarded);
    }
    expected
}
