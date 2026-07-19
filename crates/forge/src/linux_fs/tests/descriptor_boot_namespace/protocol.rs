use super::{support::*, *};
use std::time::{Duration, Instant};

fn root_event() -> FixtureBootNamespaceProtocolEvent {
    FixtureBootNamespaceProtocolEvent::Root { identity: ROOT }
}

fn lookup_event(
    directory: BootNamespaceNodeIdentity,
    requested_name: &[u8],
    boundary: BootNamespaceObservationBoundary,
    request_index: usize,
    component_index: usize,
) -> FixtureBootNamespaceProtocolEvent {
    FixtureBootNamespaceProtocolEvent::Lookup {
        directory,
        requested_name: requested_name.to_vec(),
        boundary,
        request_index,
        component_index,
    }
}

fn release_event(identity: BootNamespaceNodeIdentity) -> FixtureBootNamespaceProtocolEvent {
    FixtureBootNamespaceProtocolEvent::Release { identity }
}

#[test]
fn nested_protocol_carries_indices_and_releases_in_lifo_order() {
    let expected = b"entry";
    let requests = [request("loader/entry.conf", expected)];
    let mut fixture = nested_file_fixture(expected, expected);

    let (assessment, usage) = assess(&requests, &mut fixture).unwrap();

    assert_eq!(assessment.states(), &[BootNamespaceDestinationState::Exact]);
    assert_eq!(usage.peak_descriptors, 3);
    assert_eq!(fixture.peak_retained_nodes(), 3);
    assert_eq!(fixture.retained_node_count(), 0);
    assert_eq!(
        fixture.protocol_events(),
        &[
            root_event(),
            lookup_event(ROOT, b"loader", BootNamespaceObservationBoundary::Opening, 0, 0,),
            lookup_event(
                DIRECTORY_A,
                b"entry.conf",
                BootNamespaceObservationBoundary::Opening,
                0,
                1,
            ),
            release_event(NESTED_FILE),
            lookup_event(
                DIRECTORY_A,
                b"entry.conf",
                BootNamespaceObservationBoundary::Closing,
                0,
                1,
            ),
            release_event(DIRECTORY_A),
            lookup_event(ROOT, b"loader", BootNamespaceObservationBoundary::Closing, 0, 0,),
            release_event(ROOT),
        ]
    );
}

#[test]
fn failed_content_protocol_releases_every_retained_node() {
    let expected = b"entry";
    let requests = [request("loader/entry.conf", expected)];
    let mut fixture = nested_file_fixture(expected, b"wrong");

    let error = assess(&requests, &mut fixture).unwrap_err();

    assert!(matches!(
        error,
        BootNamespaceAssessmentError::ExpectedContentProtocolViolation { request_index: 0 }
    ));
    assert_eq!(fixture.peak_retained_nodes(), 3);
    assert_eq!(fixture.retained_node_count(), 0);
    assert_eq!(
        fixture.protocol_events(),
        &[
            root_event(),
            lookup_event(ROOT, b"loader", BootNamespaceObservationBoundary::Opening, 0, 0,),
            lookup_event(
                DIRECTORY_A,
                b"entry.conf",
                BootNamespaceObservationBoundary::Opening,
                0,
                1,
            ),
            release_event(NESTED_FILE),
            release_event(DIRECTORY_A),
            release_event(ROOT),
        ]
    );
}

#[test]
fn descriptor_limit_preflight_blocks_n_plus_one_lookup_and_unwinds() {
    let expected = b"entry";
    let requests = [request("loader/entry.conf", expected)];
    let mut fixture = nested_file_fixture(expected, expected);
    let mut limits = BootNamespaceAssessmentLimits::default();
    limits.max_descriptors = 2;

    let error = assess_with_limits(&requests, limits, &mut fixture).unwrap_err();

    assert!(matches!(
        error,
        BootNamespaceAssessmentError::DescriptorLimitExceeded {
            limit: 2,
            request_index: 0,
            component_index: 1,
        }
    ));
    assert_eq!(fixture.peak_retained_nodes(), 2);
    assert_eq!(fixture.retained_node_count(), 0);
    assert_eq!(
        fixture.protocol_events(),
        &[
            root_event(),
            lookup_event(ROOT, b"loader", BootNamespaceObservationBoundary::Opening, 0, 0,),
            release_event(DIRECTORY_A),
            release_event(ROOT),
        ]
    );
}

#[test]
fn late_deadline_after_retained_callback_releases_root_state() {
    let now = Instant::now();
    let deadline = now + Duration::from_secs(1);
    let expected = b"entry";
    let requests = [request("missing", expected)];
    let mut fixture = FixtureBootNamespace::new(
        ROOT,
        vec![FixtureDirectory::stable(ROOT, Vec::new())],
        Vec::new(),
        vec![FixtureExpectedStream::new(expected)],
        now,
    )
    .expire_while_retained(deadline + Duration::from_nanos(1));

    let error = assess_fixture_boot_namespace_until(
        &requests,
        BootNamespaceAssessmentLimits::default(),
        deadline,
        &mut fixture,
    )
    .unwrap_err();

    assert!(matches!(error, BootNamespaceAssessmentError::DeadlineExceeded { .. }));
    assert_eq!(fixture.peak_retained_nodes(), 1);
    assert_eq!(fixture.retained_node_count(), 0);
    assert_eq!(fixture.protocol_events(), &[root_event(), release_event(ROOT)]);
}
